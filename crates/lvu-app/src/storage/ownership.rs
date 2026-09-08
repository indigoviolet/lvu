//! Ownership accounting for durable capture data and investigation exports.
//!
//! Two kinds of durable data live under the capture root: source captures
//! (`<root>/<source-uuid>/`, holding the authoritative journal) and
//! investigations (`<root>/investigations/<dir>/`, holding a frozen dataset and
//! its manifest). An investigation manifest names the sources and capture
//! high-watermarks it was built from; those references are *pins*.
//!
//! Deletion here is ownership-aware and refuses rather than repairing: a pinned
//! capture is never removed, because an investigation that references a capture
//! which no longer exists is a corrupted investigation, and the exported
//! Parquet projection is not a substitute for original bytes.
//!
//! Every scan is bounded and cancellable. Every removal verifies path identity,
//! refuses to follow symlinks, and only runs after the boundary is recorded.

use lvu_core::{SourceId, journal::JournalReader};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    hash::{Hash, Hasher},
    io::Read,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

use super::ledger::{
    CaptureBoundary, DeletedKind, DeletionCause, DeletionLedger, DeletionRecord, DeletionState,
    format_bytes,
};

pub const INVESTIGATIONS_DIR: &str = "investigations";
pub const WORKSPACE_DIR: &str = "workspace";
const JOURNAL_FILE: &str = "capture.journal";
const SOURCE_METADATA_FILE: &str = "source.json";
const MANIFEST_FILE: &str = "manifest.json";
const INVESTIGATION_RECORD_FILE: &str = "lvu-investigation.json";
const OPEN_NOTE: &str = "open in the application";

const MAX_CAPTURES: usize = 256;
const MAX_INVESTIGATIONS: usize = 256;
const MAX_ROOT_ENTRIES: usize = 1024;
const MAX_TREE_FILES: usize = 8192;
const MAX_TREE_DIRECTORIES: usize = 2048;
const MAX_TREE_DEPTH: usize = 6;
const MAX_ERRORS: usize = 32;
const MAX_MANIFEST_BYTES: u64 = 8 * 1024 * 1024;
const MAX_RECORD_BYTES: u64 = 64 * 1024;
const MAX_METADATA_BYTES: u64 = 1024 * 1024;
const MAX_PINS_PER_INVESTIGATION: usize = 64;

/// One disposable derived index, already classified by the live provider.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DerivedIndexEntry {
    pub path: PathBuf,
    pub bytes: u64,
    pub source_id: Option<SourceId>,
    pub reclaimable_bytes: u64,
    pub status: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CaptureUnit {
    pub source_id: SourceId,
    pub name: String,
    pub directory: PathBuf,
    pub generation: u64,
    pub journal_bytes: u64,
    /// Whole capture directory: journal, catalog, metadata, cursors, locks.
    pub durable_bytes: u64,
    /// Disposable derived indexes attributed to this source.
    pub derived_bytes: u64,
    pub last_modified_unix_nanos: Option<i64>,
    pub first_captured_at_unix_nanos: Option<i64>,
    pub first_sequence: Option<u64>,
    /// Sequences the writer had reserved: an upper bound, not a record count.
    pub reserved_sequence: Option<u64>,
    /// A writer holds the journal lock, so this source is capturing now.
    pub active: bool,
    pub incomplete: bool,
    pub notes: Vec<String>,
}

impl CaptureUnit {
    pub fn label(&self) -> String {
        format!(
            "{} ({})",
            self.name,
            short_id(&self.source_id.0.to_string())
        )
    }

    fn boundary(&self) -> CaptureBoundary {
        CaptureBoundary {
            source_id: self.source_id.0.to_string(),
            source_name: self.name.clone(),
            generation: self.generation,
            first_sequence: self.first_sequence,
            // The journal is one append-only file with no committed-tail
            // record, and scanning to the end is unbounded. The last sequence
            // is left unrecorded rather than inferred from the reservation
            // watermark, which would overstate the number of removed records.
            last_sequence: None,
            reserved_sequence_upper_bound: self.reserved_sequence,
            first_captured_at_unix_nanos: self.first_captured_at_unix_nanos,
            journal_bytes: self.journal_bytes,
            whole_capture: true,
            probed: true,
        }
    }
}

/// An investigation's claim on a source capture.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CapturePin {
    pub source_id: SourceId,
    pub generation: u64,
    pub high_watermark: Option<u64>,
    /// The investigation exported source-context rows covering its watermark.
    /// This is a projection, not the original bytes, so it never authorizes
    /// deleting the capture; it is reported so the preview is honest.
    pub exported_projection: bool,
    pub exported_rows: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InvestigationUnit {
    pub investigation_id: String,
    pub name: String,
    pub directory: PathBuf,
    pub durable_bytes: u64,
    pub created_at_unix_nanos: Option<i64>,
    pub pins: Vec<CapturePin>,
    /// The manifest exists but could not be read or parsed, so the set of
    /// captures this investigation depends on is unknown.
    pub unknown_pins: bool,
    pub incomplete: bool,
    pub notes: Vec<String>,
}

