//! HTTP streaming acquisition.
//!
//! An HTTP source follows an endpoint and captures its response body bytes
//! losslessly into the ordinary journal, exactly like file, command and stdin
//! capture. Framing (newline records or Server-Sent Events) is an
//! *interpretation layered on top of* the retained bytes: `bytes + delimiter`
//! concatenated always reproduces the original body byte for byte.
//!
//! Reconnection is explicit and bounded. Every attempt, rejection, disconnect,
//! oversized frame and capture gap is published as source history, and each
//! connection gets a fresh acquisition identity so stored history can never
//! imply uninterrupted capture across a gap. Where the protocol offers a resume
//! point — SSE `Last-Event-ID`, or an HTTP byte `Range` on a server that
//! advertises `Accept-Ranges: bytes` — it is used, and the boundary is recorded
//! as resumed. Otherwise the boundary is recorded as a possible gap.
//!
//! Credentials never reach status or diagnostics: header values are redacted by
//! construction and the endpoint is published with userinfo and query removed.

use crate::{
    HttpFraming, HttpHeader, HttpLimits, ReconnectPolicy, StreamKind,
    acquisition::{
        BoundaryReason, Capture, CaptureCompletion, CaptureEvent, CaptureLimits, CapturedRecord,
        ChunkPosition, Framer, capture_error, capture_now, emit, emit_records, spawn_capture,
    },
    restart::{AttemptWindow, Backoff, Jitter},
    source_event::{
        DisconnectReason, MAXIMUM_DETAIL_BYTES, ResumeMode, SourceEvent, SourceEventSink,
        bounded_detail,
    },
};
use std::{io, time::Duration};
use tokio::sync::{mpsc, watch};
use uuid::Uuid;

/// Everything capture needs to follow one HTTP endpoint.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HttpAcquisition {
    pub url: String,
    pub framing: HttpFraming,
    pub reconnect: ReconnectPolicy,
    pub headers: Vec<HttpHeader>,
    pub limits: HttpLimits,
}

/// Removes credentials from a URL so it can be shown in status and history.
/// Userinfo and the whole query string are dropped; scheme, host, port and path
/// are retained because they are what a user needs to recognise the source.
pub fn redact_endpoint(url: &str) -> String {
    match reqwest::Url::parse(url) {
        Ok(parsed) => {
            let mut text = String::new();
            text.push_str(parsed.scheme());
            text.push_str("://");
            if !parsed.username().is_empty() || parsed.password().is_some() {
                text.push_str("<redacted>@");
            }
            if let Some(host) = parsed.host_str() {
                text.push_str(host);
            }
            if let Some(port) = parsed.port() {
                text.push(':');
                text.push_str(&port.to_string());
            }
            text.push_str(parsed.path());
            if parsed.query().is_some() {
                text.push_str("?<redacted>");
            }
            bounded_detail(text)
        }
        // An unparseable URL is refused before capture starts; this path only
        // exists so diagnostics never fall back to printing raw credentials.
        Err(_) => "<unparsed endpoint>".to_owned(),
    }
}

/// Starts an HTTP capture. Configuration errors (bad URL, unusable header,
/// zero bound) are reported synchronously; everything after the first request
/// is reported through capture events and source history.
pub fn capture_http(acquisition: HttpAcquisition, limits: CaptureLimits) -> io::Result<Capture> {
    let url = reqwest::Url::parse(&acquisition.url).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("invalid HTTP source URL: {error}"),
        )
    })?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "HTTP sources support only http and https URLs",
        ));
    }
    let http = validate_limits(&acquisition.limits)?;
    let headers = build_headers(&acquisition.headers)?;
    let client = reqwest::Client::builder()
        .connect_timeout(http.connect_timeout)
        .build()
        .map_err(|error| io::Error::other(format!("HTTP client setup failed: {error}")))?;
    let endpoint = redact_endpoint(&acquisition.url);
    spawn_capture(limits, move |events, history, cancelled, stopped| {
        run_http(
            HttpSession {
                client,
                url,
                headers,
                endpoint,
                framing: acquisition.framing,
                reconnect: acquisition.reconnect,
                http,
                limits,
            },
            events,
            history,
            cancelled,
            stopped,
        )
    })
}

