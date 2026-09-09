//! Background union worker: frozen replay per input, engine merge, publication.
//!
//! Ownership: Muse union-views assignment (new file). This is the executable
//! half of the union feature: registration storage, candidate submit parking,
//! the freeze/visit/merge/publish background job and the composition-tick
//! driver. `lib.rs` carries only the module lines plus re-exports, the
//! `Shared.union_views` field, one `drive_unions` call in `drain_updates`,
//! and a guard refusing ordinary queries for union views; every union method
//! below is an `impl NativeViewAdapter` block owned here. Not touched: grouping/folding
//! sections (W22), `with_enrichment` (roles agent), `worker_loop`,
//! `QueryPurpose` (W22 contact), terminal tick and `main.rs` (source owner).
//!
//! Protocol (bounded at every step):
//!
//! * Submit parks a [`UnionCandidateSpec`](super::union::UnionCandidateSpec)
//!   and spawns AT MOST one worker thread per union view; a newer submit
//!   cancels the older thread through its cancel token (correlation
//!   precedent: detached threads settle alone, never joined).
//! * The worker freezes inputs ONE at a time through the driver: it sends
//!   [`DriverCmd::Freeze`], the tick-thread driver calls the existing public
//!   `freeze_input` (cheap: lock plus clones, no journal I/O) and hands back
//!   the `FrozenInput`. At most one snapshot lease is ever held per union
//!   job, so the shared `maximum_snapshot_jobs` budget is never starved.
//! * Heavy journal replay (`FrozenInput::visit`) runs on the worker thread
//!   with the union cancel token: cancellation is between inputs and between
//!   visited batches, and every visit honors bounded `FrozenInputLimits`
//!   derived from the remaining [`UnionLimits`](super::union::UnionLimits).
//! * After all inputs decode, the worker merges through
//!   [`union_frozen_inputs`](super::union::union_frozen_inputs), then
//!   publishes under ONE `shared` lock: input revisions/generations are
//!   re-verified against the fence atomically with publication, so an input
//!   that advanced mid-job aborts as stale instead of publishing a mixed
//!   revision. Any failure preserves the prior published union.
//! * Publication is an ordinary `Membership`: per-source surviving sequences
//!   (ascending, for raw resolution through the existing provider),
//!   `NO_BASIS_TIME` where the basis had no value, and `merge_keys` set to
//!   each record's DISPLAY RANK. Ranks are strictly increasing per source, so
//!   the existing k-way merge reproduces the union order exactly; `times`
//!   still carries the honest basis values for time bounds and gap search.
//!   Union-level search/advanced filters over the merged stream need worker
//!   execution over union frames (primary-owned `run_query` region) and are
//!   an explicit follow-up: inputs arrive already filtered, and folding,
//!   grouping, export, correlation and time bounds read `Membership`
//!   generically from day one.

use super::export::{FrozenInput, FrozenInputError, FrozenInputLimits};
use super::union::{
    StoredUnionInput, UnionCandidateSpec, UnionCompletion, UnionFrozenInput, UnionFrozenRow,
    UnionLimits, detect_union_cycle, union_frozen_inputs, validate_union_spec,
};
use super::{
    Appended, Membership, MemoryBudget, NO_BASIS_TIME, Published, Reservation, SEQUENCE_BYTES,
    SOURCE_OVERHEAD, ScanState, Shared, SourceMatches, SourceTimeBounds, ViewError,
    ViewQueryStatus, ViewRegistration, ViewState, merge_order,
};
use lvu_core::SourceId;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
    mpsc::{Receiver, SyncSender, TrySendError, sync_channel},
};
use std::thread::{self, JoinHandle};
use std::time::Duration;

/// How many freeze requests the tick driver serves per `drain_updates` call.
///
/// Freezing is cheap (lock plus clones), but the tick has a frame budget and
/// union work must never crowd out query completions.
const MAX_FREEZE_PER_TICK: usize = 2;

/// How long the worker waits for a frozen delivery before re-checking cancel
/// and shutdown. Bounded like every other provider wait: a closed view or a
/// stopped adapter releases the thread within this horizon.
const FREEZE_WAIT: Duration = Duration::from_millis(100);

/// Worker thread to tick-thread driver: freeze one input for the running job.
pub(crate) struct DriverCmd {
    union_view_id: String,
    view_id: String,
    limits: FrozenInputLimits,
}

