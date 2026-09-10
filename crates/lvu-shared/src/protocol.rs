//! Versioned control-channel message table: lifecycle, acquisition, and
//! store writes. Every message is one JSON object with `schema_version`,
//! `request_id` (requests) or `event` correlation, sent as one bounded
//! frame (see `frame.rs`).
//!
//! Payload rule: commands carry the *existing* store DTOs by value shape
//! (`memory::SaveRequest`, `memory::RecipeFile`, `lvu_core` identities and
//! acquisitions, …) transferred as JSON through the endpoints that own those
//! types — never a second persistence model, and no new field semantics.
//! Until the DTOs grow canonical `Serialize` impls (proposed to their
//! owners), both ends map fields explicitly against the table below; the
//! `schema_version` bump rule is: additive optional fields only, anything
//! else increments [`PROTOCOL_VERSION`] and refuses older peers.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Wire protocol version. A window refuses a worker on mismatch with an
/// explicit error rather than guessing field meanings.
pub const PROTOCOL_VERSION: u32 = 1;

/// Lifecycle and acquisition traffic: window to worker.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "snake_case")]
pub enum WorkerRequest {
    /// Attach: `{window_pid, window_id, protocol}`. Answered by `Welcome`
    /// or a version refusal; also cancels any pending drain.
    Hello {
        request_id: String,
        window_pid: u32,
        window_id: String,
        protocol: u32,
    },
    /// Clean detach; socket close means the same thing.
    Goodbye { request_id: String },
    /// Explicit user-approved acquisition: `{definition}` is a
    /// `SourceDefinition` value. Answered by `Started`/`Refused`. The worker
    /// runs it through the same admission path as its own UI, including the
    /// acquisition comparator. Nothing else on this channel starts capture.
    RequestStart {
        request_id: String,
        definition: serde_json::Value,
    },
    /// Explicit stop of a running capture.
    RequestStop {
        request_id: String,
        source_id: String,
    },
    /// Explicit restart of a stopped/failed capture. Never invents
    /// definitions: the worker restarts the remembered one.
    RequestRestart {
        request_id: String,
        source_id: String,
    },
    /// Subscribe this connection to bounded `SourceStatus` events.
    StatusSubscribe { request_id: String },
    /// A forwarded stdin chunk (base64), `seq`-ordered per source; the
    /// worker acks with `StdinCredit`. See `StdinOpen` for ownership.
    StdinChunk {
        request_id: String,
        source_id: String,
        seq: u64,
        base64: String,
    },
    /// End a forwarded stdin stream: `{source_id}` capture closes as
    /// incomplete, keeping what arrived. Parent crash implies this for all
    /// its streams via socket EOF.
    StdinClose {
        request_id: String,
        source_id: String,
    },
    /// Poll one source's canonical progress. Answered by exactly one
    /// `SourceProgress` event, preserving the strict sequential
    /// request/reply discipline: progress is polled on demand, never
    /// pushed, so no connection ever carries an unsolicited frame
    /// mid-request. Stopped sources answer with their terminal snapshot;
    /// never-known ids are refused explicitly.
    RequestProgress {
        request_id: String,
        source_id: String,
    },
}

/// Lifecycle and acquisition traffic: worker to window (replies and events).
/// `Eq` is absent deliberately: embedded store payloads (`WorkingView`)
/// are `PartialEq`-only, and equality across the wire is never required —
/// correlation travels in `request_id`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum WorkerEvent {
    Welcome {
        request_id: String,
        worker_pid: u32,
        protocol: u32,
        /// Worker lifetime nonce (UUIDv4 per `WorkerService::open`).
        /// Windows key remote epoch on `(worker_session, generation)`:
        /// a new session forces re-registration instead of aliasing a
        /// different capture under a reused generation or pid.
        worker_session: String,
        sources: Vec<SourceSummary>,
    },
    Started {
        request_id: String,
        source_id: String,
        journal_path: String,
    },
    Refused {
        request_id: String,
        reason: String,
    },
    Stopped {
        request_id: String,
        source_id: String,
    },
    /// Bounded health/progress snapshot for sidebars (names, states, record
    /// counts, last errors). Data stays in journals; this is presence only.
    SourceStatus {
        sources: Vec<SourceSummary>,
    },
    /// Worker is draining (last detach) or stopping now.
    ShutdownNotice {
        reason: String,
    },
    /// Credit for `seq` stdin bytes: the window may emit up to `window`
    /// further chunks. Bounds both ends without a second channel.
    StdinCredit {
        source_id: String,
        ack_seq: u64,
        window: u32,
    },
    /// One mediated store reply, carrying a `StoreEvent` verbatim. Store
    /// traffic shares the framed connection; each reply still correlates
    /// by the inner event's own `request_id`.
    Store(StoreEvent),
    /// Answer to `RequestProgress`: the canonical `SourceProgress`
    /// verbatim (never a projection, never zero-filled), bound to one
    /// worker lifetime by `worker_session`. Callers validate both the
    /// source identity and the session before caching latest-wins.
    SourceProgress {
        request_id: String,
        worker_session: String,
        progress: lvu_ingest::SourceProgress,
    },
    /// Accept a forwarded stdin stream: binds `{source_id}` to this
    /// connection. A replacement worker never inherits these bindings: after
    /// a crash the pipe is gone, and a new attachment needs a fresh source
    /// identity per the existing stdin invariant.
    StdinOpen {
        request_id: String,
        source_id: String,
        chunk_bytes: u32,
    },
}