impl InvestigationUnit {
    pub fn label(&self) -> String {
        if self.name.is_empty() {
            short_id(&self.investigation_id)
        } else {
            format!("{} ({})", self.name, short_id(&self.investigation_id))
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum DeletionTarget {
    Capture(SourceId),
    Investigation(InvestigationRef),
}

/// Investigations are addressed by directory name, which is stable and is the
/// only identity available when a manifest cannot be parsed.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct InvestigationRef([u8; 64]);

impl InvestigationRef {
    pub fn new(directory_name: &str) -> Option<Self> {
        let bytes = directory_name.as_bytes();
        if bytes.is_empty() || bytes.len() > 64 {
            return None;
        }
        let mut value = [0_u8; 64];
        value[..bytes.len()].copy_from_slice(bytes);
        Some(Self(value))
    }

    pub fn as_str(&self) -> &str {
        let end = self.0.iter().position(|byte| *byte == 0).unwrap_or(64);
        std::str::from_utf8(&self.0[..end]).unwrap_or_default()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ItemClass {
    /// Captured original bytes, exports, configuration: never disposable.
    Durable,
    /// Recomputable from durable data alone.
    Disposable,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlanItem {
    pub path: PathBuf,
    pub bytes: u64,
    pub class: ItemClass,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DeletionBlocker {
    /// A capture cannot be removed while an investigation references it.
    PinnedByInvestigation {
        investigation: String,
        high_watermark: Option<u64>,
        exported_projection: bool,
    },
    /// An investigation manifest could not be read, so its dependencies are
    /// unknown and no capture may be assumed unreferenced.
    UnknownPins {
        investigation: String,
    },
    /// A writer holds the journal; stop the source before deleting it.
    SourceCapturing,
    /// The application still has this investigation open.
    InvestigationOpen,
    NotFound,
    /// Path identity, symlink or ownership check failed.
    PathRefused {
        reason: String,
    },
    /// The deletion boundary could not be recorded, so nothing was removed.
    BoundaryNotRecordable {
        reason: String,
    },
    /// The situation changed after the preview was produced.
    PlanStale,
}

impl DeletionBlocker {
    pub fn explanation(&self) -> String {
        match self {
            Self::PinnedByInvestigation {
                investigation,
                high_watermark,
                exported_projection,
            } => {
                let watermark = high_watermark
                    .map(|value| format!(" through record {value}"))
                    .unwrap_or_default();
                let copy = if *exported_projection {
                    " Its exported dataset holds a projection of those rows, not the original bytes."
                } else {
                    ""
                };
                format!(
                    "investigation \"{investigation}\" depends on this capture{watermark}. Delete that investigation first, or keep the capture.{copy}"
                )
            }
            Self::UnknownPins { investigation } => format!(
                "investigation \"{investigation}\" has a manifest that cannot be read, so the captures it depends on are unknown. Nothing is assumed unreferenced."
            ),
            Self::SourceCapturing => {
                "this source is capturing now. Stop the source, then delete it.".into()
            }
            Self::InvestigationOpen => {
                "this investigation is open in the application. Close it, then delete it.".into()
            }
            Self::NotFound => "this item is no longer present under the capture root.".into(),
            Self::PathRefused { reason } => {
                format!("refused for safety: {reason}. Nothing was removed.")
            }
            Self::BoundaryNotRecordable { reason } => format!(
                "the deletion boundary could not be recorded ({reason}), so nothing was removed. A gap that is not recorded would make the capture look uninterrupted."
            ),
            Self::PlanStale => {
                "storage changed since this preview was produced. Review it again.".into()
            }
        }
    }
}

/// What else refers to, or is referred to by, the deletion target.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Dependent {
    pub label: String,
    pub detail: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeletionPlan {
    pub target: DeletionTarget,
    pub label: String,
    pub cause: DeletionCause,
    pub policy: Option<String>,
    pub durable_bytes: u64,
    pub disposable_bytes: u64,
    pub items: Vec<PlanItem>,
    pub dependents: Vec<Dependent>,
    pub blockers: Vec<DeletionBlocker>,
    pub boundary: Option<CaptureBoundary>,
    /// Fingerprint of everything above. Confirmation must present it back so a
    /// stale preview cannot authorize a different deletion.
    pub digest: u64,
    pub truncated: bool,
}

impl DeletionPlan {
    pub fn allowed(&self) -> bool {
        self.blockers.is_empty()
    }

    pub fn freed_bytes(&self) -> u64 {
        self.durable_bytes.saturating_add(self.disposable_bytes)
    }

    /// One paragraph a confirmation dialog can show verbatim.
    pub fn summary(&self) -> String {
        if let Some(blocker) = self.blockers.first() {
            let more = if self.blockers.len() > 1 {
                format!(" ({} further reason(s).)", self.blockers.len() - 1)
            } else {
                String::new()
            };
            return format!(
                "Cannot delete {}: {}{more}",
                self.label,
                blocker.explanation()
            );
        }
        let mut text = format!(
            "Deleting {} frees {} of durable captured data",
            self.label,
            format_bytes(self.durable_bytes)
        );
        if self.disposable_bytes > 0 {
            text.push_str(&format!(
                " and {} of disposable cache",
                format_bytes(self.disposable_bytes)
            ));
        }
        text.push('.');
        if self.boundary.is_some() {
            text.push_str(" The removed range is recorded as a gap and stays visible.");
        }
        if !self.dependents.is_empty() {
            text.push_str(&format!(
                " {} related item(s) listed.",
                self.dependents.len()
            ));
        }
        if self.truncated {
            text.push_str(" The size estimate hit a scan limit and is a lower bound.");
        }
        text
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeletionOutcome {
    pub target: DeletionTarget,
    pub label: String,
    pub state: DeletionState,
    pub bytes_freed: u64,
    pub blockers: Vec<DeletionBlocker>,
    pub errors: Vec<String>,
    pub ledger_entry_id: Option<String>,
    pub detail: String,
}

/// A bounded, cancellable inventory of everything the capture root owns.
#[derive(Clone, Debug, Default)]
pub struct OwnershipIndex {
    pub root: PathBuf,
    pub captures: Vec<CaptureUnit>,
    pub investigations: Vec<InvestigationUnit>,
    pub derived: Vec<DerivedIndexEntry>,
    pub workspace_bytes: u64,
    pub truncated: bool,
    pub cancelled: bool,
    pub errors: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub struct ScanRequest {
    pub root: PathBuf,
    pub derived: Vec<DerivedIndexEntry>,
    /// Sources the application currently has open, in addition to the journal
    /// lock probe. Both are honoured; either one blocks deletion.
    pub active_sources: BTreeSet<SourceId>,
    pub open_investigations: BTreeSet<String>,
}

struct Budget {
    files: usize,
    directories: usize,
}

impl OwnershipIndex {
    pub fn build(request: &ScanRequest, cancel: &AtomicBool) -> Self {
        let mut index = Self {
            root: request.root.clone(),
            derived: request.derived.clone(),
            ..Default::default()
        };
        let mut budget = Budget {
            files: 0,
            directories: 0,
        };
        let entries = match fs::read_dir(&request.root) {
            Ok(entries) => entries,
            Err(error) => {
                index.push_error(format!("capture root: {error}"));
                return index;
            }
        };
        for (position, entry) in entries.take(MAX_ROOT_ENTRIES + 1).enumerate() {
            if cancel.load(Ordering::Acquire) {
                index.cancelled = true;
                return index;
            }
            if position == MAX_ROOT_ENTRIES {
                index.truncated = true;
                break;
            }
            let Ok(entry) = entry else {
                index.push_error("capture root entry unreadable".into());
                continue;
            };
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            let Ok(metadata) = fs::symlink_metadata(&path) else {
                index.push_error(format!("{}: metadata unavailable", path.display()));
                continue;
            };
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                continue;
            }
            if name == WORKSPACE_DIR {
                index.workspace_bytes = tree_bytes(&path, 0, &mut budget, &mut index, cancel);
            } else if name == INVESTIGATIONS_DIR {
                scan_investigations(&path, request, &mut budget, &mut index, cancel);
            } else if let Some(source_id) = parse_source_id(&name) {
                if index.captures.len() >= MAX_CAPTURES {
                    index.truncated = true;
                    continue;
                }
                let unit = scan_capture(&path, source_id, request, &mut budget, &mut index, cancel);
                index.captures.push(unit);
            }
        }
        if cancel.load(Ordering::Acquire) {
            index.cancelled = true;
        }
        let attributed: BTreeMap<SourceId, u64> =
            index
                .derived
                .iter()
                .fold(BTreeMap::new(), |mut totals, entry| {
                    if let Some(id) = entry.source_id {
                        let total = totals.entry(id).or_insert(0_u64);
                        *total = total.saturating_add(entry.bytes);
                    }
                    totals
                });
        for capture in &mut index.captures {
            capture.derived_bytes = attributed.get(&capture.source_id).copied().unwrap_or(0);
        }
        index.captures.sort_by(|a, b| a.name.cmp(&b.name));
        index
            .investigations
            .sort_by_key(|unit| std::cmp::Reverse(unit.created_at_unix_nanos));
        index
    }

    pub fn capture(&self, source_id: SourceId) -> Option<&CaptureUnit> {
        self.captures
            .iter()
            .find(|capture| capture.source_id == source_id)
    }

    pub fn investigation(&self, reference: InvestigationRef) -> Option<&InvestigationUnit> {
        let name = reference.as_str();
        self.investigations.iter().find(|investigation| {
            investigation
                .directory
                .file_name()
                .is_some_and(|value| value == name)
        })
    }

    pub fn durable_capture_bytes(&self) -> u64 {
        self.captures.iter().fold(0, |total, capture| {
            total.saturating_add(capture.durable_bytes)
        })
    }

    pub fn investigation_bytes(&self) -> u64 {
        self.investigations.iter().fold(0, |total, investigation| {
            total.saturating_add(investigation.durable_bytes)
        })
    }

    pub fn derived_bytes(&self) -> u64 {
        self.derived
            .iter()
            .fold(0, |total, entry| total.saturating_add(entry.bytes))
    }

    pub fn reclaimable_derived_bytes(&self) -> u64 {
        self.derived.iter().fold(0, |total, entry| {
            total.saturating_add(entry.reclaimable_bytes)
        })
    }

    /// Investigations that reference a capture, plus any whose dependency set
    /// is unknown. An unknown set counts as a reference.
    pub fn pins_for(&self, source_id: SourceId) -> Vec<(&InvestigationUnit, Option<&CapturePin>)> {
        self.investigations
            .iter()
            .filter_map(|investigation| {
                if investigation.unknown_pins {
                    return Some((investigation, None));
                }
                investigation
                    .pins
                    .iter()
                    .find(|pin| pin.source_id == source_id)
                    .map(|pin| (investigation, Some(pin)))
            })
            .collect()
    }

    pub fn plan_delete_capture(
        &self,
        source_id: SourceId,
        cause: DeletionCause,
        policy: Option<String>,
    ) -> DeletionPlan {
        let target = DeletionTarget::Capture(source_id);
        let Some(capture) = self.capture(source_id) else {
            return refused_plan(
                target,
                short_id(&source_id.0.to_string()),
                cause,
                policy,
                DeletionBlocker::NotFound,
            );
        };
        let mut blockers = Vec::new();
        let mut dependents = Vec::new();
        if capture.active {
            blockers.push(DeletionBlocker::SourceCapturing);
        }
        if let Err(reason) = verify_capture_path(&self.root, &capture.directory, source_id) {
            blockers.push(DeletionBlocker::PathRefused { reason });
        }
        for (investigation, pin) in self.pins_for(source_id) {
            match pin {
                Some(pin) => {
                    blockers.push(DeletionBlocker::PinnedByInvestigation {
                        investigation: investigation.label(),
                        high_watermark: pin.high_watermark,
                        exported_projection: pin.exported_projection,
                    });
                    dependents.push(Dependent {
                        label: investigation.label(),
                        detail: format!(
                            "pins generation {} through record {}{}",
                            pin.generation,
                            pin.high_watermark
                                .map(|value| value.to_string())
                                .unwrap_or_else(|| "unknown".into()),
                            if pin.exported_projection {
                                format!("; holds {} exported rows", pin.exported_rows)
                            } else {
                                String::new()
                            }
                        ),
                    });
                }
                None => {
                    blockers.push(DeletionBlocker::UnknownPins {
                        investigation: investigation.label(),
                    });
                    dependents.push(Dependent {
                        label: investigation.label(),
                        detail: "dependencies unknown; manifest unreadable".into(),
                    });
                }
            }
        }
        let mut items = vec![PlanItem {
            path: capture.directory.clone(),
            bytes: capture.durable_bytes,
            class: ItemClass::Durable,
        }];
        for entry in self
            .derived
            .iter()
            .filter(|entry| entry.source_id == Some(source_id))
        {
            items.push(PlanItem {
                path: entry.path.clone(),
                bytes: entry.bytes,
                class: ItemClass::Disposable,
            });
        }
        finish_plan(
            target,
            capture.label(),
            cause,
            policy,
            items,
            dependents,
            blockers,
            Some(capture.boundary()),
            capture.incomplete,
        )
    }

    pub fn plan_delete_investigation(
        &self,
        reference: InvestigationRef,
        cause: DeletionCause,
        policy: Option<String>,
    ) -> DeletionPlan {
        let target = DeletionTarget::Investigation(reference);
        let Some(investigation) = self.investigation(reference) else {
            return refused_plan(
                target,
                reference.as_str().into(),
                cause,
                policy,
                DeletionBlocker::NotFound,
            );
        };
        let mut blockers = Vec::new();
        if let Err(reason) = verify_investigation_path(&self.root, &investigation.directory) {
            blockers.push(DeletionBlocker::PathRefused { reason });
        }
        if investigation.notes.iter().any(|note| note == OPEN_NOTE) {
            blockers.push(DeletionBlocker::InvestigationOpen);
        }
        let dependents = investigation
            .pins
            .iter()
            .map(|pin| {
                let name = self
                    .capture(pin.source_id)
                    .map(CaptureUnit::label)
                    .unwrap_or_else(|| short_id(&pin.source_id.0.to_string()));
                Dependent {
                    label: name,
                    detail: "pin released; that capture becomes deletable".into(),
                }
            })
            .collect();
        let items = vec![PlanItem {
            path: investigation.directory.clone(),
            bytes: investigation.durable_bytes,
            class: ItemClass::Durable,
        }];
        finish_plan(
            target,
            investigation.label(),
            cause,
            policy,
            items,
            dependents,
            blockers,
            None,
            investigation.incomplete,
        )
    }

    /// Re-validates `plan` against this (freshly built) index and, if it still
    /// holds, records the boundary and performs the removal.
    ///
    /// Ordering is deliberate: intent is recorded and synced before the first
    /// unlink, and settlement is recorded afterwards. A crash in between leaves
    /// an intent entry, which is reported as an interrupted, incomplete capture
    /// rather than as uninterrupted history.
    pub fn execute(
        &self,
        plan: &DeletionPlan,
        ledger: &DeletionLedger,
        cancel: &AtomicBool,
    ) -> DeletionOutcome {
        let current = match plan.target {
            DeletionTarget::Capture(source_id) => {
                self.plan_delete_capture(source_id, plan.cause, plan.policy.clone())
            }
            DeletionTarget::Investigation(reference) => {
                self.plan_delete_investigation(reference, plan.cause, plan.policy.clone())
            }
        };
        if current.digest != plan.digest {
            return refused_outcome(plan, vec![DeletionBlocker::PlanStale]);
        }
        if !current.allowed() {
            return refused_outcome(plan, current.blockers);
        }
        let intent = DeletionRecord::intent(
            match plan.target {
                DeletionTarget::Capture(_) => DeletedKind::Capture,
                DeletionTarget::Investigation(_) => DeletedKind::Investigation,
            },
            match plan.target {
                DeletionTarget::Capture(source_id) => source_id.0.to_string(),
                DeletionTarget::Investigation(reference) => reference.as_str().into(),
            },
            plan.label.clone(),
            plan.cause,
            plan.policy.clone(),
            plan.boundary.clone(),
            plan.freed_bytes(),
        );
        if let Err(error) = ledger.append(&intent) {
            return refused_outcome(
                plan,
                vec![DeletionBlocker::BoundaryNotRecordable {
                    reason: error.to_string(),
                }],
            );
        }
        let mut removed = 0_u64;
        let mut errors = Vec::new();
        let mut complete = true;
        for item in &plan.items {
            let outcome = remove_tree(&item.path, cancel);
            removed = removed.saturating_add(outcome.bytes);
            complete &= outcome.complete;
            errors.extend(outcome.errors);
            if !complete {
                break;
            }
        }
        let cancelled = cancel.load(Ordering::Acquire);
        let state = if complete && !cancelled {
            DeletionState::Completed
        } else {
            DeletionState::Failed
        };
        let detail = match state {
            DeletionState::Completed => format!("removed {}", format_bytes(removed)),
            _ if cancelled => format!(
                "cancelled after removing {}; this item is incomplete",
                format_bytes(removed)
            ),
            _ => format!(
                "stopped after removing {}; this item is incomplete: {}",
                format_bytes(removed),
                errors.first().cloned().unwrap_or_default()
            ),
        };
        let settled = intent.settled(state, removed, detail.clone());
        if let Err(error) = ledger.append(&settled) {
            errors.push(format!(
                "deletion completed but its outcome could not be recorded: {error}"
            ));
        }
        DeletionOutcome {
            target: plan.target,
            label: plan.label.clone(),
            state,
            bytes_freed: removed,
            blockers: Vec::new(),
            errors,
            ledger_entry_id: Some(intent.entry_id),
            detail,
        }
    }

    fn push_error(&mut self, error: String) {
        if self.errors.len() < MAX_ERRORS {
            self.errors.push(error);
        } else {
            self.truncated = true;
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn finish_plan(
    target: DeletionTarget,
    label: String,
    cause: DeletionCause,
    policy: Option<String>,
    items: Vec<PlanItem>,
    dependents: Vec<Dependent>,
    blockers: Vec<DeletionBlocker>,
    boundary: Option<CaptureBoundary>,
    truncated: bool,
) -> DeletionPlan {
    let durable_bytes = items
        .iter()
        .filter(|item| item.class == ItemClass::Durable)
        .fold(0_u64, |total, item| total.saturating_add(item.bytes));
    let disposable_bytes = items
        .iter()
        .filter(|item| item.class == ItemClass::Disposable)
        .fold(0_u64, |total, item| total.saturating_add(item.bytes));
    let mut plan = DeletionPlan {
        target,
        label,
        cause,
        policy,
        durable_bytes,
        disposable_bytes,
        items,
        dependents,
        blockers,
        boundary,
        digest: 0,
        truncated,
    };
    plan.digest = digest(&plan);
    plan
}

fn refused_plan(
    target: DeletionTarget,
    label: String,
    cause: DeletionCause,
    policy: Option<String>,
    blocker: DeletionBlocker,
) -> DeletionPlan {
    finish_plan(
        target,
        label,
        cause,
        policy,
        Vec::new(),
        Vec::new(),
        vec![blocker],
        None,
        false,
    )
}

fn refused_outcome(plan: &DeletionPlan, blockers: Vec<DeletionBlocker>) -> DeletionOutcome {
    let detail = blockers
        .first()
        .map(DeletionBlocker::explanation)
        .unwrap_or_else(|| "refused".into());
    DeletionOutcome {
        target: plan.target,
        label: plan.label.clone(),
        state: DeletionState::Failed,
        bytes_freed: 0,
        blockers,
        errors: Vec::new(),
        ledger_entry_id: None,
        detail: format!("nothing was deleted: {detail}"),
    }
}

fn digest(plan: &DeletionPlan) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    plan.target.hash(&mut hasher);
    plan.label.hash(&mut hasher);
    plan.durable_bytes.hash(&mut hasher);
    plan.disposable_bytes.hash(&mut hasher);
    plan.blockers.len().hash(&mut hasher);
    for item in &plan.items {
        item.path.hash(&mut hasher);
        item.bytes.hash(&mut hasher);
    }
    for dependent in &plan.dependents {
        dependent.label.hash(&mut hasher);
    }
    if let Some(boundary) = &plan.boundary {
        boundary.source_id.hash(&mut hasher);
        boundary.first_sequence.hash(&mut hasher);
        boundary.last_sequence.hash(&mut hasher);
    }
    hasher.finish()
}

fn verify_capture_path(root: &Path, directory: &Path, source_id: SourceId) -> Result<(), String> {
    if directory.parent() != Some(root) {
        return Err("capture directory is not directly under the capture root".into());
    }
    let name = directory
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    if parse_source_id(name) != Some(source_id) {
        return Err("capture directory name does not match its source identity".into());
    }
    verify_directory(directory)
}

fn verify_investigation_path(root: &Path, directory: &Path) -> Result<(), String> {
    if directory.parent() != Some(root.join(INVESTIGATIONS_DIR).as_path()) {
        return Err("investigation directory is not under the investigations directory".into());
    }
    verify_directory(directory)
}

fn verify_directory(directory: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(directory)
        .map_err(|error| format!("{}: {error}", directory.display()))?;
    if metadata.file_type().is_symlink() {
        return Err("the target is a symbolic link".into());
    }
    if !metadata.is_dir() {
        return Err("the target is not a directory".into());
    }
    Ok(())
}

#[derive(Default)]
struct RemoveOutcome {
    bytes: u64,
    complete: bool,
    errors: Vec<String>,
}

/// Bounded, cancellable removal that never follows a symbolic link out of the
/// managed tree: a link is unlinked, never descended.
fn remove_tree(path: &Path, cancel: &AtomicBool) -> RemoveOutcome {
    let mut outcome = RemoveOutcome {
        complete: true,
        ..Default::default()
    };
    let mut budget = MAX_TREE_FILES + MAX_TREE_DIRECTORIES;
    remove_tree_inner(path, 0, &mut budget, &mut outcome, cancel);
    outcome
}

fn remove_tree_inner(
    path: &Path,
    depth: usize,
    budget: &mut usize,
    outcome: &mut RemoveOutcome,
    cancel: &AtomicBool,
) {
    if cancel.load(Ordering::Acquire) {
        outcome.complete = false;
        return;
    }
    if *budget == 0 || depth > MAX_TREE_DEPTH {
        outcome.complete = false;
        outcome
            .errors
            .push(format!("{}: removal limit reached", path.display()));
        return;
    }
    *budget -= 1;
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
        Err(error) => {
            outcome.complete = false;
            outcome.errors.push(format!("{}: {error}", path.display()));
            return;
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        let bytes = if metadata.file_type().is_symlink() {
            0
        } else {
            metadata.len()
        };
        match fs::remove_file(path) {
            Ok(()) => outcome.bytes = outcome.bytes.saturating_add(bytes),
            Err(error) => {
                outcome.complete = false;
                outcome.errors.push(format!("{}: {error}", path.display()));
            }
        }
        return;
    }
    let entries = match fs::read_dir(path) {
        Ok(entries) => entries,
        Err(error) => {
            outcome.complete = false;
            outcome.errors.push(format!("{}: {error}", path.display()));
            return;
        }
    };
    for entry in entries {
        match entry {
            Ok(entry) => remove_tree_inner(&entry.path(), depth + 1, budget, outcome, cancel),
            Err(error) => {
                outcome.complete = false;
                outcome.errors.push(format!("{}: {error}", path.display()));
            }
        }
        if !outcome.complete {
            return;
        }
    }
    if let Err(error) = fs::remove_dir(path) {
        outcome.complete = false;
        outcome.errors.push(format!("{}: {error}", path.display()));
    }
}

fn scan_capture(
    directory: &Path,
    source_id: SourceId,
    request: &ScanRequest,
    budget: &mut Budget,
    index: &mut OwnershipIndex,
    cancel: &AtomicBool,
) -> CaptureUnit {
    let before = index.truncated;
    let durable_bytes = tree_bytes(directory, 0, budget, index, cancel);
    let journal_path = directory.join(JOURNAL_FILE);
    let journal_bytes = fs::symlink_metadata(&journal_path)
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    let (name, generation) = read_source_metadata(directory, index);
    let mut notes = Vec::new();
    let active = request.active_sources.contains(&source_id)
        || match journal_writer_present(&journal_path) {
            Ok(value) => value,
            Err(error) => {
                notes.push(format!("capture activity could not be probed: {error}"));
                // Unknown activity is treated as active: never delete a capture
                // that might still be receiving records.
                true
            }
        };
    let (first_sequence, first_captured_at_unix_nanos) =
        probe_first_record(&journal_path, source_id);
    CaptureUnit {
        source_id,
        name,
        directory: directory.to_path_buf(),
        generation,
        journal_bytes,
        durable_bytes,
        derived_bytes: 0,
        last_modified_unix_nanos: modified_unix_nanos(&journal_path)
            .or_else(|| modified_unix_nanos(directory)),
        first_captured_at_unix_nanos,
        first_sequence,
        reserved_sequence: read_sequence_watermark(&journal_path),
        active,
        incomplete: index.truncated && !before,
        notes,
    }
}

fn scan_investigations(
    directory: &Path,
    request: &ScanRequest,
    budget: &mut Budget,
    index: &mut OwnershipIndex,
    cancel: &AtomicBool,
) {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) => {
            index.push_error(format!("investigations: {error}"));
            return;
        }
    };
    for (position, entry) in entries.take(MAX_ROOT_ENTRIES + 1).enumerate() {
        if cancel.load(Ordering::Acquire) {
            index.cancelled = true;
            return;
        }
        if position == MAX_ROOT_ENTRIES || index.investigations.len() >= MAX_INVESTIGATIONS {
            index.truncated = true;
            return;
        }
        let Ok(entry) = entry else {
            index.push_error("investigation entry unreadable".into());
            continue;
        };
        let path = entry.path();
        let Ok(metadata) = fs::symlink_metadata(&path) else {
            continue;
        };
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            continue;
        }
        let before = index.truncated;
        let durable_bytes = tree_bytes(&path, 0, budget, index, cancel);
        let name = entry.file_name().to_string_lossy().into_owned();
        let mut unit = InvestigationUnit {
            investigation_id: name.clone(),
            name: String::new(),
            directory: path.clone(),
            durable_bytes,
            created_at_unix_nanos: None,
            pins: Vec::new(),
            unknown_pins: false,
            incomplete: index.truncated && !before,
            notes: Vec::new(),
        };
        read_investigation_record(&path, &mut unit);
        read_manifest_pins(&path, &mut unit);
        if request.open_investigations.contains(&unit.investigation_id)
            || request.open_investigations.contains(&name)
        {
            unit.notes.push(OPEN_NOTE.into());
        }
        index.investigations.push(unit);
    }
}

fn read_source_metadata(directory: &Path, index: &mut OwnershipIndex) -> (String, u64) {
    let path = directory.join(SOURCE_METADATA_FILE);
    let fallback = directory
        .file_name()
        .map(|value| short_id(&value.to_string_lossy()))
        .unwrap_or_default();
    let value = match read_json(&path, MAX_METADATA_BYTES) {
        None => return (fallback, 0),
        Some(Err(_)) => {
            index.push_error(format!("{}: source metadata unreadable", path.display()));
            return (fallback, 0);
        }
        Some(Ok(value)) => value,
    };
    let name = value
        .get("definition")
        .and_then(|definition| definition.get("name"))
        .and_then(serde_json::Value::as_str)
        .filter(|name| !name.trim().is_empty())
        .map(|name| name.chars().take(80).collect::<String>())
        .unwrap_or(fallback);
    let generation = value
        .get("generation")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    (name, generation)
}

fn read_investigation_record(directory: &Path, unit: &mut InvestigationUnit) {
    let path = directory.join(INVESTIGATION_RECORD_FILE);
    match read_json(&path, MAX_RECORD_BYTES) {
        None => unit
            .notes
            .push("no investigation record; directory retained".into()),
        Some(Err(_)) => unit
            .notes
            .push("investigation record unreadable; directory retained".into()),
        Some(Ok(value)) => {
            if let Some(id) = value.get("id").and_then(serde_json::Value::as_str) {
                unit.investigation_id = id.into();
            }
            unit.name = value
                .get("question")
                .and_then(serde_json::Value::as_str)
                .map(|value| value.chars().take(60).collect::<String>())
                .unwrap_or_default();
            unit.created_at_unix_nanos = value
                .get("created_at")
                .and_then(serde_json::Value::as_i64)
                .or_else(|| {
                    value
                        .get("created_at_unix_nanos")
                        .and_then(serde_json::Value::as_i64)
                });
        }
    }
}

fn read_manifest_pins(directory: &Path, unit: &mut InvestigationUnit) {
    let path = directory.join(MANIFEST_FILE);
    let value = match read_json(&path, MAX_MANIFEST_BYTES) {
        // No manifest at all means the export never published a dataset, so
        // this investigation holds no reference to any capture.
        None => {
            unit.notes
                .push("no published dataset; no capture is pinned".into());
            return;
        }
        Some(Err(reason)) => {
            unit.unknown_pins = true;
            unit.notes.push(format!(
                "manifest unreadable ({reason}); dependencies unknown"
            ));
            return;
        }
        Some(Ok(value)) => value,
    };
    let parts = value
        .get("source_parts")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    let Some(sources) = value.get("sources").and_then(serde_json::Value::as_array) else {
        unit.unknown_pins = true;
        unit.notes
            .push("manifest has no source list; dependencies unknown".into());
        return;
    };
    for source in sources.iter().take(MAX_PINS_PER_INVESTIGATION) {
        let Some(source_id) = source
            .get("source_id")
            .and_then(serde_json::Value::as_str)
            .and_then(parse_source_id)
        else {
            unit.unknown_pins = true;
            unit.notes
                .push("manifest source entry is unreadable; dependencies unknown".into());
            continue;
        };
        let high_watermark = source
            .get("high_watermark")
            .and_then(serde_json::Value::as_u64);
        let key = source_id.0.to_string();
        let mut exported_rows = 0_u64;
        let mut covered: Option<u64> = None;
        for part in &parts {
            if part.get("source_id").and_then(serde_json::Value::as_str) != Some(key.as_str()) {
                continue;
            }
            exported_rows = exported_rows.saturating_add(
                part.get("rows")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0),
            );
            let last = part
                .get("last_sequence")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0);
            covered = Some(covered.map_or(last, |value| value.max(last)));
        }
        let exported_projection = exported_rows > 0
            && match (covered, high_watermark) {
                (Some(covered), Some(watermark)) => covered >= watermark,
                _ => false,
            };
        unit.pins.push(CapturePin {
            source_id,
            generation: source
                .get("generation")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0),
            high_watermark,
            exported_projection,
            exported_rows,
        });
    }
    if sources.len() > MAX_PINS_PER_INVESTIGATION {
        unit.unknown_pins = true;
        unit.notes
            .push("manifest lists more sources than the pin limit; dependencies unknown".into());
    }
}

/// `None` when the file does not exist, `Some(Err)` when it exists but cannot
/// be read or parsed. The distinction decides whether pins are absent or
/// unknown, and unknown must never be treated as absent.
fn read_json(path: &Path, limit: u64) -> Option<Result<serde_json::Value, String>> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(error) => return Some(Err(error.to_string())),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Some(Err("not a regular file".into()));
    }
    if metadata.len() > limit {
        return Some(Err("exceeds the bounded read limit".into()));
    }
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) => return Some(Err(error.to_string())),
    };
    let mut bytes = Vec::new();
    if let Err(error) = file.take(limit + 1).read_to_end(&mut bytes) {
        return Some(Err(error.to_string()));
    }
    if bytes.len() as u64 > limit {
        return Some(Err("exceeds the bounded read limit".into()));
    }
    Some(serde_json::from_slice(&bytes).map_err(|error| error.to_string()))
}

/// A held journal lock means a writer owns this capture right now.
///
/// The journal owns both halves of that answer — an in-process registry and a
/// record lock — so this asks it rather than reaching for the lock file, which
/// a probe cannot open and close without releasing our own locks on it.
fn journal_writer_present(journal: &Path) -> std::io::Result<bool> {
    lvu_core::journal::writer_present(journal)
}

fn probe_first_record(journal: &Path, source_id: SourceId) -> (Option<u64>, Option<i64>) {
    let Ok(mut reader) = JournalReader::open(journal, source_id) else {
        return (None, None);
    };
    match reader.read_page(0, 1, 64 * 1024) {
        Ok(page) => page.records.first().map_or((None, None), |record| {
            (
                Some(record.record_id.sequence),
                Some(record.captured_at_unix_nanos),
            )
        }),
        Err(_) => (None, None),
    }
}

fn read_sequence_watermark(journal: &Path) -> Option<u64> {
    let mut name = journal.as_os_str().to_owned();
    name.push(".seq");
    let bytes = fs::read(PathBuf::from(name)).ok()?;
    let bytes: [u8; 8] = bytes.try_into().ok()?;
    Some(u64::from_le_bytes(bytes))
}

fn modified_unix_nanos(path: &Path) -> Option<i64> {
    fs::symlink_metadata(path)
        .ok()?
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|value| i64::try_from(value.as_nanos()).ok())
}