fn validate_limits(limits: &HttpLimits) -> io::Result<HttpLimits> {
    if limits.maximum_frame_bytes == 0
        || limits.maximum_pending_bytes == 0
        || limits.connect_timeout.is_zero()
        || limits.read_timeout.is_zero()
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "HTTP limits must be nonzero",
        ));
    }
    Ok(*limits)
}

fn build_headers(headers: &[HttpHeader]) -> io::Result<reqwest::header::HeaderMap> {
    let mut map = reqwest::header::HeaderMap::new();
    for header in headers {
        // Only the name is ever reported; an invalid value must not be echoed.
        let name = reqwest::header::HeaderName::from_bytes(header.name.as_bytes())
            .map_err(|_| invalid_header(&header.name, "name is not a valid HTTP header"))?;
        let value = reqwest::header::HeaderValue::from_str(&header.value)
            .map_err(|_| invalid_header(&header.name, "value is not a valid header value"))?;
        map.append(name, value);
    }
    Ok(map)
}

fn invalid_header(name: &str, detail: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        format!("HTTP header {}: {detail}", bounded_detail(name)),
    )
}

struct HttpSession {
    client: reqwest::Client,
    url: reqwest::Url,
    headers: reqwest::header::HeaderMap,
    endpoint: String,
    framing: HttpFraming,
    reconnect: ReconnectPolicy,
    http: HttpLimits,
    limits: CaptureLimits,
}

/// What the previous connection left behind for the next one.
#[derive(Default)]
struct ResumeState {
    last_event_id: Option<String>,
    received_bytes: u64,
    range_supported: bool,
}

enum ConnectionEnd {
    /// The connection ended and capture may consider reconnecting.
    Ended {
        reason: DisconnectReason,
        /// Whether this connection delivered any body byte; a connection that
        /// made progress resets nothing, but it is reported.
        body_bytes: u64,
    },
    /// Capture was cancelled; nothing more may be emitted.
    Aborted { discarded_bytes: usize },
    /// A graceful stop drained the current connection.
    Stopped,
}

async fn run_http(
    session: HttpSession,
    events: mpsc::Sender<CaptureEvent>,
    history: SourceEventSink,
    mut cancelled: watch::Receiver<bool>,
    mut stopped: watch::Receiver<bool>,
) -> CaptureCompletion {
    let backoff = Backoff {
        initial: session.reconnect.delay,
        maximum: session.reconnect.maximum_delay,
        jitter_percent: session.reconnect.jitter_percent,
    };
    let mut window = AttemptWindow::new(
        session.reconnect.maximum_attempts,
        session.reconnect.attempt_window,
    );
    let mut jitter = Jitter::from_entropy();
    let mut resume = ResumeState::default();
    let mut previous: Option<Uuid> = None;
    let mut connection = 0_u32;

    loop {
        connection = connection.saturating_add(1);
        let acquisition_id = Uuid::new_v4();
        let mode = resume_mode(&session, &resume, previous.is_none());
        history.emit(SourceEvent::HttpConnecting {
            acquisition_id,
            attempt: connection,
            endpoint: session.endpoint.clone(),
            resume: mode.clone(),
        });
        if let Some(previous_acquisition_id) = previous {
            // The boundary is published before any record of the new
            // connection so no reader can read across it as continuous.
            history.emit(SourceEvent::CaptureGap {
                previous_acquisition_id,
                acquisition_id,
                resume: mode.clone(),
            });
        }
        if !emit(
            &events,
            &mut cancelled,
            CaptureEvent::Boundary {
                acquisition_id,
                reason: BoundaryReason::Started,
            },
        )
        .await
        {
            return CaptureCompletion {
                aborted: true,
                discarded_buffered_bytes: 0,
            };
        }
        previous = Some(acquisition_id);

        match connect_and_stream(
            &session,
            acquisition_id,
            &mode,
            &mut resume,
            &events,
            &history,
            &mut cancelled,
            &mut stopped,
        )
        .await
        {
            ConnectionEnd::Aborted { discarded_bytes } => {
                return CaptureCompletion {
                    aborted: true,
                    discarded_buffered_bytes: discarded_bytes,
                };
            }
            ConnectionEnd::Stopped => return CaptureCompletion::default(),
            ConnectionEnd::Ended { reason, body_bytes } => {
                history.emit(SourceEvent::HttpDisconnected {
                    acquisition_id,
                    reason,
                    body_bytes,
                });
            }
        }

        if !session.reconnect.enabled {
            history.emit(SourceEvent::ReconnectDisabled);
            return CaptureCompletion::default();
        }
        let now = std::time::Instant::now();
        if !window.permits(now) {
            history.emit(SourceEvent::RetriesExhausted {
                attempts: window.used(now),
                window_millis: duration_millis(window.window()),
            });
            let _ = emit(
                &events,
                &mut cancelled,
                capture_error(
                    acquisition_id,
                    format!(
                        "HTTP reconnect budget exhausted after {} attempts",
                        window.used(now)
                    ),
                ),
            )
            .await;
            return CaptureCompletion::default();
        }
        window.record(now);
        let attempt = window.used(now);
        let delay = backoff.delay(attempt, &mut jitter);
        history.emit(SourceEvent::RetryScheduled {
            attempt,
            maximum_attempts: window.maximum(),
            delay_millis: duration_millis(delay),
        });
        if !wait_cancellable(delay, &mut cancelled, &mut stopped).await {
            return CaptureCompletion {
                aborted: *cancelled.borrow(),
                discarded_buffered_bytes: 0,
            };
        }
    }
}

