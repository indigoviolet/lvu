//! Visible source lifecycle history.
//!
//! Reconnects, restarts, refusals and capture gaps are *published*, never
//! retried silently. Capture emits these through a bounded channel that can
//! never block or grow without limit; when a consumer falls behind the drop is
//! itself counted and reported rather than hidden.

use crate::RestartPolicy;
use serde::{Deserialize, Serialize};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use tokio::sync::mpsc;
use uuid::Uuid;

/// Default bound on undelivered history entries per source.
pub const DEFAULT_SOURCE_EVENT_CAPACITY: usize = 256;
/// Bound on any free-text detail carried by a history entry.
pub const MAXIMUM_DETAIL_BYTES: usize = 512;

/// What a reconnect was able to resume from, if anything.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "resume", rename_all = "snake_case")]
pub enum ResumeMode {
    /// First connection of this acquisition; nothing to resume.
    Initial,
    /// Server-Sent Events resumed with `Last-Event-ID`.
    LastEventId { id: String },
    /// Plain stream resumed with a byte `Range` request.
    ByteRange { offset: u64 },
    /// The protocol offered no resume point; the boundary is a possible gap.
    None,
}

impl ResumeMode {
    pub fn resumed(&self) -> bool {
        matches!(self, Self::LastEventId { .. } | Self::ByteRange { .. })
    }
}

/// Why a connection ended.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "disconnect", rename_all = "snake_case")]
pub enum DisconnectReason {
    /// The server closed a complete response body.
    EndOfStream,
    /// Bytes stopped mid-frame; the trailing partial frame is retained as a
    /// fragment record, never silently completed.
    TruncatedFrame { pending_bytes: u64 },
    /// No body byte arrived inside the configured read timeout.
    ReadTimeout,
    /// Transport failure.
    Transport { detail: String },
    /// Capture was stopped or aborted by its owner.
    Cancelled,
}

/// One published source lifecycle fact.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum SourceEvent {
    /// A connection attempt is starting. `attempt` is 1 for the first.
    HttpConnecting {
        acquisition_id: Uuid,
        attempt: u32,
        /// Endpoint with credentials removed.
        endpoint: String,
        resume: ResumeMode,
    },
    /// A 2xx response began streaming.
    HttpConnected {
        acquisition_id: Uuid,
        status: u16,
        resumed: bool,
        /// Whether the server advertised byte-range resume support.
        range_supported: bool,
    },
    /// A non-2xx response. The body is not captured.
    HttpRejected {
        acquisition_id: Uuid,
        status: u16,
        detail: String,
    },
    /// The connection ended, with the bytes it contributed.
    HttpDisconnected {
        acquisition_id: Uuid,
        reason: DisconnectReason,
        body_bytes: u64,
    },
    /// A capture boundary between two connections. `resumed == false` means
    /// records after this point may be missing input the server produced while
    /// disconnected; history must never imply otherwise.
    CaptureGap {
        previous_acquisition_id: Uuid,
        acquisition_id: Uuid,
        resume: ResumeMode,
    },
    /// A frame exceeded `maximum_frame_bytes` and was emitted as bounded
    /// fragments. The original bytes are retained; only the framing is split.
    FrameOversized {
        acquisition_id: Uuid,
        limit_bytes: u64,
    },
    /// Retained bytes were not valid UTF-8. Capture is byte-exact; this is a
    /// note for downstream interpretation only.
    InvalidUtf8 {
        acquisition_id: Uuid,
        byte_offset: u64,
    },
    /// A retry has been scheduled.
    RetryScheduled {
        attempt: u32,
        maximum_attempts: u32,
        delay_millis: u64,
    },
    /// The retry budget for the window is spent; capture stops.
    RetriesExhausted { attempts: u32, window_millis: u64 },
    /// Reconnection is disabled by policy, so the source ends here.
    ReconnectDisabled,
    /// A captured command run started.
    CommandStarted { acquisition_id: Uuid, run: u32 },
    /// A captured command run ended.
    CommandExited {
        acquisition_id: Uuid,
        code: Option<i32>,
        success: bool,
    },
    /// A run ended and the policy does not restart that outcome.
    RestartDeclined {
        policy: RestartPolicy,
        success: Option<bool>,
    },
    /// A restart has been scheduled after a bounded delay.
    RestartScheduled {
        policy: RestartPolicy,
        attempt: u32,
        maximum_restarts: u32,
        delay_millis: u64,
    },
    /// The restart budget for the window is spent.
    RestartsExhausted {
        policy: RestartPolicy,
        attempts: u32,
        window_millis: u64,
    },
    /// A restart could not be launched.
    RestartFailed { detail: String },
}

/// A published event with its capture timestamp.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SourceEventRecord {
    pub at_unix_nanos: i64,
    #[serde(flatten)]
    pub event: SourceEvent,
}

/// The capture-side end of the history channel.
#[derive(Clone, Debug)]
pub struct SourceEventSink {
    sender: mpsc::Sender<SourceEventRecord>,
    dropped: Arc<AtomicU64>,
}

impl SourceEventSink {
    /// Publishes an event. Never blocks and never grows: a saturated consumer
    /// loses the entry and increments the reported drop count.
    pub fn emit(&self, event: SourceEvent) {
        let record = SourceEventRecord {
            at_unix_nanos: crate::acquisition::capture_now(),
            event,
        };
        if self.sender.try_send(record).is_err() {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// The shared drop counter, so a consumer can report loss without holding
    /// a sender that would keep the channel open.
    pub fn dropped_counter(&self) -> Arc<AtomicU64> {
        self.dropped.clone()
    }
}

/// Creates a bounded history channel. `capacity` is clamped to at least one.
pub fn source_event_channel(
    capacity: usize,
) -> (SourceEventSink, mpsc::Receiver<SourceEventRecord>) {
    let (sender, receiver) = mpsc::channel(capacity.max(1));
    (
        SourceEventSink {
            sender,
            dropped: Arc::new(AtomicU64::new(0)),
        },
        receiver,
    )
}

/// Truncates free-text detail on a UTF-8 boundary so a hostile server cannot
/// push unbounded text into history.
pub fn bounded_detail(text: impl AsRef<str>) -> String {
    let text = text.as_ref();
    if text.len() <= MAXIMUM_DETAIL_BYTES {
        return text.to_owned();
    }
    let mut end = MAXIMUM_DETAIL_BYTES;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}