fn tree_bytes(
    path: &Path,
    depth: usize,
    budget: &mut Budget,
    index: &mut OwnershipIndex,
    cancel: &AtomicBool,
) -> u64 {
    if cancel.load(Ordering::Acquire) {
        index.cancelled = true;
        return 0;
    }
    if depth > MAX_TREE_DEPTH
        || budget.files >= MAX_TREE_FILES
        || budget.directories >= MAX_TREE_DIRECTORIES
    {
        index.truncated = true;
        return 0;
    }
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) => {
            index.push_error(format!("{}: {error}", path.display()));
            return 0;
        }
    };
    if metadata.file_type().is_symlink() {
        return 0;
    }
    if metadata.is_file() {
        budget.files += 1;
        return metadata.len();
    }
    if !metadata.is_dir() {
        return 0;
    }
    budget.directories += 1;
    let entries = match fs::read_dir(path) {
        Ok(entries) => entries,
        Err(error) => {
            index.push_error(format!("{}: {error}", path.display()));
            return 0;
        }
    };
    let mut total = 0_u64;
    for (position, entry) in entries.take(MAX_ROOT_ENTRIES + 1).enumerate() {
        if position == MAX_ROOT_ENTRIES {
            index.truncated = true;
            break;
        }
        match entry {
            Ok(entry) => {
                total = total.saturating_add(tree_bytes(
                    &entry.path(),
                    depth + 1,
                    budget,
                    index,
                    cancel,
                ))
            }
            Err(error) => index.push_error(error.to_string()),
        }
    }
    total
}

pub fn parse_source_id(value: &str) -> Option<SourceId> {
    if value.len() != 36 {
        return None;
    }
    value.parse::<uuid::Uuid>().ok().map(SourceId)
}

pub fn short_id(value: &str) -> String {
    value.chars().take(8).collect()
}
