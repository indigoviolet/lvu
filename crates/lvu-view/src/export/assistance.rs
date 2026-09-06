use super::{
    FrozenInputBatch, FrozenInputError, FrozenInputLimits, FrozenInputRow, FrozenInputStats,
    FrozenView, JobLease, visit_frozen_input,
};
use crate::{NativeViewAdapter, ViewError};
use lvu_core::SourceId;
use serde::Serialize;
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{SystemTime, UNIX_EPOCH},
};

pub const DEFAULT_MAXIMUM_INLINE_CONTEXT_BYTES: usize = 32 * 1024;
const MAXIMUM_OBSERVED_SCHEMA_VARIANTS: usize = 4_096;
const MAXIMUM_OBSERVED_SCHEMA_BYTES: usize = 512 * 1024;

#[derive(Clone, Copy, Debug)]
pub struct AssistancePreparationLimits {
    pub batch_records: usize,
    pub batch_bytes: usize,
    pub maximum_scanned_records: u64,
    pub maximum_input_bytes: u64,
    pub maximum_samples_per_source: usize,
    pub maximum_samples: usize,
    pub maximum_inline_context_bytes: usize,
    pub maximum_raw_fallback_bytes: usize,
}

impl Default for AssistancePreparationLimits {
    fn default() -> Self {
        Self {
            batch_records: 1_024,
            batch_bytes: 8 * 1024 * 1024,
            maximum_scanned_records: 50_000,
            maximum_input_bytes: 512 * 1024 * 1024,
            maximum_samples_per_source: 128,
            maximum_samples: 512,
            maximum_inline_context_bytes: DEFAULT_MAXIMUM_INLINE_CONTEXT_BYTES,
            maximum_raw_fallback_bytes: 16 * 1024,
        }
    }
}

impl AssistancePreparationLimits {
    fn valid(self) -> bool {
        self.batch_records > 0
            && self.batch_bytes > 0
            && self.maximum_scanned_records > 0
            && self.maximum_input_bytes > 0
            && self.maximum_samples_per_source > 0
            && self.maximum_samples > 0
            && self.maximum_inline_context_bytes > 0
            && self.maximum_raw_fallback_bytes > 0
    }

