//! HTTP acquisition against a real local server.

mod support;

use lvu_core::{
    Capture, CaptureEvent, CapturedRecord, ChunkPosition, DisconnectReason, HttpAcquisition,
    HttpFraming, HttpHeader, HttpLimits, ReconnectPolicy, ResumeMode, SourceEvent, StreamKind,
    acquisition::CaptureLimits, capture_http,
};
use std::time::{Duration, Instant};
use support::{Reply, TestServer};

fn limits() -> CaptureLimits {
    CaptureLimits {
        channel_capacity: 64,
        read_chunk_bytes: 4096,
        ..CaptureLimits::default()
    }
}

fn http_limits() -> HttpLimits {
    HttpLimits {
        maximum_frame_bytes: 64 * 1024,
        maximum_pending_bytes: 256 * 1024,
        connect_timeout: Duration::from_millis(500),
        read_timeout: Duration::from_millis(500),
    }
}

fn reconnect(enabled: bool, maximum_attempts: u32) -> ReconnectPolicy {
    ReconnectPolicy {
        enabled,
        delay: Duration::from_millis(10),
        maximum_delay: Duration::from_millis(40),
        jitter_percent: 0,
        maximum_attempts,
        attempt_window: Duration::from_secs(60),
        resume: true,
    }
}

fn acquisition(url: String, framing: HttpFraming, reconnect: ReconnectPolicy) -> HttpAcquisition {
    HttpAcquisition {
        url,
        framing,
        reconnect,
        headers: Vec::new(),
        limits: http_limits(),
    }
}

#[derive(Default)]
struct Collected {
    events: Vec<CaptureEvent>,
    history: Vec<SourceEvent>,
}

impl Collected {
    fn records(&self) -> Vec<&CapturedRecord> {
        self.events
            .iter()
            .flat_map(|event| event.records())
            .collect()
    }

    /// The exact bytes capture retained, reassembled in order.
    fn captured_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        for record in self.records() {
            bytes.extend_from_slice(&record.bytes);
            bytes.extend_from_slice(&record.delimiter);
        }
        bytes
    }

    fn errors(&self) -> Vec<&str> {
        self.events
            .iter()
            .filter_map(|event| match event {
                CaptureEvent::Error { message, .. } => Some(message.as_str()),
                _ => None,
            })
            .collect()
    }

    fn boundaries(&self) -> usize {
        self.events
            .iter()
            .filter(|event| matches!(event, CaptureEvent::Boundary { .. }))
            .count()
    }
}

/// Drains a capture to completion, retaining both record events and published
/// lifecycle history.
async fn drain(capture: Capture, limit: Duration) -> Collected {
    let Capture {
        handle,
        mut events,
        mut history,
        ..
    } = capture;
    let mut collected = Collected::default();
    let finished = tokio::time::timeout(limit, async {
        while let Some(event) = events.recv().await {
            collected.events.push(event);
        }
    })
    .await;
    assert!(
        finished.is_ok(),
        "HTTP capture did not finish within {limit:?}"
    );
    while let Ok(record) = history.try_recv() {
        collected.history.push(record.event);
    }
    drop(handle);
    collected
}

#[tokio::test]
async fn a_chunked_stream_is_captured_as_newline_records_without_altering_bytes() {
    let body = b"alpha\nbeta\ngamma\n";
    let server = TestServer::start(|_, _| {
        Reply::default()
            // Deliberately split across a line boundary so framing must span
            // network chunks.
            .chunk(b"alpha\nbe", Duration::ZERO)
            .chunk(b"ta\ngamma\n", Duration::ZERO)
    })
    .await;
    let capture = capture_http(
        acquisition(
            server.url("/tail"),
            HttpFraming::Newline,
            reconnect(false, 3),
        ),
        limits(),
    )
    .unwrap();
    let collected = drain(capture, Duration::from_secs(5)).await;

    assert_eq!(collected.captured_bytes(), body);
    let records = collected.records();
    assert_eq!(records.len(), 3);
    assert!(
        records
            .iter()
            .all(|record| record.stream == StreamKind::Http)
    );
    assert!(
        records
            .iter()
            .all(|record| record.chunk == ChunkPosition::Complete)
    );
    assert_eq!(records[1].bytes, b"beta");
    assert_eq!(records[1].delimiter, b"\n");
    assert!(
        collected
            .history
            .iter()
            .any(|event| matches!(event, SourceEvent::HttpConnected { status: 200, .. })),
        "a successful connection must be published: {:?}",
        collected.history
    );
    assert!(
        collected.history.iter().any(|event| matches!(
            event,
            SourceEvent::HttpDisconnected {
                reason: DisconnectReason::EndOfStream,
                ..
            }
        )),
        "the clean end of stream must be published"
    );
}