/// Tick-thread driver to worker: the frozen input, or why it cannot be had.
///
/// The input rides boxed: it holds replay state an order of magnitude larger
/// than the failure message, and the enum would otherwise pad every send.
pub(crate) enum FrozenDelivery {
    Ready(Box<FrozenInput>),
    Failed(String),
}

/// Runtime state of one union view. Registration (`inputs`) is the accepted
/// baseline; `pending` is the parked candidate; the rest is job plumbing.
pub(crate) struct UnionViewState {
    inputs: Vec<StoredUnionInput>,
    pending: Option<UnionCandidateSpec>,
    published_revision: u64,
    published_generation: u64,
    cancel: Arc<AtomicBool>,
    generation: u64,
    worker: Option<JoinHandle<()>>,
    cmd_rx: Option<Receiver<DriverCmd>>,
    frozen_tx: Option<SyncSender<FrozenDelivery>>,
    pending_delivery: Option<FrozenDelivery>,
    completed: VecDeque<UnionCompletion>,
}

impl UnionViewState {
    fn new() -> Self {
        Self {
            inputs: Vec::new(),
            pending: None,
            published_revision: 0,
            published_generation: 0,
            cancel: Arc::new(AtomicBool::new(false)),
            generation: 0,
            worker: None,
            cmd_rx: None,
            frozen_tx: None,
            pending_delivery: None,
            completed: VecDeque::new(),
        }
    }
}

/// Union-sized freeze limits from the remaining merge budget.
///
/// Output caps follow what is left to merge; batch geometry stays at the
/// export default so page service behaves identically.
fn union_freeze_limits(remaining_rows: u64, remaining_bytes: u64) -> FrozenInputLimits {
    FrozenInputLimits {
        batch_records: 1_024,
        batch_bytes: 8 * 1024 * 1024,
        maximum_scanned_records: remaining_rows.max(1),
        maximum_input_bytes: 16 * 1024 * 1024 * 1024,
        maximum_output_records: remaining_rows.max(1),
        maximum_output_bytes: remaining_bytes.max(1),
    }
}

impl super::NativeViewAdapter {
    /// Register a union view: an ordinary view entry (so paging, folding,
    /// export and time bounds work generically) plus union runtime state.
    ///
    /// `source_ids` is the app-computed deduped union of the input views'
    /// sources, for raw-row resolution through the existing provider. The
    /// view starts `Raw`; the first union candidate publishes the merge.
    pub fn register_union_view(
        &self,
        union_view_id: impl Into<String>,
        source_ids: Vec<SourceId>,
    ) -> Result<(), ViewError> {
        let union_view_id = union_view_id.into();
        let mut deduped = Vec::new();
        for source in source_ids {
            if !deduped.contains(&source) {
                deduped.push(source);
            }
        }
        if deduped.len() > self.config.maximum_sources_per_view {
            return Err(ViewError::SourceLimit);
        }
        let raw_view = format!("_lvu_native_raw_{union_view_id}");
        {
            let mut shared = self.shared.lock().expect("view state poisoned");
            if !shared.accepting {
                return Err(ViewError::Closed);
            }
            if deduped.iter().any(|id| !shared.sources.contains_key(id)) {
                return Err(ViewError::UnknownSource);
            }
            if !shared.views.contains_key(&union_view_id)
                && !shared.union_views.contains_key(&union_view_id)
                && shared.views.len().saturating_add(shared.union_views.len())
                    >= self.config.maximum_views
            {
                return Err(ViewError::ViewLimit);
            }
            if let Some(previous) = shared.views.get(&union_view_id) {
                previous.cancel.store(true, Ordering::Release);
            }
            if let Some(previous) = shared.union_views.get(&union_view_id) {
                previous.cancel.store(true, Ordering::Release);
            }
            let revision = shared
                .views
                .get(&union_view_id)
                .map_or(1, |v| v.provider_revision.saturating_add(1));
            shared.views.insert(
                union_view_id.clone(),
                ViewState {
                    registration: ViewRegistration {
                        sources: deduped.clone(),
                        raw_view: raw_view.clone(),
                    },
                    pending_sources: None,
                    pending_request: None,
                    resubmit_pending: false,
                    desired_revision: 0,
                    cancel: Arc::new(AtomicBool::new(false)),
                    published: Published::Raw,
                    provider_revision: revision,
                    status: ViewQueryStatus {
                        view_id: union_view_id.clone(),
                        revision: 0,
                        state: ScanState::Raw,
                        scanned_records: 0,
                        high_watermarks: Vec::new(),
                        matched_records: 0,
                        index_bytes: 0,
                        diagnostic: None,
                    },
                    last_request: None,
                    applied_revision: 0,
                    applied_generation: 0,
                    applied_constraints: lvu::QueryConstraints::default(),
                    refreshing: false,
                    rows_requested: 0,
                    rows_missing: 0,
                    rows_served: 0,
                    rows_raw_revision: 0,
                    rows_retry: 0,
                    rows_retry_revision: 0,
                    fold: None,
                    command_results: BTreeMap::new(),
                    retained: None,
                },
            );
            shared
                .union_views
                .insert(union_view_id.clone(), UnionViewState::new());
        }
        self.raw
            .register_raw_view(&raw_view, deduped)
            .map_err(ViewError::Live)
    }