/// Sidebar-grade source presence: identity and health, never bulk data.
/// `journal_path` lets a window open a read-only tail without knowing the
/// capture layout; it is derived by the worker that owns the layout.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SourceSummary {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub health: String,
    pub journal_path: String,
}

/// Mediated durable writes: window to worker. Each method mirrors one
/// `memory::Command` variant 1:1 (see `crates/lvu-app/src/memory.rs`), and
/// every payload is a canonical DTO, never a remodel:
///
/// - `lvu_core::{SourceDefinition, SourceId, ViewId, RecipeId}` and `Uuid`
///   transfer directly (all `Serialize`).
/// - `lvu_memory::{WorkingView, SourceMetadata, RecipeFile, SavedRecipe,
///   RecipeCandidate, SuggestionOutcome}` transfer directly.
/// - `RequestMeta` is field-identical to `lvu::RecipeRequestMeta` and
///   `SuggestionOutcomeShape` to `lvu::RecipeOutcome`; both collapse to the
///   canonical types the moment union adds two `Serialize` derives (no
///   semantic gap: pure routing scalars, documented here, version-gated).
/// - `SaveRequest` travels decomposed (`sequence` + `definition` +
///   `view_id` + `state: WorkingView` + `expected_version`); the wiring
///   constructs the existing struct 1:1, so no second model exists.
/// - `SuggestionContext` travels decomposed as native query parameters
///   (`source`/`project`/`command`/`fields`).
///
/// Every method carries `window_id`: the worker keys its newest/ack state
/// by `(window_id, view_id)`, never by view alone, because sequences reset
/// per client and two windows routinely share a view UUID. Cross-window
/// authority is the persisted row version (see `expected_version`); a stale
/// writer loses with the current version echoed, never silently and never
/// by last-writer-wins.
///
/// Byte budget: the encoded method must fit `MAX_CONTROL_MESSAGE_BYTES`
/// (see `check_store_size`, enforced on send and receipt). An oversize but
/// otherwise valid `WorkingView` is refused explicitly, never truncated;
/// chunked transfer is a follow-up, not a silent fallback.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "snake_case")]
pub enum StoreMethod {
    Load {
        request_id: String,
        window_id: String,
        definition: lvu_core::SourceDefinition,
        view_id: lvu_core::ViewId,
    },
    Save {
        request_id: String,
        window_id: String,
        sequence: u64,
        definition: lvu_core::SourceDefinition,
        view_id: lvu_core::ViewId,
        state: lvu_memory::WorkingView,
        expected_version: Option<u64>,
    },
    CreateDerivedView {
        request_id: String,
        window_id: String,
        sequence: u64,
        definition: lvu_core::SourceDefinition,
        view_id: lvu_core::ViewId,
        state: lvu_memory::WorkingView,
    },
    Recent {
        request_id: String,
        window_id: String,
    },
    ListRecipes {
        request_id: String,
        window_id: String,
        meta: RequestMeta,
        context: Option<SuggestionContextShape>,
    },
    RecipeHistory {
        request_id: String,
        window_id: String,
        meta: RequestMeta,
        recipe_id: lvu_core::RecipeId,
    },
    SaveRecipe {
        request_id: String,
        window_id: String,
        meta: RequestMeta,
        recipe: lvu_memory::RecipeFile,
        expected_revision: Option<uuid::Uuid>,
        context: Option<SuggestionContextShape>,
    },
    ImportRecipe {
        request_id: String,
        window_id: String,
        meta: RequestMeta,
        path: PathBuf,
    },
    ExportRecipe {
        request_id: String,
        window_id: String,
        meta: RequestMeta,
        recipe_id: lvu_core::RecipeId,
        revision: uuid::Uuid,
        path: PathBuf,
    },
    RecordSuggestion {
        request_id: String,
        window_id: String,
        outcome: SuggestionOutcomeShape,
    },
    Flush {
        request_id: String,
        window_id: String,
    },
}