#[tokio::test]
async fn a_mid_stream_disconnect_reconnects_and_resumes_by_byte_range() {
    let server = TestServer::start(|ordinal, request| match ordinal {
        0 => Reply::abrupt(b"one\ntwo\n").header("Accept-Ranges: bytes"),
        _ => {
            assert_eq!(
                request.header("range"),
                Some("bytes=8-"),
                "the reconnect must resume from the byte the previous connection reached"
            );
            Reply::ok(b"three\n")
                .header("Accept-Ranges: bytes")
                .header("Content-Range: bytes 8-13/14")
        }
    })
    .await;
    let mut reply_status = reconnect(true, 1);
    reply_status.resume = true;
    let capture = capture_http(
        acquisition(server.url("/tail"), HttpFraming::Newline, reply_status),
        limits(),
    )
    .unwrap();
    let collected = drain(capture, Duration::from_secs(5)).await;

    assert_eq!(collected.captured_bytes(), b"one\ntwo\nthree\n");
    assert_eq!(
        collected.boundaries(),
        2,
        "each connection is its own capture extent"
    );
    let gap = collected
        .history
        .iter()
        .find_map(|event| match event {
            SourceEvent::CaptureGap { resume, .. } => Some(resume.clone()),
            _ => None,
        })
        .expect("the reconnect boundary must be published");
    assert_eq!(gap, ResumeMode::ByteRange { offset: 8 });
    assert_eq!(server.requests().len(), 2);
}

#[tokio::test]
async fn server_sent_events_reconnect_with_the_last_event_id() {
    let server = TestServer::start(|ordinal, request| match ordinal {
        0 => Reply::abrupt(b"id: 1\ndata: first\n\n"),
        _ => {
            assert_eq!(
                request.header("last-event-id"),
                Some("1"),
                "SSE reconnection must offer the last delivered event id"
            );
            Reply::ok(b"id: 2\ndata: second\n\n")
        }
    })
    .await;
    let capture = capture_http(
        acquisition(server.url("/events"), HttpFraming::Sse, reconnect(true, 1)),
        limits(),
    )
    .unwrap();
    let collected = drain(capture, Duration::from_secs(5)).await;

    assert_eq!(
        collected.captured_bytes(),
        b"id: 1\ndata: first\n\nid: 2\ndata: second\n\n"
    );
    let records = collected.records();
    assert_eq!(records.len(), 2, "each SSE event is one record");
    assert_eq!(records[0].bytes, b"id: 1\ndata: first\n");
    assert_eq!(records[0].delimiter, b"\n");
    let gap = collected
        .history
        .iter()
        .find_map(|event| match event {
            SourceEvent::CaptureGap { resume, .. } => Some(resume.clone()),
            _ => None,
        })
        .expect("the reconnect boundary must be published");
    assert_eq!(
        gap,
        ResumeMode::LastEventId { id: "1".to_owned() },
        "a resumed boundary must say what it resumed from"
    );
}

#[tokio::test]
async fn a_reconnect_without_a_resume_point_records_a_gap_instead_of_implying_continuity() {
    let server = TestServer::start(|ordinal, _| match ordinal {
        0 => Reply::abrupt(b"one\n"),
        _ => Reply::ok(b"two\n"),
    })
    .await;
    let capture = capture_http(
        acquisition(
            server.url("/tail"),
            HttpFraming::Newline,
            reconnect(true, 1),
        ),
        limits(),
    )
    .unwrap();
    let collected = drain(capture, Duration::from_secs(5)).await;

    let gap = collected
        .history
        .iter()
        .find_map(|event| match event {
            SourceEvent::CaptureGap {
                resume,
                acquisition_id,
                previous_acquisition_id,
            } => Some((resume.clone(), *acquisition_id, *previous_acquisition_id)),
            _ => None,
        })
        .expect("a non-resumable reconnect must still publish its boundary");
    assert_eq!(gap.0, ResumeMode::None);
    assert_ne!(
        gap.1, gap.2,
        "records either side of a gap must carry different acquisition identities"
    );
    let records = collected.records();
    assert_ne!(
        records[0].acquisition_id,
        records[records.len() - 1].acquisition_id
    );
    assert!(
        server.requests()[1].header("range").is_none(),
        "a server that does not advertise ranges must not be sent one"
    );
}