fn resume_mode(session: &HttpSession, resume: &ResumeState, first: bool) -> ResumeMode {
    if first {
        return ResumeMode::Initial;
    }
    if !session.reconnect.resume {
        return ResumeMode::None;
    }
    match session.framing {
        HttpFraming::Sse => match &resume.last_event_id {
            Some(id) => ResumeMode::LastEventId { id: id.clone() },
            None => ResumeMode::None,
        },
        HttpFraming::Newline => {
            if resume.range_supported && resume.received_bytes > 0 {
                ResumeMode::ByteRange {
                    offset: resume.received_bytes,
                }
            } else {
                ResumeMode::None
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn connect_and_stream(
    session: &HttpSession,
    acquisition_id: Uuid,
    mode: &ResumeMode,
    resume: &mut ResumeState,
    events: &mpsc::Sender<CaptureEvent>,
    history: &SourceEventSink,
    cancelled: &mut watch::Receiver<bool>,
    stopped: &mut watch::Receiver<bool>,
) -> ConnectionEnd {
    let mut request = session
        .client
        .get(session.url.clone())
        .headers(session.headers.clone());
    match mode {
        ResumeMode::LastEventId { id } => {
            match reqwest::header::HeaderValue::from_str(id) {
                Ok(value) => request = request.header("last-event-id", value),
                // An event ID the server itself produced but that cannot be
                // returned as a header is not a resume point.
                Err(_) => resume.last_event_id = None,
            }
        }
        ResumeMode::ByteRange { offset } => {
            request = request.header("range", format!("bytes={offset}-"));
        }
        ResumeMode::Initial | ResumeMode::None => {}
    }

    let sent = tokio::select! {
        biased;
        _ = cancelled.changed() => return ConnectionEnd::Aborted { discarded_bytes: 0 },
        _ = stopped.changed() => return ConnectionEnd::Stopped,
        result = tokio::time::timeout(session.http.connect_timeout, request.send()) => result,
    };
    let response = match sent {
        Err(_) => {
            let detail = "no response headers within the connect timeout";
            let _ = emit(events, cancelled, capture_error(acquisition_id, detail)).await;
            return ConnectionEnd::Ended {
                reason: DisconnectReason::Transport {
                    detail: detail.to_owned(),
                },
                body_bytes: 0,
            };
        }
        Ok(Err(error)) => {
            let detail = transport_detail(&error);
            let _ = emit(events, cancelled, capture_error(acquisition_id, &detail)).await;
            return ConnectionEnd::Ended {
                reason: DisconnectReason::Transport { detail },
                body_bytes: 0,
            };
        }
        Ok(Ok(response)) => response,
    };

    let status = response.status();
    if !status.is_success() {
        let detail = format!("HTTP {} from {}", status.as_u16(), session.endpoint);
        history.emit(SourceEvent::HttpRejected {
            acquisition_id,
            status: status.as_u16(),
            detail: detail.clone(),
        });
        let _ = emit(events, cancelled, capture_error(acquisition_id, &detail)).await;
        return ConnectionEnd::Ended {
            reason: DisconnectReason::Transport { detail },
            body_bytes: 0,
        };
    }

    let range_supported = response
        .headers()
        .get(reqwest::header::ACCEPT_RANGES)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.eq_ignore_ascii_case("bytes"));
    let partial = status == reqwest::StatusCode::PARTIAL_CONTENT;
    let resumed = match mode {
        ResumeMode::ByteRange { .. } => partial,
        ResumeMode::LastEventId { .. } => true,
        ResumeMode::Initial | ResumeMode::None => false,
    };
    if matches!(mode, ResumeMode::ByteRange { .. }) && !partial {
        // The server ignored the range and is replaying from the start; the
        // byte cursor must not be treated as a continuation point.
        resume.received_bytes = 0;
    }
    resume.range_supported = range_supported;
    history.emit(SourceEvent::HttpConnected {
        acquisition_id,
        status: status.as_u16(),
        resumed,
        range_supported,
    });

    stream_body(
        session,
        acquisition_id,
        response,
        resume,
        events,
        history,
        cancelled,
        stopped,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn stream_body(
    session: &HttpSession,
    acquisition_id: Uuid,
    mut response: reqwest::Response,
    resume: &mut ResumeState,
    events: &mpsc::Sender<CaptureEvent>,
    history: &SourceEventSink,
    cancelled: &mut watch::Receiver<bool>,
    stopped: &mut watch::Receiver<bool>,
) -> ConnectionEnd {
    let mut framer = HttpFramer::new(session.framing, &session.http);
    let mut notices = FrameNotices::default();
    let mut body_bytes = 0_u64;
    loop {
        let next = tokio::select! {
            biased;
            _ = cancelled.changed() => {
                return ConnectionEnd::Aborted { discarded_bytes: framer.buffered_len() };
            }
            _ = stopped.changed() => {
                let records = framer.finish(acquisition_id);
                let _ = emit_records(events, cancelled, records).await;
                return ConnectionEnd::Stopped;
            }
            _ = events.closed() => {
                return ConnectionEnd::Aborted { discarded_bytes: framer.buffered_len() };
            }
            result = tokio::time::timeout(session.http.read_timeout, response.chunk()) => result,
        };
        let chunk = match next {
            Err(_) => {
                let pending = framer.buffered_len() as u64;
                let _ = emit(
                    events,
                    cancelled,
                    capture_error(acquisition_id, "no HTTP body bytes within the read timeout"),
                )
                .await;
                let records = framer.finish(acquisition_id);
                let _ = emit_records(events, cancelled, records).await;
                return ConnectionEnd::Ended {
                    reason: if pending > 0 {
                        DisconnectReason::TruncatedFrame {
                            pending_bytes: pending,
                        }
                    } else {
                        DisconnectReason::ReadTimeout
                    },
                    body_bytes,
                };
            }
            Ok(Err(error)) => {
                let detail = transport_detail(&error);
                let pending = framer.buffered_len() as u64;
                let records = framer.finish(acquisition_id);
                let _ = emit_records(events, cancelled, records).await;
                let _ = emit(events, cancelled, capture_error(acquisition_id, &detail)).await;
                return ConnectionEnd::Ended {
                    reason: if pending > 0 {
                        DisconnectReason::TruncatedFrame {
                            pending_bytes: pending,
                        }
                    } else {
                        DisconnectReason::Transport { detail }
                    },
                    body_bytes,
                };
            }
            Ok(Ok(None)) => {
                let pending = framer.buffered_len() as u64;
                let records = framer.finish(acquisition_id);
                if !emit_records(events, cancelled, records).await {
                    return ConnectionEnd::Aborted { discarded_bytes: 0 };
                }
                return ConnectionEnd::Ended {
                    reason: if pending > 0 {
                        DisconnectReason::TruncatedFrame {
                            pending_bytes: pending,
                        }
                    } else {
                        DisconnectReason::EndOfStream
                    },
                    body_bytes,
                };
            }
            Ok(Ok(Some(chunk))) => chunk,
        };
        // Body bytes are consumed in bounded slices so one very large chunk
        // cannot inflate the capture task's retained memory.
        for slice in chunk.chunks(session.limits.read_chunk_bytes.max(1)) {
            body_bytes += slice.len() as u64;
            resume.received_bytes = resume.received_bytes.saturating_add(slice.len() as u64);
            let outcome = framer.push(slice, acquisition_id);
            notices.report(history, acquisition_id, &outcome, body_bytes);
            if let Some(id) = outcome.last_event_id {
                resume.last_event_id = Some(id);
            }
            if !emit_records(events, cancelled, outcome.records).await {
                return ConnectionEnd::Aborted {
                    discarded_bytes: framer.buffered_len(),
                };
            }
        }
    }
}

/// One-shot notices so a pathological stream cannot flood history.
#[derive(Default)]
struct FrameNotices {
    oversized: bool,
    invalid_utf8: bool,
}

impl FrameNotices {
    fn report(
        &mut self,
        history: &SourceEventSink,
        acquisition_id: Uuid,
        outcome: &FrameOutcome,
        offset: u64,
    ) {
        if outcome.oversized && !self.oversized {
            self.oversized = true;
            history.emit(SourceEvent::FrameOversized {
                acquisition_id,
                limit_bytes: outcome.limit_bytes,
            });
        }
        if outcome.invalid_utf8 && !self.invalid_utf8 {
            self.invalid_utf8 = true;
            history.emit(SourceEvent::InvalidUtf8 {
                acquisition_id,
                byte_offset: offset,
            });
        }
    }
}

#[derive(Default)]
struct FrameOutcome {
    records: Vec<CapturedRecord>,
    last_event_id: Option<String>,
    oversized: bool,
    invalid_utf8: bool,
    limit_bytes: u64,
}

enum HttpFramer {
    Newline { framer: Framer, limit: usize },
    Sse(SseFramer),
}

impl HttpFramer {
    fn new(framing: HttpFraming, limits: &HttpLimits) -> Self {
        let limit = limits
            .maximum_frame_bytes
            .min(limits.maximum_pending_bytes)
            .max(1);
        match framing {
            HttpFraming::Newline => Self::Newline {
                framer: Framer::new(limit),
                limit,
            },
            HttpFraming::Sse => Self::Sse(SseFramer::new(limit)),
        }
    }

    fn push(&mut self, input: &[u8], acquisition_id: Uuid) -> FrameOutcome {
        match self {
            Self::Newline { framer, limit } => {
                let records = framer.push(input, StreamKind::Http, acquisition_id);
                let oversized = records
                    .iter()
                    .any(|record| record.bytes.len() >= *limit && record.delimiter.is_empty());
                FrameOutcome {
                    invalid_utf8: records
                        .iter()
                        .any(|record| std::str::from_utf8(&record.bytes).is_err()),
                    oversized,
                    limit_bytes: *limit as u64,
                    last_event_id: None,
                    records,
                }
            }
            Self::Sse(framer) => framer.push(input, acquisition_id),
        }
    }

    fn finish(&mut self, acquisition_id: Uuid) -> Vec<CapturedRecord> {
        match self {
            Self::Newline { framer, .. } => framer.finish(StreamKind::Http, acquisition_id),
            Self::Sse(framer) => framer.finish(acquisition_id),
        }
    }

    fn buffered_len(&self) -> usize {
        match self {
            Self::Newline { framer, .. } => framer.buffered_len(),
            Self::Sse(framer) => framer.pending.len(),
        }
    }
}

/// Server-Sent Events framing.
///
/// An event is the byte range up to and including the blank line that ends it.
/// The retained record holds the field lines; the delimiter holds the blank
/// line's terminator, so concatenation reproduces the stream exactly. The
/// `id:` field is read to obtain a `Last-Event-ID` resume point; reading it
/// never rewrites the retained bytes.
struct SseFramer {
    pending: Vec<u8>,
    /// Index in `pending` where the line currently being read begins.
    line_start: usize,
    /// A `\r` was seen and its terminator length is not yet decided.
    carriage_return: bool,
    fragmented: bool,
    maximum: usize,
}

impl SseFramer {
    fn new(maximum: usize) -> Self {
        Self {
            pending: Vec::with_capacity(maximum.min(8192)),
            line_start: 0,
            carriage_return: false,
            fragmented: false,
            maximum,
        }
    }

    fn push(&mut self, input: &[u8], acquisition_id: Uuid) -> FrameOutcome {
        let mut outcome = FrameOutcome {
            limit_bytes: self.maximum as u64,
            ..FrameOutcome::default()
        };
        for &byte in input {
            if self.carriage_return {
                self.carriage_return = false;
                if byte == b'\n' {
                    // The pending `\r` and this `\n` are one terminator.
                    self.pending.push(byte);
                    self.close_line(2, acquisition_id, &mut outcome);
                    self.enforce_limit(acquisition_id, &mut outcome);
                    continue;
                }
                // The `\r` alone terminated the previous line.
                self.close_line(1, acquisition_id, &mut outcome);
            }
            if byte == b'\r' {
                self.pending.push(byte);
                self.carriage_return = true;
                self.enforce_limit(acquisition_id, &mut outcome);
                continue;
            }
            self.pending.push(byte);
            if byte == b'\n' {
                self.close_line(1, acquisition_id, &mut outcome);
            }
            self.enforce_limit(acquisition_id, &mut outcome);
        }
        outcome
    }

    /// Handles a completed line whose terminator occupies the last
    /// `terminator` bytes of `pending`.
    fn close_line(&mut self, terminator: usize, acquisition_id: Uuid, outcome: &mut FrameOutcome) {
        let end = self.pending.len();
        let body_end = end - terminator;
        if body_end == self.line_start {
            // A blank line ends the event.
            let bytes: Vec<u8> = self.pending.drain(..body_end).collect();
            let delimiter: Vec<u8> = self.pending.drain(..terminator).collect();
            self.pending.clear();
            self.line_start = 0;
            if let Some(id) = last_event_id(&bytes) {
                outcome.last_event_id = Some(id);
            }
            if std::str::from_utf8(&bytes).is_err() {
                outcome.invalid_utf8 = true;
            }
            let record = self.record(bytes, delimiter, acquisition_id, true);
            self.fragmented = false;
            outcome.records.push(record);
        } else {
            self.line_start = end;
        }
    }

    /// Emits a bounded fragment when one event exceeds the frame limit. The
    /// bytes are retained; only the framing is split.
    fn enforce_limit(&mut self, acquisition_id: Uuid, outcome: &mut FrameOutcome) {
        while self.pending.len() > self.maximum {
            let bytes: Vec<u8> = self.pending.drain(..self.maximum).collect();
            if std::str::from_utf8(&bytes).is_err() {
                outcome.invalid_utf8 = true;
            }
            let record = self.record(bytes, Vec::new(), acquisition_id, false);
            self.fragmented = true;
            self.line_start = self.line_start.saturating_sub(self.maximum);
            outcome.oversized = true;
            outcome.records.push(record);
        }
    }

    fn finish(&mut self, acquisition_id: Uuid) -> Vec<CapturedRecord> {
        self.carriage_return = false;
        self.line_start = 0;
        if self.pending.is_empty() {
            if self.fragmented {
                self.fragmented = false;
                return vec![CapturedRecord {
                    captured_at_unix_nanos: capture_now(),
                    stream: StreamKind::Http,
                    bytes: Vec::new(),
                    delimiter: Vec::new(),
                    acquisition_id,
                    chunk: ChunkPosition::End,
                }];
            }
            return Vec::new();
        }
        let bytes = std::mem::take(&mut self.pending);
        let record = self.record(bytes, Vec::new(), acquisition_id, true);
        self.fragmented = false;
        vec![record]
    }

    fn record(
        &self,
        bytes: Vec<u8>,
        delimiter: Vec<u8>,
        acquisition_id: Uuid,
        end: bool,
    ) -> CapturedRecord {
        CapturedRecord {
            captured_at_unix_nanos: capture_now(),
            stream: StreamKind::Http,
            bytes,
            delimiter,
            acquisition_id,
            chunk: match (self.fragmented, end) {
                (false, true) => ChunkPosition::Complete,
                (false, false) => ChunkPosition::Start,
                (true, false) => ChunkPosition::Continue,
                (true, true) => ChunkPosition::End,
            },
        }
    }
}

/// Reads the last `id:` field of an SSE event. Per the protocol an `id`
/// containing a NUL byte is ignored.
fn last_event_id(event: &[u8]) -> Option<String> {
    let mut found = None;
    for line in split_sse_lines(event) {
        let Some(rest) = line.strip_prefix(b"id".as_slice()) else {
            continue;
        };
        let value = match rest.first() {
            None => &[][..],
            Some(b':') => {
                let value = &rest[1..];
                value.strip_prefix(b" ".as_slice()).unwrap_or(value)
            }
            Some(_) => continue,
        };
        if value.contains(&0) || value.len() > MAXIMUM_DETAIL_BYTES {
            continue;
        }
        if let Ok(text) = std::str::from_utf8(value) {
            found = Some(text.to_owned());
        }
    }
    found
}

fn split_sse_lines(event: &[u8]) -> Vec<&[u8]> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut index = 0;
    while index < event.len() {
        match event[index] {
            b'\r' => {
                lines.push(&event[start..index]);
                index += if event.get(index + 1) == Some(&b'\n') {
                    2
                } else {
                    1
                };
                start = index;
            }
            b'\n' => {
                lines.push(&event[start..index]);
                index += 1;
                start = index;
            }
            _ => index += 1,
        }
    }
    if start < event.len() {
        lines.push(&event[start..]);
    }
    lines
}

fn transport_detail(error: &reqwest::Error) -> String {
    // `reqwest::Error`'s Display includes the request URL, which can carry
    // credentials in userinfo or query parameters. Report the redacted URL and
    // the error's own text separately.
    let source = std::error::Error::source(error)
        .map(|inner| inner.to_string())
        .unwrap_or_else(|| {
            if error.is_timeout() {
                "timed out".to_owned()
            } else if error.is_connect() {
                "connection failed".to_owned()
            } else {
                "request failed".to_owned()
            }
        });
    let endpoint = error
        .url()
        .map(|url| redact_endpoint(url.as_str()))
        .unwrap_or_else(|| "<unknown endpoint>".to_owned());
    bounded_detail(format!("{endpoint}: {source}"))
}

fn duration_millis(value: Duration) -> u64 {
    value.as_millis().min(u128::from(u64::MAX)) as u64
}

/// Sleeps unless capture is cancelled or stopped first. Returns whether the
/// caller should continue.
async fn wait_cancellable(
    delay: Duration,
    cancelled: &mut watch::Receiver<bool>,
    stopped: &mut watch::Receiver<bool>,
) -> bool {
    tokio::select! {
        biased;
        _ = cancelled.changed() => false,
        _ = stopped.changed() => false,
        _ = tokio::time::sleep(delay) => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn concatenated(records: &[CapturedRecord]) -> Vec<u8> {
        let mut bytes = Vec::new();
        for record in records {
            bytes.extend_from_slice(&record.bytes);
            bytes.extend_from_slice(&record.delimiter);
        }
        bytes
    }

    #[test]
    fn credentials_never_survive_redaction() {
        assert_eq!(
            redact_endpoint("https://user:secret@logs.example/stream?token=abcd"),
            "https://<redacted>@logs.example/stream?<redacted>"
        );
        assert_eq!(
            redact_endpoint("http://127.0.0.1:8080/tail"),
            "http://127.0.0.1:8080/tail"
        );
    }

    #[test]
    fn sse_events_are_framed_without_altering_the_original_bytes() {
        let id = Uuid::new_v4();
        let mut framer = SseFramer::new(1024);
        let stream = b"id: 7\ndata: one\n\nevent: tick\r\ndata: two\r\n\r\n";
        let outcome = framer.push(stream, id);
        assert_eq!(outcome.records.len(), 2);
        assert_eq!(concatenated(&outcome.records), stream);
        assert_eq!(outcome.last_event_id.as_deref(), Some("7"));
        assert_eq!(outcome.records[0].bytes, b"id: 7\ndata: one\n");
        assert_eq!(outcome.records[0].delimiter, b"\n");
        assert_eq!(outcome.records[1].delimiter, b"\r\n");
        assert!(framer.pending.is_empty());
    }

    #[test]
    fn an_sse_event_split_across_reads_is_still_one_record() {
        let id = Uuid::new_v4();
        let mut framer = SseFramer::new(1024);
        let mut records = Vec::new();
        for slice in [&b"data: par"[..], b"tial\n", b"\ndata: next\n\n"] {
            records.extend(framer.push(slice, id).records);
        }
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].bytes, b"data: partial\n");
        assert_eq!(
            concatenated(&records),
            b"data: partial\n\ndata: next\n\n".to_vec()
        );
    }

    #[test]
    fn an_oversized_sse_event_becomes_bounded_fragments_that_still_reassemble() {
        let id = Uuid::new_v4();
        let mut framer = SseFramer::new(8);
        let stream = b"data: aaaaaaaaaaaaaaaaaaaaaaaa\n\n";
        let outcome = framer.push(stream, id);
        assert!(outcome.oversized, "the frame limit must be reported");
        let trailing = framer.finish(id);
        let mut all = outcome.records;
        all.extend(trailing);
        assert_eq!(concatenated(&all), stream);
        assert!(all.iter().all(|record| record.bytes.len() <= 8));
        assert_eq!(all[0].chunk, ChunkPosition::Start);
        assert_eq!(all[all.len() - 1].chunk, ChunkPosition::End);
    }

    #[test]
    fn a_truncated_final_sse_event_is_retained_as_a_fragment() {
        let id = Uuid::new_v4();
        let mut framer = SseFramer::new(1024);
        let outcome = framer.push(b"data: cut off\n", id);
        assert!(outcome.records.is_empty());
        assert_eq!(framer.pending.len(), 14);
        let trailing = framer.finish(id);
        assert_eq!(concatenated(&trailing), b"data: cut off\n".to_vec());
        assert_eq!(trailing[0].delimiter, Vec::<u8>::new());
    }

    #[test]
    fn invalid_utf8_is_reported_but_captured_exactly() {
        let id = Uuid::new_v4();
        let mut framer = SseFramer::new(1024);
        let outcome = framer.push(b"data: \xff\xfe\n\n", id);
        assert!(outcome.invalid_utf8);
        assert_eq!(
            concatenated(&outcome.records),
            b"data: \xff\xfe\n\n".to_vec()
        );
    }

    #[test]
    fn the_last_id_of_an_event_wins_and_nul_ids_are_ignored() {
        assert_eq!(last_event_id(b"id: a\nid:b\n").as_deref(), Some("b"));
        assert_eq!(last_event_id(b"id\n").as_deref(), Some(""));
        assert_eq!(last_event_id(b"id: a\x00b\n"), None);
        assert_eq!(last_event_id(b"data: x\n"), None);
        assert_eq!(last_event_id(b"identity: x\n"), None);
    }
}