/// Field-identical to `lvu::RecipeRequestMeta`; collapses to it when union
/// adds `Serialize`/`Deserialize` there. Pure routing scalars, no semantics.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RequestMeta {
    pub request_id: u64,
    pub dialog_id: u64,
    pub dialog_revision: u64,
}

/// Field-identical to `memory::SuggestionContext` (`lvu-app/src/memory.rs`);
/// native query parameters, so no DTO import is needed to route them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SuggestionContextShape {
    pub source: lvu_core::SourceId,
    pub project: Option<String>,
    pub command: Option<String>,
    pub fields: std::collections::BTreeMap<String, String>,
}

/// Field-identical to `lvu::RecipeOutcome`; same collapse note as above.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SuggestionOutcomeShape {
    pub source_id: String,
    pub recipe_id: String,
    pub revision: String,
    pub accepted: bool,
}

impl StoreMethod {
    /// The sending window for attachment checks. Every variant carries it;
    /// the worker refuses mismatches before any store work.
    pub fn window_id(&self) -> &str {
        match self {
            StoreMethod::Load { window_id, .. }
            | StoreMethod::Save { window_id, .. }
            | StoreMethod::CreateDerivedView { window_id, .. }
            | StoreMethod::Recent { window_id, .. }
            | StoreMethod::ListRecipes { window_id, .. }
            | StoreMethod::RecipeHistory { window_id, .. }
            | StoreMethod::SaveRecipe { window_id, .. }
            | StoreMethod::ImportRecipe { window_id, .. }
            | StoreMethod::ExportRecipe { window_id, .. }
            | StoreMethod::RecordSuggestion { window_id, .. }
            | StoreMethod::Flush { window_id, .. } => window_id,
        }
    }

    /// The correlation id echoed back in the reply. The client validates
    /// every reply against the outstanding request: a mismatch means the
    /// stream is confused (late/aborted reply meeting a new request), and
    /// the connection is retired rather than risking a shifted ack.
    pub fn request_id(&self) -> &str {
        match self {
            StoreMethod::Load { request_id, .. }
            | StoreMethod::Save { request_id, .. }
            | StoreMethod::CreateDerivedView { request_id, .. }
            | StoreMethod::Recent { request_id, .. }
            | StoreMethod::ListRecipes { request_id, .. }
            | StoreMethod::RecipeHistory { request_id, .. }
            | StoreMethod::SaveRecipe { request_id, .. }
            | StoreMethod::ImportRecipe { request_id, .. }
            | StoreMethod::ExportRecipe { request_id, .. }
            | StoreMethod::RecordSuggestion { request_id, .. }
            | StoreMethod::Flush { request_id, .. } => request_id,
        }
    }
}