#[tokio::test]
async fn a_non_success_response_is_reported_and_captures_no_body() {
    let server = TestServer::start(|_, _| Reply::status(503, "Service Unavailable")).await;
    let capture = capture_http(
        acquisition(
            server.url("/tail"),
            HttpFraming::Newline,
            reconnect(false, 3),
        ),
        limits(),
    )
    .unwrap();
    let collected = drain(capture, Duration::from_secs(5)).await;

    assert!(
        collected.records().is_empty(),
        "a rejected response body is not capture data"
    );
    assert!(
        collected
            .history
            .iter()
            .any(|event| matches!(event, SourceEvent::HttpRejected { status: 503, .. }))
    );
    assert!(
        collected
            .errors()
            .iter()
            .any(|message| message.contains("503")),
        "the rejection must be visible as a source error: {:?}",
        collected.errors()
    );
}

#[tokio::test]
async fn a_server_that_never_responds_is_bounded_by_the_connect_timeout() {
    let server = TestServer::start(|_, _| Reply::silent()).await;
    let started = Instant::now();
    let capture = capture_http(
        acquisition(
            server.url("/tail"),
            HttpFraming::Newline,
            reconnect(false, 1),
        ),
        limits(),
    )
    .unwrap();
    let collected = drain(capture, Duration::from_secs(5)).await;

    assert!(
        started.elapsed() < Duration::from_secs(3),
        "capture waited {:?} on a silent server",
        started.elapsed()
    );
    assert!(collected.records().is_empty());
    assert!(
        collected
            .errors()
            .iter()
            .any(|message| message.contains("connect timeout")),
        "{:?}",
        collected.errors()
    );
}

#[tokio::test]
async fn a_stalled_body_is_bounded_by_the_read_timeout() {
    let server = TestServer::start(|_, _| {
        Reply::default()
            .chunk(b"one\n", Duration::ZERO)
            .chunk(b"never\n", Duration::from_secs(3600))
    })
    .await;
    let capture = capture_http(
        acquisition(
            server.url("/tail"),
            HttpFraming::Newline,
            reconnect(false, 1),
        ),
        limits(),
    )
    .unwrap();
    let collected = drain(capture, Duration::from_secs(5)).await;

    assert_eq!(collected.captured_bytes(), b"one\n");
    assert!(collected.history.iter().any(|event| matches!(
        event,
        SourceEvent::HttpDisconnected {
            reason: DisconnectReason::ReadTimeout,
            ..
        }
    )));
}

#[tokio::test]
async fn an_oversized_frame_is_split_into_bounded_fragments_that_still_reassemble() {
    let body = b"aaaaaaaaaaaaaaaaaaaaaaaaa\n";
    let server = TestServer::start(|_, _| Reply::ok(b"aaaaaaaaaaaaaaaaaaaaaaaaa\n")).await;
    let mut request = acquisition(
        server.url("/tail"),
        HttpFraming::Newline,
        reconnect(false, 1),
    );
    request.limits.maximum_frame_bytes = 8;
    let capture = capture_http(request, limits()).unwrap();
    let collected = drain(capture, Duration::from_secs(5)).await;

    assert_eq!(collected.captured_bytes(), body);
    let records = collected.records();
    assert!(records.len() > 1, "an oversized line must be fragmented");
    assert!(
        records.iter().all(|record| record.bytes.len() <= 8),
        "the frame bound must hold"
    );
    assert_eq!(records[0].chunk, ChunkPosition::Start);
    assert_eq!(records[records.len() - 1].chunk, ChunkPosition::End);
    assert!(
        collected
            .history
            .iter()
            .any(|event| matches!(event, SourceEvent::FrameOversized { .. })),
        "reaching the frame bound must be visible"
    );
}

