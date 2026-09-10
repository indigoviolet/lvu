//! Immutable, bounded Parquet snapshots for local investigation agents.

mod assistance;
pub use assistance::*;

use super::{CommandColumns, Membership, NativeViewAdapter, Published, ViewError};
use lvu_core::{InvestigationId, RawRecord, SourceId};
use lvu_ingest::SourceHandle;
use lvu_query::{
    AtomicParquetPartWriter, BatchQuery, BatchValidity, DerivedState, EnrichmentStage,
    ParquetWriteBudget, SchemaContext, execute_batch, records_to_batch_with_context,
};
use polars::prelude::{
    AnyValue, BooleanChunked, DataFrame, IntoColumn, NamedFrom, NewChunkedArray, Series,
};
use serde::Serialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread::{self, JoinHandle},
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Copy, Debug)]
pub struct FrozenInputLimits {
    pub batch_records: usize,
    pub batch_bytes: usize,
    pub maximum_scanned_records: u64,
    pub maximum_input_bytes: u64,
    pub maximum_output_records: u64,
    pub maximum_output_bytes: u64,
}

impl Default for FrozenInputLimits {
    fn default() -> Self {
        Self {
            batch_records: 1_024,
            batch_bytes: 8 * 1024 * 1024,
            maximum_scanned_records: 10_000_000,
            maximum_input_bytes: 16 * 1024 * 1024 * 1024,
            maximum_output_records: 10_000_000,
            maximum_output_bytes: 16 * 1024 * 1024 * 1024,
        }
    }
}

