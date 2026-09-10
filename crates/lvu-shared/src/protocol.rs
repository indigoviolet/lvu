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
}

/// Lifecycle and acquisition traffic: worker to window (replies and events).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum WorkerEvent {
    Welcome {
        request_id: String,
        worker_pid: u32,
        protocol: u32,
        sources: Vec<SourceSummary>,
    },
    Started {
        request_id: String,
        source_id: String,
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
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SourceSummary {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub health: String,
}

/// Mediated durable writes: window to worker. Each method mirrors one
/// `memory::Command` variant 1:1 (see `crates/lvu-app/src/memory.rs`):
///
/// - `load` → `Command::Load(Box<SourceDefinition>, ViewId)`
/// - `save` → `Command::Save(Box<SaveRequest>)` with its
///   `{sequence, definition, view_id, state}` — the worker keeps the
///   existing newest-sequence guard, so a stale windowed write is refused
///   exactly as a stale in-process write is today. Windows restoring the
///   same view UUID are therefore NOT disjoint: same UUID + newer sequence
///   wins, older loses with an explicit stale failure, and fork identity
///   travels in the request for the UI to explain.
/// - `create_derived_view` → `Command::CreateDerivedView` (reply gates
///   visibility, unchanged)
/// - `recent` / `list_recipes` / `recipe_history` → read-only queries
/// - `save_recipe` → `Command::SaveRecipe` with its `expected_revision`
///   hesitation, preserved verbatim
/// - `import_recipe` / `export_recipe` → paths are absolute; the worker
///   reads/writes them directly
/// - `record_suggestion` → `Command::RecordSuggestion(RecipeOutcome)`
/// - `flush` → `Command::Flush` with a bounded reply wait
///
/// Payloads are the DTO JSON shapes; replies mirror `memory::Event`
/// (`Loaded`/`Saved`/`RecipeSaved`/… with their sequence/revision echoes).
/// Command-enrichment attempt stores and settings writes are explicitly out
/// of this table until their owners trace a mapping: attempts ride the
/// SQLite attempt reservation path (`command_execution.rs`), settings ride
/// the XDG settings save, and neither is smuggled through these methods.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "snake_case")]
pub enum StoreMethod {
    Load {
        request_id: String,
        definition: serde_json::Value,
        view_id: String,
    },
    Save {
        request_id: String,
        save: serde_json::Value,
    },
    CreateDerivedView {
        request_id: String,
        save: serde_json::Value,
    },
    Recent {
        request_id: String,
    },
    ListRecipes {
        request_id: String,
        meta: serde_json::Value,
        context: Option<serde_json::Value>,
    },
    RecipeHistory {
        request_id: String,
        meta: serde_json::Value,
        recipe_id: String,
    },
    SaveRecipe {
        request_id: String,
        meta: serde_json::Value,
        recipe: serde_json::Value,
        expected_revision: Option<String>,
    },
    ImportRecipe {
        request_id: String,
        meta: serde_json::Value,
        path: String,
    },
    ExportRecipe {
        request_id: String,
        meta: serde_json::Value,
        recipe_id: String,
        revision: String,
        path: String,
    },
    RecordSuggestion {
        request_id: String,
        outcome: serde_json::Value,
    },
    Flush {
        request_id: String,
    },
}

/// Mediated-write replies: worker to window. `{request_id}` echoes; sequence
/// and revision echoes preserve the existing stale/conflict semantics so a
/// losing window learns exactly what won.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum StoreEvent {
    StoreReply {
        request_id: String,
        ok: bool,
        payload: serde_json::Value,
    },
}

/// Failures validating control payloads before they are admitted. Endpoints
/// validate on receipt with these helpers; a violation drops the connection
/// rather than truncating or guessing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProtocolError {
    BadBase64(String),
    ChunkTooLarge { decoded: usize },
    InvalidChunkSize(u32),
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

    #[test]
    fn store_and_stdin_shapes_roundtrip() {
        let save = StoreMethod::Save {
            request_id: "r-2".into(),
            save: serde_json::json!({"sequence": 7, "view_id": "v-1"}),
        };
        let wire = encode_frame(&serde_json::to_value(&save).unwrap()).unwrap();
        let back: StoreMethod =
            serde_json::from_value(decode_frame(&wire[..wire.len() - 1]).unwrap()).unwrap();
        assert_eq!(back, save);
        let chunk = WorkerRequest::StdinChunk {
            request_id: "r-3".into(),
            source_id: "s-1".into(),
            seq: 41,
            base64: "aGk=".into(),
        };
        let wire = encode_frame(&serde_json::to_value(&chunk).unwrap()).unwrap();
        let back: WorkerRequest =
            serde_json::from_value(decode_frame(&wire[..wire.len() - 1]).unwrap()).unwrap();
        assert_eq!(back, chunk);
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
}