/// Mediated-write replies: worker to window, mirroring `memory::Event` 1:1
/// (`crates/lvu-app/src/memory.rs`). `{request_id}` echoes for routing;
/// sequence and version echoes preserve the existing stale/conflict
/// semantics so a losing window learns exactly what won: a `SaveFailed`
/// carries the currently committed version, and the window keeps its local
/// draft, surfaces the conflict, and retries only after an explicit user
/// rebase — never a silent reload-overwrite, never an unbounded retry loop.
///
/// Wire shape is deliberately *externally* tagged (`{"Saved": {...}}`):
/// these events always travel inside `WorkerEvent::Store`, which is itself
/// internally tagged on `event`, and two internal tags collide on that key
/// (found by test: the inner `saved` overwrote the outer `store`).
/// Consumers match the outer envelope first, then this enum.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum StoreEvent {
    Loaded {
        request_id: String,
        source_id: lvu_core::SourceId,
        view_id: lvu_core::ViewId,
        views: Vec<lvu_memory::WorkingView>,
    },
    LoadFailed {
        request_id: String,
        source_id: lvu_core::SourceId,
        view_id: lvu_core::ViewId,
        reason: String,
    },
    Saved {
        request_id: String,
        source_id: lvu_core::SourceId,
        view_id: lvu_core::ViewId,
        sequence: u64,
        version: u64,
    },
    SaveFailed {
        request_id: String,
        source_id: lvu_core::SourceId,
        view_id: lvu_core::ViewId,
        sequence: u64,
        reason: String,
        current_version: Option<u64>,
    },
    DerivedViewCreated {
        request_id: String,
        view_id: lvu_core::ViewId,
        error: Option<String>,
    },
    Recent {
        request_id: String,
        sources: Vec<lvu_memory::SourceMetadata>,
    },
    RecentFailed {
        request_id: String,
        reason: String,
    },
    Recipes {
        request_id: String,
        meta: RequestMeta,
        recipes: Vec<(lvu_memory::RecipeFile, String)>,
        candidates: Vec<lvu_memory::RecipeCandidate>,
    },
    RecipeHistory {
        request_id: String,
        meta: RequestMeta,
        revisions: Vec<lvu_memory::RecipeFile>,
    },
    RecipeSaved {
        request_id: String,
        meta: RequestMeta,
        saved: lvu_memory::SavedRecipe,
    },
    RecipeExported {
        request_id: String,
        meta: RequestMeta,
        saved: lvu_memory::SavedRecipe,
    },
    RecipeFailed {
        request_id: String,
        meta: RequestMeta,
        reason: String,
    },
    SuggestionRecorded {
        request_id: String,
    },
    SuggestionFailed {
        request_id: String,
        reason: String,
    },
    Flushed {
        request_id: String,
    },
    FlushFailed {
        request_id: String,
        reason: String,
    },
    Fatal {
        reason: String,
    },
}

/// Enforce the transport byte budget on an encoded store method, on send
/// *and* receipt. A valid but oversize value (a huge `WorkingView` draft)
/// is refused explicitly with its size, never truncated; chunked transfer
/// is a follow-up, not a silent fallback.
pub fn check_store_size(method: &StoreMethod) -> Result<(), ProtocolError> {
    let bytes = serde_json::to_vec(method)
        .map_err(|error| ProtocolError::Unserializable(error.to_string()))?;
    if bytes.len() + 1 > crate::MAX_CONTROL_MESSAGE_BYTES {
        return Err(ProtocolError::StoreTooLarge {
            bytes: bytes.len() + 1,
        });
    }
    Ok(())
}

/// Receive-boundary validation for inbound control requests: every
/// `StdinChunk` payload is base64- and size-checked, even though the
/// constructor checks first — a peer may not run our constructors. Windows
/// apply `check_stdin_open` to inbound `StdinOpen` the same way. A violation
/// drops the connection; callers run these before dispatching.
pub fn validate_inbound(request: &WorkerRequest) -> Result<(), ProtocolError> {
    match request {
        WorkerRequest::StdinChunk { base64, .. } => {
            check_stdin_chunk_base64(base64)?;
            Ok(())
        }
        _ => Ok(()),
    }
}

/// Failures validating control payloads before they are admitted. Endpoints
/// validate on receipt with these helpers; a violation drops the connection
/// rather than truncating or guessing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProtocolError {
    BadBase64(String),
    ChunkTooLarge { decoded: usize },
    InvalidChunkSize(u32),
    StoreTooLarge { bytes: usize },
    Unserializable(String),
}

impl std::fmt::Display for ProtocolError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProtocolError::BadBase64(reason) => write!(formatter, "invalid base64: {reason}"),
            ProtocolError::ChunkTooLarge { decoded } => write!(
                formatter,
                "stdin chunk decodes to {decoded} bytes; limit is {}",
                crate::MAX_STDIN_CHUNK_BYTES
            ),
            ProtocolError::InvalidChunkSize(size) => write!(
                formatter,
                "stdin chunk size {size} exceeds {}",
                crate::MAX_STDIN_CHUNK_BYTES
            ),
            ProtocolError::StoreTooLarge { bytes } => write!(
                formatter,
                "store command is {bytes} bytes; limit is {}",
                crate::MAX_CONTROL_MESSAGE_BYTES
            ),
            ProtocolError::Unserializable(reason) => {
                write!(formatter, "store command failed to serialize: {reason}")
            }
        }
    }
}

impl std::error::Error for ProtocolError {}