    /// Forget a union view: cancel its worker, drop runtime state and the
    /// view entry. Sources and inputs are unaffected — a union never owns
    /// capture. Detached worker threads settle alone on their cancel token,
    /// the correlation precedent.
    pub fn unregister_union_view(&self, union_view_id: &str) {
        let mut shared = self.shared.lock().expect("view state poisoned");
        if let Some(state) = shared.union_views.remove(union_view_id) {
            state.cancel.store(true, Ordering::Release);
        }
        if let Some(view) = shared.views.remove(union_view_id) {
            view.cancel.store(true, Ordering::Release);
        }
    }

    /// Accepted input references of a union view, if it is one.
    pub fn union_inputs(&self, union_view_id: &str) -> Option<Vec<StoredUnionInput>> {
        self.shared
            .lock()
            .expect("view state poisoned")
            .union_views
            .get(union_view_id)
            .map(|state| state.inputs.clone())
    }

    /// Park a union candidate and start its background job.
    ///
    /// Structural validation (count, duplicates, self, view-ID bounds) and
    /// the dependency-graph cycle check run here on the caller's thread;
    /// `resolve` maps a union view ID to its stored inputs (`None` for
    /// ordinary views) and is supplied by the caller, which owns the
    /// dependency graph. Revision fencing is per input at freeze time AND
    /// atomically at publication: any input that moved aborts the job as
    /// stale with the prior union preserved. A newer submit for the same
    /// union supersedes: the older thread is cancelled and replaced.
    /// Union revisions must strictly increase; anything at or below the
    /// published revision is a no-op `Ok`, mirroring `submit_query`.
    pub fn submit_union_candidate(
        &mut self,
        candidate: UnionCandidateSpec,
        resolve: &dyn Fn(&str) -> Option<Vec<String>>,
    ) -> Result<(), String> {
        let ids: Vec<&str> = candidate
            .inputs
            .iter()
            .map(|input| input.view_id.as_str())
            .collect();
        validate_union_spec(&candidate.union_view_id, &ids).map_err(|error| error.to_string())?;
        detect_union_cycle(&candidate.union_view_id, &ids, resolve)
            .map_err(|error| error.to_string())?;
        let (cmd_tx, cmd_rx) = sync_channel(8);
        let (frozen_tx, frozen_rx) = sync_channel(1);
        {
            let mut shared = self.shared.lock().expect("view state poisoned");
            if !shared.accepting {
                return Err("view adapter is shut down".into());
            }
            let published_revision = match shared.union_views.get(&candidate.union_view_id) {
                None => return Err("unknown union view".into()),
                Some(state) => state.published_revision,
            };
            if candidate.union_revision <= published_revision {
                return Ok(());
            }
            if let Some(unknown) = candidate
                .inputs
                .iter()
                .find(|input| !shared.views.contains_key(&input.view_id))
            {
                return Err(format!("union input view '{}' is unknown", unknown.view_id));
            }
            let Some(state) = shared.union_views.get_mut(&candidate.union_view_id) else {
                return Err("unknown union view".into());
            };
            state.cancel.store(true, Ordering::Release);
            state.cancel = Arc::new(AtomicBool::new(false));
            state.pending = Some(candidate.clone());
            state.cmd_rx = Some(cmd_rx);
            state.frozen_tx = Some(frozen_tx);
            state.pending_delivery = None;
            state.generation = state.generation.saturating_add(1);
            let generation = state.generation;
            let cancel = Arc::clone(&state.cancel);
            let ctx = UnionJobCtx {
                spec: candidate.clone(),
                generation,
                cancel,
                shared: Arc::clone(&self.shared),
                page_bytes: self.config.page_bytes,
                budget: Arc::clone(&self.budget),
                cmd_tx,
                frozen_rx,
                limits: UnionLimits::default(),
            };
            let handle = thread::Builder::new()
                .name("lvu-view-union".into())
                .spawn(move || {
                    union_job_loop(ctx);
                })
                .map_err(|error| error.to_string())?;
            state.worker = Some(handle);
        }
        Ok(())
    }