impl FrozenInputLimits {
    fn valid(self) -> bool {
        self.batch_records > 0
            && self.batch_bytes > 0
            && self.maximum_scanned_records > 0
            && self.maximum_input_bytes > 0
            && self.maximum_output_records > 0
            && self.maximum_output_bytes > 0
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FrozenInputSource {
    pub source_id: SourceId,
    pub generation: u64,
    pub high_watermark: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FrozenInputSummary {
    pub view_id: String,
    pub applied_revision: u64,
    pub applied_generation: u64,
    /// Exact for an applied filtered view. Raw views require a bounded scan.
    pub selected_records: Option<u64>,
    pub sources: Vec<FrozenInputSource>,
    /// Compiled output names from this exact accepted membership. This is the
    /// authority for workflows selecting derived cells; raw fields and caller
    /// inventories are not evidence. Slash captures are already expanded by
    /// compilation before they reach this list.
    pub accepted_enrichment_outputs: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FrozenInputRow {
    pub record: RawRecord,
    pub fields: BTreeMap<String, serde_json::Value>,
    /// Native column dtype for every field, including typed nulls.
    pub field_types: BTreeMap<String, String>,
    /// Raw parser type evidence, kept separate from the canonical physical dtype.
    pub raw_field_types: BTreeMap<String, String>,
    /// Assistance-only whole-value omissions; ordinary frozen input fails instead.
    pub omitted_fields: BTreeMap<String, String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FrozenInputBatch {
    pub rows: Vec<FrozenInputRow>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FrozenInputStats {
    pub scanned_records: u64,
    pub input_bytes: u64,
    pub output_records: u64,
    pub output_bytes: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum FrozenInputError {
    #[error("frozen input was cancelled")]
    Cancelled,
    #[error("frozen input limit reached: {0}")]
    Limited(String),
    #[error("frozen input replay failed: {0}")]
    Replay(String),
    #[error("frozen input visitor failed: {0}")]
    Visitor(String),
}

pub struct FrozenInput {
    frozen: FrozenView,
    limits: FrozenInputLimits,
    summary: FrozenInputSummary,
    _lease: JobLease,
}

impl FrozenInput {
    pub fn summary(&self) -> &FrozenInputSummary {
        &self.summary
    }

    /// Replays the accepted view on the calling thread and visits bounded batches.
    pub fn visit(
        &self,
        cancel: &AtomicBool,
        mut visitor: impl FnMut(FrozenInputBatch) -> Result<(), String>,
    ) -> Result<FrozenInputStats, FrozenInputError> {
        visit_frozen_input(
            &self.frozen,
            self.limits,
            cancel,
            false,
            false,
            &mut visitor,
        )
    }

    /// Precise replay for consumers that need native dtype evidence: every
    /// visited row carries `field_types` for each field, at the cost of
    /// recording whole-value omissions instead of failing on them. Unlike the
    /// assistance sampling path this never includes source context: only
    /// membership-selected records are visited, so counts stay exact. Added
    /// for union merges, which decode typed values under dtype authority.
    pub fn visit_precise(
        &self,
        cancel: &AtomicBool,
        mut visitor: impl FnMut(FrozenInputBatch) -> Result<(), String>,
    ) -> Result<FrozenInputStats, FrozenInputError> {
        visit_frozen_input(&self.frozen, self.limits, cancel, false, true, &mut visitor)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct SnapshotLimits {
    pub page_records: usize,
    pub page_bytes: usize,
    pub maximum_rows: u64,
    pub maximum_input_bytes: u64,
    pub maximum_disk_bytes: u64,
    pub maximum_parts: usize,
}

impl Default for SnapshotLimits {
    fn default() -> Self {
        Self {
            page_records: 1_024,
            page_bytes: 8 * 1024 * 1024,
            maximum_rows: 10_000_000,
            maximum_input_bytes: 16 * 1024 * 1024 * 1024,
            maximum_disk_bytes: 16 * 1024 * 1024 * 1024,
            maximum_parts: 20_000,
        }
    }
}

impl SnapshotLimits {
    fn valid(self) -> bool {
        self.page_records > 0
            && self.page_bytes > 0
            && self.maximum_rows > 0
            && self.maximum_input_bytes > 0
            && self.maximum_disk_bytes > 0
            && self.maximum_parts > 0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SnapshotState {
    Pending,
    Running,
    Complete,
    Cancelled,
    Limited,
    Failed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SnapshotStatus {
    pub state: SnapshotState,
    pub source_rows_scanned: u64,
    pub filtered_rows_written: u64,
    pub source_parts_written: usize,
    pub filtered_parts_written: usize,
    pub bytes_written: u64,
    pub diagnostic: Option<String>,
    pub manifest_path: Option<PathBuf>,
}

impl SnapshotStatus {
    fn pending() -> Self {
        Self {
            state: SnapshotState::Pending,
            source_rows_scanned: 0,
            filtered_rows_written: 0,
            source_parts_written: 0,
            filtered_parts_written: 0,
            bytes_written: 0,
            diagnostic: None,
            manifest_path: None,
        }
    }
}

pub struct SnapshotJob {
    output_dir: PathBuf,
    status: Arc<Mutex<SnapshotStatus>>,
    cancel: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl SnapshotJob {
    pub fn poll(&self) -> SnapshotStatus {
        self.status
            .lock()
            .expect("snapshot status poisoned")
            .clone()
    }
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Release);
    }
    pub fn output_dir(&self) -> &Path {
        &self.output_dir
    }
    pub fn wait(mut self) -> SnapshotStatus {
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        self.poll()
    }
}

impl Drop for SnapshotJob {
    fn drop(&mut self) {
        self.cancel();
        // Never block the UI owner in Drop. The worker retains its cancellation
        // flag and capacity lease until it exits between bounded pages.
        self.worker.take();
    }
}

struct JobLease(Arc<AtomicUsize>);
impl Drop for JobLease {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

#[derive(Clone)]
struct FrozenSource {
    id: SourceId,
    generation: u64,
    high_watermark: Option<u64>,
    handle: SourceHandle,
}

#[derive(Clone)]
struct FrozenView {
    investigation_id: InvestigationId,
    view_id: String,
    applied_revision: u64,
    applied_generation: u64,
    text: Option<String>,
    advanced_source: Option<String>,
    enrichment_definitions: Vec<(String, String)>,
    capture_time: Option<lvu::CaptureTimeRange>,
    time_basis: lvu::TimeBasis,
    time_field: Option<String>,
    enrichment: Vec<EnrichmentStage>,
    membership: Option<Arc<Membership>>,
    sources: Vec<FrozenSource>,
    /// Published command results the replayed stages may read, and the
    /// columns they name (see `command_columns`).
    command_results: Vec<Arc<CommandColumns>>,
    required_command_columns: BTreeSet<String>,
}

#[derive(Serialize)]
struct SnapshotManifest {
    schema_version: u32,
    investigation_id: String,
    created_at_unix_nanos: u128,
    state: SnapshotState,
    view: ManifestView,
    sources: Vec<ManifestSource>,
    schemas: Vec<SchemaManifest>,
    source_parts: Vec<PartManifest>,
    filtered_parts: Vec<PartManifest>,
    filtered_rows: u64,
    source_rows: u64,
    bytes_written: u64,
    schema_evolution: &'static str,
    inspection_sample: InspectionSample,
}

/// Explicit zero-based Parquet row locations for the agent's initial inspection.
/// This describes requested coverage, not a claim that a remote agent read it.
#[derive(Serialize)]
struct InspectionSample {
    policy: &'static str,
    maximum_rows: usize,
    maximum_rows_per_source: usize,
    requested_rows: usize,
    sources: Vec<SampleSource>,
}

#[derive(Serialize)]
struct SampleSource {
    source_id: String,
    dataset: &'static str,
    available_rows: usize,
    requested_rows: usize,
    parts: Vec<SamplePart>,
}

#[derive(Serialize)]
struct SamplePart {
    path: String,
    row_offsets: Vec<usize>,
}

fn inspection_sample(
    source_parts: &[PartManifest],
    filtered_parts: &[PartManifest],
) -> InspectionSample {
    use std::collections::BTreeSet;
    let ids = source_parts
        .iter()
        .filter(|part| part.rows > 0)
        .map(|part| part.source_id.as_str())
        .collect::<BTreeSet<_>>();
    let quota = (512 / ids.len().max(1)).min(128);
    let mut sources = Vec::new();
    for id in ids {
        let filtered = filtered_parts
            .iter()
            .filter(|part| part.source_id == id && part.rows > 0)
            .collect::<Vec<_>>();
        let (dataset, parts) = if filtered.is_empty() {
            (
                "source_context",
                source_parts
                    .iter()
                    .filter(|part| part.source_id == id && part.rows > 0)
                    .collect::<Vec<_>>(),
            )
        } else {
            ("applied_view", filtered)
        };
        let available_rows = parts.iter().map(|part| part.rows).sum::<usize>();
        let requested_rows = quota.min(available_rows);
        let targets = (0..requested_rows)
            .map(|index| {
                // u128 avoids overflow even on unusually large export settings.
                if requested_rows < 2 {
                    0
                } else {
                    ((index as u128 * (available_rows - 1) as u128) / (requested_rows - 1) as u128)
                        as usize
                }
            })
            .collect::<Vec<_>>();
        let mut base = 0usize;
        let mut locations = Vec::new();
        for part in parts {
            let offsets = targets
                .iter()
                .copied()
                .filter(|target| *target >= base && *target < base + part.rows)
                .map(|target| target - base)
                .collect::<Vec<_>>();
            base += part.rows;
            if !offsets.is_empty() {
                locations.push(SamplePart {
                    path: part.path.clone(),
                    row_offsets: offsets,
                });
            }
        }
        sources.push(SampleSource {
            source_id: id.into(),
            dataset,
            available_rows,
            requested_rows,
            parts: locations,
        });
    }
    InspectionSample {
        policy: "Read the listed zero-based row_offsets from each Parquet part. Evenly spaced across each source's applied view (including first and last); fall back to source context when no rows match. Inspect all part schemas for type variation. Additional reads must be identified separately; requested coverage is not full-data validation.",
        maximum_rows: 512,
        maximum_rows_per_source: 128,
        requested_rows: sources.iter().map(|source| source.requested_rows).sum(),
        sources,
    }
}

#[derive(Serialize)]
struct ManifestView {
    view_id: String,
    applied_revision: u64,
    applied_generation: u64,
    literal_search: Option<String>,
    advanced_polars: Option<String>,
    enrichments: Vec<ManifestEnrichment>,
    capture_time_start_unix_nanos: Option<i64>,
    capture_time_end_unix_nanos: Option<i64>,
    time_basis: &'static str,
    /// Declared field token, when the basis is a chosen field. A snapshot that
    /// did not name its field could not be read back the same way.
    #[serde(skip_serializing_if = "Option::is_none")]
    time_field: Option<String>,
    compatibility_id: Option<String>,
}

#[derive(Serialize)]
struct ManifestEnrichment {
    id: String,
    source: String,
}

#[derive(Serialize)]
struct ManifestSource {
    source_id: String,
    generation: u64,
    high_watermark: Option<u64>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
struct PartManifest {
    path: String,
    source_id: String,
    rows: usize,
    bytes: u64,
    first_sequence: u64,
    last_sequence: u64,
    schema_id: u32,
    enrichment_state: &'static str,
    diagnostics: Vec<String>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
struct FieldManifest {
    name: String,
    dtype: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct SchemaManifest {
    schema_id: u32,
    fields: Vec<FieldManifest>,
}

impl NativeViewAdapter {
    /// Freezes the currently published view for a caller-owned blocking read.
    /// Candidate drafts and subsequently captured records are excluded.
    pub fn freeze_input(
        &self,
        view_id: &str,
        limits: FrozenInputLimits,
    ) -> Result<FrozenInput, ViewError> {
        self.freeze_input_through(view_id, None, limits)
    }

    /// `freeze_input` as the input of chain step `through`: the steps before
    /// it, with the command results published before it (§12.6).
    pub fn freeze_input_through(
        &self,
        view_id: &str,
        through: Option<&str>,
        limits: FrozenInputLimits,
    ) -> Result<FrozenInput, ViewError> {
        if !limits.valid() {
            return Err(ViewError::InvalidConfig);
        }
        self.snapshot_jobs
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                (current < self.config.maximum_snapshot_jobs).then_some(current + 1)
            })
            .map_err(|_| ViewError::SnapshotCapacity)?;
        let lease = JobLease(Arc::clone(&self.snapshot_jobs));
        let frozen = self.freeze_snapshot_through(view_id, through)?;
        let mut accepted_enrichment_outputs = frozen
            .membership
            .as_ref()
            .map_or_else(Vec::new, |membership| membership.enrichment_names.clone());
        accepted_enrichment_outputs.sort();
        accepted_enrichment_outputs.dedup();
        let summary = FrozenInputSummary {
            view_id: frozen.view_id.clone(),
            applied_revision: frozen.applied_revision,
            applied_generation: frozen.applied_generation,
            selected_records: frozen
                .membership
                .as_ref()
                .map(|membership| membership.count),
            sources: frozen
                .sources
                .iter()
                .map(|source| FrozenInputSource {
                    source_id: source.id,
                    generation: source.generation,
                    high_watermark: source.high_watermark,
                })
                .collect(),
            accepted_enrichment_outputs,
        };
        Ok(FrozenInput {
            frozen,
            limits,
            summary,
            _lease: lease,
        })
    }

    /// Freezes the currently published view and starts a bounded background
    /// export. Candidate drafts and subsequently captured records are excluded.
    pub fn start_snapshot(
        &self,
        view_id: &str,
        output_root: impl AsRef<Path>,
        limits: SnapshotLimits,
    ) -> Result<SnapshotJob, ViewError> {
        if !limits.valid() {
            return Err(ViewError::InvalidConfig);
        }
        self.snapshot_jobs
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                (current < self.config.maximum_snapshot_jobs).then_some(current + 1)
            })
            .map_err(|_| ViewError::SnapshotCapacity)?;
        let lease = JobLease(Arc::clone(&self.snapshot_jobs));
        let frozen = match self.freeze_snapshot(view_id) {
            Ok(value) => value,
            Err(error) => {
                drop(lease);
                return Err(error);
            }
        };
        let output_dir = output_root
            .as_ref()
            .join(frozen.investigation_id.0.to_string());
        let status = Arc::new(Mutex::new(SnapshotStatus::pending()));
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_status = Arc::clone(&status);
        let worker_cancel = Arc::clone(&cancel);
        let worker_dir = output_dir.clone();
        let worker = match thread::Builder::new()
            .name("lvu-view-snapshot".into())
            .spawn(move || {
                run_snapshot(
                    frozen,
                    worker_dir,
                    limits,
                    worker_status,
                    worker_cancel,
                    lease,
                )
            }) {
            Ok(worker) => worker,
            Err(error) => {
                return Err(ViewError::Io(error));
            }
        };
        Ok(SnapshotJob {
            output_dir,
            status,
            cancel,
            worker: Some(worker),
        })
    }

    fn freeze_snapshot(&self, view_id: &str) -> Result<FrozenView, ViewError> {
        self.freeze_snapshot_through(view_id, None)
    }

    /// Freezes the view as the input of chain step `through`: only the steps
    /// before it are replayed and only command results published before it
    /// are joined, so a command never reads its own or a later step's output
    /// (docs/command-enrichment.md). `None` freezes the whole applied chain.
    fn freeze_snapshot_through(
        &self,
        view_id: &str,
        through: Option<&str>,
    ) -> Result<FrozenView, ViewError> {
        let shared = self.shared.lock().expect("view state poisoned");
        let view = shared.views.get(view_id).ok_or(ViewError::UnknownView)?;
        let membership = match &view.published {
            Published::Raw => None,
            Published::Filtered { membership } => Some(Arc::clone(membership)),
        };
        let chain = &view.applied_constraints.enrichments;
        let position = match through {
            Some(stage_id) => Some(
                chain
                    .iter()
                    .position(|step| step.id.0 == stage_id)
                    .ok_or(ViewError::UnknownView)?,
            ),
            None => None,
        };
        let prefix: &[lvu::EnrichmentDefinition] = match position {
            Some(position) => &chain[..position],
            None => chain,
        };
        // Stage names come from the definitions that produced them, so the
        // prefix is cut by output name rather than by position in the flat
        // stage list.
        let excluded_outputs: BTreeSet<String> = match position {
            Some(position) => chain[position..]
                .iter()
                .filter(|step| !step.is_command())
                .flat_map(|step| super::enrichment_output_names(&step.source))
                .collect(),
            None => BTreeSet::new(),
        };
        let command_results: Vec<Arc<CommandColumns>> = prefix
            .iter()
            .filter(|step| step.is_command())
            .filter_map(|step| view.command_results.get(&step.id.0).cloned())
            .collect();
        let mut sources = Vec::with_capacity(view.registration.sources.len());
        for id in &view.registration.sources {
            let handle = shared
                .sources
                .get(id)
                .ok_or(ViewError::UnknownSource)?
                .handle
                .clone();
            let (generation, high_watermark) = membership
                .as_ref()
                .and_then(|membership| {
                    membership
                        .sources
                        .iter()
                        .find(|source| source.source_id == id.0.to_string())
                })
                .map(|source| (source.generation, source.high_watermark))
                .unwrap_or_else(|| {
                    let progress = handle.progress();
                    (
                        progress.generation,
                        progress.high_watermark.map(|record| record.sequence),
                    )
                });
            sources.push(FrozenSource {
                id: *id,
                generation,
                high_watermark,
                handle,
            });
        }
        let enrichment: Vec<EnrichmentStage> = membership
            .as_ref()
            .map_or_else(Vec::new, |value| value.enrichment.clone())
            .into_iter()
            .filter(|stage| !excluded_outputs.contains(&stage.name))
            .collect();
        let command_names: Vec<&str> = prefix
            .iter()
            .filter_map(|step| step.output_prefix())
            .collect();
        let required_command_columns: BTreeSet<String> = enrichment
            .iter()
            .flat_map(|stage| stage.definition.dependencies().iter())
            .filter(|dependency| {
                crate::command_columns::command_prefix(dependency, command_names.iter().copied())
                    .is_some()
            })
            .cloned()
            .collect();
        let advanced_source = membership.as_ref().and_then(|value| {
            value
                .advanced
                .as_ref()
                .map(|definition| definition.source.clone())
        });
        Ok(FrozenView {
            investigation_id: InvestigationId::new(),
            view_id: view_id.to_owned(),
            applied_revision: view.applied_revision,
            applied_generation: view.applied_generation,
            text: view
                .applied_constraints
                .text
                .as_ref()
                .map(|value| value.literal.clone()),
            advanced_source,
            enrichment_definitions: prefix
                .iter()
                .map(|definition| (definition.id.0.clone(), definition.source.clone()))
                .collect(),
            capture_time: view.applied_constraints.capture_time,
            time_basis: view.applied_constraints.time_basis,
            time_field: view.applied_constraints.time_field.clone(),
            enrichment,
            membership,
            sources,
            command_results,
            required_command_columns,
        })
    }
}

fn run_snapshot(
    frozen: FrozenView,
    output_dir: PathBuf,
    limits: SnapshotLimits,
    status: Arc<Mutex<SnapshotStatus>>,
    cancel: Arc<AtomicBool>,
    lease: JobLease,
) {
    set_state(&status, SnapshotState::Running, None);
    let result = fs::create_dir_all(output_dir.parent().unwrap_or_else(|| Path::new(".")))
        .and_then(|()| fs::create_dir(&output_dir))
        .map_err(failed)
        .and_then(|()| export_snapshot(&frozen, &output_dir, limits, &status, &cancel));
    let (state, diagnostic, manifest_path, manifest_bytes) = match result {
        Ok(manifest) => {
            if cancel.load(Ordering::Acquire) {
                (
                    SnapshotState::Cancelled,
                    Some("snapshot cancelled".into()),
                    None,
                    0,
                )
            } else {
                let manifest_path = output_dir.join("manifest.json");
                let temporary = output_dir.join("manifest.json.partial");
                let encoded = serde_json::to_vec_pretty(&manifest).map_err(io::Error::other);
                if encoded.as_ref().is_ok_and(|bytes| {
                    manifest.bytes_written.saturating_add(bytes.len() as u64)
                        > limits.maximum_disk_bytes
                }) {
                    (
                        SnapshotState::Limited,
                        Some("snapshot disk limit reached by manifest".into()),
                        None,
                        0,
                    )
                } else {
                    let manifest_bytes = encoded.as_ref().map_or(0, Vec::len) as u64;
                    let publish = encoded
                        .and_then(|bytes| fs::write(&temporary, bytes))
                        .and_then(|()| fs::rename(&temporary, &manifest_path));
                    match publish {
                        Ok(()) => (
                            SnapshotState::Complete,
                            None,
                            Some(manifest_path),
                            manifest_bytes,
                        ),
                        Err(error) => (SnapshotState::Failed, Some(error.to_string()), None, 0),
                    }
                }
            }
        }
        Err(ExportFailure::Cancelled) => (
            SnapshotState::Cancelled,
            Some("snapshot cancelled".into()),
            None,
            0,
        ),
        Err(ExportFailure::Limited(message)) => (SnapshotState::Limited, Some(message), None, 0),
        Err(ExportFailure::Failed(message)) => (SnapshotState::Failed, Some(message), None, 0),
    };
    if state != SnapshotState::Complete {
        let _ = fs::remove_dir_all(output_dir.join("source"));
        let _ = fs::remove_dir_all(output_dir.join("filtered"));
        let _ = fs::remove_file(output_dir.join("manifest.json.partial"));
    }
    drop(lease);
    let mut current = status.lock().expect("snapshot status poisoned");
    current.state = state;
    current.diagnostic = diagnostic.map(|value| bounded(value, 1_024));
    current.manifest_path = manifest_path;
    current.bytes_written = current.bytes_written.saturating_add(manifest_bytes);
}

enum ExportFailure {
    Cancelled,
    Limited(String),
    Failed(String),
}

fn visit_frozen_input(
    frozen: &FrozenView,
    limits: FrozenInputLimits,
    cancel: &AtomicBool,
    source_context_for_empty_matches: bool,
    precise_values: bool,
    visitor: &mut impl FnMut(FrozenInputBatch) -> Result<(), String>,
) -> Result<FrozenInputStats, FrozenInputError> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(replay)?;
    let mut stats = FrozenInputStats::default();
    let mut raw_schema = SchemaContext::default();
    let enrichment_names = frozen
        .membership
        .as_ref()
        .map(|membership| membership.enrichment_names.iter().cloned().collect())
        .unwrap_or_default();
    for source in &frozen.sources {
        let Some(target) = source.high_watermark else {
            continue;
        };
        verify_frozen_generation(source, "before frozen input read")?;
        let boundaries = frozen
            .membership
            .as_ref()
            .map_or_else(Vec::new, |membership| {
                membership
                    .evaluation_batches
                    .iter()
                    .filter(|batch| {
                        batch.source_id == source.id.0.to_string()
                            && batch.generation == source.generation
                            && batch.last_sequence <= target
                    })
                    .collect::<Vec<_>>()
            });
        let mut boundary_cursor = 0usize;
        let mut offset = 0_u64;
        let mut reached = None;
        loop {
            cancelled(cancel)?;
            let boundary = boundaries.get(boundary_cursor).copied();
            let (page_records, page_bytes) =
                boundary.map_or((limits.batch_records, limits.batch_bytes), |batch| {
                    (
                        batch.record_count,
                        frozen
                            .membership
                            .as_ref()
                            .map_or(limits.batch_bytes, |value| value.evaluation_page_bytes),
                    )
                });
            let page = runtime
                .block_on(source.handle.read_page(offset, page_records, page_bytes))
                .map_err(replay)?;
            if page.records.is_empty() {
                break;
            }
            let end = page.end_of_journal;
            let records = page
                .records
                .into_iter()
                .take_while(|record| record.record_id.sequence <= target)
                .collect::<Vec<_>>();
            if records.is_empty() {
                break;
            }
            if let Some(boundary) = boundary
                && (records.first().map(|record| record.record_id.sequence)
                    != Some(boundary.first_sequence)
                    || records.last().map(|record| record.record_id.sequence)
                        != Some(boundary.last_sequence))
            {
                return Err(replay(format!(
                    "source {} no longer matches applied evaluation batch {}..={}",
                    source.id.0, boundary.first_sequence, boundary.last_sequence
                )));
            }
            reached = records.last().map(|record| record.record_id.sequence);
            stats.scanned_records = stats
                .scanned_records
                .checked_add(records.len() as u64)
                .ok_or_else(|| limited_input("scanned record count overflow"))?;
            stats.input_bytes = records
                .iter()
                .try_fold(stats.input_bytes, |sum, record| {
                    sum.checked_add(record.bytes.len() as u64)
                })
                .ok_or_else(|| limited_input("input byte count overflow"))?;
            if stats.scanned_records > limits.maximum_scanned_records {
                return Err(limited_input("scanned record limit reached"));
            }
            if stats.input_bytes > limits.maximum_input_bytes {
                return Err(limited_input("input byte limit reached"));
            }
            let mut batch = if let Some(boundary) = boundary {
                let mut schema = boundary.schema_before.clone();
                records_to_batch_with_context(&records, &mut schema).map_err(replay)?
            } else {
                records_to_batch_with_context(&records, &mut raw_schema).map_err(replay)?
            };
            crate::command_columns::join_command_columns(
                &mut batch.frame,
                &records,
                &frozen.command_results,
                &frozen.required_command_columns,
            )
            .map_err(replay)?;
            let enriched = execute_batch(
                &batch.frame,
                BatchQuery {
                    generation: source.generation,
                    definition_generation: frozen.applied_revision,
                    stages: &frozen.enrichment,
                    filter: None,
                    text_search: None,
                    colors: &[],
                },
            );
            if enriched.validity != BatchValidity::Valid {
                return Err(replay("accepted enrichment produced invalid identity"));
            }
            if let Some(diagnostic) = enriched
                .diagnostics
                .iter()
                .find(|diagnostic| diagnostic.state == DerivedState::Error)
            {
                return Err(replay(format!(
                    "accepted native stage replay failed{}: {}: {}",
                    diagnostic
                        .field
                        .as_deref()
                        .map_or_else(String::new, |field| format!(" for {field:?}")),
                    diagnostic.code,
                    diagnostic.message
                )));
            }
            let include_source_context = source_context_for_empty_matches
                && frozen.membership.as_ref().is_some_and(|membership| {
                    membership
                        .sources
                        .iter()
                        .find(|matches| matches.source_id == source.id.0.to_string())
                        .is_none_or(|matches| matches.sequences.is_empty())
                });
            let selected = records
                .iter()
                .enumerate()
                .filter(|(_, record)| {
                    include_source_context
                        || is_selected(frozen.membership.as_deref(), source.id, record)
                })
                .map(|(index, record)| {
                    input_row(
                        record,
                        &enriched.enriched_rows,
                        index,
                        precise_values,
                        &enrichment_names,
                    )
                })
                .collect::<Result<Vec<_>, _>>()?;
            visit_input_rows(selected, limits, cancel, visitor, &mut stats)?;

            boundary_cursor += usize::from(boundary.is_some());
            if end || reached == Some(target) {
                break;
            }
            offset = page.next_offset;
        }
        if frozen.membership.is_some() && boundary_cursor != boundaries.len() {
            return Err(replay(format!(
                "source {} ended before all applied evaluation batches were replayed",
                source.id.0
            )));
        }
        if reached != Some(target) {
            return Err(replay(format!(
                "source {} ended at {:?} before frozen high-watermark {target}",
                source.id.0, reached
            )));
        }
        verify_frozen_generation(source, "during frozen input read")?;
    }
    cancelled(cancel)?;
    let expected = frozen
        .membership
        .as_ref()
        .map_or(stats.scanned_records, |membership| membership.count);
    if !source_context_for_empty_matches && stats.output_records != expected {
        return Err(replay(format!(
            "frozen membership is incomplete: expected {expected} rows, visited {}",
            stats.output_records
        )));
    }
    Ok(stats)
}

fn verify_frozen_generation(source: &FrozenSource, context: &str) -> Result<(), FrozenInputError> {
    if source.handle.progress().generation != source.generation {
        return Err(replay(format!(
            "source {} generation changed {context}",
            source.id.0
        )));
    }
    Ok(())
}

fn visit_input_rows(
    rows: Vec<FrozenInputRow>,
    limits: FrozenInputLimits,
    cancel: &AtomicBool,
    visitor: &mut impl FnMut(FrozenInputBatch) -> Result<(), String>,
    stats: &mut FrozenInputStats,
) -> Result<(), FrozenInputError> {
    let mut batch = Vec::new();
    let mut batch_bytes = 0_u64;
    for row in rows {
        let bytes = input_row_bytes(&row)?;
        if bytes > limits.batch_bytes as u64 {
            return Err(limited_input(
                "one selected row exceeds the batch byte limit",
            ));
        }
        if !batch.is_empty()
            && (batch.len() == limits.batch_records
                || batch_bytes.saturating_add(bytes) > limits.batch_bytes as u64)
        {
            cancelled(cancel)?;
            visitor(FrozenInputBatch {
                rows: std::mem::take(&mut batch),
            })
            .map_err(|message| FrozenInputError::Visitor(bounded(message, 1_024)))?;
            batch_bytes = 0;
        }
        stats.output_records = stats
            .output_records
            .checked_add(1)
            .ok_or_else(|| limited_input("output record count overflow"))?;
        stats.output_bytes = stats
            .output_bytes
            .checked_add(bytes)
            .ok_or_else(|| limited_input("output byte count overflow"))?;
        if stats.output_records > limits.maximum_output_records {
            return Err(limited_input("output record limit reached"));
        }
        if stats.output_bytes > limits.maximum_output_bytes {
            return Err(limited_input("output byte limit reached"));
        }
        batch_bytes += bytes;
        batch.push(row);
    }
    if !batch.is_empty() {
        cancelled(cancel)?;
        visitor(FrozenInputBatch { rows: batch })
            .map_err(|message| FrozenInputError::Visitor(bounded(message, 1_024)))?;
    }
    Ok(())
}

fn input_row(
    record: &RawRecord,
    frame: &DataFrame,
    index: usize,
    precise_assistance_values: bool,
    enrichment_names: &BTreeSet<String>,
) -> Result<FrozenInputRow, FrozenInputError> {
    let mut fields = BTreeMap::new();
    let mut field_types = BTreeMap::new();
    let mut raw_field_types = BTreeMap::new();
    let mut omitted_fields = BTreeMap::new();
    for column in frame.columns() {
        let name = column.name().as_str();
        if name == "raw" || name.starts_with("_lvu_") {
            continue;
        }
        let raw_observed_type = frame
            .column(&format!("_lvu_type_{name}"))
            .ok()
            .and_then(|provenance| provenance.str().ok())
            .and_then(|provenance| provenance.get(index));
        let derived = enrichment_names.contains(name);
        let observed_type = (!derived).then_some(raw_observed_type).flatten();
        if precise_assistance_values && let Some(observed_type) = observed_type {
            raw_field_types.insert(name.to_owned(), observed_type.to_owned());
        }
        if precise_assistance_values && !derived && observed_type.is_none() {
            field_types.insert(name.to_owned(), format!("{:?}", column.dtype()));
            omitted_fields.insert(name.to_owned(), "field missing from source record".into());
            continue;
        }
        if matches!(observed_type, Some("object" | "array")) {
            if precise_assistance_values {
                field_types.insert(name.to_owned(), format!("{:?}", column.dtype()));
                omitted_fields.insert(
                    name.to_owned(),
                    "structured object/array requires explicit conversion".into(),
                );
                continue;
            }
            return Err(replay(format!(
                "column {name:?} at record {} is structured input represented internally as text; explicit JSON conversion is required",
                record.record_id.sequence
            )));
        }
        let value = column.get(index).map_err(replay)?;
        if precise_assistance_values {
            field_types.insert(name.to_owned(), format!("{:?}", column.dtype()));
        }
        match if precise_assistance_values {
            precise_json_value_for_field(observed_type, value, derived)
        } else {
            json_value(value)
        } {
            Ok(value) => {
                fields.insert(name.to_owned(), value);
            }
            Err(message) if precise_assistance_values => {
                omitted_fields.insert(name.to_owned(), message);
            }
            Err(message) => {
                return Err(replay(format!(
                    "column {name:?} at record {} cannot be represented as JSON: {message}",
                    record.record_id.sequence
                )));
            }
        }
    }
    Ok(FrozenInputRow {
        record: record.clone(),
        fields,
        field_types,
        raw_field_types,
        omitted_fields,
    })
}

fn precise_json_value_for_field(
    observed_type: Option<&str>,
    value: AnyValue<'_>,
    derived: bool,
) -> Result<serde_json::Value, String> {
    if !derived && matches!(observed_type, Some("int64" | "uint64")) {
        match value {
            AnyValue::Null => {
                return Err(
                    "canonical projection is null after a raw integer type conflict".into(),
                );
            }
            AnyValue::Float32(_) | AnyValue::Float64(_) => {
                return Err(
                    "canonical projection coerces a raw integer through a potentially lossy float"
                        .into(),
                );
            }
            _ => {}
        }
    }
    precise_json_value(value)
}

fn precise_json_value(value: AnyValue<'_>) -> Result<serde_json::Value, String> {
    use serde_json::{Number, Value, json};
    const MAX_SAFE_INTEGER: u64 = (1_u64 << 53) - 1;
    Ok(match value {
        AnyValue::Null => Value::Null,
        AnyValue::Boolean(value) => Value::Bool(value),
        AnyValue::String(value) => Value::String(value.to_owned()),
        AnyValue::StringOwned(value) => Value::String(value.to_string()),
        AnyValue::UInt8(value) => Value::Number(Number::from(value)),
        AnyValue::UInt16(value) => Value::Number(Number::from(value)),
        AnyValue::UInt32(value) => Value::Number(Number::from(value)),
        AnyValue::UInt64(value) if value <= MAX_SAFE_INTEGER => Value::Number(Number::from(value)),
        AnyValue::UInt64(value) => json!({"kind":"u64","decimal":value.to_string()}),
        AnyValue::UInt128(value) => json!({"kind":"u128","decimal":value.to_string()}),
        AnyValue::Int8(value) => Value::Number(Number::from(value)),
        AnyValue::Int16(value) => Value::Number(Number::from(value)),
        AnyValue::Int32(value) => Value::Number(Number::from(value)),
        AnyValue::Int64(value) if value.unsigned_abs() <= MAX_SAFE_INTEGER => {
            Value::Number(Number::from(value))
        }
        AnyValue::Int64(value) => json!({"kind":"i64","decimal":value.to_string()}),
        AnyValue::Int128(value) => json!({"kind":"i128","decimal":value.to_string()}),
        AnyValue::Float16(value) => {
            let value = f32::from(value);
            Value::Number(Number::from_f64(f64::from(value)).ok_or("non-finite Float16")?)
        }
        AnyValue::Float32(value) => {
            Value::Number(Number::from_f64(f64::from(value)).ok_or("non-finite Float32")?)
        }
        AnyValue::Float64(value) => {
            Value::Number(Number::from_f64(value).ok_or("non-finite Float64")?)
        }
        AnyValue::Date(days) => json!({"kind":"date","days_since_unix_epoch":days.to_string()}),
        AnyValue::Datetime(value, unit, timezone) => json!({
            "kind":"datetime",
            "integer":value.to_string(),
            "unit":format!("{unit:?}").to_lowercase(),
            "timezone":timezone.map(ToString::to_string),
        }),
        AnyValue::DatetimeOwned(value, unit, timezone) => json!({
            "kind":"datetime",
            "integer":value.to_string(),
            "unit":format!("{unit:?}").to_lowercase(),
            "timezone":timezone.map(|value| value.to_string()),
        }),
        AnyValue::Duration(value, unit) => json!({
            "kind":"duration",
            "integer":value.to_string(),
            "unit":format!("{unit:?}").to_lowercase(),
        }),
        AnyValue::Time(value) => {
            json!({"kind":"time","nanoseconds_since_midnight":value.to_string()})
        }
        AnyValue::List(values) => Value::Array(
            values
                .iter()
                .map(precise_json_value)
                .collect::<Result<Vec<_>, _>>()?,
        ),
        unsupported => return Err(format!("unsupported value type {:?}", unsupported.dtype())),
    })
}

fn json_value(value: AnyValue<'_>) -> Result<serde_json::Value, String> {
    use serde_json::{Number, Value};
    Ok(match value {
        AnyValue::Null => Value::Null,
        AnyValue::Boolean(value) => Value::Bool(value),
        AnyValue::String(value) => Value::String(value.to_owned()),
        AnyValue::StringOwned(value) => Value::String(value.to_string()),
        AnyValue::UInt8(value) => Value::Number(Number::from(value)),
        AnyValue::UInt16(value) => Value::Number(Number::from(value)),
        AnyValue::UInt32(value) => Value::Number(Number::from(value)),
        AnyValue::UInt64(value) => Value::Number(Number::from(value)),
        AnyValue::Int8(value) => Value::Number(Number::from(value)),
        AnyValue::Int16(value) => Value::Number(Number::from(value)),
        AnyValue::Int32(value) => Value::Number(Number::from(value)),
        AnyValue::Int64(value) => Value::Number(Number::from(value)),
        AnyValue::UInt128(value) => Value::Number(Number::from(
            u64::try_from(value).map_err(|_| "UInt128 exceeds JSON's exact integer range")?,
        )),
        AnyValue::Int128(value) => Value::Number(Number::from(
            i64::try_from(value).map_err(|_| "Int128 exceeds JSON's exact integer range")?,
        )),
        AnyValue::Float32(value) => {
            Value::Number(Number::from_f64(f64::from(value)).ok_or("non-finite Float32")?)
        }
        AnyValue::Float64(value) => {
            Value::Number(Number::from_f64(value).ok_or("non-finite Float64")?)
        }
        AnyValue::List(values) => Value::Array(
            values
                .iter()
                .map(json_value)
                .collect::<Result<Vec<_>, _>>()?,
        ),
        unsupported => return Err(format!("unsupported value type {:?}", unsupported.dtype())),
    })
}

fn input_row_bytes(row: &FrozenInputRow) -> Result<u64, FrozenInputError> {
    let fields = serde_json::to_vec(&row.fields).map_err(replay)?;
    let field_types = if row.field_types.is_empty() {
        0
    } else {
        serde_json::to_vec(&row.field_types).map_err(replay)?.len()
    };
    let omitted_fields = if row.omitted_fields.is_empty() {
        0
    } else {
        serde_json::to_vec(&row.omitted_fields)
            .map_err(replay)?
            .len()
    };
    let raw_field_types = if row.raw_field_types.is_empty() {
        0
    } else {
        serde_json::to_vec(&row.raw_field_types)
            .map_err(replay)?
            .len()
    };
    u64::try_from(
        row.record
            .bytes
            .len()
            .saturating_add(row.record.delimiter.len())
            .saturating_add(fields.len())
            .saturating_add(field_types)
            .saturating_add(raw_field_types)
            .saturating_add(omitted_fields),
    )
    .map_err(|_| limited_input("output byte count overflow"))
}

fn cancelled(cancel: &AtomicBool) -> Result<(), FrozenInputError> {
    if cancel.load(Ordering::Acquire) {
        Err(FrozenInputError::Cancelled)
    } else {
        Ok(())
    }
}

fn replay(error: impl std::fmt::Display) -> FrozenInputError {
    FrozenInputError::Replay(bounded(error.to_string(), 1_024))
}

fn limited_input(message: impl Into<String>) -> FrozenInputError {
    FrozenInputError::Limited(message.into())
}

fn export_snapshot(
    frozen: &FrozenView,
    output_dir: &Path,
    limits: SnapshotLimits,
    status: &Arc<Mutex<SnapshotStatus>>,
    cancel: &AtomicBool,
) -> Result<SnapshotManifest, ExportFailure> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(failed)?;
    let source_dir = output_dir.join("source");
    let filtered_dir = output_dir.join("filtered");
    fs::create_dir(&source_dir).map_err(failed)?;
    fs::create_dir(&filtered_dir).map_err(failed)?;
    let mut source_parts = Vec::new();
    let mut filtered_parts = Vec::new();
    let mut schemas = SchemaRegistry::default();
    let write_budget =
        ParquetWriteBudget::new(limits.maximum_disk_bytes).map_err(map_write_error)?;
    let mut part_slots = 0usize;
    let mut raw_schema = SchemaContext::default();
    let mut total_rows = 0_u64;
    let mut input_bytes = 0_u64;
    let mut disk_bytes = 0_u64;
    for source in &frozen.sources {
        let Some(target) = source.high_watermark else {
            continue;
        };
        if source.handle.progress().generation != source.generation {
            return Err(ExportFailure::Failed(format!(
                "source {} generation changed before snapshot read",
                source.id.0
            )));
        }
        let mut offset = 0_u64;
        let mut reached = None;
        let boundaries = frozen
            .membership
            .as_ref()
            .map_or_else(Vec::new, |membership| {
                membership
                    .evaluation_batches
                    .iter()
                    .filter(|batch| {
                        batch.source_id == source.id.0.to_string()
                            && batch.generation == source.generation
                            && batch.last_sequence <= target
                    })
                    .collect::<Vec<_>>()
            });
        let mut boundary_cursor = 0usize;
        let mut source_packer =
            OutputPacker::new(&source_dir, "source", source, write_budget.clone());
        let mut filtered_packer =
            OutputPacker::new(&filtered_dir, "filtered", source, write_budget.clone());
        loop {
            if cancel.load(Ordering::Acquire) {
                return Err(ExportFailure::Cancelled);
            }
            let boundary = boundaries.get(boundary_cursor).copied();
            let (evaluation_records, evaluation_bytes) =
                boundary.map_or((limits.page_records, limits.page_bytes), |batch| {
                    (
                        batch.record_count,
                        frozen
                            .membership
                            .as_ref()
                            .map_or(limits.page_bytes, |membership| {
                                membership.evaluation_page_bytes
                            }),
                    )
                });
            let page = runtime
                .block_on(
                    source
                        .handle
                        .read_page(offset, evaluation_records, evaluation_bytes),
                )
                .map_err(failed)?;
            if page.records.is_empty() {
                break;
            }
            let end = page.end_of_journal;
            let records = page
                .records
                .into_iter()
                .take_while(|record| record.record_id.sequence <= target)
                .collect::<Vec<_>>();
            if records.is_empty() {
                break;
            }
            if let Some(boundary) = boundary
                && (records.first().map(|record| record.record_id.sequence)
                    != Some(boundary.first_sequence)
                    || records.last().map(|record| record.record_id.sequence)
                        != Some(boundary.last_sequence))
            {
                return Err(ExportFailure::Failed(format!(
                    "source {} no longer matches applied evaluation batch {}..={}",
                    source.id.0, boundary.first_sequence, boundary.last_sequence
                )));
            }
            reached = records.last().map(|record| record.record_id.sequence);
            total_rows = total_rows
                .checked_add(records.len() as u64)
                .ok_or_else(|| limited("row count overflow"))?;
            input_bytes = records
                .iter()
                .try_fold(input_bytes, |sum, record| {
                    sum.checked_add(record.bytes.len() as u64)
                })
                .ok_or_else(|| limited("input byte count overflow"))?;
            if total_rows > limits.maximum_rows {
                return Err(limited("snapshot row limit reached"));
            }
            if input_bytes > limits.maximum_input_bytes {
                return Err(limited("snapshot input byte limit reached"));
            }
            let mut batch = if let Some(boundary) = boundary {
                let mut schema = boundary.schema_before.clone();
                records_to_batch_with_context(&records, &mut schema).map_err(failed)?
            } else {
                records_to_batch_with_context(&records, &mut raw_schema).map_err(failed)?
            };
            let event_times = records
                .iter()
                .map(
                    |record| match lvu_live::recognize_event_time(&record.bytes) {
                        lvu_live::EventTimeRecognition::Valid { unix_nanos, .. } => {
                            Some(unix_nanos)
                        }
                        lvu_live::EventTimeRecognition::Invalid { .. }
                        | lvu_live::EventTimeRecognition::Missing => None,
                    },
                )
                .collect::<Vec<_>>();
            batch
                .frame
                .with_column(
                    Series::new("_lvu_event_time_unix_nanos".into(), event_times).into_column(),
                )
                .map_err(|error| failed(format!("event-time projection failed: {error}")))?;
            crate::command_columns::join_command_columns(
                &mut batch.frame,
                &records,
                &frozen.command_results,
                &frozen.required_command_columns,
            )
            .map_err(|error| failed(format!("command column projection failed: {error}")))?;
            let stages = frozen.enrichment.as_slice();
            let mut enriched = execute_batch(
                &batch.frame,
                BatchQuery {
                    generation: source.generation,
                    definition_generation: frozen.applied_revision,
                    stages,
                    filter: None,
                    text_search: None,
                    colors: &[],
                },
            );
            if enriched.validity != BatchValidity::Valid {
                return Err(ExportFailure::Failed(
                    "native export produced invalid identity".into(),
                ));
            }
            // Record the resolved time basis separately from raw event-time recognition.
            // Evaluate from the replayed accepted enrichment, never the current UI draft.
            let extracted_failed = enriched.diagnostics.iter().any(|diagnostic| {
                diagnostic.field.as_deref() == Some(crate::time_basis::EXTRACTED_COLUMN)
                    && diagnostic.state == DerivedState::Error
            });
            // Read through the engine's own basis expression, the same one the
            // live query uses, so an export and the view it came from cannot
            // disagree about what a `timestamp_utc` string means.
            let extracted = enriched
                .enriched_rows
                .column(crate::time_basis::EXTRACTED_COLUMN)
                .ok()
                .and_then(|column| column.str().ok())
                .map(|column| {
                    let values: Vec<Option<&str>> = column.iter().collect();
                    crate::time_basis::read_extracted(&values)
                });
            // A declared field basis reads the frozen batch once, the same way
            // the live query does, so an export and the view it came from agree.
            let declared = frozen.time_field.as_deref().map(|token| {
                let column = match lvu_live::time::TimeFieldSelection::parse_token(token) {
                    Ok(selection) => match selection.field {
                        lvu_live::time::TimeFieldRef::Column(name) => Some(name),
                        _ => None,
                    },
                    Err(_) => None,
                };
                let values = column.as_deref().and_then(|name| {
                    enriched
                        .enriched_rows
                        .column(name)
                        .ok()
                        .and_then(|column| column.str().ok())
                });
                let mut index = 0usize;
                crate::time_basis::read_records(token, &records, |_| {
                    let value = values.and_then(|values| values.get(index));
                    index += 1;
                    value
                })
            });
            let selected_times = records
                .iter()
                .enumerate()
                .map(|(index, record)| match frozen.time_basis {
                    lvu::TimeBasis::Capture => Some(record.captured_at_unix_nanos),
                    lvu::TimeBasis::Event => match lvu_live::recognize_event_time(&record.bytes) {
                        lvu_live::EventTimeRecognition::Valid { unix_nanos, .. } => {
                            Some(unix_nanos)
                        }
                        _ => None,
                    },
                    lvu::TimeBasis::Extracted if !extracted_failed => extracted
                        .as_ref()
                        .and_then(|read| read.as_ref().ok())
                        .and_then(|times| times.get(index).copied())
                        .flatten(),
                    lvu::TimeBasis::Extracted => None,
                    lvu::TimeBasis::Selected => declared
                        .as_ref()
                        .and_then(|declared| declared.by_sequence.get(&record.record_id.sequence))
                        .copied(),
                })
                .collect::<Vec<_>>();
            let selected_times =
                Series::new("_lvu_selected_time_unix_nanos".into(), selected_times).into_column();
            batch
                .frame
                .with_column(selected_times.clone())
                .map_err(failed)?;
            enriched
                .enriched_rows
                .with_column(selected_times)
                .map_err(failed)?;
            let enrichment_state = if frozen.enrichment.is_empty() {
                "not_configured"
            } else if enriched
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.state == DerivedState::Error)
            {
                "error"
            } else {
                "ready"
            };
            let diagnostics = enriched
                .diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.state == DerivedState::Error)
                .take(32)
                .map(|diagnostic| {
                    bounded(format!("{}: {}", diagnostic.code, diagnostic.message), 512)
                })
                .collect::<Vec<_>>();

            let source_schema = schemas.id_for(&batch.frame)?;
            source_packer.push(
                &records,
                batch.frame,
                source_schema,
                "not_configured",
                &[],
                limits,
                cancel,
                &mut part_slots,
                &mut source_parts,
            )?;
            let selected_mask = records
                .iter()
                .map(|record| is_selected(frozen.membership.as_deref(), source.id, record))
                .collect::<Vec<_>>();
            if selected_mask.iter().any(|selected| *selected) {
                let selected = records
                    .iter()
                    .zip(&selected_mask)
                    .filter(|(_, selected)| **selected)
                    .map(|(record, _)| record.clone())
                    .collect::<Vec<_>>();
                let frame = enriched
                    .enriched_rows
                    .filter(&BooleanChunked::from_slice(
                        "selected".into(),
                        &selected_mask,
                    ))
                    .map_err(failed)?;
                let filtered_schema = schemas.id_for(&frame)?;
                filtered_packer.push(
                    &selected,
                    frame,
                    filtered_schema,
                    enrichment_state,
                    &diagnostics,
                    limits,
                    cancel,
                    &mut part_slots,
                    &mut filtered_parts,
                )?;
            }
            disk_bytes = write_budget.bytes_written();
            {
                let mut current = status.lock().expect("snapshot status poisoned");
                current.source_rows_scanned = total_rows;
                current.filtered_rows_written =
                    filtered_parts.iter().map(|part| part.rows as u64).sum();
                current.source_parts_written = source_parts.len();
                current.filtered_parts_written = filtered_parts.len();
                current.bytes_written = disk_bytes;
            }
            boundary_cursor += usize::from(boundary.is_some());
            if end
                || records
                    .last()
                    .is_some_and(|record| record.record_id.sequence == target)
            {
                break;
            }
            offset = page.next_offset;
        }
        source_packer.finish(&mut source_parts)?;
        filtered_packer.finish(&mut filtered_parts)?;
        disk_bytes = write_budget.bytes_written();
        {
            let mut current = status.lock().expect("snapshot status poisoned");
            current.filtered_rows_written =
                filtered_parts.iter().map(|part| part.rows as u64).sum();
            current.source_parts_written = source_parts.len();
            current.filtered_parts_written = filtered_parts.len();
            current.bytes_written = disk_bytes;
        }
        if frozen.membership.is_some() && boundary_cursor != boundaries.len() {
            return Err(ExportFailure::Failed(format!(
                "source {} ended before all applied evaluation batches were replayed",
                source.id.0
            )));
        }
        if reached != Some(target) {
            return Err(ExportFailure::Failed(format!(
                "source {} ended at {:?} before frozen high-watermark {}",
                source.id.0, reached, target
            )));
        }
        if source.handle.progress().generation != source.generation {
            return Err(ExportFailure::Failed(format!(
                "source {} generation changed during snapshot read",
                source.id.0
            )));
        }
    }
    let filtered_rows = filtered_parts.iter().map(|part| part.rows as u64).sum();
    let expected_filtered = frozen
        .membership
        .as_ref()
        .map_or(total_rows, |membership| membership.count);
    if filtered_rows != expected_filtered {
        return Err(ExportFailure::Failed(format!(
            "filtered membership is incomplete: expected {expected_filtered} rows, exported {filtered_rows}"
        )));
    }
    let inspection_sample = inspection_sample(&source_parts, &filtered_parts);
    Ok(SnapshotManifest {
        schema_version: 2,
        investigation_id: frozen.investigation_id.0.to_string(),
        created_at_unix_nanos: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
        state: SnapshotState::Complete,
        view: ManifestView {
            view_id: frozen.view_id.clone(),
            applied_revision: frozen.applied_revision,
            applied_generation: frozen.applied_generation,
            literal_search: frozen.text.clone(),
            advanced_polars: frozen.advanced_source.clone(),
            enrichments: frozen
                .enrichment_definitions
                .iter()
                .map(|(id, source)| ManifestEnrichment {
                    id: id.clone(),
                    source: source.clone(),
                })
                .collect(),
            capture_time_start_unix_nanos: frozen
                .capture_time
                .map(|window| window.start_unix_nanos),
            capture_time_end_unix_nanos: frozen.capture_time.map(|window| window.end_unix_nanos),
            time_basis: match frozen.time_basis {
                lvu::TimeBasis::Capture => "capture",
                lvu::TimeBasis::Event => "event",
                lvu::TimeBasis::Extracted => "extracted_timestamp_utc",
                lvu::TimeBasis::Selected => "selected_field",
            },
            time_field: frozen.time_field.clone(),
            compatibility_id: frozen
                .enrichment
                .first()
                .map(|stage| stage.definition.compatibility_id().to_owned())
                .or_else(|| {
                    frozen.membership.as_ref().and_then(|membership| {
                        membership
                            .advanced
                            .as_ref()
                            .map(|definition| definition.compatibility_id().to_owned())
                    })
                }),
        },
        sources: frozen
            .sources
            .iter()
            .map(|source| ManifestSource {
                source_id: source.id.0.to_string(),
                generation: source.generation,
                high_watermark: source.high_watermark,
            })
            .collect(),
        schemas: schemas.schemas,
        source_parts,
        filtered_parts,
        filtered_rows,
        source_rows: total_rows,
        bytes_written: disk_bytes,
        inspection_sample,
        schema_evolution: "Manifest schema v2 stores each physical schema once in schemas; every part references schema_id. Packers flush before schema changes, so incompatible schemas remain distinct. Tolerant projection preserves typed homogeneous fields; missing values are null, conflicts retain _lvu_type_* provenance, and nested values remain JSON strings pending an evolving nested-schema contract.",
    })
}

const PACKED_PART_ROWS: usize = 16 * 1024;
const PACKED_ROW_GROUP_ROWS: usize = 1024;
const PACKED_PART_ROW_GROUPS: usize = 64;
const PACKED_PART_BYTES: u64 = 256 * 1024 * 1024;
const PACKED_FLUSH_BYTES: u64 = 192 * 1024 * 1024;
const MAX_SCHEMA_DICTIONARY_ENTRIES: usize = 1024;
const MAX_SCHEMA_DICTIONARY_BYTES: usize = 1024 * 1024;

#[derive(Default)]
struct SchemaRegistry {
    ids: BTreeMap<Vec<FieldManifest>, u32>,
    schemas: Vec<SchemaManifest>,
    encoded_bytes: usize,
}

impl SchemaRegistry {
    fn id_for(&mut self, frame: &DataFrame) -> Result<u32, ExportFailure> {
        let fields = frame
            .columns()
            .iter()
            .map(|column| FieldManifest {
                name: column.name().to_string(),
                dtype: column.dtype().to_string(),
            })
            .collect::<Vec<_>>();
        self.id_for_fields(fields)
    }

    fn id_for_fields(&mut self, fields: Vec<FieldManifest>) -> Result<u32, ExportFailure> {
        if let Some(id) = self.ids.get(&fields) {
            return Ok(*id);
        }
        if self.schemas.len() >= MAX_SCHEMA_DICTIONARY_ENTRIES {
            return Err(limited("snapshot schema dictionary count limit reached"));
        }
        let id = u32::try_from(self.schemas.len())
            .map_err(|_| limited("snapshot schema dictionary limit reached"))?;
        let schema = SchemaManifest {
            schema_id: id,
            fields: fields.clone(),
        };
        let encoded = serde_json::to_vec(&schema).map_err(failed)?.len();
        let next_bytes = self
            .encoded_bytes
            .checked_add(encoded)
            .ok_or_else(|| limited("snapshot schema dictionary byte count overflow"))?;
        if next_bytes > MAX_SCHEMA_DICTIONARY_BYTES {
            return Err(limited("snapshot schema dictionary byte limit reached"));
        }
        self.ids.insert(fields.clone(), id);
        self.schemas.push(schema);
        self.encoded_bytes = next_bytes;
        Ok(id)
    }
}

struct ActivePart {
    writer: AtomicParquetPartWriter,
    path: String,
    source_id: String,
    schema_id: u32,
    rows: usize,
    row_groups: usize,
    first_sequence: u64,
    last_sequence: u64,
    enrichment_state: &'static str,
    diagnostics: Vec<String>,
}

struct OutputPacker<'a> {
    directory: &'a Path,
    prefix: &'static str,
    source: &'a FrozenSource,
    write_budget: ParquetWriteBudget,
    active: Option<ActivePart>,
}

impl<'a> OutputPacker<'a> {
    fn new(
        directory: &'a Path,
        prefix: &'static str,
        source: &'a FrozenSource,
        write_budget: ParquetWriteBudget,
    ) -> Self {
        Self {
            directory,
            prefix,
            source,
            write_budget,
            active: None,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn push(
        &mut self,
        records: &[RawRecord],
        frame: DataFrame,
        schema_id: u32,
        enrichment_state: &'static str,
        diagnostics: &[String],
        limits: SnapshotLimits,
        cancel: &AtomicBool,
        slots: &mut usize,
        completed: &mut Vec<PartManifest>,
    ) -> Result<(), ExportFailure> {
        if frame.height() != records.len() {
            return Err(ExportFailure::Failed(
                "packed frame row count does not match record identities".into(),
            ));
        }
        let mut start = 0usize;
        while start < records.len() {
            if cancel.load(Ordering::Acquire) {
                return Err(ExportFailure::Cancelled);
            }
            let compatible = self.active.as_ref().is_some_and(|part| {
                part.schema_id == schema_id
                    && part.enrichment_state == enrichment_state
                    && part.diagnostics == diagnostics
                    && part.rows < PACKED_PART_ROWS
                    && part.row_groups < PACKED_PART_ROW_GROUPS
            });
            if !compatible && self.active.is_some() {
                let part = self.finish_active()?;
                completed.push(part);
            }
            if self.active.is_none() {
                if *slots >= limits.maximum_parts {
                    return Err(limited("snapshot part limit reached"));
                }
                let filename =
                    format!("{}-{}-{:06}.parquet", self.prefix, self.source.id.0, *slots);
                let destination = self.directory.join(&filename);
                let writer = AtomicParquetPartWriter::create(
                    &destination,
                    frame.schema().as_ref(),
                    PACKED_PART_BYTES,
                    self.write_budget.clone(),
                )
                .map_err(map_write_error)?;
                self.active = Some(ActivePart {
                    writer,
                    path: format!("{}/{filename}", self.prefix),
                    source_id: self.source.id.0.to_string(),
                    schema_id,
                    rows: 0,
                    row_groups: 0,
                    first_sequence: records[start].record_id.sequence,
                    last_sequence: records[start].record_id.sequence,
                    enrichment_state,
                    diagnostics: diagnostics.to_vec(),
                });
                *slots += 1;
            }
            let active = self.active.as_mut().expect("part created");
            let maximum_rows = limits
                .page_records
                .min(PACKED_ROW_GROUP_ROWS)
                .min(PACKED_PART_ROWS.saturating_sub(active.rows));
            let count = output_group_len(
                records.len().saturating_sub(start),
                maximum_rows,
                limits.page_bytes,
                |offset| records[start + offset].bytes.len(),
            )?;
            let mut group = frame.slice(start as i64, count);
            active
                .writer
                .write_row_group(&mut group)
                .map_err(map_write_error)?;
            active.rows += count;
            active.row_groups += 1;
            active.last_sequence = records[start + count - 1].record_id.sequence;
            start += count;
            let active_bytes = active.writer.bytes_written().map_err(failed)?;
            let flush = active.rows >= PACKED_PART_ROWS
                || active.row_groups >= PACKED_PART_ROW_GROUPS
                || active_bytes >= PACKED_FLUSH_BYTES;
            if flush {
                let part = self.finish_active()?;
                completed.push(part);
            }
        }
        Ok(())
    }

    fn finish_active(&mut self) -> Result<PartManifest, ExportFailure> {
        let part = self.active.take().expect("active part");
        let bytes = part.writer.finish().map_err(map_write_error)?;
        Ok(PartManifest {
            path: part.path,
            source_id: part.source_id,
            rows: part.rows,
            bytes,
            first_sequence: part.first_sequence,
            last_sequence: part.last_sequence,
            schema_id: part.schema_id,
            enrichment_state: part.enrichment_state,
            diagnostics: part.diagnostics,
        })
    }

    fn finish(mut self, completed: &mut Vec<PartManifest>) -> Result<(), ExportFailure> {
        if self.active.is_some() {
            completed.push(self.finish_active()?);
        }
        Ok(())
    }
}

fn output_group_len(
    remaining_records: usize,
    maximum_rows: usize,
    maximum_bytes: usize,
    record_bytes: impl Fn(usize) -> usize,
) -> Result<usize, ExportFailure> {
    let mut count = 0usize;
    let mut bytes = 0usize;
    while count < maximum_rows && count < remaining_records {
        let next_bytes = record_bytes(count);
        if next_bytes > maximum_bytes {
            return Err(limited("record exceeds snapshot output page byte limit"));
        }
        if count > 0 && bytes.saturating_add(next_bytes) > maximum_bytes {
            break;
        }
        bytes += next_bytes;
        count += 1;
    }
    if count == 0 {
        Err(limited("snapshot output page limits admit no records"))
    } else {
        Ok(count)
    }
}

fn map_write_error(error: io::Error) -> ExportFailure {
    if error.kind() == io::ErrorKind::FileTooLarge
        || error
            .to_string()
            .contains("Parquet part byte limit reached")
    {
        limited("snapshot disk or packed-part byte limit reached")
    } else {
        failed(error)
    }
}

fn is_selected(membership: Option<&Membership>, source_id: SourceId, record: &RawRecord) -> bool {
    let Some(membership) = membership else {
        return true;
    };
    let Some(source) = membership
        .sources
        .iter()
        .find(|source| source.source_id == source_id.0.to_string())
    else {
        return false;
    };
    source
        .sequences
        .binary_search(&record.record_id.sequence)
        .is_ok()
}

fn set_state(
    status: &Arc<Mutex<SnapshotStatus>>,
    state: SnapshotState,
    diagnostic: Option<String>,
) {
    let mut current = status.lock().expect("snapshot status poisoned");
    current.state = state;
    current.diagnostic = diagnostic.map(|value| bounded(value, 1_024));
}
fn failed(error: impl std::fmt::Display) -> ExportFailure {
    ExportFailure::Failed(bounded(error.to_string(), 1_024))
}
fn limited(message: impl Into<String>) -> ExportFailure {
    ExportFailure::Limited(message.into())
}
fn bounded(mut value: String, maximum: usize) -> String {
    if value.len() > maximum {
        let mut end = maximum;
        while !value.is_char_boundary(end) {
            end -= 1;
        }
        value.truncate(end);
    }
    value
}

#[cfg(test)]
mod sampling_tests {
    use super::*;
    fn part(source: &str, name: &str, rows: usize) -> PartManifest {
        PartManifest {
            path: name.into(),
            source_id: source.into(),
            rows,
            bytes: 0,
            first_sequence: 0,
            last_sequence: rows.saturating_sub(1) as u64,
            schema_id: 0,
            enrichment_state: "not_configured",
            diagnostics: vec![],
        }
    }
    #[test]
    fn sample_locations_cover_parts_endpoints_and_each_source_without_exceeding_caps() {
        let mut raw = Vec::new();
        for source in 0..32 {
            raw.push(part(
                &source.to_string(),
                &format!("{source}-a.parquet"),
                100,
            ));
            raw.push(part(
                &source.to_string(),
                &format!("{source}-b.parquet"),
                100,
            ));
        }
        let sample = inspection_sample(&raw, &[]);
        assert_eq!(sample.requested_rows, 512);
        assert_eq!(sample.sources.len(), 32);
        for source in &sample.sources {
            assert_eq!(source.requested_rows, 16);
            assert_eq!(source.parts[0].row_offsets[0], 0);
            assert_eq!(
                *source.parts.last().unwrap().row_offsets.last().unwrap(),
                99
            );
            let mut actual = source.parts[0].row_offsets.clone();
            actual.extend(source.parts[1].row_offsets.iter().map(|row| row + 100));
            assert_eq!(actual, (0..16).map(|i| i * 199 / 15).collect::<Vec<_>>());
        }
    }
    #[test]
    fn sample_uses_applied_typed_outputs_and_explicit_fallback_for_empty_views() {
        let raw = vec![
            part("a", "raw-a", 400),
            part("b", "raw-b", 1),
            part("empty", "empty", 0),
        ];
        let filtered = vec![part("a", "filtered-a", 3)];
        let sample = inspection_sample(&raw, &filtered);
        assert_eq!(sample.requested_rows, 4);
        assert_eq!(sample.sources[0].dataset, "applied_view");
        assert_eq!(sample.sources[0].parts[0].path, "filtered-a");
        assert_eq!(sample.sources[0].parts[0].row_offsets, [0, 1, 2]);
        assert_eq!(sample.sources[1].dataset, "source_context");
        assert_eq!(sample.sources[1].parts[0].row_offsets, [0]);
        assert_eq!(inspection_sample(&raw[..1], &[]).requested_rows, 128);
        assert_eq!(inspection_sample(&[], &[]).requested_rows, 0);
    }

    #[test]
    fn schema_dictionary_refuses_excessive_evolution_before_accumulating_it() {
        let mut registry = SchemaRegistry::default();
        for index in 0..MAX_SCHEMA_DICTIONARY_ENTRIES {
            registry
                .id_for_fields(vec![FieldManifest {
                    name: format!("field-{index}"),
                    dtype: "String".into(),
                }])
                .unwrap_or_else(|_| panic!("bounded schema should be admitted"));
        }
        let prior_bytes = registry.encoded_bytes;
        assert!(matches!(
            registry.id_for_fields(vec![FieldManifest {
                name: "one-too-many".into(),
                dtype: "String".into(),
            }]),
            Err(ExportFailure::Limited(_))
        ));
        assert_eq!(registry.schemas.len(), MAX_SCHEMA_DICTIONARY_ENTRIES);
        assert_eq!(registry.encoded_bytes, prior_bytes);
    }

    #[test]
    fn schema_dictionary_refuses_wide_encoded_schema_before_accumulating_it() {
        let mut registry = SchemaRegistry::default();
        let fields = (0..256)
            .map(|index| FieldManifest {
                name: format!("field-{index}-{}", "x".repeat(4_096)),
                dtype: "String".into(),
            })
            .collect();
        assert!(matches!(
            registry.id_for_fields(fields),
            Err(ExportFailure::Limited(_))
        ));
        assert!(registry.schemas.is_empty());
        assert!(registry.ids.is_empty());
        assert_eq!(registry.encoded_bytes, 0);
    }

    #[test]
    fn output_groups_obey_configured_record_and_byte_bounds() {
        let lengths = [3, 4, 5, 6];
        assert_eq!(
            output_group_len(lengths.len(), 2, 100, |index| lengths[index])
                .unwrap_or_else(|_| panic!("record-bounded group should be admitted")),
            2
        );
        assert_eq!(
            output_group_len(lengths.len(), 100, 8, |index| lengths[index])
                .unwrap_or_else(|_| panic!("byte-bounded group should be admitted")),
            2
        );
    }

    #[test]
    fn output_groups_refuse_a_single_oversized_record() {
        assert!(matches!(
            output_group_len(1, 1, 8, |_| 9),
            Err(ExportFailure::Limited(_))
        ));
    }

    #[test]
    fn assistance_scalar_codec_preserves_unsafe_integers_temporal_units_and_nonfinite_errors() {
        use polars::prelude::TimeUnit;
        assert_eq!(
            precise_json_value(AnyValue::UInt64(u64::MAX)).unwrap(),
            serde_json::json!({"kind":"u64","decimal":u64::MAX.to_string()})
        );
        assert_eq!(
            precise_json_value(AnyValue::Int64(i64::MIN)).unwrap(),
            serde_json::json!({"kind":"i64","decimal":i64::MIN.to_string()})
        );
        assert_eq!(
            precise_json_value(AnyValue::Datetime(
                1_234_567_890_123_456_789,
                TimeUnit::Nanoseconds,
                None,
            ))
            .unwrap(),
            serde_json::json!({
                "kind":"datetime",
                "integer":"1234567890123456789",
                "unit":"nanoseconds",
                "timezone":null,
            })
        );
        assert!(precise_json_value(AnyValue::Float64(f64::NAN)).is_err());
    }

    #[test]
    fn assistance_codec_distinguishes_derived_shadow_missing_and_explicit_null() {
        use lvu_core::{ChunkPosition, RecordId, StreamKind};
        let source_id = SourceId::new();
        let record = |sequence, bytes: &[u8]| RawRecord {
            record_id: RecordId {
                source_id,
                sequence,
            },
            captured_at_unix_nanos: 0,
            stream: StreamKind::File,
            bytes: bytes.to_vec().into(),
            delimiter: b"\n".to_vec().into(),
            acquisition_id: SourceId::new().0,
            chunk: ChunkPosition::End,
        };
        let shadow = DataFrame::new(
            1,
            vec![
                Series::new("code".into(), [9_007_199_254_740_993_i64]).into(),
                Series::new("_lvu_type_code".into(), [Some("int64")]).into(),
            ],
        )
        .unwrap();
        let shadowed = input_row(
            &record(0, br#"{"code":1}"#),
            &shadow,
            0,
            true,
            &BTreeSet::from(["code".to_owned()]),
        )
        .unwrap();
        assert_eq!(
            shadowed.fields["code"],
            serde_json::json!({"kind":"i64","decimal":"9007199254740993"})
        );

        let nulls = DataFrame::new(
            2,
            vec![
                Series::new("value".into(), [None::<i64>, None]).into(),
                Series::new("_lvu_type_value".into(), [None, Some("null")]).into(),
            ],
        )
        .unwrap();
        let missing = input_row(&record(1, br#"{}"#), &nulls, 0, true, &BTreeSet::new()).unwrap();
        assert!(!missing.fields.contains_key("value"));
        assert_eq!(
            missing.omitted_fields["value"],
            "field missing from source record"
        );
        let explicit_null = input_row(
            &record(2, br#"{"value":null}"#),
            &nulls,
            1,
            true,
            &BTreeSet::new(),
        )
        .unwrap();
        assert!(explicit_null.fields["value"].is_null());
    }
}