const BASE64_ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Minimal base64 encoder: the channel only produces chunks from raw pipe
/// bytes, so it needs no general codec dependency.
pub fn base64_encode(raw: &[u8]) -> String {
    let mut out = String::with_capacity(raw.len().div_ceil(3) * 4);
    for piece in raw.chunks(3) {
        let mut block = [0u8; 3];
        block[..piece.len()].copy_from_slice(piece);
        let triple = u32::from_be_bytes([0, block[0], block[1], block[2]]);
        for shift in [18, 12, 6, 0] {
            out.push(BASE64_ALPHABET[((triple >> shift) & 63) as usize] as char);
        }
        // Replace the trailing positions the short final quantum did not
        // fill. Truncate-then-pad: popping in a loop would eat the `=`
        // just pushed (a real bug this comment guards).
        out.truncate(out.len() - (3 - piece.len()));
        out.extend(std::iter::repeat_n('=', 3 - piece.len()));
    }
    out
}

/// Validate base64 spelling and return the decoded byte count *without*
/// decoding: padding, alphabet, and length rules are checked first, so a
/// 192 KiB string that would decode past the raw chunk cap is measured and
/// refused rather than materialized.
pub fn decoded_base64_len(encoded: &str) -> Result<usize, ProtocolError> {
    let bytes = encoded.as_bytes();
    if !bytes.len().is_multiple_of(4) {
        return Err(ProtocolError::BadBase64(
            "length is not a multiple of 4".into(),
        ));
    }
    let mut padding_seen = false;
    let mut padding = 0usize;
    for (index, byte) in bytes.iter().enumerate() {
        if *byte == b'=' {
            padding_seen = true;
            padding += 1;
        } else {
            if padding_seen {
                return Err(ProtocolError::BadBase64(format!(
                    "data at byte {index} follows padding"
                )));
            }
            if !BASE64_ALPHABET.contains(byte) {
                return Err(ProtocolError::BadBase64(format!(
                    "byte {index} is outside the alphabet"
                )));
            }
        }
    }
    if padding > 2 {
        return Err(ProtocolError::BadBase64(
            "more than two padding bytes".into(),
        ));
    }
    Ok(bytes.len() / 4 * 3 - padding)
}

/// Decode base64 that [`decoded_base64_len`] already measured: spelling and
/// size validated, so this only materializes bytes. Returns `None` on any
/// inconsistency rather than trusting the precomputed length.
pub fn base64_decode_bounded(encoded: &str, decoded: usize) -> Option<Vec<u8>> {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes = encoded.as_bytes();
    if !bytes.len().is_multiple_of(4) {
        return None;
    }
    let mut out = Vec::with_capacity(decoded);
    let (quanta, _) = bytes.as_chunks::<4>();
    for quantum in quanta {
        let mut triple = 0u32;
        let mut padding = 0usize;
        for byte in quantum {
            triple <<= 6;
            if *byte == b'=' {
                padding += 1;
            } else {
                let value = ALPHABET.iter().position(|candidate| candidate == byte)?;
                if padding > 0 {
                    return None;
                }
                triple |= value as u32;
            }
        }
        if padding > 2 {
            return None;
        }
        out.push((triple >> 16) as u8);
        if padding < 2 {
            out.push((triple >> 8) as u8);
        }
        if padding == 0 {
            out.push(triple as u8);
        }
    }
    (out.len() == decoded).then_some(out)
}

/// Validate an inbound `StdinChunk` payload: well-formed base64 decoding to
/// at most `MAX_STDIN_CHUNK_BYTES` raw bytes. Returns the decoded size.
pub fn check_stdin_chunk_base64(encoded: &str) -> Result<usize, ProtocolError> {
    let decoded = decoded_base64_len(encoded)?;
    if decoded > crate::MAX_STDIN_CHUNK_BYTES {
        return Err(ProtocolError::ChunkTooLarge { decoded });
    }
    Ok(decoded)
}

/// Validate an inbound `StdinOpen` chunk-size offer against the same cap.
pub fn check_stdin_open(chunk_bytes: u32) -> Result<(), ProtocolError> {
    if chunk_bytes == 0 || chunk_bytes as usize > crate::MAX_STDIN_CHUNK_BYTES {
        return Err(ProtocolError::InvalidChunkSize(chunk_bytes));
    }
    Ok(())
}