    /// Drain finished union jobs: publication acknowledgements the shell
    /// routes once `QueryPurpose::Union` exists (W22 contact). Published
    /// membership is already installed; this only reports it.
    pub fn take_union_completions(&self) -> Vec<UnionCompletion> {
        let mut shared = self.shared.lock().expect("view state poisoned");
        let mut out = Vec::new();
        for state in shared.union_views.values_mut() {
            out.extend(state.completed.drain(..));
        }
        out
    }

    /// Serve freeze traffic for running union jobs: parked deliveries first,
    /// then at most [`MAX_FREEZE_PER_TICK`] new freezes. Called from
    /// `drain_updates`; freezing is lock-plus-clones work and never journal
    /// I/O. Cancelled unions drop their parked deliveries (releasing snapshot
    /// leases) instead of serving them.
    pub(crate) fn drive_unions(&self) {
        enum Action {
            Deliver { union_view_id: String },
            Freeze(DriverCmd),
        }
        let mut served = 0;
        while served < MAX_FREEZE_PER_TICK {
            let action = {
                let mut shared = self.shared.lock().expect("view state poisoned");
                let mut found = None;
                for (union_view_id, state) in shared.union_views.iter_mut() {
                    if state.cancel.load(Ordering::Acquire) {
                        // A superseded job's parked lease must not outlive it.
                        state.pending_delivery.take();
                        continue;
                    }
                    if state.pending_delivery.is_some() {
                        found = Some(Action::Deliver {
                            union_view_id: union_view_id.clone(),
                        });
                        break;
                    }
                    if let Some(rx) = &state.cmd_rx
                        && let Ok(cmd) = rx.try_recv()
                    {
                        found = Some(Action::Freeze(cmd));
                        break;
                    }
                }
                found
            };
            match action {
                None => break,
                Some(Action::Deliver { union_view_id }) => {
                    let mut shared = self.shared.lock().expect("view state poisoned");
                    if let Some(state) = shared.union_views.get_mut(&union_view_id)
                        && let Some(delivery) = state.pending_delivery.take()
                    {
                        // `match`, not `map`: returning the send result from
                        // a closure would carry the frozen input in its error
                        // type (clippy `result_large_err`).
                        let mut parked = Some(delivery);
                        if let Some(tx) = state.frozen_tx.as_ref()
                            && let Some(delivery) = parked.take()
                        {
                            match tx.try_send(delivery) {
                                Ok(()) => {}
                                Err(TrySendError::Full(delivery)) => {
                                    parked = Some(delivery);
                                }
                                Err(TrySendError::Disconnected(_)) => {}
                            }
                        }
                        state.pending_delivery = parked;
                    }
                    served += 1;
                }
                Some(Action::Freeze(cmd)) => {
                    let cancelled = self
                        .shared
                        .lock()
                        .expect("view state poisoned")
                        .union_views
                        .get(&cmd.union_view_id)
                        .map(|state| state.cancel.load(Ordering::Acquire))
                        .unwrap_or(true);
                    if !cancelled {
                        let delivery = match self.freeze_input(&cmd.view_id, cmd.limits) {
                            Ok(frozen) => FrozenDelivery::Ready(Box::new(frozen)),
                            Err(error) => FrozenDelivery::Failed(error.to_string()),
                        };
                        let mut shared = self.shared.lock().expect("view state poisoned");
                        if let Some(state) = shared.union_views.get_mut(&cmd.union_view_id) {
                            let mut parked = Some(delivery);
                            if let Some(tx) = state.frozen_tx.as_ref()
                                && let Some(delivery) = parked.take()
                            {
                                match tx.try_send(delivery) {
                                    Ok(()) => {}
                                    Err(TrySendError::Full(delivery)) => {
                                        parked = Some(delivery);
                                    }
                                    Err(TrySendError::Disconnected(_)) => {}
                                }
                            }
                            state.pending_delivery = parked;
                        }
                    }
                    served += 1;
                }
            }
        }
    }
}

