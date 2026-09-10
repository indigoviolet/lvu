//! Bounded local protocol for one background capture worker shared by
//! multiple lvu windows.
//!
//! Scope: worker election/lifetime over record-locked files, a bounded
//! JSON-lines control channel, a versioned message table, and a pure
//! viewer-lifetime policy. This crate owns no capture, executes no source,
//! and persists nothing: journals stay in `lvu-core`/`lvu-ingest`, durable
//! state stays behind the existing store APIs, and the application owns all
//! wiring. It is internal acquisition infrastructure, not a headless
//! product: there is no user-facing daemon mode, no network transport, no
//! provider authentication, and no remote staging.
//!
//! Bounds (see each module): control messages are at most
//! [`MAX_CONTROL_MESSAGE_BYTES`] bytes; at most [`MAX_VIEWERS`] windows
//! attach; handshakes and probes carry explicit deadlines. Queues refuse
//! rather than grow; failures are explicit strings, never panics on I/O.

pub mod election;
pub mod frame;
pub mod lifetime;
pub mod protocol;
pub mod spawn;

pub use election::{
    ElectionError, OwnerGuard, ViewerGuard, WorkerPaths, live_viewers, owner_is_live,
    try_take_owner,
};
pub use frame::{FrameDecoder, FrameError, decode_frame, encode_frame};
pub use lifetime::{ViewerAdmission, ViewerSet};
pub use protocol::{
    PROTOCOL_VERSION, ProtocolError, SourceSummary, StoreEvent, StoreMethod, WorkerEvent,
    WorkerRequest,
};
pub use spawn::{SpawnSpec, WORKER_CHILD_FLAG, exit};

/// Largest single control-channel message, including framing. Bulk data
/// (journal bytes, index pages, snapshots) never flows here; it stays in
/// files. Sized so a stdin-forwarding chunk (64 KiB raw as base64) fits with
/// headroom, and anything larger is a protocol violation, not a realloc.
pub const MAX_CONTROL_MESSAGE_BYTES: usize = 256 * 1024;

/// Largest raw stdin-forwarding chunk a window may emit before credit.
/// Matches the bridge's line discipline scale; keeps one chunk well under
/// [`MAX_CONTROL_MESSAGE_BYTES`] after base64 expansion.
pub const MAX_STDIN_CHUNK_BYTES: usize = 64 * 1024;

/// Most windows attached to one worker. Sized off the source cap order;
/// admission beyond it is refused with an explicit error, never queued.
pub const MAX_VIEWERS: usize = 16;

/// How long a spawning window waits for the worker socket handshake before
/// reporting failure explicitly.
pub const WORKER_HANDSHAKE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Grace after worker start (and after the last detach) before zero-viewer
/// shutdown may fire, so spawn-attach races and quick relaunches do not flap.
pub const WORKER_SHUTDOWN_GRACE: std::time::Duration = std::time::Duration::from_secs(10);