#[tokio::test]
async fn a_truncated_final_frame_is_retained_and_reported_as_truncated() {
    let server = TestServer::start(|_, _| Reply::abrupt(b"complete\npartial without")).await;
    let capture = capture_http(
        acquisition(
            server.url("/tail"),
            HttpFraming::Newline,
            reconnect(false, 1),
        ),
        limits(),
    )
    .unwrap();
    let collected = drain(capture, Duration::from_secs(5)).await;

    assert_eq!(collected.captured_bytes(), b"complete\npartial without");
    let records = collected.records();
    assert_eq!(records[records.len() - 1].bytes, b"partial without");
    assert!(
        records[records.len() - 1].delimiter.is_empty(),
        "a truncated frame must not gain a delimiter it never had"
    );
    assert!(collected.history.iter().any(|event| matches!(
        event,
        SourceEvent::HttpDisconnected {
            reason: DisconnectReason::TruncatedFrame { .. },
            ..
        }
    )));
}

#[tokio::test]
async fn invalid_utf8_is_captured_exactly_and_reported() {
    let server = TestServer::start(|_, _| Reply::ok(b"good\n\xff\xfe\xfd\n")).await;
    let capture = capture_http(
        acquisition(
            server.url("/tail"),
            HttpFraming::Newline,
            reconnect(false, 1),
        ),
        limits(),
    )
    .unwrap();
    let collected = drain(capture, Duration::from_secs(5)).await;

    assert_eq!(collected.captured_bytes(), b"good\n\xff\xfe\xfd\n");
    assert!(
        collected
            .history
            .iter()
            .any(|event| matches!(event, SourceEvent::InvalidUtf8 { .. }))
    );
}