/// Everything one union job needs, bundled so the thread entry takes one
/// argument.
struct UnionJobCtx {
    spec: UnionCandidateSpec,
    generation: u64,
    cancel: Arc<AtomicBool>,
    shared: Arc<Mutex<Shared>>,
    page_bytes: usize,
    budget: Arc<MemoryBudget>,
    cmd_tx: SyncSender<DriverCmd>,
    frozen_rx: Receiver<FrozenDelivery>,
    limits: UnionLimits,
}

/// Per-input frozen data the worker carries between freeze and publication.
struct FrozenUnionWork {
    view_id: String,
    summary_revision: u64,
    summary_generation: u64,
    basis: lvu::TimeBasis,
    /// Union-source identity, generation and high-watermark per source the
    /// frozen input covers, straight from its summary.
    source_meta: Vec<(SourceId, u64, Option<u64>)>,
    scanned_records: u64,
    rows: Vec<UnionFrozenRow>,
}

/// Entry point of the per-candidate background thread. Records exactly one
/// [`UnionCompletion`] and never touches published state on failure.
fn union_job_loop(ctx: UnionJobCtx) {
    let error = run_union_job(&ctx).err();
    let completion = UnionCompletion {
        union_view_id: ctx.spec.union_view_id.clone(),
        union_revision: ctx.spec.union_revision,
        generation: ctx.generation,
        error,
    };
    let mut shared = ctx.shared.lock().expect("view state poisoned");
    let Some(state) = shared.union_views.get_mut(&ctx.spec.union_view_id) else {
        return;
    };
    // A superseded job's report must not confuse the shell: only the live
    // generation lands. Older threads settle alone (correlation precedent).
    if state.generation != ctx.generation {
        return;
    }
    let failed = completion.error.clone();
    if failed.is_some() {
        state.pending = None;
    }
    let was_published = state.published_revision;
    state.completed.push_back(completion);
    if let Some(error) = failed
        && let Some(view) = shared.views.get_mut(&ctx.spec.union_view_id)
    {
        view.status.state = if was_published > 0 {
            ScanState::Ready
        } else {
            ScanState::Raw
        };
        view.status.diagnostic = Some(error);
    }
}

/// Freeze, visit, decode, merge and publish one union candidate.
///
/// Freeze, visit, decode, merge and publish one union candidate.
///
/// Every fallible step returns an error with published state untouched; only
/// the final locked section mutates anything.
fn run_union_job(ctx: &UnionJobCtx) -> Result<(), String> {
    let mut frozen_inputs = Vec::with_capacity(ctx.spec.inputs.len());
    let mut remaining_rows = ctx.limits.maximum_rows as u64;
    let mut remaining_bytes = ctx.limits.maximum_bytes;
    for input in ctx.spec.inputs.iter() {
        check_cancelled(&ctx.cancel, &ctx.shared)?;
        ctx.cmd_tx
            .send(DriverCmd {
                union_view_id: ctx.spec.union_view_id.clone(),
                view_id: input.view_id.clone(),
                limits: union_freeze_limits(remaining_rows, remaining_bytes),
            })
            .map_err(|_| "union driver is gone".to_owned())?;
        let frozen = loop {
            check_cancelled(&ctx.cancel, &ctx.shared)?;
            match ctx.frozen_rx.recv_timeout(FREEZE_WAIT) {
                Ok(FrozenDelivery::Ready(frozen)) => break *frozen,
                Ok(FrozenDelivery::Failed(error)) => return Err(error),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    return Err("union driver is gone".to_owned());
                }
            }
        };
        let work = visit_union_input(
            ctx,
            input,
            frozen,
            &mut remaining_rows,
            &mut remaining_bytes,
        )?;
        frozen_inputs.push(work);
    }
    check_cancelled(&ctx.cancel, &ctx.shared)?;
    let frozen_refs: Vec<UnionFrozenInput> = frozen_inputs
        .iter()
        .map(|work| UnionFrozenInput {
            view_id: work.view_id.clone(),
            applied_revision: work.summary_revision,
            applied_generation: work.summary_generation,
            timestamp_column: String::new(),
            rows: work.rows.clone(),
        })
        .collect();
    let merged = union_frozen_inputs(&ctx.spec.union_view_id, &frozen_refs, &ctx.limits)
        .map_err(|error| error.to_string())?;
    publish_union(ctx, frozen_inputs, merged)
}

