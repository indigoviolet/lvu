//! The bounded worker and typed API the Storage screen renders.
//!
//! Every request returns a fresh inventory alongside its result, so a preview,
//! a deletion and a retention run all leave the screen consistent without a
//! second round trip. Nothing here blocks the UI thread: `GovernanceJob` owns a
//! cancellable worker with the same lifecycle as the existing storage scan.

use std::{
    collections::BTreeSet,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use lvu::{StorageCategory, StorageEntry, StorageSnapshot};
use lvu_core::SourceId;
use lvu_live::{DerivedArtifactIdentity, DerivedArtifactStatus, LiveRowProvider};

use super::{
    ledger::{
        CaptureGap, DeletionCause, DeletionLedger, DeletionState, capture_gaps, format_bytes,
        now_unix_nanos,
    },
    ownership::{
        CaptureUnit, DeletionBlocker, DeletionOutcome, DeletionPlan, DeletionTarget,
        DerivedIndexEntry, InvestigationRef, InvestigationUnit, OwnershipIndex, ScanRequest,
        parse_source_id,
    },
    pressure::{
        CacheClass, CacheUsage, DiskSpace, Durability, PressureDecision, PressureInputs,
        PressureLevel, assess as assess_pressure, disk_space,
    },
    retention::{
        RetentionAssessment, RetentionRules, apply as apply_retention, assess as assess_retention,
    },
};

const MAX_DERIVED_ENTRIES: usize = 512;

/// Everything the worker needs that the application owns.
#[derive(Clone)]
pub(crate) struct GovernanceContext {
    pub root: PathBuf,
    pub provider: Arc<LiveRowProvider>,
    /// Sources the application currently has open.
    pub active_sources: BTreeSet<SourceId>,
    /// Investigation identities or directory names currently open.
    pub open_investigations: BTreeSet<String>,
    pub rules: RetentionRules,
    pub reserve_bytes: u64,
    pub membership_bytes: u64,
    pub membership_limit: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum GovernanceRequest {
    /// Refresh usage, gaps, pressure and the retention assessment.
    Inventory,
    /// Produce a deletion preview. Nothing is removed.
    PreviewCapture(SourceId),
    PreviewInvestigation(InvestigationRef),
    /// Execute a previously previewed deletion. `digest` must be the digest of
    /// that preview; a changed situation is refused, never silently re-planned.
    Delete {
        target: DeletionTarget,
        digest: u64,
    },
    /// Execute the previewed retention assessment.
    ApplyRetention {
        digest: u64,
    },
    /// Reclaim verified-unused disposable derived indexes only.
    ReclaimDisposable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum GovernanceKind {
    Inventory,
    Preview,
    Deletion,
    Retention,
    Reclaim,
}

pub(crate) struct GovernanceResult {
    pub generation: u64,
    pub kind: GovernanceKind,
    /// Always present unless the request was cancelled before the scan.
    pub inventory: StorageInventory,
    pub plan: Option<DeletionPlan>,
    pub outcomes: Vec<DeletionOutcome>,
    pub status: String,
}

#[derive(Clone, Debug)]
pub(crate) struct UsageTotals {
    pub durable_bytes: u64,
    pub disposable_bytes: u64,
    /// Space that can be freed without deleting anything durable.
    pub reclaimable_bytes: u64,
    pub capture_bytes: u64,
    pub investigation_bytes: u64,
    pub workspace_bytes: u64,
    pub memory_bytes: u64,
}

#[derive(Clone, Debug)]
pub(crate) struct CaptureUsage {
    pub source_id: SourceId,
    pub name: String,
    pub label: String,
    /// Captured original bytes and their metadata.
    pub durable_bytes: u64,
    /// Disposable indexes attributed to this source.
    pub disposable_bytes: u64,
    pub active: bool,
    pub last_modified_unix_nanos: Option<i64>,
    /// False when an investigation pins it or it is still capturing.
    pub deletable: bool,
    pub blocked_reason: Option<String>,
    /// Recorded deletions affecting this source's history.
    pub recorded_gaps: usize,
    pub notes: Vec<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct InvestigationUsage {
    pub reference: InvestigationRef,
    pub investigation_id: String,
    pub label: String,
    pub durable_bytes: u64,
    pub created_at_unix_nanos: Option<i64>,
    pub pinned_captures: Vec<String>,
    pub unknown_pins: bool,
    pub deletable: bool,
    pub blocked_reason: Option<String>,
    pub notes: Vec<String>,
}

/// Inspectable accounting: usage by source, by investigation and by cache
/// class, with durable and disposable kept apart everywhere.
#[derive(Clone, Debug)]
pub(crate) struct StorageInventory {
    pub root: PathBuf,
    pub captures: Vec<CaptureUsage>,
    pub investigations: Vec<InvestigationUsage>,
    pub caches: Vec<CacheUsage>,
    /// Recorded deletion boundaries. The screen must show these so a gap is
    /// never presented as uninterrupted capture.
    pub gaps: Vec<CaptureGap>,
    pub totals: UsageTotals,
    pub pressure: PressureDecision,
    pub retention: RetentionAssessment,
    pub disk: Option<DiskSpace>,
    pub truncated: bool,
    pub cancelled: bool,
    pub errors: Vec<String>,
}

impl StorageInventory {
    /// Digest of the retention assessment, for `ApplyRetention`.
    pub fn retention_digest(&self) -> u64 {
        self.retention
            .plans
            .iter()
            .fold(0_u64, |total, plan| total.rotate_left(7) ^ plan.digest)
    }

    /// Projection onto the existing Storage dialog snapshot so the current
    /// screen keeps working while the richer surface is wired.
    pub fn to_snapshot(
        &self,
        row_cache: (u64, u64),
        query_index: (u64, u64),
        derived_limits: (u64, u64),
    ) -> StorageSnapshot {
        let mut entries = Vec::new();
        for capture in &self.captures {
            entries.push(StorageEntry {
                category: StorageCategory::Capture,
                label: capture.label.clone(),
                bytes: capture.durable_bytes,
                reclaimable: 0,
                status: if capture.deletable {
                    "durable; deletable with a recorded boundary".into()
                } else {
                    capture
                        .blocked_reason
                        .clone()
                        .unwrap_or_else(|| "durable; preserved".into())
                },
            });
        }
        for investigation in &self.investigations {
            entries.push(StorageEntry {
                category: StorageCategory::Investigation,
                label: investigation.label.clone(),
                bytes: investigation.durable_bytes,
                reclaimable: 0,
                status: if investigation.pinned_captures.is_empty() {
                    "durable; pins no capture".into()
                } else {
                    format!(
                        "durable; pins {} capture(s)",
                        investigation.pinned_captures.len()
                    )
                },
            });
        }
        for cache in &self.caches {
            if cache.durability != Durability::Disposable || cache.class == CacheClass::RowCache {
                continue;
            }
            entries.push(StorageEntry {
                category: StorageCategory::Derived,
                label: cache.label.clone(),
                bytes: cache.bytes,
                reclaimable: cache.reclaimable_bytes,
                status: cache.note.clone(),
            });
        }
        if self.totals.workspace_bytes > 0 {
            entries.push(StorageEntry {
                category: StorageCategory::Workspace,
                label: "workspace memory + recipes".into(),
                bytes: self.totals.workspace_bytes,
                reclaimable: 0,
                status: "durable; preserved".into(),
            });
        }
        StorageSnapshot {
            entries,
            total_bytes: self
                .totals
                .durable_bytes
                .saturating_add(self.totals.disposable_bytes),
            reclaimable_bytes: self.totals.reclaimable_bytes,
            row_cache_bytes: row_cache.0,
            row_cache_limit: row_cache.1,
            query_index_bytes: query_index.0,
            query_index_limit: query_index.1,
            derived_index_limit_per_source: derived_limits.0,
            derived_index_limit_total: derived_limits.1,
            truncated: self.truncated,
            errors: self.errors.clone(),
        }
    }
}

pub(crate) struct GovernanceJob {
    cancel: Arc<AtomicBool>,
    rx: mpsc::Receiver<GovernanceResult>,
    task: Option<JoinHandle<()>>,
}

impl GovernanceJob {
    pub fn start(generation: u64, request: GovernanceRequest, context: GovernanceContext) -> Self {
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel);
        let (tx, rx) = mpsc::sync_channel(1);
        let task = thread::spawn(move || {
            let result = run(generation, request, &context, &worker_cancel);
            let _ = tx.send(result);
        });
        Self {
            cancel,
            rx,
            task: Some(task),
        }
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Release);
    }

    pub fn poll(&self) -> Option<GovernanceResult> {
        self.rx.try_recv().ok()
    }

    pub fn finished(&self) -> bool {
        self.task.as_ref().is_none_or(JoinHandle::is_finished)
    }

    pub fn join(&mut self) {
        if let Some(task) = self.task.take() {
            let _ = task.join();
        }
    }

    pub fn settle(&mut self, timeout: Duration) -> Result<(), String> {
        self.cancel();
        let deadline = std::time::Instant::now() + timeout;
        while !self.finished() && std::time::Instant::now() < deadline {
            thread::sleep(Duration::from_millis(1));
        }
        if !self.finished() {
            return Err(
                "storage governance worker did not settle before the shutdown deadline".into(),
            );
        }
        self.join();
        Ok(())
    }
}

fn run(
    generation: u64,
    request: GovernanceRequest,
    context: &GovernanceContext,
    cancel: &AtomicBool,
) -> GovernanceResult {
    let ledger = DeletionLedger::new(&context.root);
    let (index, reviewed, mut errors) = build_index(context, cancel);
    let mut plan = None;
    let mut outcomes = Vec::new();
    let mut kind = GovernanceKind::Inventory;
    let mut status = String::new();

    match request {
        GovernanceRequest::Inventory => {}
        GovernanceRequest::PreviewCapture(source_id) => {
            kind = GovernanceKind::Preview;
            let value = index.plan_delete_capture(source_id, DeletionCause::UserRequested, None);
            status = value.summary();
            plan = Some(value);
        }
        GovernanceRequest::PreviewInvestigation(reference) => {
            kind = GovernanceKind::Preview;
            let value =
                index.plan_delete_investigation(reference, DeletionCause::UserRequested, None);
            status = value.summary();
            plan = Some(value);
        }
        GovernanceRequest::Delete { target, digest } => {
            kind = GovernanceKind::Deletion;
            let current = match target {
                DeletionTarget::Capture(source_id) => {
                    index.plan_delete_capture(source_id, DeletionCause::UserRequested, None)
                }
                DeletionTarget::Investigation(reference) => {
                    index.plan_delete_investigation(reference, DeletionCause::UserRequested, None)
                }
            };
            let mut confirmed = current.clone();
            confirmed.digest = digest;
            let outcome = index.execute(&confirmed, &ledger, cancel);
            status = describe(&outcome);
            plan = Some(current);
            outcomes.push(outcome);
        }
        GovernanceRequest::ApplyRetention { digest } => {
            kind = GovernanceKind::Retention;
            let assessment = assess_retention(&index, &context.rules, now_unix_nanos());
            let expected = assessment
                .plans
                .iter()
                .fold(0_u64, |total, plan| total.rotate_left(7) ^ plan.digest);
            if expected != digest {
                status = "Storage changed since this retention preview was produced. Nothing was deleted; review it again.".into();
            } else if assessment.plans.is_empty() {
                status = assessment.summary.clone();
            } else {
                outcomes = apply_retention(&index, &assessment, &ledger, cancel);
                status = describe_many(&outcomes);
            }
        }
        GovernanceRequest::ReclaimDisposable => {
            kind = GovernanceKind::Reclaim;
            let (freed, busy, message) = reclaim(context, &reviewed, cancel);
            status = format!(
                "reclaimed {} of disposable cache; {busy} in use and skipped; {message}",
                format_bytes(freed)
            );
        }
    }

    // Actions change the picture, so rebuild before reporting.
    let (index, _, more_errors) =
        if matches!(kind, GovernanceKind::Inventory | GovernanceKind::Preview) {
            (index, reviewed, Vec::new())
        } else {
            build_index(context, cancel)
        };
    errors.extend(more_errors);
    let mut inventory = summarize(context, index, &ledger, cancel);
    inventory.errors.extend(errors);
    if status.is_empty() {
        status = inventory_status(&inventory);
    }
    GovernanceResult {
        generation,
        kind,
        inventory,
        plan,
        outcomes,
        status,
    }
}

fn describe(outcome: &DeletionOutcome) -> String {
    match outcome.state {
        DeletionState::Completed => format!(
            "Deleted {}: {} freed. The boundary is recorded and stays visible.",
            outcome.label,
            format_bytes(outcome.bytes_freed)
        ),
        _ if !outcome.blockers.is_empty() => outcome.detail.clone(),
        _ => format!("{}: {}", outcome.label, outcome.detail),
    }
}

fn describe_many(outcomes: &[DeletionOutcome]) -> String {
    let completed = outcomes
        .iter()
        .filter(|outcome| outcome.state == DeletionState::Completed)
        .count();
    let freed = outcomes.iter().fold(0_u64, |total, outcome| {
        total.saturating_add(outcome.bytes_freed)
    });
    let failed = outcomes.len() - completed;
    let mut text = format!(
        "Retention removed {completed} capture(s), freeing {}. Each boundary is recorded.",
        format_bytes(freed)
    );
    if failed > 0 {
        text.push_str(&format!(
            " {failed} were refused or left incomplete: {}",
            outcomes
                .iter()
                .find(|outcome| outcome.state != DeletionState::Completed)
                .map(|outcome| outcome.detail.clone())
                .unwrap_or_default()
        ));
    }
    text
}

fn inventory_status(inventory: &StorageInventory) -> String {
    let mut text = format!(
        "{} durable, {} disposable, {} reclaimable without deleting captured data.",
        format_bytes(inventory.totals.durable_bytes),
        format_bytes(inventory.totals.disposable_bytes),
        format_bytes(inventory.totals.reclaimable_bytes)
    );
    if inventory.pressure.level != PressureLevel::Normal {
        text.push(' ');
        text.push_str(&inventory.pressure.explanation);
    }
    if !inventory.gaps.is_empty() {
        text.push_str(&format!(
            " {} recorded deletion boundary(ies).",
            inventory.gaps.len()
        ));
    }
    if inventory.truncated {
        text.push_str(" Scan limit reached; totals are a lower bound.");
    }
    text
}

fn build_index(
    context: &GovernanceContext,
    cancel: &AtomicBool,
) -> (OwnershipIndex, Vec<DerivedArtifactIdentity>, Vec<String>) {
    let (derived, reviewed, errors) = inspect_derived(&context.provider, cancel);
    let request = ScanRequest {
        root: context.root.clone(),
        derived,
        active_sources: context.active_sources.clone(),
        open_investigations: context.open_investigations.clone(),
    };
    (OwnershipIndex::build(&request, cancel), reviewed, errors)
}

fn inspect_derived(
    provider: &LiveRowProvider,
    cancel: &AtomicBool,
) -> (
    Vec<DerivedIndexEntry>,
    Vec<DerivedArtifactIdentity>,
    Vec<String>,
) {
    let mut entries = Vec::new();
    let mut reviewed = Vec::new();
    let mut errors = Vec::new();
    match provider.artifact_directory_is_current() {
        Ok(true) => {}
        Ok(false) => {
            errors.push(
                "derived index directory changed or is a symlink; preserved without traversal"
                    .into(),
            );
            return (entries, reviewed, errors);
        }
        Err(error) => {
            errors.push(format!("derived index directory: {error}"));
            return (entries, reviewed, errors);
        }
    }
    let paths = match provider.derived_artifact_paths(MAX_DERIVED_ENTRIES) {
        Ok((paths, truncated)) => {
            if truncated {
                errors.push("derived index listing hit its scan limit".into());
            }
            paths
        }
        Err(error) => {
            errors.push(format!("derived indexes: {error}"));
            return (entries, reviewed, errors);
        }
    };
    for (path, known_bytes) in paths {
        if cancel.load(Ordering::Acquire) {
            break;
        }
        let source_id = path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.split('.').next())
            .and_then(parse_source_id);
        let (bytes, reclaimable, status) =
            match provider.inspect_derived_artifact_cancellable(&path, cancel) {
                Ok(DerivedArtifactStatus::Unused { bytes, identity }) => {
                    reviewed.push(identity);
                    (bytes, bytes, "unused; recomputable".to_string())
                }
                Ok(DerivedArtifactStatus::Active) => {
                    (known_bytes, 0, "active or locked; skipped".to_string())
                }
                Ok(DerivedArtifactStatus::NotOwned) => (
                    known_bytes,
                    0,
                    "unrecognized or symlink; preserved".to_string(),
                ),
                Ok(DerivedArtifactStatus::Unverified { bytes, reason }) => (bytes, 0, reason),
                Ok(DerivedArtifactStatus::Missing) => continue,
                Err(error) => {
                    errors.push(format!("{}: {error}", path.display()));
                    continue;
                }
            };
        entries.push(DerivedIndexEntry {
            path,
            bytes,
            source_id,
            reclaimable_bytes: reclaimable,
            status,
        });
    }
    (entries, reviewed, errors)
}

fn reclaim(
    context: &GovernanceContext,
    reviewed: &[DerivedArtifactIdentity],
    cancel: &AtomicBool,
) -> (u64, usize, String) {
    let mut freed = 0_u64;
    let mut busy = 0_usize;
    let mut message = "captured records and command results preserved".to_string();
    for identity in reviewed {
        if cancel.load(Ordering::Acquire) {
            message = "cancelled; captured records and command results preserved".into();
            break;
        }
        match context
            .provider
            .remove_unused_derived_artifact_cancellable(identity, cancel)
        {
            Ok(bytes) if bytes > 0 => freed = freed.saturating_add(bytes),
            Ok(_) => busy += 1,
            Err(error) => {
                busy += 1;
                message = format!("reclaim incomplete: {error}; durable data preserved");
            }
        }
    }
    (freed, busy, message)
}

fn summarize(
    context: &GovernanceContext,
    index: OwnershipIndex,
    ledger: &DeletionLedger,
    _cancel: &AtomicBool,
) -> StorageInventory {
    let readout = ledger.read();
    let gaps = capture_gaps(&readout);
    let budget = context.provider.storage_budget();
    let retention = assess_retention(&index, &context.rules, now_unix_nanos());

    let captures = index
        .captures
        .iter()
        .map(|capture| capture_usage(&index, capture, &gaps))
        .collect::<Vec<_>>();
    let investigations = index
        .investigations
        .iter()
        .map(|investigation| investigation_usage(&index, investigation))
        .collect::<Vec<_>>();

    let derived_bytes = index.derived_bytes();
    let reclaimable_derived = index.reclaimable_derived_bytes();
    let capture_bytes = index.durable_capture_bytes();
    let investigation_bytes = index.investigation_bytes();
    let caches = vec![
        CacheUsage::new(CacheClass::RawCapture, capture_bytes, None, 0),
        CacheUsage::new(
            CacheClass::InvestigationExport,
            investigation_bytes,
            None,
            0,
        ),
        CacheUsage::new(CacheClass::Workspace, index.workspace_bytes, None, 0),
        CacheUsage::new(
            CacheClass::DerivedIndex,
            derived_bytes,
            Some(budget.maximum_total_index_bytes),
            reclaimable_derived,
        ),
        CacheUsage::new(
            CacheClass::RowCache,
            budget.row_cache_bytes as u64,
            Some(budget.row_cache_limit as u64),
            budget.row_cache_bytes as u64,
        ),
        CacheUsage::new(
            CacheClass::QueryMembership,
            context.membership_bytes,
            Some(context.membership_limit),
            context.membership_bytes,
        ),
    ];

    let disk = disk_space(&context.root).ok();
    let pressure = assess_pressure(&PressureInputs {
        disk,
        reserve_bytes: context.reserve_bytes,
        derived_index_bytes: derived_bytes,
        derived_index_limit: budget.maximum_total_index_bytes,
        row_cache_bytes: budget.row_cache_bytes as u64,
        row_cache_limit: budget.row_cache_limit as u64,
        membership_bytes: context.membership_bytes,
        membership_limit: context.membership_limit,
        reclaimable_disk_bytes: reclaimable_derived,
    });

    let durable_bytes = capture_bytes
        .saturating_add(investigation_bytes)
        .saturating_add(index.workspace_bytes);
    let memory_bytes = (budget.row_cache_bytes as u64).saturating_add(context.membership_bytes);
    let mut errors = index.errors.clone();
    errors.extend(readout.errors.iter().cloned());

    StorageInventory {
        root: index.root.clone(),
        captures,
        investigations,
        caches,
        gaps,
        totals: UsageTotals {
            durable_bytes,
            disposable_bytes: derived_bytes.saturating_add(memory_bytes),
            reclaimable_bytes: reclaimable_derived,
            capture_bytes,
            investigation_bytes,
            workspace_bytes: index.workspace_bytes,
            memory_bytes,
        },
        pressure,
        retention,
        disk,
        truncated: index.truncated || readout.truncated,
        cancelled: index.cancelled,
        errors,
    }
}

fn capture_usage(
    index: &OwnershipIndex,
    capture: &CaptureUnit,
    gaps: &[CaptureGap],
) -> CaptureUsage {
    let plan = index.plan_delete_capture(capture.source_id, DeletionCause::UserRequested, None);
    let key = capture.source_id.0.to_string();
    CaptureUsage {
        source_id: capture.source_id,
        name: capture.name.clone(),
        label: capture.label(),
        durable_bytes: capture.durable_bytes,
        disposable_bytes: capture.derived_bytes,
        active: capture.active,
        last_modified_unix_nanos: capture.last_modified_unix_nanos,
        deletable: plan.allowed(),
        blocked_reason: plan.blockers.first().map(DeletionBlocker::explanation),
        recorded_gaps: gaps.iter().filter(|gap| gap.source_id == key).count(),
        notes: capture.notes.clone(),
    }
}

fn investigation_usage(
    index: &OwnershipIndex,
    investigation: &InvestigationUnit,
) -> InvestigationUsage {
    let reference = investigation
        .directory
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(InvestigationRef::new);
    let plan = reference.map(|reference| {
        index.plan_delete_investigation(reference, DeletionCause::UserRequested, None)
    });
    InvestigationUsage {
        reference: reference.unwrap_or_else(|| InvestigationRef::new("?").expect("literal")),
        investigation_id: investigation.investigation_id.clone(),
        label: investigation.label(),
        durable_bytes: investigation.durable_bytes,
        created_at_unix_nanos: investigation.created_at_unix_nanos,
        pinned_captures: investigation
            .pins
            .iter()
            .map(|pin| {
                index
                    .capture(pin.source_id)
                    .map(CaptureUnit::label)
                    .unwrap_or_else(|| format!("{} (missing)", pin.source_id.0))
            })
            .collect(),
        unknown_pins: investigation.unknown_pins,
        deletable: plan.as_ref().is_some_and(DeletionPlan::allowed),
        blocked_reason: plan
            .as_ref()
            .and_then(|plan| plan.blockers.first().map(DeletionBlocker::explanation)),
        notes: investigation.notes.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lvu_core::{ChunkPosition, Journal, RawRecord, RecordId, StreamKind};
    use lvu_live::LiveConfig;
    use std::{fs, path::Path};
    use tempfile::tempdir;

    fn capture(root: &Path, name: &str) -> SourceId {
        let source_id = SourceId::new();
        let directory = root.join(source_id.0.to_string());
        fs::create_dir_all(&directory).unwrap();
        fs::write(
            directory.join("source.json"),
            serde_json::json!({
                "schema_version": 1,
                "generation": 1,
                "definition": {"name": name},
            })
            .to_string(),
        )
        .unwrap();
        let (mut journal, _) = Journal::open(directory.join("capture.journal"), source_id).unwrap();
        for index in 0..8 {
            journal
                .append(RawRecord {
                    record_id: RecordId {
                        source_id,
                        sequence: 0,
                    },
                    captured_at_unix_nanos: 1_000 + index,
                    stream: StreamKind::File,
                    bytes: vec![b'x'; 128],
                    delimiter: b"\n".to_vec(),
                    acquisition_id: uuid::Uuid::nil(),
                    chunk: ChunkPosition::Complete,
                })
                .unwrap();
        }
        journal.sync_data().unwrap();
        source_id
    }

    fn investigation(root: &Path, pinned: SourceId) {
        let directory = root.join("investigations").join("inv-1");
        fs::create_dir_all(&directory).unwrap();
        fs::write(
            directory.join("lvu-investigation.json"),
            serde_json::json!({"schema_version": 1, "id": "inv-1", "question": "why"}).to_string(),
        )
        .unwrap();
        fs::write(
            directory.join("manifest.json"),
            serde_json::json!({
                "schema_version": 2,
                "sources": [{"source_id": pinned.0.to_string(), "generation": 1, "high_watermark": 7}],
                "source_parts": [],
            })
            .to_string(),
        )
        .unwrap();
        fs::write(directory.join("part-0.parquet"), vec![3_u8; 2048]).unwrap();
    }

    fn context(root: &Path, cache: &Path) -> GovernanceContext {
        GovernanceContext {
            root: root.to_path_buf(),
            provider: Arc::new(LiveRowProvider::new(LiveConfig::new(cache)).unwrap()),
            active_sources: BTreeSet::new(),
            open_investigations: BTreeSet::new(),
            rules: RetentionRules::default(),
            reserve_bytes: 0,
            membership_bytes: 1_024,
            membership_limit: 256 * 1024 * 1024,
        }
    }

    fn wait(mut job: GovernanceJob) -> GovernanceResult {
        loop {
            if let Some(result) = job.poll() {
                job.join();
                return result;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[tokio::test]
    async fn inventory_separates_durable_data_from_disposable_cache_and_names_pins() {
        let temp = tempdir().unwrap();
        let cache = tempdir().unwrap();
        let pinned = capture(temp.path(), "pinned");
        let free = capture(temp.path(), "free");
        investigation(temp.path(), pinned);
        fs::create_dir_all(temp.path().join("workspace")).unwrap();
        fs::write(
            temp.path().join("workspace/workspace.sqlite3"),
            vec![1_u8; 512],
        )
        .unwrap();

        let result = wait(GovernanceJob::start(
            7,
            GovernanceRequest::Inventory,
            context(temp.path(), cache.path()),
        ));
        assert_eq!(result.generation, 7);
        let inventory = result.inventory;
        assert_eq!(inventory.captures.len(), 2);
        let pinned_row = inventory
            .captures
            .iter()
            .find(|row| row.source_id == pinned)
            .unwrap();
        assert!(!pinned_row.deletable);
        assert!(
            pinned_row
                .blocked_reason
                .as_deref()
                .unwrap()
                .contains("depends on this capture")
        );
        assert!(
            inventory
                .captures
                .iter()
                .find(|row| row.source_id == free)
                .unwrap()
                .deletable
        );
        assert_eq!(inventory.investigations.len(), 1);
        assert_eq!(inventory.investigations[0].pinned_captures.len(), 1);

        // Durable totals never leak into what the user is told is reclaimable.
        assert!(inventory.totals.capture_bytes > 0);
        assert!(inventory.totals.workspace_bytes >= 512);
        assert_eq!(inventory.totals.reclaimable_bytes, 0);
        assert!(
            inventory
                .caches
                .iter()
                .filter(|cache| cache.durability == Durability::Durable)
                .all(|cache| cache.reclaimable_bytes == 0)
        );
        assert!(inventory.gaps.is_empty());
        assert!(!inventory.retention.active);
        assert_eq!(inventory.pressure.acquisition_error, None);
        assert!(inventory.to_snapshot((0, 0), (0, 0), (0, 0)).total_bytes > 0);
    }

    #[tokio::test]
    async fn a_confirmed_deletion_frees_space_and_leaves_the_gap_in_the_next_inventory() {
        let temp = tempdir().unwrap();
        let cache = tempdir().unwrap();
        let source = capture(temp.path(), "syslog");
        let context = context(temp.path(), cache.path());

        let preview = wait(GovernanceJob::start(
            1,
            GovernanceRequest::PreviewCapture(source),
            context.clone(),
        ));
        let plan = preview.plan.expect("a preview plan");
        assert!(plan.allowed());
        assert!(
            preview.status.contains("recorded as a gap"),
            "{}",
            preview.status
        );

        let confirmed = wait(GovernanceJob::start(
            2,
            GovernanceRequest::Delete {
                target: DeletionTarget::Capture(source),
                digest: plan.digest,
            },
            context.clone(),
        ));
        assert_eq!(confirmed.outcomes.len(), 1);
        assert_eq!(confirmed.outcomes[0].state, DeletionState::Completed);
        assert!(!temp.path().join(source.0.to_string()).exists());
        assert!(confirmed.inventory.captures.is_empty());
        assert_eq!(confirmed.inventory.gaps.len(), 1);

        // A replayed confirmation cannot delete anything else.
        let replay = wait(GovernanceJob::start(
            3,
            GovernanceRequest::Delete {
                target: DeletionTarget::Capture(source),
                digest: plan.digest,
            },
            context,
        ));
        assert!(replay.outcomes[0].bytes_freed == 0);
        assert!(!replay.outcomes[0].blockers.is_empty());
        assert_eq!(replay.inventory.gaps.len(), 1);
    }

    #[tokio::test]
    async fn a_worker_settles_on_cancellation_within_its_deadline() {
        let temp = tempdir().unwrap();
        let cache = tempdir().unwrap();
        capture(temp.path(), "api");
        let mut job = GovernanceJob::start(
            1,
            GovernanceRequest::Inventory,
            context(temp.path(), cache.path()),
        );
        job.settle(Duration::from_secs(5)).unwrap();
        assert!(job.finished());
    }
}