#[tokio::test]
async fn reconnect_attempts_are_capped_and_then_reported_as_exhausted() {
    let server = TestServer::start(|_, _| Reply::abrupt(b"")).await;
    let started = Instant::now();
    let capture = capture_http(
        acquisition(
            server.url("/tail"),
            HttpFraming::Newline,
            reconnect(true, 3),
        ),
        limits(),
    )
    .unwrap();
    let collected = drain(capture, Duration::from_secs(10)).await;

    assert_eq!(
        server.requests().len(),
        4,
        "one initial connection plus exactly the permitted retries"
    );
    let delays: Vec<u64> = collected
        .history
        .iter()
        .filter_map(|event| match event {
            SourceEvent::RetryScheduled { delay_millis, .. } => Some(*delay_millis),
            _ => None,
        })
        .collect();
    assert_eq!(
        delays,
        vec![10, 20, 40],
        "backoff must grow exponentially and stop at the configured maximum"
    );
    assert!(
        collected
            .history
            .iter()
            .any(|event| matches!(event, SourceEvent::RetriesExhausted { attempts: 3, .. })),
        "exhaustion must be published rather than looping silently"
    );
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[tokio::test]
async fn reconnection_can_be_refused_by_policy() {
    let server = TestServer::start(|_, _| Reply::abrupt(b"one\n")).await;
    let capture = capture_http(
        acquisition(
            server.url("/tail"),
            HttpFraming::Newline,
            reconnect(false, 5),
        ),
        limits(),
    )
    .unwrap();
    let collected = drain(capture, Duration::from_secs(5)).await;

    assert_eq!(server.requests().len(), 1);
    assert!(
        collected
            .history
            .iter()
            .any(|event| matches!(event, SourceEvent::ReconnectDisabled))
    );
}

#[tokio::test]
async fn cancellation_while_connected_stops_capture_promptly() {
    let server = TestServer::start(|_, _| {
        let mut reply = Reply::default();
        for _ in 0..10_000 {
            reply = reply.chunk(b"line\n", Duration::from_millis(1));
        }
        reply
    })
    .await;
    let capture = capture_http(
        acquisition(
            server.url("/tail"),
            HttpFraming::Newline,
            reconnect(true, 5),
        ),
        limits(),
    )
    .unwrap();
    let Capture {
        mut handle,
        mut events,
        ..
    } = capture;
    // Wait until the stream is actually flowing before cancelling.
    let mut seen = 0;
    while seen < 2 {
        match tokio::time::timeout(Duration::from_secs(5), events.recv()).await {
            Ok(Some(event)) if !event.records().is_empty() => seen += event.records().len(),
            Ok(Some(_)) => {}
            Ok(None) => panic!("capture ended before delivering records"),
            Err(_) => panic!("capture delivered no records"),
        }
    }
    let started = Instant::now();
    handle.cancel();
    let completion = tokio::time::timeout(Duration::from_secs(2), handle.wait())
        .await
        .expect("cancellation did not settle")
        .expect("capture task panicked");
    assert!(completion.aborted);
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[tokio::test]
async fn graceful_stop_while_connected_retains_the_partial_frame() {
    let server = TestServer::start(|_, _| {
        Reply::default()
            .chunk(b"one\ntw", Duration::ZERO)
            .chunk(b"o\n", Duration::from_secs(3600))
    })
    .await;
    let capture = capture_http(
        acquisition(
            server.url("/tail"),
            HttpFraming::Newline,
            reconnect(true, 5),
        ),
        limits(),
    )
    .unwrap();
    let Capture {
        mut handle,
        mut events,
        ..
    } = capture;
    let mut records: Vec<CapturedRecord> = Vec::new();
    while records.is_empty() {
        match tokio::time::timeout(Duration::from_secs(5), events.recv()).await {
            Ok(Some(event)) if !event.records().is_empty() => {
                records.extend_from_slice(event.records())
            }
            Ok(Some(_)) => {}
            _ => panic!("capture delivered no records"),
        }
    }
    handle.stop();
    let completion = tokio::time::timeout(Duration::from_secs(3), handle.wait())
        .await
        .expect("graceful stop did not settle")
        .expect("capture task panicked");
    assert!(!completion.aborted);
    while let Ok(Some(event)) = tokio::time::timeout(Duration::from_secs(1), events.recv()).await {
        records.extend_from_slice(event.records());
    }
    let bytes: Vec<u8> = records
        .iter()
        .flat_map(|record| {
            record
                .bytes
                .iter()
                .chain(record.delimiter.iter())
                .copied()
                .collect::<Vec<u8>>()
        })
        .collect();
    assert_eq!(
        bytes, b"one\ntw",
        "a graceful stop must flush the bytes already received, not invent a delimiter"
    );
}

#[tokio::test]
async fn credentials_never_appear_in_status_history_or_errors() {
    let server = TestServer::start(|_, request| {
        assert_eq!(request.header("authorization"), Some("Bearer s3cr3t-token"));
        Reply::status(401, "Unauthorized")
    })
    .await;
    let mut request = acquisition(
        format!("{}?access_token=s3cr3t-query", server.url("/tail")),
        HttpFraming::Newline,
        reconnect(false, 1),
    );
    request
        .headers
        .push(HttpHeader::new("authorization", "Bearer s3cr3t-token"));
    let capture = capture_http(request.clone(), limits()).unwrap();
    let collected = drain(capture, Duration::from_secs(5)).await;

    let rendered = format!("{:?}{:?}", collected.history, collected.errors());
    assert!(
        !rendered.contains("s3cr3t"),
        "credentials leaked into diagnostics: {rendered}"
    );
    assert!(
        rendered.contains("?<redacted>"),
        "the endpoint should still be recognisable: {rendered}"
    );
    assert!(
        !format!("{:?}", request.headers).contains("s3cr3t"),
        "a source definition must not print header values"
    );
}

#[tokio::test]
async fn an_unusable_endpoint_is_refused_before_anything_is_contacted() {
    let mut request = acquisition(
        "file:///etc/passwd".to_owned(),
        HttpFraming::Newline,
        reconnect(false, 1),
    );
    assert!(capture_http(request.clone(), limits()).is_err());

    request.url = "not a url".to_owned();
    assert!(capture_http(request.clone(), limits()).is_err());

    request.url = "http://127.0.0.1:1/tail".to_owned();
    request.limits.maximum_frame_bytes = 0;
    assert!(capture_http(request.clone(), limits()).is_err());

    request.limits = http_limits();
    request.headers = vec![HttpHeader::new("bad name", "value")];
    assert!(capture_http(request, limits()).is_err());
}