    fn replay(self) -> FrozenInputLimits {
        FrozenInputLimits {
            batch_records: self.batch_records,
            batch_bytes: self.batch_bytes,
            maximum_scanned_records: self.maximum_scanned_records,
            maximum_input_bytes: self.maximum_input_bytes,
            maximum_output_records: self.maximum_scanned_records,
            maximum_output_bytes: self.maximum_input_bytes,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AssistancePreparationState {
    Pending,
    Running,
    Complete,
    Cancelled,
    Limited,
    Failed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AssistancePreparationResult {
    pub inline_context: String,
    pub context: Value,
    pub context_path: PathBuf,
    pub serialized_bytes: usize,
    pub view_id: String,
    pub applied_revision: u64,
    pub applied_generation: u64,
    pub sources: Vec<AssistancePreparedSource>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct AssistancePreparedSource {
    pub source_id: SourceId,
    pub generation: u64,
    pub high_watermark: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AssistancePreparationStatus {
    pub state: AssistancePreparationState,
    pub scanned_records: u64,
    pub materialized_samples: usize,
    pub serialized_bytes: usize,
    pub diagnostic: Option<String>,
    pub result: Option<AssistancePreparationResult>,
}

impl AssistancePreparationStatus {
    fn pending() -> Self {
        Self {
            state: AssistancePreparationState::Pending,
            scanned_records: 0,
            materialized_samples: 0,
            serialized_bytes: 0,
            diagnostic: None,
            result: None,
        }
    }
}

pub struct AssistancePreparationJob {
    output_dir: PathBuf,
    status: Arc<Mutex<AssistancePreparationStatus>>,
    cancel: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl AssistancePreparationJob {
    pub fn poll(&self) -> AssistancePreparationStatus {
        self.status
            .lock()
            .expect("assistance preparation status poisoned")
            .clone()
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Release);
    }

    pub fn output_dir(&self) -> &Path {
        &self.output_dir
    }

    /// Reap a finished worker without blocking the caller's event loop.
    pub fn try_wait(&mut self) -> Option<AssistancePreparationStatus> {
        if self
            .worker
            .as_ref()
            .is_some_and(|worker| !worker.is_finished())
        {
            return None;
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        Some(self.poll())
    }

    pub fn wait(mut self) -> AssistancePreparationStatus {
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        self.poll()
    }
}

impl Drop for AssistancePreparationJob {
    fn drop(&mut self) {
        self.cancel();
        self.worker.take();
    }
}

impl NativeViewAdapter {
    pub fn start_assistance_preparation(
        &self,
        view_id: &str,
        output_root: impl AsRef<Path>,
        limits: AssistancePreparationLimits,
    ) -> Result<AssistancePreparationJob, ViewError> {
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
            Ok(frozen) => frozen,
            Err(error) => {
                drop(lease);
                return Err(error);
            }
        };
        let output_root = if output_root.as_ref().is_absolute() {
            output_root.as_ref().to_owned()
        } else {
            std::env::current_dir()
                .map_err(ViewError::Io)?
                .join(output_root.as_ref())
        };
        let output_dir = output_root.join(frozen.investigation_id.0.to_string());
        let status = Arc::new(Mutex::new(AssistancePreparationStatus::pending()));
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_status = Arc::clone(&status);
        let worker_cancel = Arc::clone(&cancel);
        let worker_dir = output_dir.clone();
        let worker = thread::Builder::new()
            .name("lvu-assistance-prepare".into())
            .spawn(move || {
                run_preparation(
                    frozen,
                    worker_dir,
                    limits,
                    worker_status,
                    worker_cancel,
                    lease,
                )
            })
            .map_err(ViewError::Io)?;
        Ok(AssistancePreparationJob {
            output_dir,
            status,
            cancel,
            worker: Some(worker),
        })
    }
}

#[derive(Serialize)]
struct PreparedContext {
    schema_version: u32,
    view: PreparedView,
    sources: Vec<PreparedCoverage>,
    native_definitions: NativeDefinitions,
    schemas: Vec<PreparedSchema>,
    samples: Vec<PreparedSample>,
    omissions: PreparedOmissions,
}

#[derive(Serialize)]
struct PreparedView {
    id: String,
    applied_revision: String,
    applied_generation: String,
    created_at_unix_nanos: String,
    scanned_records: u64,
    input_bytes: u64,
    inline_byte_limit: usize,
    raw_fallback_byte_limit_per_row: usize,
    sample_policy: &'static str,
}

#[derive(Serialize)]
struct PreparedCoverage {
    source_id: String,
    generation: String,
    high_watermark: Option<String>,
    dataset: &'static str,
    available_rows: u64,
    candidate_rows: usize,
    materialized_rows: usize,
    omitted_rows: u64,
    omitted_values: usize,
    omitted_raw: usize,
}

#[derive(Clone, Serialize)]
struct NativeDefinitions {
    text: Option<String>,
    advanced: Option<String>,
    enrichments: Vec<NativeEnrichment>,
}

#[derive(Clone, Serialize)]
struct NativeEnrichment {
    id: String,
    source: String,
}

#[derive(Clone, Serialize)]
struct PreparedSchema {
    id: usize,
    field: String,
    dtype: String,
    provenance: &'static str,
    raw_observed_types: Vec<String>,
    nullable_observed: bool,
    missing_observed: bool,
    projection_conflict_observed: bool,
    projection_loss_observed: bool,
    type_conflict_observed: bool,
}

#[derive(Clone, Serialize)]
struct PreparedSample {
    source_id: String,
    sequence: String,
    captured_at: PreparedTimestamp,
    values: BTreeMap<String, PreparedValue>,
    omitted_values: BTreeMap<String, String>,
    raw: Option<PreparedRaw>,
}

#[derive(Clone, Serialize)]
struct PreparedTimestamp {
    kind: &'static str,
    integer: String,
    unit: &'static str,
}

#[derive(Clone, Serialize)]
struct PreparedValue {
    schema: usize,
    value: Value,
}

#[derive(Clone, Serialize)]
#[serde(tag = "encoding", content = "value", rename_all = "snake_case")]
enum PreparedRaw {
    Utf8(String),
    Hex(String),
}

#[derive(Clone, Default, Serialize)]
struct PreparedOmissions {
    rows_for_inline_byte_limit: usize,
    values_for_inline_byte_limit: usize,
    raw_for_structured_rows: usize,
    raw_over_per_row_limit: usize,
    native_definitions_for_inline_byte_limit: usize,
    reason: Option<&'static str>,
}

struct SourcePlan {
    source_id: String,
    generation: u64,
    high_watermark: Option<u64>,
    dataset: &'static str,
    available_rows: u64,
    seen_rows: u64,
    targets: Vec<u64>,
    next_target: usize,
    candidates: Vec<FrozenInputRow>,
}

type SchemaKey = (String, String, &'static str);

#[derive(Clone, Default)]
struct SchemaObservation {
    raw_observed_types: BTreeSet<String>,
    nullable_observed: bool,
    missing_observed: bool,
    projection_conflict_observed: bool,
    projection_loss_observed: bool,
}

type SchemaCatalog = BTreeMap<SchemaKey, SchemaObservation>;

struct ContextBasis<'a> {
    frozen: &'a FrozenView,
    plans: &'a [SourcePlan],
    scanned_records: u64,
    input_bytes: u64,
    schema_catalog: &'a SchemaCatalog,
    limits: AssistancePreparationLimits,
}

fn run_preparation(
    frozen: FrozenView,
    output_dir: PathBuf,
    limits: AssistancePreparationLimits,
    status: Arc<Mutex<AssistancePreparationStatus>>,
    cancel: Arc<AtomicBool>,
    _lease: JobLease,
) {
    status.lock().expect("status poisoned").state = AssistancePreparationState::Running;
    let result = prepare_context(&frozen, &output_dir, limits, &cancel);
    let mut current = status.lock().expect("status poisoned");
    match result {
        Ok((prepared, scanned, samples, bytes)) => {
            current.state = AssistancePreparationState::Complete;
            current.scanned_records = scanned;
            current.materialized_samples = samples;
            current.serialized_bytes = bytes;
            current.result = Some(prepared);
        }
        Err(PreparationFailure::Cancelled) => {
            current.state = AssistancePreparationState::Cancelled;
            current.diagnostic = Some("assistance preparation cancelled".into());
        }
        Err(PreparationFailure::Limited(message, scanned_records)) => {
            current.state = AssistancePreparationState::Limited;
            current.scanned_records = scanned_records;
            current.diagnostic = Some(message);
        }
        Err(PreparationFailure::Failed(message)) => {
            current.state = AssistancePreparationState::Failed;
            current.diagnostic = Some(message);
        }
    }
}

enum PreparationFailure {
    Cancelled,
    Limited(String, u64),
    Failed(String),
}

fn prepare_context(
    frozen: &FrozenView,
    output_dir: &Path,
    limits: AssistancePreparationLimits,
    cancel: &AtomicBool,
) -> Result<(AssistancePreparationResult, u64, usize, usize), PreparationFailure> {
    let quota = sample_quota(frozen.sources.len(), limits);
    if quota == 0 {
        return Err(PreparationFailure::Limited(
            "sample cap cannot represent every frozen source".into(),
            0,
        ));
    }
    let mut plans = frozen
        .sources
        .iter()
        .map(|source| {
            let matched = frozen.membership.as_ref().and_then(|membership| {
                membership
                    .sources
                    .iter()
                    .find(|matches| matches.source_id == source.id.0.to_string())
            });
            let sequences = matched
                .map(|matches| matches.sequences.as_ref())
                .unwrap_or(&[]);
            let source_context = frozen.membership.is_some() && sequences.is_empty();
            SourcePlan {
                source_id: source.id.0.to_string(),
                generation: source.generation,
                high_watermark: source.high_watermark,
                dataset: if source_context {
                    "source_context_fallback"
                } else {
                    "applied_view"
                },
                available_rows: 0,
                seen_rows: 0,
                targets: Vec::new(),
                next_target: 0,
                candidates: Vec::new(),
            }
        })
        .collect::<Vec<_>>();
    let plan_index = plans
        .iter()
        .enumerate()
        .map(|(index, plan)| (plan.source_id.clone(), index))
        .collect::<HashMap<_, _>>();
    let enrichment_names = frozen
        .membership
        .as_ref()
        .map(|membership| membership.enrichment_names.iter().cloned().collect())
        .unwrap_or_default();
    let mut schema_catalog = SchemaCatalog::new();
    let mut schema_bytes = 0usize;
    let count_stats = visit_frozen_input(
        frozen,
        limits.replay(),
        cancel,
        true,
        &mut |batch: FrozenInputBatch| {
            for row in batch.rows {
                observe_schema(
                    &mut schema_catalog,
                    &mut schema_bytes,
                    &row,
                    &enrichment_names,
                )?;
                let Some(index) = plan_index.get(&row.record.record_id.source_id.0.to_string())
                else {
                    return Err("replay returned an unknown source".into());
                };
                plans[*index].seen_rows = plans[*index].seen_rows.saturating_add(1);
            }
            Ok(())
        },
    )
    .map_err(map_replay_failure)?;
    if cancel.load(Ordering::Acquire) {
        return Err(PreparationFailure::Cancelled);
    }
    for plan in &mut plans {
        plan.available_rows = plan.seen_rows;
        plan.targets = evenly_spaced_ordinal_targets(plan.available_rows, quota);
        plan.seen_rows = 0;
    }
    let replay_stats = if plans.iter().any(|plan| !plan.targets.is_empty()) {
        visit_frozen_input(
            frozen,
            remaining_replay_limits(limits, count_stats)?,
            cancel,
            true,
            &mut |batch: FrozenInputBatch| {
                for row in batch.rows {
                    let Some(index) = plan_index.get(&row.record.record_id.source_id.0.to_string())
                    else {
                        return Err("replay returned an unknown source".into());
                    };
                    select_ordinal_row(&mut plans[*index], row);
                }
                Ok(())
            },
        )
        .map_err(map_replay_failure)?
    } else {
        FrozenInputStats::default()
    };
    if plans
        .iter()
        .any(|plan| plan.next_target != plan.targets.len())
    {
        return Err(PreparationFailure::Failed(
            "frozen assistance replay did not reach every selected ordinal".into(),
        ));
    }
    let scanned_records = count_stats
        .scanned_records
        .saturating_add(replay_stats.scanned_records);
    let input_bytes = count_stats
        .input_bytes
        .saturating_add(replay_stats.input_bytes);
    let (mut samples, mut omissions) =
        build_samples(&plans, &enrichment_names, &schema_catalog, limits);
    let native_definitions = NativeDefinitions {
        text: frozen.text.clone(),
        advanced: frozen.advanced_source.clone(),
        enrichments: frozen
            .enrichment_definitions
            .iter()
            .map(|(id, source)| NativeEnrichment {
                id: id.clone(),
                source: source.clone(),
            })
            .collect(),
    };
    let basis = ContextBasis {
        frozen,
        plans: &plans,
        scanned_records,
        input_bytes,
        schema_catalog: &schema_catalog,
        limits,
    };
    let mut context = build_context(
        &basis,
        native_definitions,
        samples.clone(),
        omissions.clone(),
    );
    let mut serialized = serde_json::to_vec(&context).map_err(failed)?;
    while serialized.len() > limits.maximum_inline_context_bytes
        && remove_fairest_row(&mut samples, true)
    {
        omissions.rows_for_inline_byte_limit += 1;
        omissions.reason = Some("whole rows and values omitted to satisfy total inline byte limit");
        context = build_context(
            &basis,
            context.native_definitions.clone(),
            samples.clone(),
            omissions.clone(),
        );
        serialized = serde_json::to_vec(&context).map_err(failed)?;
    }
    while serialized.len() > limits.maximum_inline_context_bytes
        && samples.iter().any(|sample| !sample.values.is_empty())
    {
        remove_largest_value(&mut samples);
        omissions.values_for_inline_byte_limit += 1;
        omissions.reason = Some("whole rows and values omitted to satisfy total inline byte limit");
        context = build_context(
            &basis,
            context.native_definitions.clone(),
            samples.clone(),
            omissions.clone(),
        );
        serialized = serde_json::to_vec(&context).map_err(failed)?;
    }
    while serialized.len() > limits.maximum_inline_context_bytes
        && remove_fairest_row(&mut samples, false)
    {
        omissions.rows_for_inline_byte_limit += 1;
        omissions.reason = Some("whole rows and values omitted to satisfy total inline byte limit");
        context = build_context(
            &basis,
            context.native_definitions.clone(),
            samples.clone(),
            omissions.clone(),
        );
        serialized = serde_json::to_vec(&context).map_err(failed)?;
    }
    while serialized.len() > limits.maximum_inline_context_bytes
        && !context.native_definitions.enrichments.is_empty()
    {
        context.native_definitions.enrichments.pop();
        omissions.native_definitions_for_inline_byte_limit += 1;
        omissions.reason = Some(
            "whole rows, values, schemas, or definitions omitted to satisfy total inline byte limit",
        );
        context = build_context(
            &basis,
            context.native_definitions.clone(),
            samples.clone(),
            omissions.clone(),
        );
        serialized = serde_json::to_vec(&context).map_err(failed)?;
    }
    if serialized.len() > limits.maximum_inline_context_bytes
        && context.native_definitions.advanced.take().is_some()
    {
        omissions.native_definitions_for_inline_byte_limit += 1;
        omissions.reason = Some(
            "whole rows, values, schemas, or definitions omitted to satisfy total inline byte limit",
        );
        context.omissions = omissions.clone();
        serialized = serde_json::to_vec(&context).map_err(failed)?;
    }
    if serialized.len() > limits.maximum_inline_context_bytes
        && context.native_definitions.text.take().is_some()
    {
        omissions.native_definitions_for_inline_byte_limit += 1;
        omissions.reason = Some(
            "whole rows, values, schemas, or definitions omitted to satisfy total inline byte limit",
        );
        context.omissions = omissions;
        serialized = serde_json::to_vec(&context).map_err(failed)?;
    }
    if serialized.len() > limits.maximum_inline_context_bytes {
        return Err(PreparationFailure::Limited(
            format!(
                "fixed assistance context metadata requires {} bytes, exceeding the {} byte inline limit",
                serialized.len(),
                limits.maximum_inline_context_bytes
            ),
            scanned_records,
        ));
    }
    if cancel.load(Ordering::Acquire) {
        return Err(PreparationFailure::Cancelled);
    }
    let temp_dir = output_dir.with_extension("preparing");
    if temp_dir.exists() || output_dir.exists() {
        return Err(PreparationFailure::Failed(
            "assistance output path already exists".into(),
        ));
    }
    fs::create_dir_all(&temp_dir).map_err(failed)?;
    let temp_path = temp_dir.join("context.json");
    if let Err(error) = fs::write(&temp_path, &serialized) {
        let _ = fs::remove_dir_all(&temp_dir);
        return Err(failed(error));
    }
    if cancel.load(Ordering::Acquire) {
        let _ = fs::remove_dir_all(&temp_dir);
        return Err(PreparationFailure::Cancelled);
    }
    fs::rename(&temp_dir, output_dir).map_err(|error| {
        let _ = fs::remove_dir_all(&temp_dir);
        failed(error)
    })?;
    let context_path = fs::canonicalize(output_dir.join("context.json")).map_err(failed)?;
    let inline_context = String::from_utf8(serialized).map_err(failed)?;
    let parsed = serde_json::from_str(&inline_context).map_err(failed)?;
    let sources = frozen
        .sources
        .iter()
        .map(|source| AssistancePreparedSource {
            source_id: source.id,
            generation: source.generation,
            high_watermark: source.high_watermark,
        })
        .collect();
    let materialized = samples.len();
    let bytes = inline_context.len();
    Ok((
        AssistancePreparationResult {
            inline_context,
            context: parsed,
            context_path,
            serialized_bytes: bytes,
            view_id: frozen.view_id.clone(),
            applied_revision: frozen.applied_revision,
            applied_generation: frozen.applied_generation,
            sources,
        },
        scanned_records,
        materialized,
        bytes,
    ))
}

fn sample_quota(source_count: usize, limits: AssistancePreparationLimits) -> usize {
    limits
        .maximum_samples_per_source
        .min(limits.maximum_samples / source_count.max(1))
}

fn remaining_replay_limits(
    limits: AssistancePreparationLimits,
    used: FrozenInputStats,
) -> Result<FrozenInputLimits, PreparationFailure> {
    let remaining = FrozenInputLimits {
        batch_records: limits.batch_records,
        batch_bytes: limits.batch_bytes,
        maximum_scanned_records: limits
            .maximum_scanned_records
            .saturating_sub(used.scanned_records),
        maximum_input_bytes: limits.maximum_input_bytes.saturating_sub(used.input_bytes),
        maximum_output_records: limits
            .maximum_scanned_records
            .saturating_sub(used.output_records),
        maximum_output_bytes: limits.maximum_input_bytes.saturating_sub(used.output_bytes),
    };
    if remaining.maximum_scanned_records < used.scanned_records
        || remaining.maximum_input_bytes < used.input_bytes
        || remaining.maximum_output_records < used.output_records
        || remaining.maximum_output_bytes < used.output_bytes
    {
        return Err(PreparationFailure::Limited(
            "assistance count pass exhausted the cumulative replay budget before ordinal sampling"
                .into(),
            used.scanned_records,
        ));
    }
    Ok(remaining)
}

fn evenly_spaced_ordinal_targets(rows: u64, quota: usize) -> Vec<u64> {
    let count = quota.min(usize::try_from(rows).unwrap_or(usize::MAX));
    (0..count)
        .map(|index| {
            if count < 2 {
                0
            } else {
                (index as u128 * u128::from(rows - 1) / (count - 1) as u128) as u64
            }
        })
        .collect()
}

fn select_ordinal_row(plan: &mut SourcePlan, row: FrozenInputRow) {
    let ordinal = plan.seen_rows;
    plan.seen_rows = plan.seen_rows.saturating_add(1);
    if plan.targets.get(plan.next_target) == Some(&ordinal) {
        plan.candidates.push(row);
        plan.next_target += 1;
    }
}

fn observe_schema(
    catalog: &mut SchemaCatalog,
    catalog_bytes: &mut usize,
    row: &FrozenInputRow,
    enrichment_names: &BTreeSet<String>,
) -> Result<(), String> {
    for (field, dtype) in &row.field_types {
        let provenance = if enrichment_names.contains(field) {
            "native_enrichment"
        } else {
            "structured_input"
        };
        let key = (field.clone(), dtype.clone(), provenance);
        if !catalog.contains_key(&key) {
            *catalog_bytes = catalog_bytes
                .checked_add(field.len() + dtype.len() + provenance.len())
                .ok_or("observed schema byte count overflow")?;
            if catalog.len() >= MAXIMUM_OBSERVED_SCHEMA_VARIANTS
                || *catalog_bytes > MAXIMUM_OBSERVED_SCHEMA_BYTES
            {
                return Err("observed schema exceeds bounded assistance catalog".into());
            }
        }
        let observation = catalog.entry(key).or_default();
        if let Some(raw_type) = row.raw_field_types.get(field)
            && observation.raw_observed_types.insert(raw_type.clone())
        {
            *catalog_bytes = catalog_bytes
                .checked_add(raw_type.len())
                .ok_or("observed schema byte count overflow")?;
            if *catalog_bytes > MAXIMUM_OBSERVED_SCHEMA_BYTES {
                return Err("observed schema exceeds bounded assistance catalog".into());
            }
        }
        observation.nullable_observed |= row.fields.get(field).is_some_and(Value::is_null);
        observation.missing_observed |= row
            .omitted_fields
            .get(field)
            .is_some_and(|reason| reason == "field missing from source record");
        observation.projection_conflict_observed |= row
            .omitted_fields
            .get(field)
            .is_some_and(|reason| reason.contains("type conflict"));
        observation.projection_loss_observed |= row
            .omitted_fields
            .get(field)
            .is_some_and(|reason| reason.contains("lossy float"));
    }
    Ok(())
}

fn build_samples(
    plans: &[SourcePlan],
    enrichment_names: &BTreeSet<String>,
    schema_catalog: &SchemaCatalog,
    limits: AssistancePreparationLimits,
) -> (Vec<PreparedSample>, PreparedOmissions) {
    let mut samples = Vec::new();
    let mut omissions = PreparedOmissions::default();
    let schema_ids = schema_catalog
        .keys()
        .cloned()
        .enumerate()
        .map(|(id, key)| (key, id))
        .collect::<BTreeMap<_, _>>();
    let rounds = plans
        .iter()
        .map(|plan| plan.candidates.len())
        .max()
        .unwrap_or(0);
    for position in 0..rounds {
        for plan in plans {
            let Some(row) = plan.candidates.get(position) else {
                continue;
            };
            let mut values = BTreeMap::new();
            for (field, value) in &row.fields {
                let dtype = row
                    .field_types
                    .get(field)
                    .cloned()
                    .unwrap_or_else(|| "Unknown".into());
                let provenance = if enrichment_names.contains(field) {
                    "native_enrichment"
                } else {
                    "structured_input"
                };
                let schema = schema_ids[&(field.clone(), dtype, provenance)];
                values.insert(
                    field.clone(),
                    PreparedValue {
                        schema,
                        value: value.clone(),
                    },
                );
            }
            let projection_omitted = row
                .omitted_fields
                .values()
                .any(|reason| reason.starts_with("canonical projection"));
            let raw = if values.is_empty() || projection_omitted {
                if row.record.bytes.len() <= limits.maximum_raw_fallback_bytes {
                    Some(match std::str::from_utf8(&row.record.bytes) {
                        Ok(text) => PreparedRaw::Utf8(text.to_owned()),
                        Err(_) => PreparedRaw::Hex(hex(&row.record.bytes)),
                    })
                } else {
                    omissions.raw_over_per_row_limit += 1;
                    None
                }
            } else {
                omissions.raw_for_structured_rows += 1;
                None
            };
            samples.push(PreparedSample {
                source_id: plan.source_id.clone(),
                sequence: row.record.record_id.sequence.to_string(),
                captured_at: PreparedTimestamp {
                    kind: "timestamp",
                    integer: row.record.captured_at_unix_nanos.to_string(),
                    unit: "nanoseconds_since_unix_epoch",
                },
                values,
                omitted_values: row.omitted_fields.clone(),
                raw,
            });
        }
    }
    (samples, omissions)
}

fn build_context(
    basis: &ContextBasis<'_>,
    native_definitions: NativeDefinitions,
    samples: Vec<PreparedSample>,
    omissions: PreparedOmissions,
) -> PreparedContext {
    let mut schemas = Vec::new();
    let conflicts = basis.schema_catalog.keys().fold(
        BTreeMap::<String, BTreeSet<String>>::new(),
        |mut map, (field, dtype, _)| {
            map.entry(field.clone()).or_default().insert(dtype.clone());
            map
        },
    );
    let raw_types = basis.schema_catalog.iter().fold(
        BTreeMap::<String, BTreeSet<String>>::new(),
        |mut map, ((field, _, _), observation)| {
            map.entry(field.clone())
                .or_default()
                .extend(observation.raw_observed_types.iter().cloned());
            map
        },
    );
    for (id, ((field, dtype, provenance), observation)) in basis.schema_catalog.iter().enumerate() {
        schemas.push(PreparedSchema {
            id,
            type_conflict_observed: conflicts.get(field).is_some_and(|types| types.len() > 1)
                || raw_types.get(field).is_some_and(|types| types.len() > 1)
                || observation.projection_conflict_observed,
            field: field.clone(),
            dtype: dtype.clone(),
            provenance,
            raw_observed_types: observation.raw_observed_types.iter().cloned().collect(),
            nullable_observed: observation.nullable_observed,
            missing_observed: observation.missing_observed,
            projection_conflict_observed: observation.projection_conflict_observed,
            projection_loss_observed: observation.projection_loss_observed,
        });
    }
    let sources = basis
        .plans
        .iter()
        .map(|plan| {
            let materialized_rows = samples
                .iter()
                .filter(|sample| sample.source_id == plan.source_id)
                .count();
            let omitted_values = samples
                .iter()
                .filter(|sample| sample.source_id == plan.source_id)
                .map(|sample| sample.omitted_values.len())
                .sum();
            PreparedCoverage {
                source_id: plan.source_id.clone(),
                generation: plan.generation.to_string(),
                high_watermark: plan.high_watermark.map(|value| value.to_string()),
                dataset: plan.dataset,
                available_rows: plan.available_rows,
                candidate_rows: plan.candidates.len(),
                materialized_rows,
                omitted_rows: plan.available_rows.saturating_sub(materialized_rows as u64),
                omitted_values,
                omitted_raw: samples
                    .iter()
                    .filter(|sample| sample.source_id == plan.source_id && sample.raw.is_none())
                    .count(),
            }
        })
        .collect();
    PreparedContext {
        schema_version: 1,
        view: PreparedView {
            id: basis.frozen.view_id.clone(),
            applied_revision: basis.frozen.applied_revision.to_string(),
            applied_generation: basis.frozen.applied_generation.to_string(),
            created_at_unix_nanos: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
                .to_string(),
            scanned_records: basis.scanned_records,
            input_bytes: basis.input_bytes,
            inline_byte_limit: basis.limits.maximum_inline_context_bytes,
            raw_fallback_byte_limit_per_row: basis.limits.maximum_raw_fallback_bytes,
            sample_policy: "At most 128 rows/source and 512 total, allocated fairly and spread from first through last frozen rows; actual coverage and whole-value omissions are explicit.",
        },
        sources,
        native_definitions,
        schemas,
        samples,
        omissions,
    }
}

fn remove_fairest_row(samples: &mut Vec<PreparedSample>, preserve_endpoints: bool) -> bool {
    let counts = samples
        .iter()
        .fold(HashMap::<String, usize>::new(), |mut counts, sample| {
            *counts.entry(sample.source_id.clone()).or_default() += 1;
            counts
        });
    let source = counts
        .into_iter()
        .filter(|(_, count)| !preserve_endpoints || *count > 2)
        .max_by(|left, right| left.1.cmp(&right.1).then_with(|| right.0.cmp(&left.0)))
        .map(|(source, _)| source);
    let Some(source) = source else { return false };
    let positions = samples
        .iter()
        .enumerate()
        .filter(|(_, sample)| sample.source_id == source)
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    let index = positions[positions.len() / 2];
    samples.remove(index);
    true
}

fn remove_largest_value(samples: &mut [PreparedSample]) {
    let candidate = samples
        .iter()
        .enumerate()
        .flat_map(|(sample_index, sample)| {
            sample.values.iter().map(move |(field, value)| {
                let bytes = serde_json::to_vec(&(field, value)).map_or(0, |bytes| bytes.len());
                (bytes, sample_index, field.clone())
            })
        })
        .max_by(|left, right| left.cmp(right));
    if let Some((_, sample, field)) = candidate {
        samples[sample].values.remove(&field);
        samples[sample]
            .omitted_values
            .insert(field, "omitted to satisfy total inline byte limit".into());
    }
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        encoded.push(DIGITS[(byte >> 4) as usize] as char);
        encoded.push(DIGITS[(byte & 0xf) as usize] as char);
    }
    encoded
}

fn map_replay_failure(error: FrozenInputError) -> PreparationFailure {
    match error {
        FrozenInputError::Cancelled => PreparationFailure::Cancelled,
        FrozenInputError::Limited(message) => PreparationFailure::Limited(message, 0),
        FrozenInputError::Replay(message) | FrozenInputError::Visitor(message) => {
            PreparationFailure::Failed(message)
        }
    }
}

fn failed(error: impl std::fmt::Display) -> PreparationFailure {
    PreparationFailure::Failed(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancelled_job_is_retained_until_worker_settlement() {
        let (release, blocked) = std::sync::mpsc::sync_channel::<()>(1);
        let cancel = Arc::new(AtomicBool::new(false));
        let status = Arc::new(Mutex::new(AssistancePreparationStatus::pending()));
        let worker_status = Arc::clone(&status);
        let mut job = AssistancePreparationJob {
            output_dir: PathBuf::new(),
            status,
            cancel: Arc::clone(&cancel),
            worker: Some(thread::spawn(move || {
                blocked.recv().unwrap();
                worker_status.lock().unwrap().state = AssistancePreparationState::Cancelled;
            })),
        };
        job.cancel();
        assert!(cancel.load(Ordering::Acquire));
        assert!(job.try_wait().is_none(), "cancel does not mean worker exit");
        assert!(job.worker.is_some());
        release.send(()).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            if let Some(status) = job.try_wait() {
                assert_eq!(status.state, AssistancePreparationState::Cancelled);
                assert!(job.worker.is_none());
                break;
            }
            assert!(std::time::Instant::now() < deadline);
            thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    #[test]
    fn many_sources_receive_a_fair_global_quota() {
        let limits = AssistancePreparationLimits::default();
        assert_eq!(sample_quota(1, limits), 128);
        assert_eq!(sample_quota(4, limits), 128);
        assert_eq!(sample_quota(32, limits), 16);
        assert_eq!(sample_quota(512, limits), 1);
        assert_eq!(sample_quota(513, limits), 0);
    }
}
