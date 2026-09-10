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
//!   re-verified against the adopted (frozen) fence atomically with
//!   publication, so an input that advanced mid-job aborts as stale instead
//!   of publishing a mixed revision. Submit-time skew from a live tail is
//!   adopted at freeze time, never failed: only movement during the job's
//!   own visit/merge window aborts. Any failure preserves the prior
//!   published union.
//! * Publication is an ordinary `Membership`: per-source surviving sequences
//!   (ascending, for raw resolution through the existing provider),
//!   `NO_BASIS_TIME` where the basis had no value, and `merge_keys` set to
//!   each record's DISPLAY RANK. Ranks are strictly increasing per source, so
//!   the existing k-way merge reproduces the union order exactly; `times`
//!   still carries the honest basis values for time bounds and gap search.
//!   Union-level search, native Advanced and exact-key predicates execute on
//!   the merged/deduplicated typed frame before this membership is built.
//!   Grouping consumes native typed flags (or the established lexical rule),
//!   while paging, folding, export, raw context and time bounds read the same
//!   ordinary `Membership` downstream.

use super::export::{FrozenInput, FrozenInputError, FrozenInputLimits};
use super::union::{
    INPUT_COLUMN, StoredUnionInput, UnionCandidateSpec, UnionCompletion, UnionFilterSpec,
    UnionFrozenInput, UnionFrozenRow, UnionLimits, detect_union_cycle, lossy_utf8_len,
    union_frozen_inputs, union_row_carrier_bytes, union_workspace_bytes, validate_union_spec,
};
use super::{
    Appended, AutoLine, ContinuationRule, GroupRange, MAX_CONFIGURED_GROUP_STORED,
    MAX_GROUP_LINE_DISPLAY_BYTES, MAX_GROUP_LINES, MAX_GROUP_PAYLOAD_BYTES, Membership,
    MemoryBudget, NO_BASIS_TIME, Published, Reservation, SEQUENCE_BYTES, SOURCE_OVERHEAD,
    ScanState, Shared, SourceMatches, SourceTimeBounds, ViewError, ViewQueryStatus,
    ViewRegistration, ViewState, auto_group_within_span, color_match_bytes,
    display_projection_bytes, group_state_bytes, merge_order,
};
use lvu_core::{RecordId, SourceId};
use lvu_query::{
    BatchQuery, BatchValidity, CompiledDefinition, KeyFlag, TextSearch, exact_column_expr,
    exact_key_flags, execute_batch_with_native_predicate, non_null_flags, scalar_projection,
};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
    mpsc::{Receiver, SyncSender, TrySendError, sync_channel},
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// How many freeze requests the tick driver serves per `drain_updates` call.
///
/// Freezing is cheap (lock plus clones), but the tick has a frame budget and
/// union work must never crowd out query completions.
const MAX_FREEZE_PER_TICK: usize = 2;

/// How long the worker waits for a frozen delivery before re-checking cancel
/// and shutdown. Bounded like every other provider wait: a closed view or a
/// stopped adapter releases the thread within this horizon.
const FREEZE_WAIT: Duration = Duration::from_millis(100);
const UNION_RETRY_INITIAL: Duration = Duration::from_millis(25);
const UNION_RETRY_MAXIMUM: Duration = Duration::from_secs(1);
const SOURCE_SET_MOVED: &str =
    "a union input source set moved; refresh requires a new union definition";

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

/// Deterministic test instrumentation for the freeze/publish window.
///
/// The worker signals `frozen` once every input is frozen and visited, then
/// waits for `release` before merging. Production jobs never carry one.
/// This is how tests advance an input strictly inside the window instead of
/// racing it: without a barrier the interleaving cannot be arranged on
/// purpose, and sleep-based tests only prove the race is usually lost.
pub struct UnionTestBarrier {
    pub frozen: SyncSender<()>,
    pub release: Receiver<()>,
}

/// Test-only barrier after guarded raw progress snapshots are read and before
/// the final shared-state install. A producer racing here must either publish
/// first (and make the candidate stale) or block until the union installs.
pub struct UnionPublishTestBarrier {
    pub checked: SyncSender<()>,
    pub release: Receiver<()>,
}

/// One-shot counters for discriminating allocation/complexity regressions.
/// Production jobs carry no probe.
pub struct UnionPhaseTestProbe {
    pub retained_rows: Arc<AtomicUsize>,
    pub polars_builds: Arc<AtomicUsize>,
    pub grouping_indexed_rows: Arc<AtomicUsize>,
    pub grouping_lookups: Arc<AtomicUsize>,
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
    /// One-shot deterministic test barrier for the next job only. Taken by
    /// the worker at start; production code never sets it.
    test_barrier: Option<UnionTestBarrier>,
    publish_test_barrier: Option<UnionPublishTestBarrier>,
    phase_test_probe: Option<UnionPhaseTestProbe>,
    transient_test_failure: Option<String>,
    completed: VecDeque<UnionCompletion>,
    /// Compiled/parsed predicates become reusable only with the publication
    /// they helped produce. Failed or stale candidates never mutate this.
    prepared_filter: Option<PreparedUnionFilter>,
    /// Per-input source progress accepted by the last publication. This is
    /// the live-refresh trigger; definition revision/generation alone cannot
    /// observe an append to a raw input, which intentionally stays at 0/0.
    published_source_fences: Vec<(String, Vec<super::union::UnionSourceFence>)>,
    /// A dependency state with a terminal definition/registration failure.
    /// Live refresh must not resubmit that identical state on every tick;
    /// transient execution failures use the bounded retry state below.
    rejected_attempt: Option<UnionDependencyAttempt>,
    /// Transient resource/execution failures retry the identical dependency
    /// state after a bounded delay. They must not be memoized as permanent,
    /// but retrying every render tick would be a busy loop.
    retry_attempt: Option<UnionDependencyAttempt>,
    retry_not_before: Option<Instant>,
    retry_delay: Duration,
    published_filter: UnionFilterSpec,
    published_color_rules: Vec<lvu::ColorRule>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct UnionDependencyAttempt {
    inputs: Vec<StoredUnionInput>,
    filter: UnionFilterSpec,
    color_rules: Vec<lvu::ColorRule>,
    registered_sources: Vec<SourceId>,
    source_fences: Vec<(String, Vec<super::union::UnionSourceFence>)>,
    /// A definition fence can stay unchanged while its accepted membership
    /// finishes publishing during restore. Include that publication movement
    /// so an attempt against the temporary raw carrier is not memoized after
    /// the derived frame becomes authoritative.
    input_publication_revisions: Vec<(String, u64)>,
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
            test_barrier: None,
            publish_test_barrier: None,
            phase_test_probe: None,
            transient_test_failure: None,
            completed: VecDeque::new(),
            prepared_filter: None,
            published_source_fences: Vec::new(),
            rejected_attempt: None,
            retry_attempt: None,
            retry_not_before: None,
            retry_delay: UNION_RETRY_INITIAL,
            published_filter: UnionFilterSpec::default(),
            published_color_rules: Vec::new(),
        }
    }
}

fn dependency_attempt(
    shared: &Shared,
    union_view_id: &str,
    inputs: &[StoredUnionInput],
    filter: &UnionFilterSpec,
    color_rules: &[lvu::ColorRule],
    refresh_input_fences: bool,
) -> Option<UnionDependencyAttempt> {
    let registered_sources = shared
        .views
        .get(union_view_id)?
        .registration
        .sources
        .clone();
    let mut current_inputs = Vec::with_capacity(inputs.len());
    let mut source_fences = Vec::with_capacity(inputs.len());
    let mut input_publication_revisions = Vec::with_capacity(inputs.len());
    for input in inputs {
        let view = shared.views.get(&input.view_id)?;
        current_inputs.push(if refresh_input_fences {
            StoredUnionInput {
                view_id: input.view_id.clone(),
                accepted_revision: view.applied_revision,
                applied_generation: view.applied_generation,
            }
        } else {
            input.clone()
        });
        source_fences.push((input.view_id.clone(), current_source_fence(shared, view)));
        input_publication_revisions.push((input.view_id.clone(), view.provider_revision));
    }
    Some(UnionDependencyAttempt {
        inputs: current_inputs,
        filter: filter.clone(),
        color_rules: color_rules.to_vec(),
        registered_sources,
        source_fences,
        input_publication_revisions,
    })
}

#[derive(Clone)]
struct PreparedUnionFilter {
    search_source: String,
    advanced_source: Option<String>,
    search: Option<TextSearch>,
    advanced: Option<CompiledDefinition>,
    grouping_source: Option<String>,
    grouping: Option<ContinuationRule>,
    color_rules_source: Vec<lvu::ColorRule>,
    colors: Vec<(String, TextSearch)>,
    column_colors: Vec<(String, String, String)>,
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

