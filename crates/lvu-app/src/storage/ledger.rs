//! Durable, append-only record of every explicit deletion of captured data.
//!
//! Captured original bytes survive everything except an *explicit, recorded*
//! deletion. This module is the "recorded" half of that invariant: nothing may
//! unlink a capture or an investigation before an intent entry is durable here,
//! and the resulting gap stays visible for the life of the capture root.
//!
//! The file is JSON Lines under the capture root so a partially written tail
//! from a crash is skipped without discarding earlier history. Reads are
//! bounded; a ledger that outgrows the read budget reports truncation rather
//! than silently presenting a shorter deletion history.

use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::OpenOptions,
    io::{self, BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

pub const LEDGER_FILE: &str = "deletions.jsonl";
pub const LEDGER_SCHEMA_VERSION: u32 = 1;
const MAX_RECORD_BYTES: usize = 8 * 1024;
const MAX_LEDGER_BYTES: u64 = 8 * 1024 * 1024;
const MAX_RECORDS: usize = 4096;
static ENTRY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeletionCause {
    /// The user selected this capture or investigation and confirmed.
    UserRequested,
    /// A configured retention rule selected it.
    Retention,
}

impl DeletionCause {
    pub fn label(self) -> &'static str {
        match self {
            Self::UserRequested => "requested",
            Self::Retention => "retention",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeletionState {
    /// Written and synced before the first unlink.
    Intended,
    /// Every planned path was removed.
    Completed,
    /// Removal stopped early. The target may be partially removed.
    Failed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeletedKind {
    Capture,
    Investigation,
}

/// What was removed from a source's captured history.
///
/// `probed` records that the range came from a bounded probe of the journal
/// head plus the reserved sequence watermark, so it describes the boundary
/// honestly without claiming an exact record count that was never scanned.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct CaptureBoundary {
    pub source_id: String,
    pub source_name: String,
    pub generation: u64,
    pub first_sequence: Option<u64>,
    pub last_sequence: Option<u64>,
    /// Sequences the writer had reserved. This is an upper bound on the last
    /// record, not a record count, and is never presented as one.
    #[serde(default)]
    pub reserved_sequence_upper_bound: Option<u64>,
    pub first_captured_at_unix_nanos: Option<i64>,
    pub journal_bytes: u64,
    pub whole_capture: bool,
    pub probed: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DeletionRecord {
    pub schema_version: u32,
    pub entry_id: String,
    pub recorded_at_unix_nanos: i64,
    pub state: DeletionState,
    pub kind: DeletedKind,
    pub target_id: String,
    pub target_label: String,
    pub cause: DeletionCause,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub boundary: Option<CaptureBoundary>,
    pub bytes_freed: u64,
    pub detail: String,
}

impl DeletionRecord {
    pub fn intent(
        kind: DeletedKind,
        target_id: String,
        target_label: String,
        cause: DeletionCause,
        policy: Option<String>,
        boundary: Option<CaptureBoundary>,
        planned_bytes: u64,
    ) -> Self {
        let recorded_at_unix_nanos = now_unix_nanos();
        let sequence = ENTRY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        Self {
            schema_version: LEDGER_SCHEMA_VERSION,
            entry_id: format!(
                "{recorded_at_unix_nanos:x}-{:x}-{sequence:x}",
                std::process::id()
            ),
            recorded_at_unix_nanos,
            state: DeletionState::Intended,
            kind,
            target_id,
            target_label,
            cause,
            policy,
            boundary,
            bytes_freed: planned_bytes,
            detail: "deletion authorized; removal not yet finished".into(),
        }
    }

    pub fn settled(&self, state: DeletionState, bytes_freed: u64, detail: String) -> Self {
        Self {
            state,
            bytes_freed,
            detail,
            recorded_at_unix_nanos: now_unix_nanos(),
            ..self.clone()
        }
    }
}

pub struct DeletionLedger {
    path: PathBuf,
}

impl DeletionLedger {
    pub fn new(root: &Path) -> Self {
        Self {
            path: root.join(LEDGER_FILE),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Appends one record and returns only after it is on stable storage.
    ///
    /// A failure here must abort the deletion: an unrecorded removal would make
    /// the application imply uninterrupted capture across a gap it created.
    pub fn append(&self, record: &DeletionRecord) -> io::Result<()> {
        let mut line = serde_json::to_vec(record)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        if line.len() + 1 > MAX_RECORD_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "deletion record exceeds the ledger entry limit",
            ));
        }
        line.push(b'\n');
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        file.write_all(&line)?;
        file.sync_data()
    }

    pub fn read(&self) -> LedgerReadout {
        let mut out = LedgerReadout::default();
        let file = match OpenOptions::new().read(true).open(&self.path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return out,
            Err(error) => {
                out.errors.push(format!("deletion ledger: {error}"));
                out.readable = false;
                return out;
            }
        };
        match file.metadata() {
            Ok(metadata) if metadata.len() > MAX_LEDGER_BYTES => out.truncated = true,
            Ok(_) => {}
            Err(error) => out.errors.push(format!("deletion ledger: {error}")),
        }
        let mut reader = BufReader::new(file).take(MAX_LEDGER_BYTES);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) => break,
                Ok(_) => {}
                Err(error) => {
                    out.errors.push(format!("deletion ledger: {error}"));
                    break;
                }
            }
            if !line.ends_with('\n') {
                // A torn tail from an interrupted append is not history.
                out.errors
                    .push("deletion ledger has an incomplete final entry".into());
                break;
            }
            if out.records.len() >= MAX_RECORDS {
                out.truncated = true;
                break;
            }
            match serde_json::from_str::<DeletionRecord>(line.trim_end()) {
                Ok(record) if record.schema_version == LEDGER_SCHEMA_VERSION => {
                    out.records.push(record)
                }
                Ok(record) => out.errors.push(format!(
                    "deletion ledger entry uses unsupported schema {}",
                    record.schema_version
                )),
                Err(error) => out.errors.push(format!("deletion ledger entry: {error}")),
            }
        }
        out
    }
}

