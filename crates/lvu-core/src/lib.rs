//! Durable identities, lossless raw capture, and bounded acquisition for lvu.

pub mod acquisition;
pub mod correlation;
pub mod http;
pub mod journal;
pub mod model;
pub mod restart;
pub mod source_event;

pub use acquisition::{
    Capture, CaptureCompletion, CaptureEvent, CapturedRecord, ChunkPosition, FileCaptureResume,
    FileContentHasher, FileEncoding, FileIdentity, FileResumeCursor,
};
pub use correlation::*;
pub use http::{HttpAcquisition, capture_http, redact_endpoint};
pub use journal::{Journal, JournalError, JournalPage, JournalReader, Recovery};
pub use model::*;
pub use restart::{AttemptWindow, Backoff, Jitter, RestartBounds, RestartDecision};
pub use source_event::{
    DisconnectReason, ResumeMode, SourceEvent, SourceEventRecord, SourceEventSink,
};