/// True when the adapter is shutting down or this job was superseded.
fn check_cancelled(cancel: &Arc<AtomicBool>, shared: &Arc<Mutex<Shared>>) -> Result<(), String> {
    if cancel.load(Ordering::Acquire) {
        return Err("union superseded".into());
    }
    if !shared.lock().expect("view state poisoned").accepting {
        return Err("view adapter is shut down".into());
    }
    Ok(())
}

/// Freeze-check, timestamp-map and bounded visit for one input.
///
/// The fence is checked against the frozen summary first (the input may have
/// advanced between submit and freeze), then the accepted basis times are
/// mapped from the input's published membership: filtered views contribute
/// their basis vector, raw views contribute capture nanos. A visited record
/// missing from the membership map means the input moved mid-freeze and the
/// job aborts as stale — never a silent mix.
fn visit_union_input(
    ctx: &UnionJobCtx,
    input: &StoredUnionInput,
    frozen: FrozenInput,
    remaining_rows: &mut u64,
    remaining_bytes: &mut u64,
) -> Result<FrozenUnionWork, String> {
    let summary = frozen.summary().clone();
    if union_input_stale_fence(input, &summary) {
        return Err(format!(
            "union input '{}' moved from revision {} to {}",
            input.view_id, input.accepted_revision, summary.applied_revision
        ));
    }
    // Timestamp authority per record, read under one lock with the fence.
    enum TimeSource {
        Membership(HashMap<(String, u64), Option<i64>>),
        Capture,
    }
    let (time_source, basis) = {
        let shared = ctx.shared.lock().expect("view state poisoned");
        let Some(view) = shared.views.get(&input.view_id) else {
            return Err(format!("union input view '{}' is unknown", input.view_id));
        };
        if view.applied_revision != input.accepted_revision {
            return Err(format!(
                "union input '{}' moved from revision {} to {}",
                input.view_id, input.accepted_revision, view.applied_revision
            ));
        }
        match &view.published {
            Published::Filtered { membership } => {
                let mut map = HashMap::new();
                for source in &membership.sources {
                    let times: Vec<i64> = source.times.iter().copied().collect();
                    for (index, sequence) in source.sequences.iter().copied().enumerate() {
                        let time = times
                            .get(index)
                            .copied()
                            .and_then(|value| (value != NO_BASIS_TIME).then_some(value));
                        map.insert((source.source_id.clone(), sequence), time);
                    }
                }
                (TimeSource::Membership(map), membership.basis)
            }
            Published::Raw => (TimeSource::Capture, lvu::TimeBasis::Capture),
        }
    };
    let cancel = Arc::clone(&ctx.cancel);
    let mut rows: Vec<UnionFrozenRow> = Vec::new();
    let mut bytes: u64 = 0;
    let stats = frozen
        .visit(&cancel, |batch| {
            if cancel.load(Ordering::Acquire) {
                return Err("union superseded".into());
            }
            for row in &batch.rows {
                let timestamp_nanos = match &time_source {
                    TimeSource::Membership(map) => *map
                        .get(&(
                            row.record.record_id.source_id.0.to_string(),
                            row.record.record_id.sequence,
                        ))
                        .ok_or_else(|| {
                            format!("union input '{}' moved during freeze", input.view_id)
                        })?,
                    TimeSource::Capture => Some(row.record.captured_at_unix_nanos),
                };
                bytes =
                    bytes.saturating_add(u64::try_from(row.record.bytes.len()).unwrap_or(u64::MAX));
                if bytes > *remaining_bytes {
                    return Err(format!(
                        "union input '{}' exceeds the byte budget",
                        input.view_id
                    ));
                }
                rows.push(UnionFrozenRow {
                    record_id: row.record.record_id,
                    timestamp_nanos,
                    fields: row.fields.clone(),
                    field_types: row.field_types.clone(),
                });
            }
            if (rows.len() as u64) > *remaining_rows {
                return Err(format!(
                    "union input '{}' exceeds the row budget",
                    input.view_id
                ));
            }
            Ok(())
        })
        .map_err(|error| match error {
            FrozenInputError::Cancelled => "union superseded".to_owned(),
            other => other.to_string(),
        })?;
    *remaining_rows = remaining_rows.saturating_sub(rows.len() as u64);
    *remaining_bytes = remaining_bytes.saturating_sub(bytes);
    // The lease releases here, before the next input freezes: at most one
    // snapshot lease is ever held per union job.
    drop(frozen);
    Ok(FrozenUnionWork {
        view_id: input.view_id.clone(),
        summary_revision: summary.applied_revision,
        summary_generation: summary.applied_generation,
        basis,
        source_meta: summary
            .sources
            .iter()
            .map(|source| (source.source_id, source.generation, source.high_watermark))
            .collect(),
        scanned_records: stats.scanned_records,
        rows,
    })
}