    /// Whether any accepted input has moved since this union last published.
    /// Includes raw source generation/high-watermark movement, which does not
    /// change an input view's accepted definition state. Read-only and
    /// bounded by union input/source limits; the app coalesces the resulting
    /// refresh while a union job is in flight.
    pub fn union_needs_refresh(&self, union_view_id: &str) -> Option<bool> {
        let shared = self.shared.lock().expect("view state poisoned");
        let state = shared.union_views.get(union_view_id)?;
        // Before a first successful publication, retry the definition whose
        // failure state the worker retained. This is still dependency-driven:
        // terminal failures remain quiet, while retryable failures obey their
        // bounded backoff and publish-readiness movement invalidates it.
        let attempted = (state.published_revision == 0)
            .then(|| {
                state
                    .retry_attempt
                    .as_ref()
                    .or(state.rejected_attempt.as_ref())
            })
            .flatten();
        let (inputs, filter, color_rules) = attempted
            .map(|attempt| {
                (
                    attempt.inputs.as_slice(),
                    &attempt.filter,
                    attempt.color_rules.as_slice(),
                )
            })
            .unwrap_or((
                &state.inputs,
                &state.published_filter,
                &state.published_color_rules,
            ));
        if state.published_revision == 0 && attempted.is_none() {
            return Some(false);
        }
        let mut needs_refresh =
            state.published_revision == 0 || inputs.len() != state.published_source_fences.len();
        for input in inputs {
            let Some(view) = shared.views.get(&input.view_id) else {
                return Some(true);
            };
            if view.applied_revision != input.accepted_revision
                || view.applied_generation != input.applied_generation
            {
                needs_refresh = true;
            }
            if state.published_revision > 0 {
                let Some((_, published)) = state
                    .published_source_fences
                    .iter()
                    .find(|(view_id, _)| view_id == &input.view_id)
                else {
                    return Some(true);
                };
                if &current_source_fence(&shared, view) != published {
                    needs_refresh = true;
                }
            }
        }
        if !needs_refresh {
            return Some(false);
        }
        let current = dependency_attempt(&shared, union_view_id, inputs, filter, color_rules, true);
        if current.as_ref() == state.rejected_attempt.as_ref() {
            return Some(false);
        }
        if current.as_ref() == state.retry_attempt.as_ref()
            && state
                .retry_not_before
                .is_some_and(|deadline| Instant::now() < deadline)
        {
            return Some(false);
        }
        Some(true)
    }