impl WorkerRequest {
    /// Build a `StdinChunk` from raw pipe bytes, encoding and bounding it
    /// here so an oversized read never reaches the wire.
    pub fn stdin_chunk(
        request_id: String,
        source_id: String,
        seq: u64,
        raw: &[u8],
    ) -> Result<Self, ProtocolError> {
        if raw.len() > crate::MAX_STDIN_CHUNK_BYTES {
            return Err(ProtocolError::ChunkTooLarge { decoded: raw.len() });
        }
        Ok(WorkerRequest::StdinChunk {
            request_id,
            source_id,
            seq,
            base64: base64_encode(raw),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{decode_frame, encode_frame};

    #[test]
    fn lifecycle_messages_roundtrip_through_frames() {
        let request = WorkerRequest::Hello {
            request_id: "r-1".into(),
            window_pid: 4242,
            window_id: "w-9".into(),
            protocol: PROTOCOL_VERSION,
        };
        let wire = encode_frame(&serde_json::to_value(&request).unwrap()).unwrap();
        let back: WorkerRequest =
            serde_json::from_value(decode_frame(&wire[..wire.len() - 1]).unwrap()).unwrap();
        assert_eq!(back, request);
        let event = WorkerEvent::Refused {
            request_id: "r-1".into(),
            reason: "source admission limit reached".into(),
        };
        let wire = encode_frame(&serde_json::to_value(&event).unwrap()).unwrap();
        let back: WorkerEvent =
            serde_json::from_value(decode_frame(&wire[..wire.len() - 1]).unwrap()).unwrap();
        assert_eq!(back, event);
    }

    fn test_working_view() -> lvu_memory::WorkingView {
        lvu_memory::WorkingView {
            id: lvu_core::ViewId(uuid::Uuid::from_u128(1)),
            source_id: lvu_core::SourceId(uuid::Uuid::from_u128(2)),
            name: "All events".into(),
            role: lvu_memory::ViewRole::Derived,
            applied_revision_id: None,
            applied_search: String::new(),
            search_draft: None,
            applied_advanced_filter: None,
            advanced_filter_draft: None,
            navigation: lvu_memory::NavigationState {
                selected: None,
                anchor: None,
                follow: true,
            },
            presentation: lvu_memory::PresentationState::default(),
            version: 3,
        }
    }

    fn test_definition() -> lvu_core::SourceDefinition {
        lvu_core::SourceDefinition {
            schema_version: 1,
            id: lvu_core::SourceId(uuid::Uuid::from_u128(9)),
            name: "fixture".into(),
            acquisition: lvu_core::Acquisition::File {
                path: "/tmp/fixture.log".into(),
                follow: true,
            },
            identity_hints: Default::default(),
            retention: None,
        }
    }

    #[test]
    fn store_and_stdin_shapes_roundtrip() {
        let save = StoreMethod::Save {
            request_id: "r-2".into(),
            window_id: "w-1".into(),
            sequence: 7,
            definition: test_definition(),
            view_id: lvu_core::ViewId(uuid::Uuid::from_u128(1)),
            state: test_working_view(),
            expected_version: Some(3),
        };
        check_store_size(&save).unwrap();
        let wire = encode_frame(&serde_json::to_value(&save).unwrap()).unwrap();
        let back: StoreMethod =
            serde_json::from_value(decode_frame(&wire[..wire.len() - 1]).unwrap()).unwrap();
        assert_eq!(back, save);
        let saved = StoreEvent::Saved {
            request_id: "r-2".into(),
            source_id: lvu_core::SourceId(uuid::Uuid::from_u128(2)),
            view_id: lvu_core::ViewId(uuid::Uuid::from_u128(1)),
            sequence: 7,
            version: 4,
        };
        let wire = encode_frame(&serde_json::to_value(&saved).unwrap()).unwrap();
        let back: StoreEvent =
            serde_json::from_value(decode_frame(&wire[..wire.len() - 1]).unwrap()).unwrap();
        assert_eq!(back, saved);
        let chunk = WorkerRequest::StdinChunk {
            request_id: "r-3".into(),
            source_id: "s-1".into(),
            seq: 41,
            base64: "aGk=".into(),
        };
        validate_inbound(&chunk).unwrap();
        let wire = encode_frame(&serde_json::to_value(&chunk).unwrap()).unwrap();
        let back: WorkerRequest =
            serde_json::from_value(decode_frame(&wire[..wire.len() - 1]).unwrap()).unwrap();
        assert_eq!(back, chunk);
    }

    #[test]
    fn receive_boundary_rejects_hostile_payloads_despite_constructors() {
        // A peer need not run our constructors: oversized base64 and
        // malformed spelling fail here, before dispatch.
        let hostile = WorkerRequest::StdinChunk {
            request_id: "r-9".into(),
            source_id: "s-9".into(),
            seq: 0,
            base64: "A".repeat(192 * 1024),
        };
        assert!(matches!(
            validate_inbound(&hostile),
            Err(ProtocolError::ChunkTooLarge { .. })
        ));
        let malformed = WorkerRequest::StdinChunk {
            request_id: "r-9".into(),
            source_id: "s-9".into(),
            seq: 0,
            base64: "!!!not-base64!!!".into(),
        };
        assert!(matches!(
            validate_inbound(&malformed),
            Err(ProtocolError::BadBase64(_))
        ));
        let benign = WorkerRequest::Hello {
            request_id: "r-9".into(),
            window_pid: 1,
            window_id: "w".into(),
            protocol: PROTOCOL_VERSION,
        };
        validate_inbound(&benign).unwrap();
    }

    #[test]
    fn oversize_valid_working_view_is_refused_never_truncated() {
        let mut view = test_working_view();
        view.search_draft = Some("x".repeat(300 * 1024));
        let save = StoreMethod::Save {
            request_id: "r-4".into(),
            window_id: "w-1".into(),
            sequence: 1,
            definition: test_definition(),
            view_id: lvu_core::ViewId(uuid::Uuid::from_u128(1)),
            state: view,
            expected_version: Some(0),
        };
        assert!(matches!(
            check_store_size(&save),
            Err(ProtocolError::StoreTooLarge { .. })
        ));
    }

    #[test]
    fn base64_codec_matches_rfc_vectors() {
        for (raw, encoded) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64_encode(raw.as_bytes()), encoded);
            assert_eq!(decoded_base64_len(encoded).unwrap(), raw.len());
        }
        for bad in ["Zg=", "Zg===", "====", "ZZ=Z", "Zm9!", "abc"] {
            assert!(
                decoded_base64_len(bad).is_err(),
                "{bad:?} must not validate"
            );
        }
    }

    #[test]
    fn stdin_chunk_constructor_bounds_raw_before_encoding() {
        let raw = vec![7u8; crate::MAX_STDIN_CHUNK_BYTES];
        let built = WorkerRequest::stdin_chunk("r-1".into(), "s-1".into(), 9, &raw).unwrap();
        let WorkerRequest::StdinChunk { base64, .. } = built else {
            panic!("constructor must build a chunk");
        };
        assert_eq!(check_stdin_chunk_base64(&base64).unwrap(), raw.len());
        let over = vec![7u8; crate::MAX_STDIN_CHUNK_BYTES + 1];
        assert!(matches!(
            WorkerRequest::stdin_chunk("r-1".into(), "s-1".into(), 9, &over),
            Err(ProtocolError::ChunkTooLarge { .. })
        ));
        assert!(check_stdin_open(crate::MAX_STDIN_CHUNK_BYTES as u32).is_ok());
        assert!(check_stdin_open(0).is_err());
        assert!(check_stdin_open(crate::MAX_STDIN_CHUNK_BYTES as u32 + 1).is_err());
    }

    #[test]
    fn valid_base64_past_the_raw_cap_is_measured_not_materialized() {
        // 192 KiB of valid base64 decodes to 144 KiB: inside a 256 KiB
        // frame, far past the 64 KiB raw promise.
        let encoded = "A".repeat(192 * 1024);
        assert_eq!(decoded_base64_len(&encoded).unwrap(), 147_456);
        assert!(matches!(
            check_stdin_chunk_base64(&encoded),
            Err(ProtocolError::ChunkTooLarge { decoded: 147_456 })
        ));
    }

    #[test]
    fn decode_materializes_only_pre_measured_bytes() {
        for (raw, encoded) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foobar", "Zm9vYmFy"),
        ] {
            let measured = decoded_base64_len(encoded).unwrap();
            assert_eq!(measured, raw.len());
            assert_eq!(
                base64_decode_bounded(encoded, measured).unwrap(),
                raw.as_bytes()
            );
        }
        // Length mismatch against the precomputed size refuses rather than
        // truncating or padding.
        assert_eq!(base64_decode_bounded("Zg==", 99), None);
        assert_eq!(base64_decode_bounded("!!!", 0), None);
    }
}