/// The fence for one input: revision AND generation must match.
fn union_input_stale_fence(
    input: &StoredUnionInput,
    summary: &super::export::FrozenInputSummary,
) -> bool {
    input.accepted_revision != summary.applied_revision
        || input.applied_generation != summary.applied_generation
}

/// Merge result publication: fence re-verification and membership install
/// happen atomically under one lock, so no input can advance between the
/// check and the install. Any failure leaves the prior union untouched.
fn publish_union(
    ctx: &UnionJobCtx,
    frozen_inputs: Vec<FrozenUnionWork>,
    merged: polars::prelude::DataFrame,
) -> Result<(), String> {
    use super::union::{SEQUENCE_COLUMN, SOURCE_ID_COLUMN, UNION_TS_COLUMN};
    use polars::prelude::AnyValue;
    let height = merged.height();
    // Display rank per identity, in engine output order.
    let sources = merged
        .column(SOURCE_ID_COLUMN)
        .map_err(|error| error.to_string())?;
    let sequences = merged
        .column(SEQUENCE_COLUMN)
        .map_err(|error| error.to_string())?;
    let times = merged
        .column(UNION_TS_COLUMN)
        .map_err(|error| error.to_string())?;
    let mut ranks: HashMap<(String, u64), i64> = HashMap::with_capacity(height);
    let mut per_source: HashMap<String, Vec<(u64, Option<i64>, i64)>> = HashMap::new();
    for index in 0..height {
        let source = match sources.get(index).map_err(|error| error.to_string())? {
            AnyValue::Null => {
                return Err("union result carries a null source identity".into());
            }
            value => value.str_value().to_string(),
        };
        let sequence = sequences
            .get(index)
            .map_err(|error| error.to_string())?
            .try_extract::<u64>()
            .map_err(|error| error.to_string())?;
        let time = match times.get(index).map_err(|error| error.to_string())? {
            AnyValue::Null => None,
            value => Some(
                value
                    .try_extract::<i64>()
                    .map_err(|error| error.to_string())?,
            ),
        };
        let rank = i64::try_from(index).map_err(|error| error.to_string())?;
        ranks.insert((source.clone(), sequence), rank);
        per_source
            .entry(source)
            .or_default()
            .push((sequence, time, rank));
    }
    let mut shared = ctx.shared.lock().expect("view state poisoned");
    // Atomic fence re-verification: every input still at its fenced revision
    // and generation, and this job still the live generation.
    let Some(state) = shared.union_views.get(&ctx.spec.union_view_id) else {
        return Err("unknown union view".into());
    };
    if state.generation != ctx.generation || ctx.cancel.load(Ordering::Acquire) {
        return Err("union superseded".into());
    }
    for input in &ctx.spec.inputs {
        let Some(view) = shared.views.get(&input.view_id) else {
            return Err(format!("union input view '{}' is unknown", input.view_id));
        };
        if view.applied_revision != input.accepted_revision
            || view.applied_generation != input.applied_generation
        {
            return Err(format!(
                "union input '{}' moved during the merge",
                input.view_id
            ));
        }
    }
    let Some(view) = shared.views.get(&ctx.spec.union_view_id) else {
        return Err("unknown union view".into());
    };
    // Charge the publication like any membership: per-row identity/time/rank
    // plus per-source overhead. Over budget fails as limited with the prior
    // union preserved.
    let mut reservation = Reservation::new(Arc::clone(&ctx.budget));
    let source_count = view.registration.sources.len();
    if !reservation.add(
        (height as u64)
            .saturating_mul(SEQUENCE_BYTES.saturating_mul(2))
            .saturating_add(
                (source_count as u64).saturating_mul(SOURCE_OVERHEAD.saturating_add(64)),
            ),
    ) {
        return Err("union membership exceeds the memory budget".into());
    }
    let union_basis = frozen_inputs
        .first()
        .map(|work| work.basis)
        .unwrap_or_default();
    // Mixed input bases compare as nanos against nanos; coherent unions keep
    // coherent input bases (normalization is the inputs' own enrichment, like
    // schema). The publication records the first input's basis honestly
    // rather than inventing one.
    let mut sources_out = Vec::with_capacity(source_count);
    let mut high_watermarks = Vec::with_capacity(source_count);
    let mut event_time_missing = 0usize;
    for source_id in view.registration.sources.clone() {
        let key = source_id.0.to_string();
        let mut members = per_source.remove(&key).unwrap_or_default();
        members.sort_by_key(|member| member.0);
        let (generation, high) = frozen_inputs
            .iter()
            .flat_map(|work| work.source_meta.iter())
            .find(|(id, _, _)| *id == source_id)
            .map(|(_, generation, high)| (*generation, *high))
            .unwrap_or((0, None));
        high_watermarks.push((source_id, high));
        let mut bounds = SourceTimeBounds::default();
        let mut sequences_out = Appended::default();
        let mut times_out = Appended::default();
        let mut keys_out = Appended::default();
        let mut ascending = true;
        let mut previous_rank = i64::MIN;
        for (sequence, time, rank) in members {
            bounds.observe(time);
            if time.is_none() {
                event_time_missing += 1;
            }
            if rank < previous_rank {
                ascending = false;
            }
            previous_rank = rank;
            sequences_out.extend(vec![sequence]);
            times_out.extend(vec![time.unwrap_or(NO_BASIS_TIME)]);
            keys_out.extend(vec![rank]);
        }
        sources_out.push(SourceMatches {
            source_id: key,
            generation,
            high_watermark: high,
            sequences: sequences_out,
            times: times_out,
            groups: Appended::default(),
            bounds,
            merge_keys: keys_out,
            ascending,
        });
    }
    let count = height as u64;
    let (order, result_ranks, max_key) = merge_order(&sources_out, false, true, None);
    let bytes = reservation.bytes;
    reservation.committed = true;
    let membership = Membership {
        sources: sources_out,
        count,
        bytes,
        budget: Arc::clone(&ctx.budget),
        enrichment_names: Vec::new(),
        derived: HashMap::new(),
        color_matches: HashMap::new(),
        color_rules: Vec::new(),
        advanced: None,
        enrichment: Vec::new(),
        evaluation_page_bytes: ctx.page_bytes,
        evaluation_batches: Vec::new().into(),
        event_time_missing,
        event_time_invalid: 0,
        basis: union_basis,
        grouped: false,
        order,
        ranks: result_ranks,
        max_key,
    };
    let view = shared
        .views
        .get_mut(&ctx.spec.union_view_id)
        .expect("checked");
    view.pending_request = None;
    view.resubmit_pending = false;
    view.published = Published::Filtered {
        membership: Arc::new(membership),
    };
    view.applied_revision = ctx.spec.union_revision;
    view.applied_generation = ctx.spec.generation;
    view.applied_constraints = lvu::QueryConstraints::default();
    view.last_request = None;
    view.refreshing = false;
    view.provider_revision = view.provider_revision.saturating_add(1);
    view.status = ViewQueryStatus {
        view_id: ctx.spec.union_view_id.clone(),
        revision: ctx.spec.union_revision,
        state: ScanState::Ready,
        scanned_records: frozen_inputs.iter().map(|work| work.scanned_records).sum(),
        high_watermarks,
        matched_records: count,
        index_bytes: bytes,
        diagnostic: None,
    };
    let state = shared
        .union_views
        .get_mut(&ctx.spec.union_view_id)
        .expect("checked");
    state.inputs = ctx.spec.inputs.clone();
    state.pending = None;
    state.published_revision = ctx.spec.union_revision;
    state.published_generation = ctx.spec.generation;
    Ok(())
}