    /// Park a union candidate and start its background job.
    ///
    /// Structural validation (count, duplicates, self, view-ID bounds) and
    /// the dependency-graph cycle check run here on the caller's thread;
    /// `resolve` maps a union view ID to its stored inputs (`None` for
    /// ordinary views) and is supplied by the caller, which owns the
    /// dependency graph. The submit-time fence travels with the candidate;
    /// the worker requires each input's frozen revision to equal the submitted
    /// definition fence and re-verifies that fence atomically at publication.
    /// Legitimate movement is submitted as a new candidate; an older request
    /// never adopts it opportunistically. A newer submit for the same
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
            let dependency_attempt = dependency_attempt(
                &shared,
                &candidate.union_view_id,
                &candidate.inputs,
                &candidate.filter,
                &candidate.color_rules,
                false,
            );
            let Some(state) = shared.union_views.get_mut(&candidate.union_view_id) else {
                return Err("unknown union view".into());
            };
            if state.retry_attempt.as_ref() != dependency_attempt.as_ref() {
                state.retry_attempt = None;
                state.retry_not_before = None;
                state.retry_delay = UNION_RETRY_INITIAL;
            }
            state.cancel.store(true, Ordering::Release);
            state.cancel = Arc::new(AtomicBool::new(false));
            state.pending = Some(candidate.clone());
            state.cmd_rx = Some(cmd_rx);
            state.frozen_tx = Some(frozen_tx);
            state.pending_delivery = None;
            state.generation = state.generation.saturating_add(1);
            let generation = state.generation;
            let cancel = Arc::clone(&state.cancel);
            let test_barrier = state.test_barrier.take();
            let publish_test_barrier = state.publish_test_barrier.take();
            let phase_test_probe = state.phase_test_probe.take();
            let transient_test_failure = state.transient_test_failure.take();
            let ctx = UnionJobCtx {
                spec: candidate.clone(),
                generation,
                cancel,
                shared: Arc::clone(&self.shared),
                page_bytes: self.config.page_bytes,
                budget: Arc::clone(&self.budget),
                cmd_tx,
                frozen_rx,
                compiler_tx: self
                    .work
                    .as_ref()
                    .cloned()
                    .ok_or_else(|| "view adapter is shut down".to_owned())?,
                limits: UnionLimits::default(),
                test_barrier,
                publish_test_barrier,
                phase_test_probe,
                transient_test_failure,
                dependency_attempt,
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

    /// Arm the deterministic freeze/publish barrier for the next job on this
    /// union view. Test instrumentation only: production code never calls it,
    /// and each barrier is consumed once. The worker signals `frozen` after
    /// visiting every input and waits for `release` before merging, so tests
    /// can advance an input strictly inside the window.
    pub fn arm_union_test_barrier(
        &self,
        union_view_id: &str,
        barrier: UnionTestBarrier,
    ) -> Result<(), String> {
        let mut shared = self.shared.lock().expect("view state poisoned");
        let Some(state) = shared.union_views.get_mut(union_view_id) else {
            return Err("unknown union view".into());
        };
        state.test_barrier = Some(barrier);
        Ok(())
    }

    pub fn arm_union_publish_test_barrier(
        &self,
        union_view_id: &str,
        barrier: UnionPublishTestBarrier,
    ) -> Result<(), String> {
        let mut shared = self.shared.lock().expect("view state poisoned");
        let Some(state) = shared.union_views.get_mut(union_view_id) else {
            return Err("unknown union view".into());
        };
        state.publish_test_barrier = Some(barrier);
        Ok(())
    }

    pub fn arm_union_phase_test_probe(
        &self,
        union_view_id: &str,
        probe: UnionPhaseTestProbe,
    ) -> Result<(), String> {
        let mut shared = self.shared.lock().expect("view state poisoned");
        let Some(state) = shared.union_views.get_mut(union_view_id) else {
            return Err("unknown union view".into());
        };
        state.phase_test_probe = Some(probe);
        Ok(())
    }

    /// Inject one retryable worker failure into the next job. Test
    /// instrumentation only; consuming the value at submit keeps it one-shot.
    pub fn arm_union_transient_test_failure(
        &self,
        union_view_id: &str,
        message: impl Into<String>,
    ) -> Result<(), String> {
        let mut shared = self.shared.lock().expect("view state poisoned");
        let Some(state) = shared.union_views.get_mut(union_view_id) else {
            return Err("unknown union view".into());
        };
        state.transient_test_failure = Some(message.into());
        Ok(())
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
    compiler_tx: SyncSender<super::Work>,
    limits: UnionLimits,
    test_barrier: Option<UnionTestBarrier>,
    publish_test_barrier: Option<UnionPublishTestBarrier>,
    phase_test_probe: Option<UnionPhaseTestProbe>,
    transient_test_failure: Option<String>,
    dependency_attempt: Option<UnionDependencyAttempt>,
}

/// Per-input frozen metadata for the atomic publication fence. Small scalar
/// data only — rows travel separately by move, never cloned.
struct FrozenUnionMeta {
    basis: lvu::TimeBasis,
    source_meta: Vec<(SourceId, u64, Option<u64>)>,
    scanned_records: u64,
    accepted_enrichment_outputs: Vec<String>,
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
    accepted_enrichment_outputs: Vec<String>,
    rows: Vec<UnionFrozenRow>,
}

/// Entry point of the per-candidate background thread. Records exactly one
/// [`UnionCompletion`] and never touches published state on failure.
///
/// The completion carries the CALLER's submission generation
/// (`spec.generation`, the dialog/request generation the shell routes on).
/// The adapter's private per-union worker counter (`ctx.generation`) is only
/// for internal cancellation and supersession correlation: reporting it
/// would misroute completions whenever the two differ.
fn union_job_loop(ctx: UnionJobCtx) {
    let error = run_union_job(&ctx).err();
    let completion = UnionCompletion {
        union_view_id: ctx.spec.union_view_id.clone(),
        union_revision: ctx.spec.union_revision,
        generation: ctx.spec.generation,
        error,
    };
    let mut shared = ctx.shared.lock().expect("view state poisoned");
    let Some(state_generation) = shared
        .union_views
        .get(&ctx.spec.union_view_id)
        .map(|state| state.generation)
    else {
        return;
    };
    // A superseded job's report must not confuse the shell: only the live
    // generation lands. Older threads settle alone (correlation precedent).
    if state_generation != ctx.generation {
        return;
    }
    let failed = completion.error.clone();
    let state = shared
        .union_views
        .get_mut(&ctx.spec.union_view_id)
        .expect("checked above");
    if failed.is_some() {
        state.pending = None;
        if failed.as_deref().is_some_and(terminal_dependency_failure) {
            state.rejected_attempt = ctx.dependency_attempt.clone();
            state.retry_attempt = None;
            state.retry_not_before = None;
            state.retry_delay = UNION_RETRY_INITIAL;
        } else {
            state.rejected_attempt = None;
            state.retry_attempt = ctx.dependency_attempt.clone();
            state.retry_not_before = Some(Instant::now() + state.retry_delay);
            state.retry_delay = state
                .retry_delay
                .checked_mul(2)
                .unwrap_or(UNION_RETRY_MAXIMUM)
                .min(UNION_RETRY_MAXIMUM);
        }
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

fn terminal_dependency_failure(error: &str) -> bool {
    // Registration owns the raw provider's fixed source set. Re-running the
    // same accepted definition cannot make a newly introduced source
    // representable; only a definition/registration change can resolve it.
    error == SOURCE_SET_MOVED
}

/// Freeze, visit, decode, merge and publish one union candidate.
///
/// Every fallible step returns an error with published state untouched; only
/// the final locked section mutates anything.
fn run_union_job(ctx: &UnionJobCtx) -> Result<(), String> {
    if let Some(error) = &ctx.transient_test_failure {
        return Err(error.clone());
    }
    // Definition compilation belongs to the one existing CompilerHost and
    // happens before any input snapshot work. An unchanged successfully
    // published definition reuses its cached native expression on refresh.
    let prepared_filter = prepare_union_filter(ctx)?;
    check_cancelled(&ctx.cancel, &ctx.shared)?;
    let mut frozen_inputs = Vec::with_capacity(ctx.spec.inputs.len());
    let mut reservation = Reservation::new(Arc::clone(&ctx.budget));
    let mut carrier_bytes = 0u64;
    let mut remaining_rows = ctx.limits.maximum_rows as u64;
    let mut remaining_bytes = ctx.limits.maximum_bytes;
    // The submitted definition fence is authority. Live source progress is
    // tracked separately by source generation/high-watermarks; a later input
    // definition must produce a new candidate through the controller, never
    // be adopted opportunistically by this older request.
    let fence: Vec<StoredUnionInput> = ctx.spec.inputs.clone();
    for input in &ctx.spec.inputs {
        check_cancelled(&ctx.cancel, &ctx.shared)?;
        let view_id = input.view_id.clone();
        ctx.cmd_tx
            .send(DriverCmd {
                union_view_id: ctx.spec.union_view_id.clone(),
                view_id,
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
            &mut reservation,
            &mut carrier_bytes,
        )?;
        frozen_inputs.push(work);
    }
    check_cancelled(&ctx.cancel, &ctx.shared)?;
    // Move rows out of the per-input work: no second copy of the frozen
    // evaluation exists anywhere in the pipeline. Small metadata travels
    // beside the owned inputs for the atomic publication fence.
    let mut metas = Vec::with_capacity(frozen_inputs.len());
    let mut decoded_inputs = Vec::with_capacity(frozen_inputs.len());
    for work in frozen_inputs {
        metas.push(FrozenUnionMeta {
            basis: work.basis,
            source_meta: work.source_meta.clone(),
            scanned_records: work.scanned_records,
            accepted_enrichment_outputs: work.accepted_enrichment_outputs.clone(),
        });
        decoded_inputs.push(UnionFrozenInput {
            view_id: work.view_id,
            applied_revision: work.summary_revision,
            applied_generation: work.summary_generation,
            timestamp_column: String::new(),
            rows: work.rows,
        });
    }
    let accepted_enrichment_outputs = accepted_enrichment_outputs(&metas);
    project_accepted_output_authority(&mut decoded_inputs, &metas, &accepted_enrichment_outputs);
    // Deterministic test barrier, if armed: every input is frozen and
    // visited, nothing is merged or published yet.
    if let Some(barrier) = &ctx.test_barrier {
        let _ = barrier.frozen.send(());
        loop {
            check_cancelled(&ctx.cancel, &ctx.shared)?;
            match barrier.release.recv_timeout(FREEZE_WAIT) {
                Ok(()) => break,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    return Err("union test barrier is gone".to_owned());
                }
            }
        }
    }
    check_cancelled(&ctx.cancel, &ctx.shared)?;
    let workspace = union_workspace_bytes(&decoded_inputs).map_err(|error| error.to_string())?;
    if workspace > ctx.limits.maximum_bytes {
        return Err(format!(
            "union merge needs {workspace} bytes but the limit is {}",
            ctx.limits.maximum_bytes
        ));
    }
    let source_count = ctx
        .shared
        .lock()
        .expect("view state poisoned")
        .views
        .get(&ctx.spec.union_view_id)
        .ok_or_else(|| "unknown union view".to_owned())?
        .registration
        .sources
        .len() as u64;
    let retained_rows = decoded_inputs
        .iter()
        .try_fold(0u64, |total, input| {
            total.checked_add(input.rows.len() as u64)
        })
        .ok_or_else(|| "union identity index size overflow".to_owned())?;
    let identity_index_bytes = retained_rows
        .checked_mul(96)
        .ok_or_else(|| "union identity index size overflow".to_owned())?;
    let color_match_bytes =
        union_color_workspace_bytes(retained_rows, &prepared_filter.color_rules_source)?;
    let derived_bytes = union_derived_workspace_bytes(&decoded_inputs, &metas)?;
    // Configured grouping temporarily carries the engine flag vector and the
    // identity-indexed flag map together. Reserve both before either native
    // helper or map builder runs; Run keys may retain the full exact-key
    // bound and are never truncated to fit the budget.
    let configured_grouping_bytes = match prepared_filter.grouping.as_ref() {
        Some(ContinuationRule::Filter { .. }) => retained_rows
            .checked_mul(96)
            .and_then(|bytes| bytes.checked_mul(2)),
        Some(ContinuationRule::Run { .. }) => retained_rows
            .checked_mul(96 + lvu_query::MAX_EXACT_KEY_BYTES as u64)
            .and_then(|bytes| bytes.checked_mul(2)),
        _ => Some(0),
    }
    .ok_or_else(|| "union configured grouping size overflow".to_owned())?;
    let reserved_bytes = source_count
        .checked_mul(
            SOURCE_OVERHEAD
                .checked_add(64)
                .ok_or_else(|| "union workspace size overflow".to_owned())?,
        )
        .and_then(|overhead| workspace.checked_add(overhead))
        .and_then(|bytes| bytes.checked_add(identity_index_bytes))
        .and_then(|bytes| bytes.checked_add(configured_grouping_bytes))
        .and_then(|bytes| bytes.checked_add(color_match_bytes))
        .and_then(|bytes| bytes.checked_add(derived_bytes))
        .ok_or_else(|| "union workspace size overflow".to_owned())?;
    let additional = reserved_bytes
        .checked_sub(carrier_bytes)
        .ok_or_else(|| "union carrier accounting exceeded workspace".to_owned())?;
    if !reservation.add(additional) {
        return Err("union workspace exceeds the memory budget".into());
    }
    if let Some(probe) = &ctx.phase_test_probe {
        probe.polars_builds.fetch_add(1, Ordering::AcqRel);
    }
    let merged = union_frozen_inputs(&ctx.spec.union_view_id, &decoded_inputs, &ctx.limits)
        .map_err(|error| error.to_string())?;
    let exact = match ctx.spec.filter.exact_key.as_ref() {
        Some(key) => {
            let exact = exact_column_expr(&merged, key).map_err(|error| error.to_string())?;
            let authority = accepted_output_authority(&metas, key.field())?.ok_or_else(|| {
                format!(
                    "exact key column {:?} is not an accepted enrichment output",
                    key.field()
                )
            })?;
            Some(exact.and(authority))
        }
        None => None,
    };
    let column_color_authority = accepted_output_authorities(&metas)?;
    check_cancelled(&ctx.cancel, &ctx.shared)?;
    let result = execute_batch_with_native_predicate(
        &merged,
        BatchQuery {
            generation: ctx.generation,
            definition_generation: ctx.spec.union_revision,
            stages: &[],
            filter: prepared_filter.advanced.as_ref(),
            text_search: prepared_filter.search.as_ref(),
            colors: &prepared_filter.colors,
            column_colors: &prepared_filter.column_colors,
        },
        exact,
        &column_color_authority,
    );
    check_cancelled(&ctx.cancel, &ctx.shared)?;
    if result.generation != ctx.generation
        || result.definition_generation != ctx.spec.union_revision
        || result.validity != BatchValidity::Valid
    {
        let message = result
            .diagnostics
            .iter()
            .map(|diagnostic| {
                if diagnostic.code.is_empty() {
                    diagnostic.message.clone()
                } else {
                    format!("{}: {}", diagnostic.code, diagnostic.message)
                }
            })
            .collect::<Vec<_>>()
            .join("; ");
        return Err(if message.is_empty() {
            "union filter returned an invalid result".into()
        } else {
            message
        });
    }
    if let Some(diagnostic) = result
        .color_diagnostics
        .iter()
        .find(|diagnostic| diagnostic.state == lvu_query::DerivedState::Error)
    {
        let position = diagnostic
            .field
            .as_deref()
            .and_then(|field| field.parse::<usize>().ok())
            .unwrap_or(0)
            .saturating_add(1);
        return Err(format!("colour rule {position}: {}", diagnostic.message));
    }
    let (derived, precise_derived, derived_bytes) = union_derived_projection(
        &result.enriched_rows,
        &decoded_inputs,
        &metas,
        &accepted_enrichment_outputs,
        &result.matched_ids,
    )?;
    publish_union(
        ctx,
        &fence,
        metas,
        result.enriched_rows,
        result.matched_ids,
        result.color_matches,
        decoded_inputs,
        accepted_enrichment_outputs,
        derived,
        precise_derived,
        derived_bytes,
        prepared_filter,
        reservation,
    )
}

fn prepare_union_filter(ctx: &UnionJobCtx) -> Result<PreparedUnionFilter, String> {
    if ctx.spec.color_rules.len() > lvu::MAX_COLOR_RULES {
        return Err(format!(
            "a union takes at most {} colour rules",
            lvu::MAX_COLOR_RULES
        ));
    }
    let cached = {
        let shared = ctx.shared.lock().expect("view state poisoned");
        shared
            .union_views
            .get(&ctx.spec.union_view_id)
            .and_then(|state| state.prepared_filter.clone())
    };
    if let Some(cached) = cached
        && cached.search_source == ctx.spec.filter.search
        && cached.advanced_source == ctx.spec.filter.advanced_polars
        && cached.grouping_source == ctx.spec.filter.grouping
        && cached.color_rules_source == ctx.spec.color_rules
    {
        return Ok(cached);
    }
    let search_compiled = if TextSearch::is_polars(&ctx.spec.filter.search) {
        Some(compile_union_filter(ctx, &ctx.spec.filter.search)?)
    } else {
        None
    };
    let search = if ctx.spec.filter.search.is_empty() {
        None
    } else {
        Some(TextSearch::parse(
            ctx.spec.filter.search.clone(),
            search_compiled.as_ref(),
        )?)
    };
    let advanced = match ctx.spec.filter.advanced_polars.as_deref() {
        Some(source) if !source.trim().is_empty() => Some(compile_union_filter(ctx, source)?),
        _ => None,
    };
    let grouping = ctx
        .spec
        .filter
        .grouping
        .as_deref()
        .map(ContinuationRule::parse)
        .transpose()?;
    let mut colors = Vec::new();
    let mut column_colors = Vec::new();
    for (index, rule) in ctx.spec.color_rules.iter().enumerate() {
        let position = index + 1;
        if rule.is_column() {
            let column = rule.column.clone().unwrap_or_default();
            if column.trim().is_empty() {
                return Err(format!("colour rule {position}: no column to classify"));
            }
            let Some(value) = rule.value.clone() else {
                return Err(format!("colour rule {position}: no value to match"));
            };
            column_colors.push((index.to_string(), column, value));
            continue;
        }
        if rule.predicate.trim().is_empty() {
            return Err(format!("colour rule {position}: predicate is empty"));
        }
        let compiled = if TextSearch::is_polars(&rule.predicate) {
            let definition = compile_union_filter(ctx, &rule.predicate)
                .map_err(|error| format!("colour rule {position}: {error}"))?;
            TextSearch::parse(rule.predicate.clone(), Some(&definition))
        } else {
            TextSearch::parse(rule.predicate.clone(), None)
        }
        .map_err(|error| format!("colour rule {position}: {error}"))?;
        colors.push((index.to_string(), compiled));
    }
    Ok(PreparedUnionFilter {
        search_source: ctx.spec.filter.search.clone(),
        advanced_source: ctx.spec.filter.advanced_polars.clone(),
        search,
        advanced,
        grouping_source: ctx.spec.filter.grouping.clone(),
        grouping,
        color_rules_source: ctx.spec.color_rules.clone(),
        colors,
        column_colors,
    })
}

fn compile_union_filter(ctx: &UnionJobCtx, source: &str) -> Result<CompiledDefinition, String> {
    check_cancelled(&ctx.cancel, &ctx.shared)?;
    let (reply, result) = sync_channel(1);
    ctx.compiler_tx
        .try_send(super::Work::CompileUnionFilter {
            source: source.to_owned(),
            cancel: Arc::clone(&ctx.cancel),
            reply,
        })
        .map_err(|error| match error {
            TrySendError::Full(_) => {
                String::from("query worker is busy; union filter was not submitted")
            }
            TrySendError::Disconnected(_) => String::from("query worker is unavailable"),
        })?;
    loop {
        check_cancelled(&ctx.cancel, &ctx.shared)?;
        match result.recv_timeout(FREEZE_WAIT) {
            Ok(compiled) => return compiled,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                return Err("query worker dropped the union filter compilation".into());
            }
        }
    }
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
/// The submitted revision/generation is authoritative. The lease must be a
/// self-consistent snapshot of exactly that input definition or the candidate
/// is stale. Timestamp authority per record is read from
/// the input's published membership: filtered views contribute their basis
/// vector, raw views contribute capture nanos. Membership only ever appends
/// under a live tail, so every frozen row is still mapped; a visited record
/// missing from the membership map means the input moved non-monotonically
/// mid-freeze and the job aborts as stale — never a silent mix. Movement
/// after the freeze is the publication fence's business, not the visit's.
fn visit_union_input(
    ctx: &UnionJobCtx,
    input: &StoredUnionInput,
    frozen: FrozenInput,
    remaining_rows: &mut u64,
    remaining_bytes: &mut u64,
    reservation: &mut Reservation,
    carrier_bytes: &mut u64,
) -> Result<FrozenUnionWork, String> {
    let summary = frozen.summary().clone();
    if summary.applied_revision != input.accepted_revision
        || summary.applied_generation != input.applied_generation
    {
        return Err(format!(
            "union input '{}' is stale: submitted revision {} generation {}, current revision {} generation {}",
            input.view_id,
            input.accepted_revision,
            input.applied_generation,
            summary.applied_revision,
            summary.applied_generation
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
    let stats = frozen
        .visit_precise(&cancel, |batch| {
            if cancel.load(Ordering::Acquire) {
                return Err("union superseded".into());
            }
            for row in &batch.rows {
                // Precise replay records whole-value omissions instead of
                // failing on them. An omitted field is usually a structured
                // value with no native union encoding: letting it vanish
                // into nulls would silently change the column, so the
                // candidate fails explicitly naming field and native type.
                if let Some((field, reason)) = row
                    .omitted_fields
                    .iter()
                    .find(|(_, reason)| *reason != "field missing from source record")
                {
                    // Diagonal union semantics represent a field absent on
                    // one physical record as typed null. Precise replay still
                    // carries its column dtype in `field_types`; only lossy
                    // whole-value omissions reject here.
                    let dtype = row
                        .field_types
                        .get(field)
                        .cloned()
                        .unwrap_or_else(|| "unknown".into());
                    return Err(format!(
                        "union input '{}' field '{field}' omitted by precise replay (native {dtype}): {reason}",
                        input.view_id
                    ));
                }
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
                let raw_len = lossy_utf8_len(&row.record.bytes).map_err(|error| error.to_string())?;
                let retained_bytes = union_row_carrier_bytes(
                    raw_len,
                    row.record.bytes.len(),
                    &row.fields,
                    &row.field_types,
                )
                .map_err(|error| error.to_string())?;
                if !reservation.add(retained_bytes) {
                    return Err("union frozen carriers exceed the shared memory budget".into());
                }
                *carrier_bytes = carrier_bytes
                    .checked_add(retained_bytes)
                    .ok_or_else(|| "union carrier size overflow".to_owned())?;
                if let Some(probe) = &ctx.phase_test_probe {
                    probe.retained_rows.fetch_add(1, Ordering::AcqRel);
                }
                // Byte accounting uses the replay's own output figure, which
                // covers serialized fields and dtype evidence — not just the
                // raw record bytes. The visit itself was already capped by
                // the remaining budget through its input limits; the actual
                // is subtracted here so later inputs freeze under a truthful
                // remainder. A field-heavy input consumes its true share.
                rows.push(UnionFrozenRow {
                    record_id: row.record.record_id,
                    timestamp_nanos,
                    fields: row.fields.clone(),
                    field_types: row.field_types.clone(),
                    raw: String::from_utf8_lossy(&row.record.bytes).into_owned(),
                    raw_bytes: row.record.bytes.as_ref().to_vec(),
                    captured_at_unix_nanos: row.record.captured_at_unix_nanos,
                    stream: row.record.stream,
                    acquisition_id: *row.record.acquisition_id.as_bytes(),
                    chunk: row.record.chunk,
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
    *remaining_bytes = remaining_bytes.saturating_sub(stats.output_bytes);
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
        accepted_enrichment_outputs: summary.accepted_enrichment_outputs,
        rows,
    })
}

/// Current per-source fence for one input view: published membership state,
/// or live source progress for raw views. Read under the publication lock so
/// the comparison with frozen metadata is atomic with the install.
fn current_source_fence(shared: &Shared, view: &ViewState) -> Vec<super::union::UnionSourceFence> {
    current_source_fence_with_guarded(shared, view, &[])
}

fn current_source_fence_with_guarded(
    shared: &Shared,
    view: &ViewState,
    guarded: &[(SourceId, u64, Option<u64>)],
) -> Vec<super::union::UnionSourceFence> {
    match &view.published {
        Published::Filtered { membership } => membership
            .sources
            .iter()
            .map(|source| super::union::UnionSourceFence {
                source_id: source.source_id.clone(),
                generation: source.generation,
                high_watermark: source.high_watermark,
            })
            .collect(),
        Published::Raw => view
            .registration
            .sources
            .iter()
            .filter_map(|id| {
                let guarded_progress = guarded.iter().find(|(source_id, _, _)| source_id == id);
                let (generation, high_watermark) = match guarded_progress {
                    Some((_, generation, high_watermark)) => (*generation, *high_watermark),
                    None => {
                        let progress = shared.sources.get(id)?.handle.progress();
                        (
                            progress.generation,
                            progress.high_watermark.map(|record| record.sequence),
                        )
                    }
                };
                Some(super::union::UnionSourceFence {
                    source_id: id.0.to_string(),
                    generation,
                    high_watermark,
                })
            })
            .collect(),
    }
}

fn accepted_enrichment_outputs(inputs: &[FrozenUnionMeta]) -> Vec<String> {
    let mut outputs = inputs
        .iter()
        .flat_map(|input| input.accepted_enrichment_outputs.iter().cloned())
        .collect::<Vec<_>>();
    outputs.sort();
    outputs.dedup();
    outputs
}

fn input_accepts_output(inputs: &[FrozenUnionMeta], input: usize, output: &str) -> bool {
    inputs.get(input).is_some_and(|input| {
        input
            .accepted_enrichment_outputs
            .iter()
            .any(|accepted| accepted == output)
    })
}

/// Remove same-named raw fields from inputs that do not structurally accept
/// an output before Polars constructs the diagonal union. This is authority
/// projection, not data loss: `raw`, `raw_bytes` and capture identity remain
/// untouched for display/export context. Accepted inputs retain their exact
/// dtype evidence, so genuine accepted-vs-accepted type conflicts still fail
/// in the native concat instead of being coerced or hidden.
fn project_accepted_output_authority(
    inputs: &mut [UnionFrozenInput],
    metas: &[FrozenUnionMeta],
    outputs: &[String],
) {
    for (position, input) in inputs.iter_mut().enumerate() {
        for output in outputs {
            if input_accepts_output(metas, position, output) {
                continue;
            }
            for row in &mut input.rows {
                row.fields.remove(output);
                row.field_types.remove(output);
            }
        }
    }
}

fn accepted_output_authority(
    inputs: &[FrozenUnionMeta],
    output: &str,
) -> Result<Option<polars::prelude::Expr>, String> {
    use polars::prelude::{col, lit};
    let mut authority: Option<polars::prelude::Expr> = None;
    for (position, input) in inputs.iter().enumerate() {
        if !input
            .accepted_enrichment_outputs
            .iter()
            .any(|accepted| accepted == output)
        {
            continue;
        }
        let position = u32::try_from(position)
            .map_err(|_| "union input position exceeds UInt32 provenance".to_owned())?;
        let owns = col(INPUT_COLUMN).eq(lit(position));
        authority = Some(authority.map_or(owns.clone(), |prior| prior.or(owns)));
    }
    Ok(authority)
}

fn accepted_output_authorities(
    inputs: &[FrozenUnionMeta],
) -> Result<Vec<(String, polars::prelude::Expr)>, String> {
    accepted_enrichment_outputs(inputs)
        .into_iter()
        .map(|output| {
            let authority = accepted_output_authority(inputs, &output)?
                .ok_or_else(|| format!("accepted output {output:?} has no input provenance"))?;
            Ok((output, authority))
        })
        .collect()
}

fn union_input_positions(frame: &polars::prelude::DataFrame) -> Result<Vec<usize>, String> {
    let inputs = frame
        .column(INPUT_COLUMN)
        .map_err(|_| "union frame has no input provenance".to_owned())?;
    (0..frame.height())
        .map(|index| {
            inputs
                .get(index)
                .map_err(|error| error.to_string())?
                .try_extract::<u32>()
                .map(|position| position as usize)
                .map_err(|error| error.to_string())
        })
        .collect()
}

fn union_color_workspace_bytes(rows: u64, rules: &[lvu::ColorRule]) -> Result<u64, String> {
    if rules.is_empty() {
        return Ok(0);
    }
    // The engine currently materialises one complete StableRecordId vector
    // per rule. Reserve all of them, plus the selected-ID vector, publication
    // membership set and final first-match map, before entering native
    // evaluation. MAX_COLOR_RULES bounds the multiplier at sixteen.
    let identity_copies = u64::try_from(rules.len())
        .ok()
        .and_then(|rules| rules.checked_add(3))
        .and_then(|copies| copies.checked_mul(96))
        .and_then(|per_row| per_row.checked_mul(rows))
        .ok_or_else(|| "union colour match size overflow".to_owned())?;
    let strings = rules.iter().try_fold(0u64, |total, rule| {
        let bytes = rule
            .predicate
            .len()
            .saturating_add(rule.column.as_ref().map_or(0, String::len))
            .saturating_add(rule.value.as_ref().map_or(0, String::len))
            .saturating_add(rule.color.label().len())
            .saturating_add(192);
        total.checked_add(bytes as u64)
    });
    identity_copies
        .checked_add(strings.ok_or_else(|| "union colour rule size overflow".to_owned())?)
        .ok_or_else(|| "union colour workspace size overflow".to_owned())
}

fn json_workspace_bytes(value: &serde_json::Value) -> Result<u64, String> {
    match value {
        serde_json::Value::Null => Ok(4),
        serde_json::Value::Bool(_) => Ok(5),
        serde_json::Value::Number(number) => Ok(number.to_string().len() as u64),
        // Six bytes per input byte safely covers JSON control escapes without
        // allocating a serialized copy merely to measure it.
        serde_json::Value::String(text) => (text.len() as u64)
            .checked_mul(6)
            .and_then(|bytes| bytes.checked_add(2))
            .ok_or_else(|| "union derived value size overflow".to_owned()),
        serde_json::Value::Array(values) => values.iter().try_fold(2u64, |bytes, value| {
            bytes
                .checked_add(1)
                .and_then(|bytes| bytes.checked_add(json_workspace_bytes(value).ok()?))
                .ok_or_else(|| "union derived value size overflow".to_owned())
        }),
        serde_json::Value::Object(values) => values.iter().try_fold(2u64, |bytes, (key, value)| {
            let key_bytes = (key.len() as u64)
                .checked_mul(6)
                .and_then(|bytes| bytes.checked_add(4))
                .ok_or_else(|| "union derived value size overflow".to_owned())?;
            bytes
                .checked_add(key_bytes)
                .and_then(|bytes| bytes.checked_add(json_workspace_bytes(value).ok()?))
                .ok_or_else(|| "union derived value size overflow".to_owned())
        }),
    }
}

fn union_derived_workspace_bytes(
    inputs: &[UnionFrozenInput],
    metas: &[FrozenUnionMeta],
) -> Result<u64, String> {
    if metas
        .iter()
        .all(|input| input.accepted_enrichment_outputs.is_empty())
    {
        return Ok(0);
    }
    let mut bytes = 0u64;
    let mut rows = 0u64;
    for (input, meta) in inputs.iter().zip(metas) {
        rows = rows
            .checked_add(input.rows.len() as u64)
            .ok_or_else(|| "union derived row count overflow".to_owned())?;
        for row in &input.rows {
            for output in &meta.accepted_enrichment_outputs {
                let Some(value) = row.fields.get(output) else {
                    continue;
                };
                let dtype = row.field_types.get(output).map_or(0, String::len);
                // This only sizes the frozen carrier. `decode_union_field`
                // remains the single semantic authority for exact scalars,
                // including lossless integer wrappers and nested rejection.
                let scalar = json_workspace_bytes(value)?;
                let entry = 36u64
                    .checked_add((output.len().saturating_mul(2)) as u64)
                    .and_then(|size| size.checked_add(dtype as u64))
                    .and_then(|size| size.checked_add(scalar))
                    .and_then(|size| size.checked_add(512 + 160))
                    .ok_or_else(|| "union derived projection size overflow".to_owned())?;
                bytes = bytes
                    .checked_add(entry)
                    .ok_or_else(|| "union derived projection size overflow".to_owned())?;
            }
        }
    }
    // One temporary scalar projection and the per-input winner indexes exist
    // beside the retained display and precise maps.
    bytes
        .checked_add(
            rows.checked_mul(704)
                .ok_or_else(|| "union derived workspace size overflow".to_owned())?,
        )
        .ok_or_else(|| "union derived workspace size overflow".to_owned())
}

type FrozenDerived = HashMap<(String, u64, String), (serde_json::Value, String)>;

fn union_derived_projection(
    frame: &polars::prelude::DataFrame,
    inputs: &[UnionFrozenInput],
    metas: &[FrozenUnionMeta],
    outputs: &[String],
    matched_ids: &[lvu_query::StableRecordId],
) -> Result<
    (
        HashMap<(String, u64, String), Option<String>>,
        FrozenDerived,
        u64,
    ),
    String,
> {
    if outputs.is_empty() {
        return Ok((HashMap::new(), HashMap::new(), 0));
    }
    let positions = union_input_positions(frame)?;
    let selected: HashSet<(&str, u64)> = matched_ids
        .iter()
        .map(|id| (id.source_id.as_str(), id.sequence))
        .collect();
    let indexes = inputs
        .iter()
        .map(|input| {
            input
                .rows
                .iter()
                .map(|row| {
                    (
                        (
                            row.record_id.source_id.0.to_string(),
                            row.record_id.sequence,
                        ),
                        row,
                    )
                })
                .collect::<HashMap<_, _>>()
        })
        .collect::<Vec<_>>();
    let mut display = HashMap::new();
    let mut precise = HashMap::new();
    let mut bytes = 0u64;
    for output in outputs {
        let projection = scalar_projection(frame, output, 512)?;
        for ((id, value), input) in projection.into_iter().zip(&positions) {
            if !selected.contains(&(id.source_id.as_str(), id.sequence))
                || !input_accepts_output(metas, *input, output)
            {
                continue;
            }
            let Some(row) = indexes
                .get(*input)
                .and_then(|index| index.get(&(id.source_id.clone(), id.sequence)))
            else {
                return Err("union derived projection lost winning input provenance".into());
            };
            let (Some(typed), Some(dtype)) = (row.fields.get(output), row.field_types.get(output))
            else {
                continue;
            };
            let key = (id.source_id, id.sequence, output.clone());
            bytes = bytes
                .checked_add(
                    key.0.len() as u64
                        + key.2.len() as u64 * 2
                        + value.as_ref().map_or(1, String::len) as u64
                        + dtype.len() as u64
                        + json_workspace_bytes(typed)?
                        + 160,
                )
                .ok_or_else(|| "union derived publication size overflow".to_owned())?;
            display.insert(key.clone(), value);
            precise.insert(key, (typed.clone(), dtype.clone()));
        }
    }
    Ok((display, precise, bytes))
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum UnionGroupingFlag {
    Start,
    Continue,
    Key(Vec<u8>),
    KeyNull,
    KeyNan,
    KeyOversize,
}

type UnionGroupingFlags = HashMap<(String, u64), UnionGroupingFlag>;

fn union_configured_grouping_flags(
    frame: &polars::prelude::DataFrame,
    rule: &ContinuationRule,
    inputs: &[FrozenUnionMeta],
) -> Result<Option<UnionGroupingFlags>, String> {
    let Some(column) = rule.configured_column() else {
        return Ok(None);
    };
    if accepted_output_authority(inputs, column)?.is_none() {
        return Err(format!(
            "grouping column {column:?} is not an accepted enrichment output; add it in Enrichment first"
        ));
    }
    let positions = union_input_positions(frame)?;
    let mut flags = HashMap::with_capacity(frame.height());
    match rule {
        ContinuationRule::Filter { .. } => {
            for ((id, starts), input) in non_null_flags(frame, column)?.into_iter().zip(&positions)
            {
                let starts = starts && input_accepts_output(inputs, *input, column);
                flags.insert(
                    (id.source_id, id.sequence),
                    if starts {
                        UnionGroupingFlag::Start
                    } else {
                        UnionGroupingFlag::Continue
                    },
                );
            }
        }
        ContinuationRule::Run { .. } => {
            for ((id, mut key), input) in exact_key_flags(frame, column)
                .map_err(|error| match error {
                    lvu_query::KeyError::Unavailable(message)
                    | lvu_query::KeyError::Unsupported(message) => message,
                })?
                .into_iter()
                .zip(&positions)
            {
                if !input_accepts_output(inputs, *input, column) {
                    key = KeyFlag::Null;
                }
                flags.insert(
                    (id.source_id, id.sequence),
                    match key {
                        KeyFlag::Value(key) => UnionGroupingFlag::Key(key),
                        KeyFlag::Null => UnionGroupingFlag::KeyNull,
                        KeyFlag::Nan => UnionGroupingFlag::KeyNan,
                        KeyFlag::Oversize => UnionGroupingFlag::KeyOversize,
                    },
                );
            }
        }
        ContinuationRule::Auto | ContinuationRule::Custom(_) => unreachable!("checked above"),
    }
    if flags.len() != frame.height() {
        return Err("grouping flags do not cover the merged union frame".into());
    }
    Ok(Some(flags))
}

fn union_grouping_index(inputs: &[UnionFrozenInput]) -> HashMap<RecordId, &UnionFrozenRow> {
    let rows = inputs.iter().map(|input| input.rows.len()).sum();
    let mut indexed = HashMap::with_capacity(rows);
    for input in inputs {
        for row in &input.rows {
            // Dedup semantics are first-input precedence. The grouping
            // projection must resolve the same winning row when an identity
            // appears in more than one accepted input.
            indexed.entry(row.record_id).or_insert(row);
        }
    }
    indexed
}

fn union_groups(
    source_id: SourceId,
    members: &[(u64, Option<i64>, i64)],
    indexed_rows: &HashMap<RecordId, &UnionFrozenRow>,
    rule: &ContinuationRule,
    configured_flags: Option<&UnionGroupingFlags>,
) -> Result<(Appended<GroupRange>, u64, usize), String> {
    let mut built = Vec::<GroupRange>::new();
    let mut charged = 0u64;
    let mut lookups = 0usize;
    for (position, (sequence, _, _)) in members.iter().enumerate() {
        lookups = lookups.saturating_add(1);
        let record_id = RecordId {
            source_id,
            sequence: *sequence,
        };
        let row = indexed_rows
            .get(&record_id)
            .copied()
            .ok_or_else(|| format!("union grouping lost record {}:{sequence}", source_id.0))?;
        let display_bytes = &row.raw_bytes[..row.raw_bytes.len().min(MAX_GROUP_LINE_DISPLAY_BYTES)];
        let projection = lvu::DisplayRow {
            id: lvu::RowId::new(source_id.0.to_string(), *sequence),
            timestamp: String::new(),
            captured_at_unix_nanos: Some(row.captured_at_unix_nanos),
            level: String::new(),
            text: String::from_utf8_lossy(display_bytes).into_owned(),
            details: Vec::new(),
            fields: Vec::new(),
        };
        if rule.is_configured() {
            // The native engine computed every configured flag from the
            // actual merged typed column. This loop only segments stable
            // identities for presentation; it never evaluates field values.
            let flag = configured_flags
                .and_then(|flags| flags.get(&(source_id.0.to_string(), *sequence)))
                .ok_or_else(|| {
                    format!(
                        "union grouping has no native flag for {}:{sequence}",
                        source_id.0
                    )
                })?;
            let previous_sequence = position
                .checked_sub(1)
                .and_then(|index| members.get(index))
                .map(|member| member.0);
            let physically_adjacent =
                previous_sequence.and_then(|previous| previous.checked_add(1)) == Some(*sequence);
            let same_stream = built.last().is_some_and(|group| group.stream == row.stream);
            let same_acquisition = built
                .last()
                .is_some_and(|group| group.acquisition_id == row.acquisition_id);
            let chunk_join = built.last().is_some_and(|group| {
                group.partial_open
                    && group.acquisition_id == row.acquisition_id
                    && matches!(
                        (group.last_chunk, row.chunk),
                        (
                            lvu_core::ChunkPosition::Start | lvu_core::ChunkPosition::Continue,
                            lvu_core::ChunkPosition::Continue | lvu_core::ChunkPosition::End
                        )
                    )
            }) && physically_adjacent
                && same_stream;
            let join_open = !chunk_join
                && physically_adjacent
                && same_stream
                && same_acquisition
                && built
                    .last()
                    .is_some_and(|group| !group.orphan && !group.pending && !group.split)
                && match (flag, rule) {
                    (UnionGroupingFlag::Start, ContinuationRule::Filter { .. }) => false,
                    (UnionGroupingFlag::Continue, ContinuationRule::Filter { .. }) => true,
                    (UnionGroupingFlag::Key(key), ContinuationRule::Run { .. }) => built
                        .last()
                        .is_some_and(|group| group.run_key.as_ref() == Some(key)),
                    _ => false,
                };
            if chunk_join || join_open {
                let group = built.last_mut().expect("joinable configured group");
                group.len += 1;
                if !chunk_join {
                    group.logical_lines = group.logical_lines.saturating_add(1);
                }
                group.payload_bytes = group.payload_bytes.saturating_add(row.raw_bytes.len());
                group.last_chunk = row.chunk;
                group.last_capture_nanos = row.captured_at_unix_nanos;
                if matches!(row.chunk, lvu_core::ChunkPosition::Start) {
                    group.partial_open = true;
                }
                if matches!(row.chunk, lvu_core::ChunkPosition::End) {
                    group.partial_open = false;
                }
                if group.projection.len() < MAX_CONFIGURED_GROUP_STORED {
                    Arc::make_mut(&mut group.projection).push(projection);
                }
            } else {
                let (orphan, key_refused, run_key) = match flag {
                    UnionGroupingFlag::Start => (false, false, None),
                    UnionGroupingFlag::Key(key) => (false, false, Some(key.clone())),
                    UnionGroupingFlag::KeyNull | UnionGroupingFlag::KeyNan => (false, false, None),
                    UnionGroupingFlag::KeyOversize => (false, true, None),
                    UnionGroupingFlag::Continue => (true, false, None),
                };
                built.push(GroupRange {
                    start: position,
                    len: 1,
                    logical_lines: 1,
                    payload_bytes: row.raw_bytes.len(),
                    stream: row.stream,
                    orphan,
                    split: false,
                    oversized: false,
                    pending: false,
                    run_key,
                    configured: true,
                    key_refused,
                    auto_open: false,
                    auto_structured: false,
                    auto_structure_depth: 0,
                    partial_open: matches!(row.chunk, lvu_core::ChunkPosition::Start),
                    partial_truncated: false,
                    structure_truncated: false,
                    partial_prefix: Vec::new(),
                    structure_prefix: Vec::new(),
                    acquisition_id: row.acquisition_id,
                    last_chunk: row.chunk,
                    first_capture_nanos: row.captured_at_unix_nanos,
                    last_capture_nanos: row.captured_at_unix_nanos,
                    projection: Arc::new(vec![projection]),
                });
            }
            continue;
        }
        let previous_sequence = position
            .checked_sub(1)
            .and_then(|index| members.get(index))
            .map(|member| member.0);
        let physically_adjacent =
            previous_sequence.and_then(|previous| previous.checked_add(1)) == Some(*sequence);
        let auto_open = built.last().is_some_and(|group| group.auto_open);
        let auto_line = rule.auto_line(&row.raw_bytes, auto_open);
        let continuation = rule.custom_matches(&row.raw_bytes).unwrap_or_else(|| {
            matches!(
                row.chunk,
                lvu_core::ChunkPosition::Continue | lvu_core::ChunkPosition::End
            ) || auto_line == Some(AutoLine::Continuation)
        });
        let can_extend = built.last().is_some_and(|group| {
            continuation
                && physically_adjacent
                && group.stream == row.stream
                && (!rule.is_auto() || group.acquisition_id == row.acquisition_id)
                && (!rule.is_auto()
                    || auto_group_within_span(
                        group.first_capture_nanos,
                        group.last_capture_nanos,
                        row.captured_at_unix_nanos,
                    ))
                && group.auto_open
                && group.len < MAX_GROUP_LINES
                && group.payload_bytes.saturating_add(row.raw_bytes.len())
                    <= MAX_GROUP_PAYLOAD_BYTES
        });
        if can_extend {
            let group = built.last_mut().expect("checked");
            group.len += 1;
            group.logical_lines = group.logical_lines.saturating_add(1);
            group.payload_bytes = group.payload_bytes.saturating_add(row.raw_bytes.len());
            group.last_chunk = row.chunk;
            group.last_capture_nanos = row.captured_at_unix_nanos;
            group.auto_open = rule.is_auto()
                && !matches!(auto_line, Some(AutoLine::Ambiguous))
                && !matches!(row.chunk, lvu_core::ChunkPosition::End);
            Arc::make_mut(&mut group.projection).push(projection);
        } else {
            let credible_start =
                rule.custom_matches(&row.raw_bytes).is_some() || auto_line == Some(AutoLine::Start);
            built.push(GroupRange {
                start: position,
                len: 1,
                logical_lines: 1,
                payload_bytes: row.raw_bytes.len(),
                stream: row.stream,
                orphan: continuation,
                split: false,
                oversized: row.raw_bytes.len() > MAX_GROUP_PAYLOAD_BYTES,
                pending: false,
                run_key: None,
                configured: false,
                key_refused: false,
                auto_open: !rule.is_auto() || credible_start,
                auto_structured: false,
                auto_structure_depth: 0,
                partial_open: matches!(row.chunk, lvu_core::ChunkPosition::Start),
                partial_truncated: false,
                structure_truncated: false,
                partial_prefix: Vec::new(),
                structure_prefix: Vec::new(),
                acquisition_id: row.acquisition_id,
                last_chunk: row.chunk,
                first_capture_nanos: row.captured_at_unix_nanos,
                last_capture_nanos: row.captured_at_unix_nanos,
                projection: Arc::new(vec![projection]),
            });
        }
    }
    for group in &built {
        charged = charged
            .checked_add(group_state_bytes(group))
            .and_then(|bytes| {
                group.projection.iter().try_fold(bytes, |subtotal, row| {
                    subtotal.checked_add(display_projection_bytes(row))
                })
            })
            .ok_or_else(|| "union grouping size overflow".to_owned())?;
    }
    let mut groups = Appended::default();
    groups.extend(built);
    Ok((groups, charged, lookups))
}

/// Merge result publication: fence re-verification and membership install
/// happen atomically under one lock, so no input can advance between the
/// check and the install. Any failure leaves the prior union untouched.
#[allow(clippy::too_many_arguments)] // One owned phase result; bundling would only hide the fence.
fn publish_union(
    ctx: &UnionJobCtx,
    fence: &[StoredUnionInput],
    frozen_inputs: Vec<FrozenUnionMeta>,
    merged: polars::prelude::DataFrame,
    matched_ids: Vec<lvu_query::StableRecordId>,
    engine_color_matches: BTreeMap<String, Vec<lvu_query::StableRecordId>>,
    decoded_inputs: Vec<UnionFrozenInput>,
    accepted_enrichment_outputs: Vec<String>,
    derived: HashMap<(String, u64, String), Option<String>>,
    precise_derived: FrozenDerived,
    derived_bytes: u64,
    prepared_filter: PreparedUnionFilter,
    mut reservation: Reservation,
) -> Result<(), String> {
    use super::union::{SEQUENCE_COLUMN, SOURCE_ID_COLUMN, UNION_TS_COLUMN};
    use polars::prelude::AnyValue;
    let height = matched_ids.len();
    let configured_grouping_flags = prepared_filter
        .grouping
        .as_ref()
        .map(|rule| union_configured_grouping_flags(&merged, rule, &frozen_inputs))
        .transpose()?
        .flatten();
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
    // A raw provider is registered over one fixed source set. Never publish a
    // count/order containing identities that provider cannot resolve. A moved
    // input source set requires a newly registered union definition; this
    // candidate rejects and preserves the previous membership.
    let (registered_order, registered_sources) = {
        let shared = ctx.shared.lock().expect("view state poisoned");
        let view = shared
            .views
            .get(&ctx.spec.union_view_id)
            .ok_or_else(|| "unknown union view".to_owned())?;
        (
            view.registration.sources.clone(),
            view.registration
                .sources
                .iter()
                .copied()
                .collect::<HashSet<_>>(),
        )
    };
    let frozen_sources = frozen_inputs
        .iter()
        .flat_map(|input| input.source_meta.iter().map(|(source, _, _)| *source))
        .collect::<HashSet<_>>();
    if frozen_sources != registered_sources {
        return Err(SOURCE_SET_MOVED.into());
    }
    for matched in &matched_ids {
        if !registered_sources
            .iter()
            .any(|source| source.0.to_string() == matched.source_id)
        {
            return Err(format!(
                "union engine returned an unregistered source identity {}",
                matched.source_id
            ));
        }
    }
    // Display rank per identity, in engine output order. This transient map
    // is decode workspace bounded by the already-budgeted row count; the
    // durable publication below is reserved separately before it is built.
    let mut per_source: HashMap<String, Vec<(u64, Option<i64>, i64)>> = HashMap::new();
    let mut next_match = matched_ids.iter().peekable();
    let mut survivor_rank = 0i64;
    for index in 0..merged.height() {
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
        let is_match = next_match
            .peek()
            .is_some_and(|matched| matched.source_id == source && matched.sequence == sequence);
        if !is_match {
            continue;
        }
        next_match.next();
        let time = match times.get(index).map_err(|error| error.to_string())? {
            AnyValue::Null => None,
            value => Some(
                value
                    .try_extract::<i64>()
                    .map_err(|error| error.to_string())?,
            ),
        };
        let rank = survivor_rank;
        survivor_rank = survivor_rank
            .checked_add(1)
            .ok_or_else(|| "union survivor rank overflow".to_owned())?;
        per_source
            .entry(source)
            .or_default()
            .push((sequence, time, rank));
    }
    if let Some(unconsumed) = next_match.next() {
        return Err(format!(
            "union engine returned an unknown or out-of-order identity {}:{}",
            unconsumed.source_id, unconsumed.sequence
        ));
    }
    // Charge the publication like any membership, plus rank keys and map
    // overhead: per-row identity/time/rank plus per-source overhead. The
    // reservation precedes every publication allocation below; over budget
    // fails as limited with the prior union preserved.
    let source_count = registered_order.len();
    let mut publication_bytes = (height as u64)
        .checked_mul(
            SEQUENCE_BYTES
                .checked_mul(3)
                .ok_or_else(|| "union membership size overflow".to_owned())?,
        )
        .and_then(|bytes| {
            (source_count as u64)
                .checked_mul(SOURCE_OVERHEAD.checked_add(64)?)
                .and_then(|overhead| bytes.checked_add(overhead))
        })
        .ok_or_else(|| "union membership size overflow".to_owned())?;
    if publication_bytes > reservation.bytes {
        return Err("union membership exceeds the memory budget".into());
    }
    publication_bytes = publication_bytes
        .checked_add(derived_bytes)
        .ok_or_else(|| "union derived publication size overflow".to_owned())?;
    let matched: HashSet<(&str, u64)> = matched_ids
        .iter()
        .map(|id| (id.source_id.as_str(), id.sequence))
        .collect();
    let mut color_matches: HashMap<(String, u64), u16> = HashMap::new();
    for (name, ids) in engine_color_matches {
        let Ok(index) = name.parse::<u16>() else {
            continue;
        };
        for id in ids {
            if !matched.contains(&(id.source_id.as_str(), id.sequence)) {
                continue;
            }
            let key = (id.source_id, id.sequence);
            match color_matches.entry(key) {
                std::collections::hash_map::Entry::Occupied(mut entry) => {
                    *entry.get_mut() = (*entry.get()).min(index);
                }
                std::collections::hash_map::Entry::Vacant(entry) => {
                    publication_bytes = publication_bytes
                        .checked_add(color_match_bytes(&entry.key().0))
                        .ok_or_else(|| "union colour match size overflow".to_owned())?;
                    entry.insert(index);
                }
            }
        }
    }
    let union_basis = frozen_inputs
        .first()
        .map(|work| work.basis)
        .unwrap_or_default();
    // Built once after the global reservation and before grouping. Every
    // constituent is then one stable-identity hash lookup; no input/row scan
    // remains in the per-member loop.
    let grouping_index = prepared_filter.grouping.as_ref().map(|_| {
        let indexed = union_grouping_index(&decoded_inputs);
        if let Some(probe) = &ctx.phase_test_probe {
            probe
                .grouping_indexed_rows
                .store(indexed.len(), Ordering::Release);
        }
        indexed
    });
    // Mixed input bases compare as nanos against nanos; coherent unions keep
    // coherent input bases (normalization is the inputs' own enrichment, like
    // schema). The publication records the first input's basis honestly
    // rather than inventing one.
    let mut sources_out = Vec::with_capacity(source_count);
    let mut high_watermarks = Vec::with_capacity(source_count);
    let mut event_time_missing = 0usize;
    for source_id in registered_order {
        let key = source_id.0.to_string();
        let mut members = per_source.remove(&key).unwrap_or_default();
        members.sort_by_key(|member| member.0);
        let groups = match prepared_filter.grouping.as_ref() {
            Some(rule) => {
                let (groups, bytes, lookups) = union_groups(
                    source_id,
                    &members,
                    grouping_index.as_ref().expect("built for grouping"),
                    rule,
                    configured_grouping_flags.as_ref(),
                )?;
                debug_assert_eq!(lookups, members.len());
                if let Some(probe) = &ctx.phase_test_probe {
                    probe.grouping_lookups.fetch_add(lookups, Ordering::AcqRel);
                }
                publication_bytes = publication_bytes
                    .checked_add(bytes)
                    .ok_or_else(|| "union grouping size overflow".to_owned())?;
                groups
            }
            None => Appended::default(),
        };
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
            groups,
            bounds,
            merge_keys: keys_out,
            ascending,
        });
    }
    let count = height as u64;
    let grouped = prepared_filter.grouping.is_some();
    let (order, result_ranks, max_key) = merge_order(&sources_out, grouped, true, None);
    if !reservation.retain(publication_bytes) {
        return Err("union membership exceeds the reserved workspace".into());
    }
    let bytes = reservation.bytes;
    reservation.committed = true;
    let membership = Membership {
        sources: sources_out,
        count,
        bytes,
        budget: Arc::clone(&ctx.budget),
        enrichment_names: accepted_enrichment_outputs,
        derived,
        derived_errors: HashSet::new(),
        frozen_derived: Some(precise_derived),
        color_matches,
        color_rules: prepared_filter.color_rules_source.clone(),
        advanced: None,
        enrichment: Vec::new(),
        evaluation_page_bytes: ctx.page_bytes,
        evaluation_batches: Vec::new().into(),
        event_time_missing,
        event_time_invalid: 0,
        basis: union_basis,
        grouped,
        order,
        ranks: result_ranks,
        max_key,
    };
    let frozen_fences = fence
        .iter()
        .zip(frozen_inputs.iter())
        .map(|(input, work)| {
            (
                input.view_id.clone(),
                work.source_meta
                    .iter()
                    .map(|(id, generation, high)| super::union::UnionSourceFence {
                        source_id: id.0.to_string(),
                        generation: *generation,
                        high_watermark: *high,
                    })
                    .collect::<Vec<_>>(),
            )
        })
        .collect::<Vec<_>>();
    // Pin only raw source progress publications during the short final fence
    // check and install. All allocations above completed before these guards;
    // filtered input publications use `shared` itself.
    let mut raw_handles = {
        let shared = ctx.shared.lock().expect("view state poisoned");
        let mut handles = Vec::new();
        for input in fence {
            let Some(view) = shared.views.get(&input.view_id) else {
                return Err(format!("union input view '{}' is unknown", input.view_id));
            };
            if matches!(view.published, Published::Raw) {
                for source_id in &view.registration.sources {
                    if handles.iter().all(|(existing, _)| existing != source_id) {
                        let handle = shared
                            .sources
                            .get(source_id)
                            .ok_or_else(|| "union input source is no longer open".to_owned())?
                            .handle
                            .clone();
                        handles.push((*source_id, handle));
                    }
                }
            }
        }
        handles
    };
    raw_handles.sort_by_key(|(source_id, _)| source_id.0);
    let mut raw_guards = Vec::with_capacity(raw_handles.len());
    let mut guarded_progress = Vec::with_capacity(raw_handles.len());
    for (source_id, handle) in &raw_handles {
        let guard = handle.lock_progress();
        guarded_progress.push((
            *source_id,
            guard.generation(),
            guard.high_watermark().map(|record| record.sequence),
        ));
        raw_guards.push(guard);
    }
    if let Some(barrier) = &ctx.publish_test_barrier {
        let _ = barrier.checked.send(());
        loop {
            if ctx.cancel.load(Ordering::Acquire) {
                return Err("union superseded".into());
            }
            match barrier.release.recv_timeout(FREEZE_WAIT) {
                Ok(()) => break,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    return Err("union publication test barrier is gone".into());
                }
            }
        }
    }
    let mut shared = ctx.shared.lock().expect("view state poisoned");
    let Some(state) = shared.union_views.get(&ctx.spec.union_view_id) else {
        return Err("unknown union view".into());
    };
    if state.generation != ctx.generation || ctx.cancel.load(Ordering::Acquire) {
        return Err("union superseded".into());
    }
    for input in fence {
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
        let current = current_source_fence_with_guarded(&shared, view, &guarded_progress);
        let frozen = frozen_fences
            .iter()
            .find_map(|(view_id, fences)| (view_id == &input.view_id).then_some(fences))
            .ok_or_else(|| format!("union input '{}' has no frozen fence", input.view_id))?;
        super::union::verify_source_fence(&input.view_id, frozen, &current)
            .map_err(|error| error.to_string())?;
    }
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
    state.inputs = fence.to_vec();
    state.pending = None;
    state.published_revision = ctx.spec.union_revision;
    state.published_generation = ctx.spec.generation;
    state.prepared_filter = Some(prepared_filter);
    state.published_filter = ctx.spec.filter.clone();
    state.published_color_rules = ctx.spec.color_rules.clone();
    state.rejected_attempt = None;
    state.retry_attempt = None;
    state.retry_not_before = None;
    state.retry_delay = UNION_RETRY_INITIAL;
    state.published_source_fences = fence
        .iter()
        .zip(frozen_inputs.iter())
        .map(|(input, frozen)| {
            (
                input.view_id.clone(),
                frozen
                    .source_meta
                    .iter()
                    .map(
                        |(id, generation, high_watermark)| super::union::UnionSourceFence {
                            source_id: id.0.to_string(),
                            generation: *generation,
                            high_watermark: *high_watermark,
                        },
                    )
                    .collect(),
            )
        })
        .collect();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{NativeViewAdapter, ViewConfig};
    use lvu_core::{Acquisition, SourceDefinition};
    use lvu_ingest::{RuntimeConfig, SourceManager};
    use lvu_live::{LiveConfig, LiveRowProvider};
    use std::time::Duration;
    use tempfile::TempDir;

    /// Byte accounting charges the replay's serialized output — fields plus
    /// dtype evidence — never the raw record bytes alone. A field-heavy input
    /// with tiny raw lines must consume its true share, or later inputs would
    /// freeze under a remainder the first input already spent.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn visit_accounts_serialized_output_not_raw_bytes() {
        let root = TempDir::new().unwrap();
        let path = root.path().join("fat.log");
        std::fs::write(
            &path,
            format!("{{\"k\":\"{}\"}}\n{{\"k\":\"b\"}}\n", "x".repeat(5000)),
        )
        .unwrap();
        let manager =
            SourceManager::new(root.path().join("capture"), RuntimeConfig::default()).unwrap();
        let handle = manager
            .start(SourceDefinition {
                schema_version: 1,
                id: SourceId::new(),
                name: "fat".into(),
                acquisition: Acquisition::File { path, follow: true },
                identity_hints: BTreeMap::new(),
                retention: None,
            })
            .await
            .unwrap();
        let mut progress = handle.subscribe();
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if progress.borrow().records >= 2 {
                    break;
                }
                progress.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
        let live = LiveConfig::new(root.path().join("raw-index"));
        let raw = Arc::new(LiveRowProvider::new(live).unwrap());
        let view = ViewConfig::new(root.path().join("view-index"));
        let mut adapter = NativeViewAdapter::new(raw, view).unwrap();
        adapter.register_source(handle.clone()).unwrap();
        adapter
            .register_view("v", vec![handle.source_id()])
            .unwrap();
        let frozen = adapter
            .freeze_input("v", FrozenInputLimits::default())
            .unwrap();
        let (cmd_tx, _cmd_rx) = sync_channel(8);
        let (_frozen_tx, frozen_rx) = sync_channel(1);
        let ctx = UnionJobCtx {
            spec: UnionCandidateSpec {
                union_view_id: "u".into(),
                union_revision: 1,
                generation: 1,
                inputs: vec![StoredUnionInput {
                    view_id: "v".into(),
                    // Fresh raw view: adapter revision and generation are both
                    // still zero, matching the frozen summary exactly.
                    accepted_revision: 0,
                    applied_generation: 0,
                }],
                filter: Default::default(),
                color_rules: Vec::new(),
            },
            generation: 1,
            cancel: Arc::new(AtomicBool::new(false)),
            shared: Arc::clone(&adapter.shared),
            page_bytes: adapter.config.page_bytes,
            budget: Arc::clone(&adapter.budget),
            cmd_tx,
            frozen_rx,
            compiler_tx: adapter.work.as_ref().expect("worker").clone(),
            limits: UnionLimits::default(),
            test_barrier: None,
            publish_test_barrier: None,
            phase_test_probe: None,
            transient_test_failure: None,
            dependency_attempt: None,
        };
        let mut remaining_rows = 1_000_000u64;
        let mut remaining_bytes = 1_000_000u64;
        let mut reservation = Reservation::new(Arc::clone(&adapter.budget));
        let mut carrier_bytes = 0;
        let input = ctx.spec.inputs[0].clone();
        let work = visit_union_input(
            &ctx,
            &input,
            frozen,
            &mut remaining_rows,
            &mut remaining_bytes,
            &mut reservation,
            &mut carrier_bytes,
        )
        .expect("visit succeeds");
        assert_eq!(work.rows.len(), 2);
        let raw_total: u64 = work
            .rows
            .iter()
            .map(|row| u64::try_from(row.raw.len()).unwrap_or(u64::MAX))
            .sum();
        let consumed = 1_000_000u64 - remaining_bytes;
        // The 5KB field value dominates: serialized output far exceeds raw
        // line bytes, and the remainder reflects the output figure exactly.
        assert!(
            consumed > raw_total,
            "consumed {consumed} must exceed raw {raw_total}"
        );
        let expect = 1_000_000u64
            - work
                .rows
                .iter()
                .map(|row| {
                    serde_json::to_vec(&row.fields)
                        .map(|bytes| bytes.len() as u64)
                        .unwrap_or(0)
                })
                .sum::<u64>();
        assert!(
            remaining_bytes <= expect,
            "remainder {remaining_bytes} must account for at least the serialized fields"
        );
        adapter.shutdown();
        manager.shutdown().await;
    }
}