#[derive(Clone, Debug)]
pub struct LedgerReadout {
    pub records: Vec<DeletionRecord>,
    pub truncated: bool,
    pub readable: bool,
    pub errors: Vec<String>,
}

impl Default for LedgerReadout {
    fn default() -> Self {
        Self {
            records: Vec::new(),
            truncated: false,
            readable: true,
            errors: Vec::new(),
        }
    }
}

/// A recorded discontinuity in a source's captured history.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CaptureGap {
    pub source_id: String,
    pub source_name: String,
    pub recorded_at_unix_nanos: i64,
    pub cause: DeletionCause,
    pub state: DeletionState,
    pub sequence_range: Option<(u64, u64)>,
    pub first_captured_at_unix_nanos: Option<i64>,
    pub bytes_freed: u64,
    pub policy: Option<String>,
    pub summary: String,
}

/// Pairs intent entries with their settlement.
///
/// An intent with no settlement means the process died mid-removal; the gap is
/// reported as interrupted rather than assumed complete or assumed absent.
pub fn capture_gaps(readout: &LedgerReadout) -> Vec<CaptureGap> {
    let mut settled: BTreeMap<&str, &DeletionRecord> = BTreeMap::new();
    for record in &readout.records {
        if record.state != DeletionState::Intended {
            settled.insert(record.entry_id.as_str(), record);
        }
    }
    let mut gaps = Vec::new();
    for record in &readout.records {
        if record.state != DeletionState::Intended || record.kind != DeletedKind::Capture {
            continue;
        }
        let outcome = settled.get(record.entry_id.as_str()).copied();
        let state = outcome.map_or(DeletionState::Intended, |value| value.state);
        let bytes_freed = outcome.map_or(record.bytes_freed, |value| value.bytes_freed);
        let boundary = record.boundary.clone().unwrap_or_default();
        let sequence_range = boundary
            .first_sequence
            .zip(boundary.last_sequence)
            .filter(|(first, last)| first <= last);
        gaps.push(CaptureGap {
            source_id: boundary.source_id.clone(),
            source_name: if boundary.source_name.is_empty() {
                record.target_label.clone()
            } else {
                boundary.source_name.clone()
            },
            recorded_at_unix_nanos: record.recorded_at_unix_nanos,
            cause: record.cause,
            state,
            sequence_range,
            first_captured_at_unix_nanos: boundary.first_captured_at_unix_nanos,
            bytes_freed,
            policy: record.policy.clone(),
            summary: gap_summary(record, state, &boundary, sequence_range, bytes_freed),
        });
    }
    gaps
}

fn gap_summary(
    record: &DeletionRecord,
    state: DeletionState,
    boundary: &CaptureBoundary,
    sequence_range: Option<(u64, u64)>,
    bytes_freed: u64,
) -> String {
    let range = match (sequence_range, boundary.first_sequence) {
        (Some((first, last)), _) => format!("records {first}\u{2013}{last}"),
        (None, Some(first)) => format!("records from {first} onward"),
        (None, None) => "an unrecorded record range".into(),
    };
    let verb = match state {
        DeletionState::Completed => "removed",
        DeletionState::Failed => "partially removed",
        DeletionState::Intended => "left in an interrupted removal",
    };
    let scope = if boundary.whole_capture {
        "the whole capture"
    } else {
        "part of the capture"
    };
    let tail = match state {
        DeletionState::Intended => {
            " Restart did not observe a completion entry; treat this source as incomplete."
        }
        DeletionState::Failed => " Some files may remain; this source is incomplete.",
        DeletionState::Completed => "",
    };
    format!(
        "{scope} of \"{}\" was {verb} by {} ({range}, {} freed).{tail}",
        record.target_label,
        record.cause.label(),
        format_bytes(bytes_freed)
    )
}

pub fn now_unix_nanos() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|value| i64::try_from(value.as_nanos()).ok())
        .unwrap_or(0)
}

pub fn format_bytes(value: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut scaled = value as f64;
    let mut unit = 0;
    while scaled >= 1024.0 && unit + 1 < UNITS.len() {
        scaled /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{value} B")
    } else {
        format!("{scaled:.1} {}", UNITS[unit])
    }
}
