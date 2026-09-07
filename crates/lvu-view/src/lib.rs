//! Native, bounded live-view query adapter.

mod export;
pub mod folding;
pub mod time_basis;
pub use export::*;

use lvu::{
    DisplayRow, QueryCompletion, QueryFailure, QueryPurpose, QueryRequest, RowId, RowPage,
    RowProvider, ViewportRequest, terminal::QueryDispatcher,
};
use lvu_core::SourceId;
use lvu_ingest::SourceHandle;
use lvu_live::{IndexState, LiveRowProvider, SourceViewStatus};
use lvu_query::{
    BatchQuery, BatchValidity, CompiledEnrichment, CompilerHost, CompilerHostConfig, DerivedState,
    EnrichmentDefinition as NativeEnrichmentDefinition, EnrichmentStage,
    EnrichmentStageId as NativeEnrichmentStageId, ExpressionKind, SchemaContext, TextSearch,
    compile_enrichment_chain, execute_batch, parse_regex_enrichment, records_to_batch_with_context,
    scalar_projection,
};
use polars::prelude::{Column, DataFrame, NamedFrom, Series};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    io,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::Duration,
};
use thiserror::Error;

const SEQUENCE_BYTES: u64 = 8;
// Conservative charge for SourceMatches, Arc allocation metadata and UUID text.
const SOURCE_OVERHEAD: u64 = 128;
/// Raw row lookups are individually enqueued into the live provider's bounded
/// request queue. Asking for a whole tall viewport at once overflows that queue
/// and silently drops every request, so each frame asks for a bounded prefix of
/// the rows it is still missing and lets the next frame continue.
pub const MAX_ROW_REQUESTS_PER_PAGE: usize = 16;
/// A satisfied membership whose rows never arrive must not retry forever. After
/// this many consecutive frames without progress the view reports `Stalled`
/// instead of continuing to redraw.
pub const MAX_ROW_FETCH_RETRIES: u32 = 64;
/// Bound for reasons copied out of the raw provider into readiness.
const MAX_READINESS_REASON_BYTES: usize = 2 * 1024;

#[derive(Clone, Debug)]
pub struct ViewConfig {
    pub artifact_dir: PathBuf,
    pub request_capacity: usize,
    pub update_capacity: usize,
    pub completion_capacity: usize,
    pub page_records: usize,
    pub page_bytes: usize,
    pub maximum_index_bytes: u64,
    pub maximum_views: usize,
    pub maximum_sources_per_view: usize,
    pub maximum_viewport_rows: usize,
    pub compiler: Option<CompilerHostConfig>,
    pub maximum_snapshot_jobs: usize,
}

impl ViewConfig {
    pub fn new(artifact_dir: impl Into<PathBuf>) -> Self {
        Self {
            artifact_dir: artifact_dir.into(),
            request_capacity: 16,
            update_capacity: 64,
            completion_capacity: 32,
            page_records: 256,
            page_bytes: 2 * 1024 * 1024,
            maximum_index_bytes: 256 * 1024 * 1024,
            maximum_views: 128,
            maximum_sources_per_view: 32,
            maximum_viewport_rows: 256,
            compiler: None,
            maximum_snapshot_jobs: 2,
        }
    }
}

#[derive(Debug, Error)]
pub enum ViewError {
    #[error("view adapter configuration contains a zero bound")]
    InvalidConfig,
    #[error("view adapter source is not registered")]
    UnknownSource,
    #[error("view adapter view is not registered")]
    UnknownView,
    #[error("snapshot worker capacity is full")]
    SnapshotCapacity,
    #[error("view source limit exceeded")]
    SourceLimit,
    #[error("view limit exceeded")]
    ViewLimit,
    #[error("view adapter is shut down")]
    Closed,
    #[error("live row adapter: {0}")]
    Live(String),
    #[error("I/O: {0}")]
    Io(#[from] io::Error),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScanState {
    Raw,
    Pending,
    Ready,
    Limited,
    Error,
    Shutdown,
}

/// The bounded raw-row seam every view drives to turn matched record identities
/// into displayable rows. `LiveRowProvider` is the production implementation;
/// substituting another one lets tests inject the delay, starvation, lookup
/// failure and supersession behaviour that a real journal only produces by luck.
///
/// Implementations must not block on filesystem I/O. `row_by_id` returning
/// `None` means "not resolvable yet", never "absent".
pub trait RawRowSource: Send + Sync {
    fn page(&self, view_id: &str, request: ViewportRequest) -> RowPage;
    fn row_by_id(&self, view_id: &str, id: &RowId) -> Option<DisplayRow>;
    fn index_of_id(&self, view_id: &str, id: &RowId) -> Option<usize>;
    fn context_page(
        &self,
        view_id: &str,
        anchor: &RowId,
        offset: isize,
        len: usize,
    ) -> lvu::ContextPage;
    fn revision(&self, view_id: &str) -> u64;
    fn register_source(&self, handle: SourceHandle) -> Result<(), String>;
    fn register_raw_view(&self, view_id: &str, sources: Vec<SourceId>) -> Result<(), String>;
    fn drain_ready_updates(&self, maximum: usize) -> usize;
    fn source_status(&self, source_id: SourceId) -> Option<SourceViewStatus>;
    fn stats(&self) -> lvu_live::AdapterStats;
}

impl RawRowSource for LiveRowProvider {
    fn page(&self, view_id: &str, request: ViewportRequest) -> RowPage {
        RowProvider::page(self, view_id, request)
    }
    fn row_by_id(&self, view_id: &str, id: &RowId) -> Option<DisplayRow> {
        RowProvider::row_by_id(self, view_id, id)
    }
    fn index_of_id(&self, view_id: &str, id: &RowId) -> Option<usize> {
        RowProvider::index_of_id(self, view_id, id)
    }
    fn context_page(
        &self,
        view_id: &str,
        anchor: &RowId,
        offset: isize,
        len: usize,
    ) -> lvu::ContextPage {
        RowProvider::context_page(self, view_id, anchor, offset, len)
    }
    fn revision(&self, view_id: &str) -> u64 {
        RowProvider::revision(self, view_id)
    }
    fn register_source(&self, handle: SourceHandle) -> Result<(), String> {
        LiveRowProvider::register_source(self, handle).map_err(|error| error.to_string())
    }
    fn register_raw_view(&self, view_id: &str, sources: Vec<SourceId>) -> Result<(), String> {
        LiveRowProvider::register_raw_view(self, view_id, sources).map_err(|e| e.to_string())
    }
    fn drain_ready_updates(&self, maximum: usize) -> usize {
        LiveRowProvider::drain_ready_updates(self, maximum)
    }
    fn source_status(&self, source_id: SourceId) -> Option<SourceViewStatus> {
        LiveRowProvider::source_status(self, source_id)
    }
    fn stats(&self) -> lvu_live::AdapterStats {
        LiveRowProvider::stats(self)
    }
}

/// Whether the rows the UI is about to render are the complete answer, and if
/// not, why. A view whose membership is satisfied but whose rows are not yet
/// displayable must never render as an ordinary empty result: every variant
/// other than [`RowReadiness::Ready`] has a [`RowReadiness::describe`] sentence
/// the UI is required to show in place of a blank pane.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RowReadiness {
    /// The last requested range was served completely.
    Ready,
    /// The query worker is still scanning; membership is not final yet.
    QueryPending { scanned: u64 },
    /// Membership is satisfied, but rows in the last requested range are not in
    /// the bounded raw-row cache yet. The provider is re-requesting them.
    RowsPending { pending: usize, requested: usize },
    /// A source's derived index is still being built, so matched records may not
    /// be addressable yet. Counts are physical records, not display rows.
    Indexing {
        indexed_records: u64,
        reported_records: u64,
    },
    /// A raw row lookup or index operation reported a failure. Captured data is
    /// intact; this is a read failure, not an empty result.
    LookupFailed { reason: String, pending: usize },
    /// Rows did not arrive within the bounded retry budget. Refreshing the view
    /// or scrolling re-requests them.
    Stalled { pending: usize, requested: usize },
    /// The query completed and genuinely matched no records.
    NoMatches,
    /// The query itself failed, was limited or shut down.
    QueryFailed { reason: String },
}

impl RowReadiness {
    /// True only when the served rows are the whole answer.
    pub fn is_ready(&self) -> bool {
        matches!(self, RowReadiness::Ready)
    }

    /// True when rows may still arrive without any further user action.
    pub fn is_pending(&self) -> bool {
        matches!(
            self,
            RowReadiness::QueryPending { .. }
                | RowReadiness::RowsPending { .. }
                | RowReadiness::Indexing { .. }
        )
    }

    /// A bounded sentence for every state that is not [`RowReadiness::Ready`].
    /// `None` means, and only means, that the rows on screen are complete.
    pub fn describe(&self) -> Option<String> {
        Some(match self {
            RowReadiness::Ready => return None,
            RowReadiness::QueryPending { scanned } => {
                format!("Filtering: scanned {scanned} records so far.")
            }
            RowReadiness::RowsPending { pending, requested } => {
                format!("Loading {pending} of {requested} matched rows.")
            }
            RowReadiness::Indexing {
                indexed_records,
                reported_records,
            } => format!(
                "Building the record index: {indexed_records} of {reported_records} records ready."
            ),
            RowReadiness::LookupFailed { reason, pending } => {
                format!("Could not load {pending} matched rows: {reason}")
            }
            RowReadiness::Stalled { pending, requested } => format!(
                "{pending} of {requested} matched rows did not load. Scroll or refresh to retry."
            ),
            RowReadiness::NoMatches => "No records match this view.".to_owned(),
            RowReadiness::QueryFailed { reason } => {
                format!("This view could not be built: {reason}")
            }
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ViewQueryStatus {
    pub view_id: String,
    pub revision: u64,
    pub state: ScanState,
    pub scanned_records: u64,
    pub high_watermarks: Vec<(SourceId, Option<u64>)>,
    pub matched_records: u64,
    pub index_bytes: u64,
    pub diagnostic: Option<String>,
}

#[derive(Clone)]
struct SourceRegistration {
    handle: SourceHandle,
}

#[derive(Clone)]
struct PreparedDefinition {
    revision: u64,
    constraints: lvu::QueryConstraints,
    text: Option<TextSearch>,
    advanced: Option<lvu_query::CompiledDefinition>,
    enrichment: Vec<EnrichmentStage>,
    grouping: Option<ContinuationRule>,
    schema_seed: SchemaContext,
    schema: SchemaContext,
    checkpoints: HashMap<String, (u64, u64, Option<u64>)>,
    membership: Option<Arc<Membership>>,
}

#[derive(Clone)]
struct ViewRegistration {
    sources: Vec<SourceId>,
    raw_view: String,
}

#[derive(Clone)]
struct SourceMatches {
    source_id: String,
    generation: u64,
    high_watermark: Option<u64>,
    sequences: Arc<[u64]>,
    groups: Arc<[GroupRange]>,
}

#[derive(Clone)]
struct GroupRange {
    start: usize,
    len: usize,
    bytes: usize,
    stream: lvu_core::StreamKind,
    orphan: bool,
    split: bool,
    oversized: bool,
    projection: Arc<Vec<DisplayRow>>,
}

const MAX_GROUP_LINES: usize = 64;
const MAX_GROUP_BYTES: usize = 64 * 1024;
const MAX_GROUP_REGEX_BYTES: usize = 16 * 1024;
const MAX_GROUP_REGEX_COMPILED_BYTES: usize = 1024 * 1024;
const MAX_GROUP_REGEX_NESTING: u32 = 64;
const MAX_GROUP_LINE_DISPLAY_BYTES: usize = 4 * 1024;
const MAX_GROUP_LINE_PROJECTION_BYTES: usize = 8 * 1024;

#[derive(Clone)]
struct EvaluationBatch {
    source_id: String,
    generation: u64,
    record_count: usize,
    first_sequence: u64,
    last_sequence: u64,
    schema_before: SchemaContext,
}

struct MemoryBudget {
    used: AtomicU64,
    maximum: u64,
}

struct Membership {
    sources: Vec<SourceMatches>,
    count: u64,
    bytes: u64,
    budget: Arc<MemoryBudget>,
    enrichment_names: Vec<String>,
    derived: HashMap<(String, u64, String), Option<String>>,
    advanced: Option<lvu_query::CompiledDefinition>,
    enrichment: Vec<EnrichmentStage>,
    evaluation_page_bytes: usize,
    evaluation_batches: Arc<[EvaluationBatch]>,
    event_time_missing: usize,
    event_time_invalid: usize,
    grouped: bool,
}

struct Reservation {
    budget: Arc<MemoryBudget>,
    bytes: u64,
    committed: bool,
}

impl Reservation {
    fn new(budget: Arc<MemoryBudget>) -> Self {
        Self {
            budget,
            bytes: 0,
            committed: false,
        }
    }
    fn add(&mut self, bytes: u64) -> bool {
        let mut used = self.budget.used.load(Ordering::Acquire);
        loop {
            let Some(next) = used.checked_add(bytes) else {
                return false;
            };
            if next > self.budget.maximum {
                return false;
            }
            match self.budget.used.compare_exchange_weak(
                used,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    self.bytes += bytes;
                    return true;
                }
                Err(actual) => used = actual,
            }
        }
    }
    #[allow(clippy::too_many_arguments)]
    fn finish(
        mut self,
        sources: Vec<SourceMatches>,
        count: u64,
        enrichment_names: Vec<String>,
        derived: HashMap<(String, u64, String), Option<String>>,
        advanced: Option<lvu_query::CompiledDefinition>,
        enrichment: Vec<EnrichmentStage>,
        evaluation_page_bytes: usize,
        evaluation_batches: Vec<EvaluationBatch>,
        event_time_missing: usize,
        event_time_invalid: usize,
        grouped: bool,
    ) -> Arc<Membership> {
        self.committed = true;
        Arc::new(Membership {
            sources,
            count,
            bytes: self.bytes,
            budget: Arc::clone(&self.budget),
            enrichment_names,
            derived,
            advanced,
            enrichment,
            evaluation_page_bytes,
            evaluation_batches: evaluation_batches.into(),
            event_time_missing,
            event_time_invalid,
            grouped,
        })
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        if !self.committed {
            self.budget.used.fetch_sub(self.bytes, Ordering::AcqRel);
        }
    }
}

impl Drop for Membership {
    fn drop(&mut self) {
        self.budget.used.fetch_sub(self.bytes, Ordering::AcqRel);
    }
}

fn evaluation_batch_bytes(batch: &EvaluationBatch) -> u64 {
    batch
        .schema_before
        .fields()
        .keys()
        .fold(128_u64, |total, field| {
            total.saturating_add(field.len() as u64 + 24)
        })
        .saturating_add(batch.source_id.len() as u64)
}

fn display_projection_bytes(row: &DisplayRow) -> u64 {
    let text = row
        .timestamp
        .len()
        .saturating_add(row.level.len())
        .saturating_add(row.text.len());
    let fields = row.fields.iter().fold(0usize, |total, (key, value)| {
        total.saturating_add(key.len()).saturating_add(value.len())
    });
    let details = row.details.iter().fold(0usize, |total, (key, value)| {
        total.saturating_add(key.len()).saturating_add(value.len())
    });
    u64::try_from(
        text.saturating_add(fields)
            .saturating_add(details)
            .saturating_add(128),
    )
    .unwrap_or(u64::MAX)
}

fn group_projection_bytes(group: &GroupRange) -> u64 {
    group.projection.iter().fold(64_u64, |total, row| {
        total.saturating_add(display_projection_bytes(row))
    })
}

#[derive(Clone)]
enum Published {
    Raw,
    Filtered { membership: Arc<Membership> },
}

struct ViewState {
    registration: ViewRegistration,
    pending_sources: Option<Vec<SourceId>>,
    pending_request: Option<QueryRequest>,
    resubmit_pending: bool,
    desired_revision: u64,
    cancel: Arc<AtomicBool>,
    published: Published,
    provider_revision: u64,
    status: ViewQueryStatus,
    last_request: Option<QueryRequest>,
    applied_revision: u64,
    applied_generation: u64,
    applied_constraints: lvu::QueryConstraints,
    refreshing: bool,
    /// Row-fetch bookkeeping for the last non-empty range `page` was asked for.
    /// It is what separates "this view matched nothing" from "these rows have
    /// not been served yet".
    rows_requested: usize,
    rows_missing: usize,
    rows_served: usize,
    rows_raw_revision: u64,
    rows_retry: u32,
    /// Advances while rows are outstanding so the terminal redraws and
    /// re-requests them. Bounded by `MAX_ROW_FETCH_RETRIES`.
    rows_retry_revision: u64,
}

struct Shared {
    accepting: bool,
    sources: HashMap<SourceId, SourceRegistration>,
    views: HashMap<String, ViewState>,
}

enum Work {
    Query(Box<QueryRequest>),
    Incremental(String),
    Shutdown,
}

enum Update {
    Progress {
        view_id: String,
        revision: u64,
        scanned: u64,
        token: Arc<AtomicBool>,
    },
    Publish {
        request: QueryRequest,
        published: Published,
        scanned: u64,
        watermarks: Vec<(SourceId, Option<u64>)>,
        index_bytes: u64,
        diagnostic: Option<String>,
        token: Arc<AtomicBool>,
    },
    Failed {
        request: QueryRequest,
        purpose: QueryPurpose,
        message: String,
        limited: bool,
        token: Arc<AtomicBool>,
    },
}

pub struct NativeViewAdapter {
    raw: Arc<dyn RawRowSource>,
    config: ViewConfig,
    shared: Arc<Mutex<Shared>>,
    work: Option<mpsc::SyncSender<Work>>,
    updates: mpsc::Receiver<Update>,
    completions: VecDeque<QueryCompletion>,
    admitted: HashMap<String, u64>,
    worker: Option<JoinHandle<()>>,
    shutdown: Arc<AtomicBool>,
    budget: Arc<MemoryBudget>,
    snapshot_jobs: Arc<std::sync::atomic::AtomicUsize>,
    compiler_calls: Arc<AtomicU64>,
}

/// Cloneable read-only half for terminal composition. Keep the adapter itself as
/// the mutable `QueryDispatcher` and pass this handle as the `RowProvider`.
#[derive(Clone)]
pub struct NativeViewRows {
    raw: Arc<dyn RawRowSource>,
    config: ViewConfig,
    shared: Arc<Mutex<Shared>>,
}

impl NativeViewAdapter {
    pub fn new(raw: Arc<LiveRowProvider>, config: ViewConfig) -> Result<Self, ViewError> {
        Self::with_raw_rows(raw, config)
    }

    /// Same adapter over any bounded raw-row seam. Tests use this to inject
    /// deterministic row-request delay, starvation and lookup failure.
    pub fn with_raw_rows(
        raw: Arc<dyn RawRowSource>,
        config: ViewConfig,
    ) -> Result<Self, ViewError> {
        if config.request_capacity == 0
            || config.update_capacity == 0
            || config.completion_capacity == 0
            || config.page_records == 0
            || config.page_bytes == 0
            || config.maximum_index_bytes < SOURCE_OVERHEAD
            || config.maximum_views == 0
            || config.maximum_sources_per_view == 0
            || config.maximum_viewport_rows == 0
            || config.maximum_snapshot_jobs == 0
        {
            return Err(ViewError::InvalidConfig);
        }
        let (work_tx, work_rx) = mpsc::sync_channel(config.request_capacity);
        let (update_tx, update_rx) = mpsc::sync_channel(config.update_capacity);
        let shutdown = Arc::new(AtomicBool::new(false));
        let budget = Arc::new(MemoryBudget {
            used: AtomicU64::new(0),
            maximum: config.maximum_index_bytes,
        });
        let shared = Arc::new(Mutex::new(Shared {
            accepting: true,
            sources: HashMap::new(),
            views: HashMap::new(),
        }));
        let worker_shared = Arc::clone(&shared);
        let worker_config = config.clone();
        let worker_budget = Arc::clone(&budget);
        let compiler_calls = Arc::new(AtomicU64::new(0));
        let worker_compiler_calls = Arc::clone(&compiler_calls);
        let worker = thread::Builder::new()
            .name("lvu-view-query".into())
            .spawn(move || {
                worker_loop(
                    worker_config,
                    worker_shared,
                    work_rx,
                    update_tx,
                    worker_budget,
                    worker_compiler_calls,
                )
            })?;
        Ok(Self {
            raw,
            config,
            shared,
            work: Some(work_tx),
            updates: update_rx,
            completions: VecDeque::new(),
            admitted: HashMap::new(),
            worker: Some(worker),
            shutdown,
            budget,
            snapshot_jobs: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            compiler_calls,
        })
    }

    /// Number of Python definition compilations requested by this adapter.
    pub fn compiler_calls(&self) -> u64 {
        self.compiler_calls.load(Ordering::Acquire)
    }

    pub fn register_source(&self, handle: SourceHandle) -> Result<(), ViewError> {
        {
            let shared = self.shared.lock().expect("view state poisoned");
            if !shared.accepting {
                return Err(ViewError::Closed);
            }
        }
        self.raw
            .register_source(handle.clone())
            .map_err(ViewError::Live)?;
        let mut shared = self.shared.lock().expect("view state poisoned");
        let source_id = handle.source_id();
        shared
            .sources
            .insert(source_id, SourceRegistration { handle });
        for view in shared.views.values_mut().filter(|view| {
            view.registration.sources.contains(&source_id)
                || view
                    .pending_sources
                    .as_ref()
                    .is_some_and(|sources| sources.contains(&source_id))
        }) {
            view.cancel.store(true, Ordering::Release);
            view.cancel = Arc::new(AtomicBool::new(false));
            view.refreshing = false;
            view.resubmit_pending = view.pending_request.is_some();
            if let Some((_, high)) = view
                .status
                .high_watermarks
                .iter_mut()
                .find(|(id, _)| *id == source_id)
            {
                *high = None;
            }
            view.provider_revision = view.provider_revision.saturating_add(1);
        }
        Ok(())
    }

    /// Releases composition ownership when registering the source's first view
    /// fails. The caller remains responsible for stopping the capture handle.
    pub fn rollback_source_registration(&self, source_id: SourceId, view_id: &str) {
        let mut shared = self.shared.lock().expect("view state poisoned");
        shared.views.remove(view_id);
        if shared.views.values().all(|view| {
            !view.registration.sources.contains(&source_id)
                && !view
                    .pending_sources
                    .as_ref()
                    .is_some_and(|sources| sources.contains(&source_id))
        }) {
            shared.sources.remove(&source_id);
        }
    }

    pub fn register_view(
        &self,
        view_id: impl Into<String>,
        sources: Vec<SourceId>,
    ) -> Result<(), ViewError> {
        if sources.len() > self.config.maximum_sources_per_view {
            return Err(ViewError::SourceLimit);
        }
        let view_id = view_id.into();
        let mut deduped = Vec::new();
        for source in sources {
            if !deduped.contains(&source) {
                deduped.push(source);
            }
        }
        let raw_view = format!("_lvu_native_raw_{view_id}");
        {
            let mut shared = self.shared.lock().expect("view state poisoned");
            if !shared.accepting {
                return Err(ViewError::Closed);
            }
            if deduped.iter().any(|id| !shared.sources.contains_key(id)) {
                return Err(ViewError::UnknownSource);
            }
            if !shared.views.contains_key(&view_id)
                && shared.views.len() >= self.config.maximum_views
            {
                return Err(ViewError::ViewLimit);
            }
            let revision = shared
                .views
                .get(&view_id)
                .map_or(1, |v| v.provider_revision.saturating_add(1));
            if let Some(previous) = shared.views.get(&view_id) {
                previous.cancel.store(true, Ordering::Release);
            }
            shared.views.insert(
                view_id.clone(),
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
                        view_id,
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
                },
            );
        }
        self.raw
            .register_raw_view(&raw_view, deduped)
            .map_err(ViewError::Live)
    }

    pub fn status(&self, view_id: &str) -> Option<ViewQueryStatus> {
        self.shared
            .lock()
            .expect("view state poisoned")
            .views
            .get(view_id)
            .map(|v| v.status.clone())
    }

    pub fn rows(&self) -> NativeViewRows {
        NativeViewRows {
            raw: Arc::clone(&self.raw),
            config: self.config.clone(),
            shared: Arc::clone(&self.shared),
        }
    }

    pub fn raw_stats(&self) -> lvu_live::AdapterStats {
        self.raw.stats()
    }

    pub fn membership_bytes_used(&self) -> u64 {
        self.budget.used.load(Ordering::Acquire)
    }

    /// Applies bounded worker updates and schedules at most one incremental scan
    /// per view. It never waits for journal I/O, Python, or Polars work.
    pub fn drain_updates(&mut self, maximum: usize) -> usize {
        self.raw.drain_ready_updates(maximum);
        let mut count = 0;
        while count < maximum {
            let Ok(update) = self.updates.try_recv() else {
                break;
            };
            count += 1;
            self.apply_update(update);
        }
        if let Some(tx) = &self.work {
            {
                let mut shared = self.shared.lock().expect("view state poisoned");
                for view in shared
                    .views
                    .values_mut()
                    .filter(|view| view.resubmit_pending)
                {
                    if let Some(request) = &view.pending_request
                        && tx.try_send(Work::Query(Box::new(request.clone()))).is_err()
                    {
                        break;
                    }
                    view.resubmit_pending = false;
                }
            }
            let views: Vec<String> = {
                let shared = self.shared.lock().expect("view state poisoned");
                shared
                    .views
                    .iter()
                    .filter(|(_, v)| {
                        matches!(v.status.state, ScanState::Ready)
                            && !v.refreshing
                            && v.registration.sources.iter().any(|source_id| {
                                let current = self
                                    .raw
                                    .source_status(*source_id)
                                    .and_then(|s| s.high_watermark);
                                let scanned = v
                                    .status
                                    .high_watermarks
                                    .iter()
                                    .find(|(id, _)| id == source_id)
                                    .and_then(|(_, high)| *high);
                                current > scanned
                            })
                    })
                    .map(|(id, _)| id.clone())
                    .collect()
            };
            for view in views {
                let mut shared = self.shared.lock().expect("view state poisoned");
                if let Some(state) = shared.views.get_mut(&view) {
                    state.refreshing = true;
                    if tx.try_send(Work::Incremental(view)).is_err() {
                        state.refreshing = false;
                    }
                }
            }
        }
        count
    }

    fn apply_update(&mut self, update: Update) {
        let mut shared = self.shared.lock().expect("view state poisoned");
        if let Update::Publish { request, token, .. } = &update
            && !token.load(Ordering::Acquire)
            && let Some(view) = shared.views.get_mut(&request.view_id)
            && view.desired_revision == request.revision
            && !view.cancel.load(Ordering::Acquire)
            && let Some(sources) = view.pending_sources.clone()
        {
            if let Err(error) = self
                .raw
                .register_raw_view(&view.registration.raw_view, sources.clone())
            {
                let failed = Update::Failed {
                    request: request.clone(),
                    purpose: request.purpose,
                    message: format!("source membership publication: {error}"),
                    limited: false,
                    token: Arc::clone(token),
                };
                drop(shared);
                self.apply_update(failed);
                return;
            }
            view.registration.sources = sources;
            view.pending_sources = None;
        }
        let completion = match update {
            Update::Progress {
                view_id,
                revision,
                scanned,
                token,
            } => {
                if token.load(Ordering::Acquire) {
                    return;
                }
                if let Some(view) = shared.views.get_mut(&view_id)
                    && view.desired_revision == revision
                {
                    view.status.scanned_records = scanned;
                }
                None
            }
            Update::Publish {
                request,
                published,
                scanned,
                watermarks,
                index_bytes,
                diagnostic,
                token,
            } => {
                if token.load(Ordering::Acquire) {
                    return;
                }
                let Some(view) = shared.views.get_mut(&request.view_id) else {
                    return;
                };
                if view.desired_revision != request.revision || view.cancel.load(Ordering::Acquire)
                {
                    return;
                }
                let matched = match &published {
                    Published::Raw => 0,
                    Published::Filtered { membership } => membership.count,
                };
                let complete_request = matches!(view.status.state, ScanState::Pending);
                view.pending_request = None;
                view.resubmit_pending = false;
                view.published = published;
                view.applied_revision = request.revision;
                view.applied_generation = request.generation;
                view.applied_constraints = request.constraints.clone();
                view.last_request = Some(request.clone());
                view.refreshing = false;
                view.provider_revision = view.provider_revision.saturating_add(1);
                view.status = ViewQueryStatus {
                    view_id: request.view_id.clone(),
                    revision: request.revision,
                    state: if matches!(view.published, Published::Raw) {
                        ScanState::Raw
                    } else {
                        ScanState::Ready
                    },
                    scanned_records: scanned,
                    high_watermarks: watermarks,
                    matched_records: matched,
                    index_bytes,
                    diagnostic,
                };
                complete_request.then_some(QueryCompletion {
                    view_id: request.view_id,
                    generation: request.generation,
                    revision: request.revision,
                    purpose: request.purpose,
                    result: Ok(()),
                })
            }
            Update::Failed {
                request,
                purpose,
                message,
                limited,
                token,
            } => {
                if token.load(Ordering::Acquire) {
                    return;
                }
                let Some(view) = shared.views.get_mut(&request.view_id) else {
                    return;
                };
                if view.desired_revision != request.revision {
                    return;
                }
                view.refreshing = false;
                view.pending_sources = None;
                view.pending_request = None;
                view.resubmit_pending = false;
                view.desired_revision = view.applied_revision;
                view.cancel = Arc::new(AtomicBool::new(false));
                view.status.state = if limited {
                    ScanState::Limited
                } else if matches!(view.published, Published::Raw) {
                    ScanState::Raw
                } else {
                    ScanState::Ready
                };
                view.status.diagnostic = Some(message.clone());
                Some(QueryCompletion {
                    view_id: request.view_id,
                    generation: request.generation,
                    revision: request.revision,
                    purpose: request.purpose,
                    result: Err(QueryFailure { purpose, message }),
                })
            }
        };
        drop(shared);
        if let Some(completion) = completion {
            if self.admitted.get(&completion.view_id) == Some(&completion.revision) {
                self.admitted.remove(&completion.view_id);
            }
            self.push_completion(completion);
        }
    }

    fn push_completion(&mut self, completion: QueryCompletion) {
        debug_assert!(self.completions.len() < self.config.completion_capacity);
        self.completions.push_back(completion);
    }

    pub fn shutdown(&mut self) {
        {
            let mut shared = self.shared.lock().expect("view state poisoned");
            shared.accepting = false;
            for view in shared.views.values_mut() {
                view.cancel.store(true, Ordering::Release);
                view.status.state = ScanState::Shutdown;
            }
        }
        self.shutdown.store(true, Ordering::Release);
        if let Some(tx) = self.work.take() {
            let _ = tx.try_send(Work::Shutdown);
            drop(tx);
        }
        if let Some(worker) = self.worker.take() {
            while !worker.is_finished() {
                while self.updates.try_recv().is_ok() {}
                thread::sleep(Duration::from_millis(2));
            }
            let _ = worker.join();
        }
    }
}

impl NativeViewAdapter {
    /// Atomically change ordered source membership with a composite query.
    /// Existing rows, constraints and source registration remain applied until
    /// the candidate succeeds. This never starts or stops source acquisition.
    pub fn submit_source_change(
        &mut self,
        request: QueryRequest,
        sources: Vec<SourceId>,
    ) -> Result<(), String> {
        self.submit_query(request, Some(sources))
    }

    pub fn view_sources(&self, view_id: &str) -> Option<Vec<SourceId>> {
        self.shared
            .lock()
            .expect("view state poisoned")
            .views
            .get(view_id)
            .map(|view| view.registration.sources.clone())
    }

    fn submit_query(
        &mut self,
        request: QueryRequest,
        sources: Option<Vec<SourceId>>,
    ) -> Result<(), String> {
        if !self.admitted.contains_key(&request.view_id)
            && self.completions.len().saturating_add(self.admitted.len())
                >= self.config.completion_capacity
        {
            return Err("query completion capacity is full".into());
        }
        let tx = self.work.as_ref().ok_or("view adapter is shut down")?;
        {
            let mut shared = self.shared.lock().expect("view state poisoned");
            if !shared.accepting {
                return Err("view adapter is shut down".into());
            }
            if let Some(sources) = &sources {
                if sources.is_empty() || sources.len() > self.config.maximum_sources_per_view {
                    return Err("source membership must contain between one and the configured source limit".into());
                }
                let mut seen = HashSet::new();
                if sources.iter().any(|id| !seen.insert(*id)) {
                    return Err("source membership contains duplicate identities".into());
                }
                if sources.iter().any(|id| !shared.sources.contains_key(id)) {
                    return Err("source membership references an unavailable source".into());
                }
            }
            let view = shared
                .views
                .get_mut(&request.view_id)
                .ok_or("unknown view")?;
            if request.revision <= view.desired_revision {
                return Ok(());
            }
            if request.base_revision != view.applied_revision
                || request.base_constraints != view.applied_constraints
            {
                return Err("query base snapshot does not match the applied view".into());
            }
            let old_cancel = Arc::clone(&view.cancel);
            let old_desired = view.desired_revision;
            let old_status = view.status.clone();
            let old_sources = view.pending_sources.take();
            view.pending_sources = sources;
            view.cancel = Arc::new(AtomicBool::new(false));
            view.desired_revision = request.revision;
            view.status.state = ScanState::Pending;
            view.status.revision = request.revision;
            view.status.diagnostic = None;
            if tx.try_send(Work::Query(Box::new(request.clone()))).is_err() {
                view.cancel = old_cancel;
                view.desired_revision = old_desired;
                view.status = old_status;
                view.pending_sources = old_sources;
                return Err("query queue is full".into());
            }
            view.pending_request = Some(request.clone());
            view.resubmit_pending = false;
            old_cancel.store(true, Ordering::Release);
        }
        self.admitted
            .insert(request.view_id.clone(), request.revision);
        Ok(())
    }
}

impl QueryDispatcher for NativeViewAdapter {
    fn submit(&mut self, request: QueryRequest) -> Result<(), String> {
        self.submit_query(request, None)
    }

    fn poll(&mut self) -> Option<QueryCompletion> {
        self.completions.pop_front()
    }
}

impl RowProvider for NativeViewAdapter {
    fn page(&self, view_id: &str, request: ViewportRequest) -> RowPage {
        self.rows().page(view_id, request)
    }

    fn row_by_id(&self, view_id: &str, id: &RowId) -> Option<DisplayRow> {
        self.rows().row_by_id(view_id, id)
    }

    fn index_of_id(&self, view_id: &str, id: &RowId) -> Option<usize> {
        self.rows().index_of_id(view_id, id)
    }

    fn context_page(
        &self,
        view_id: &str,
        anchor: &RowId,
        offset: isize,
        len: usize,
    ) -> lvu::ContextPage {
        self.rows().context_page(view_id, anchor, offset, len)
    }

    fn revision(&self, view_id: &str) -> u64 {
        self.rows().revision(view_id)
    }
}

impl RowProvider for NativeViewRows {
    fn page(&self, view_id: &str, request: ViewportRequest) -> RowPage {
        let mut shared = self.shared.lock().expect("view state poisoned");
        let Some(view) = shared.views.get(view_id) else {
            return RowPage {
                total: 0,
                rows: Vec::new(),
            };
        };
        let raw_view = view.registration.raw_view.clone();
        let (page, requested, missing) = match &view.published {
            Published::Raw => {
                let page = self.raw.page(&raw_view, request);
                let requested = request.len.min(page.total.saturating_sub(request.start));
                let missing = requested.saturating_sub(page.rows.len());
                (page, requested, missing)
            }
            Published::Filtered { membership } => {
                let total = membership_display_count(membership);
                let len = request
                    .len
                    .min(self.config.maximum_viewport_rows)
                    .min(total.saturating_sub(request.start));
                let mut rows = Vec::with_capacity(len);
                let mut missing = 0usize;
                if membership.grouped {
                    // Grouped rows are projected inside membership; they never
                    // need a raw lookup and so are never partially available.
                    for group in membership_groups(membership, request.start, len) {
                        rows.push(project_group(
                            group.projection.to_vec(),
                            group.orphan,
                            group.split,
                            group.oversized,
                        ));
                    }
                } else {
                    let ids = membership_ids(membership, request.start, len);
                    let mut issued = 0usize;
                    let mut probed = 0usize;
                    for id in &ids {
                        // Each miss costs one slot in the raw provider's bounded
                        // request queue. Probing a whole tall viewport overflows
                        // it and loses every request, leaving a blank pane that
                        // nothing will ever refill.
                        if issued >= MAX_ROW_REQUESTS_PER_PAGE {
                            break;
                        }
                        probed += 1;
                        match self.raw.row_by_id(&raw_view, id) {
                            // A page is positional: rows after a hole cannot be
                            // placed. Keep probing anyway so the whole visible
                            // range is requested, not just its first missing row.
                            Some(row) if missing == 0 => {
                                rows.push(with_enrichment(row, membership));
                            }
                            Some(_) => {}
                            None => {
                                missing += 1;
                                issued += 1;
                            }
                        }
                    }
                    missing = missing.saturating_add(ids.len().saturating_sub(probed));
                }
                let requested = len;
                (RowPage { total, rows }, requested, missing)
            }
        };

        // A zero-length probe (the terminal's total-only sync call) asks for no
        // rows and must not overwrite what the last real viewport observed.
        if requested > 0 {
            let raw_revision = self.raw.revision(&raw_view);
            let served = page.rows.len();
            let view = shared.views.get_mut(view_id).expect("view exists");
            let progressed = raw_revision != view.rows_raw_revision || served != view.rows_served;
            if missing == 0 || progressed {
                view.rows_retry = 0;
            } else {
                view.rows_retry = view.rows_retry.saturating_add(1);
            }
            view.rows_raw_revision = raw_revision;
            view.rows_served = served;
            view.rows_requested = requested;
            view.rows_missing = missing;
            if missing > 0 && view.rows_retry <= MAX_ROW_FETCH_RETRIES {
                // Nothing else will wake the terminal when a row request was
                // dropped at the bounded queue, so advance our own revision and
                // let the next frame re-request. Bounded: once the budget is
                // spent the view reports `Stalled` instead of spinning.
                view.rows_retry_revision = view.rows_retry_revision.wrapping_add(1);
            }
        }
        page
    }

    fn row_by_id(&self, view_id: &str, id: &RowId) -> Option<DisplayRow> {
        let shared = self.shared.lock().expect("view state poisoned");
        let view = shared.views.get(view_id)?;
        match &view.published {
            Published::Raw => self.raw.row_by_id(&view.registration.raw_view, id),
            Published::Filtered { membership } => {
                if membership.grouped {
                    let group = membership_group_for_id(membership, id)?;
                    Some(project_group(
                        group.projection.to_vec(),
                        group.orphan,
                        group.split,
                        group.oversized,
                    ))
                } else {
                    membership_index(membership, id)?;
                    self.raw
                        .row_by_id(&view.registration.raw_view, id)
                        .map(|row| with_enrichment(row, membership))
                }
            }
        }
    }

    fn index_of_id(&self, view_id: &str, id: &RowId) -> Option<usize> {
        let shared = self.shared.lock().expect("view state poisoned");
        let view = shared.views.get(view_id)?;
        match &view.published {
            Published::Raw => self.raw.index_of_id(&view.registration.raw_view, id),
            Published::Filtered { membership } if membership.grouped => {
                membership_group_index(membership, id)
            }
            Published::Filtered { membership } => membership_index(membership, id),
        }
    }

    fn context_page(
        &self,
        view_id: &str,
        anchor: &RowId,
        offset: isize,
        len: usize,
    ) -> lvu::ContextPage {
        let shared = self.shared.lock().expect("view state poisoned");
        let Some(view) = shared.views.get(view_id) else {
            return lvu::ContextPage {
                anchor_position: None,
                start: 0,
                total: 0,
                rows: Vec::new(),
                pending: false,
                diagnostic: Some("context view is no longer registered".into()),
            };
        };
        self.raw
            .context_page(&view.registration.raw_view, anchor, offset, len)
    }

    fn revision(&self, view_id: &str) -> u64 {
        self.shared
            .lock()
            .expect("view state poisoned")
            .views
            .get(view_id)
            .map_or(0, |v| {
                v.provider_revision
                    .wrapping_add(self.raw.revision(&v.registration.raw_view))
                    .wrapping_add(v.rows_retry_revision)
            })
    }
}

impl NativeViewRows {
    /// Typed reason the pane looks the way it does. Call it every time rows are
    /// rendered: whenever it is not [`RowReadiness::Ready`], the UI must show
    /// [`RowReadiness::describe`] rather than an ordinary empty result.
    ///
    /// It reads only state already published for this view, so a stale query or
    /// row reply that lost its generation fence cannot influence it.
    pub fn readiness(&self, view_id: &str) -> RowReadiness {
        let shared = self.shared.lock().expect("view state poisoned");
        let Some(view) = shared.views.get(view_id) else {
            return RowReadiness::QueryFailed {
                reason: "this view is no longer registered".into(),
            };
        };
        match view.status.state {
            ScanState::Error => {
                return RowReadiness::QueryFailed {
                    reason: bounded_text(
                        view.status
                            .diagnostic
                            .clone()
                            .unwrap_or_else(|| "the filter could not be evaluated".into()),
                        MAX_READINESS_REASON_BYTES,
                    ),
                };
            }
            ScanState::Shutdown => {
                return RowReadiness::QueryFailed {
                    reason: "the query worker has shut down".into(),
                };
            }
            _ => {}
        }

        let total = match &view.published {
            Published::Raw => {
                self.raw
                    .page(
                        &view.registration.raw_view,
                        ViewportRequest { start: 0, len: 0 },
                    )
                    .total
            }
            Published::Filtered { membership } => membership_display_count(membership),
        };
        let sources: Vec<SourceViewStatus> = view
            .registration
            .sources
            .iter()
            .filter_map(|id| self.raw.source_status(*id))
            .collect();
        let failure = sources
            .iter()
            .find_map(|status| status.last_error.clone())
            .map(|reason| bounded_text(reason, MAX_READINESS_REASON_BYTES));
        let indexing = sources
            .iter()
            .find(|status| {
                !matches!(
                    status.index,
                    IndexState::Ready | IndexState::Limited | IndexState::Error
                ) || status.indexed_records < status.reported_records
            })
            .map(|status| RowReadiness::Indexing {
                indexed_records: status.indexed_records,
                reported_records: status.reported_records.max(status.indexed_records),
            });

        if view.rows_missing > 0 {
            let pending = view.rows_missing;
            let requested = view.rows_requested;
            // A read failure explains the gap better than "still loading", and
            // index progress explains it better than a bare retry count.
            if let Some(reason) = failure {
                return RowReadiness::LookupFailed { reason, pending };
            }
            if let Some(state) = indexing {
                return state;
            }
            if view.rows_retry > MAX_ROW_FETCH_RETRIES {
                return RowReadiness::Stalled { pending, requested };
            }
            return RowReadiness::RowsPending { pending, requested };
        }

        if total > 0 {
            return RowReadiness::Ready;
        }
        // Nothing is displayable. Only a settled query over a settled index may
        // be reported as a genuine zero-match result.
        if matches!(view.status.state, ScanState::Pending) {
            return RowReadiness::QueryPending {
                scanned: view.status.scanned_records,
            };
        }
        if let Some(reason) = failure {
            return RowReadiness::LookupFailed { reason, pending: 0 };
        }
        if let Some(state) = indexing {
            return state;
        }
        RowReadiness::NoMatches
    }
}

impl NativeViewAdapter {
    /// See [`NativeViewRows::readiness`].
    pub fn readiness(&self, view_id: &str) -> RowReadiness {
        self.rows().readiness(view_id)
    }
}

fn with_enrichment(mut row: DisplayRow, membership: &Membership) -> DisplayRow {
    for name in &membership.enrichment_names {
        let value = membership
            .derived
            .get(&(row.id.source_id.clone(), row.id.sequence, name.clone()))
            .and_then(Clone::clone)
            .unwrap_or_else(|| "null".into());
        row.fields.retain(|(field, _)| field != name);
        row.fields.push((name.clone(), value.clone()));
        row.details.push((format!("derived.{name}"), value));
    }
    row
}

impl Drop for NativeViewAdapter {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn worker_loop(
    config: ViewConfig,
    shared: Arc<Mutex<Shared>>,
    rx: mpsc::Receiver<Work>,
    tx: mpsc::SyncSender<Update>,
    budget: Arc<MemoryBudget>,
    compiler_calls: Arc<AtomicU64>,
) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("view runtime");
    let mut compiler = config.compiler.clone().map(CompilerHost::new);
    let mut prepared: HashMap<(String, u64), PreparedDefinition> = HashMap::new();
    while let Ok(work) = rx.recv() {
        {
            let state = shared.lock().expect("view state poisoned");
            prepared.retain(|(view_id, revision), _| {
                state.views.get(view_id).is_some_and(|view| {
                    *revision == view.applied_revision || *revision == view.desired_revision
                })
            });
        }
        match work {
            Work::Shutdown => break,
            Work::Incremental(view_id) => {
                let snapshot = {
                    let state = shared.lock().expect("view state poisoned");
                    let Some(view) = state.views.get(&view_id) else {
                        continue;
                    };
                    let Some(request) = view.last_request.clone() else {
                        continue;
                    };
                    if view.desired_revision != request.revision {
                        continue;
                    }
                    let sources = view
                        .registration
                        .sources
                        .iter()
                        .filter_map(|id| state.sources.get(id).map(|s| s.handle.clone()))
                        .collect::<Vec<_>>();
                    (request, sources, Arc::clone(&view.cancel))
                };
                run_query(
                    &runtime,
                    &config,
                    &mut compiler,
                    snapshot.0,
                    snapshot.1,
                    snapshot.2,
                    &tx,
                    &mut prepared,
                    Arc::clone(&budget),
                    Arc::clone(&compiler_calls),
                );
            }
            Work::Query(request) => {
                let snapshot = {
                    let state = shared.lock().expect("view state poisoned");
                    let Some(view) = state.views.get(&request.view_id) else {
                        continue;
                    };
                    if view.desired_revision != request.revision {
                        continue;
                    }
                    let sources = view
                        .pending_sources
                        .as_ref()
                        .unwrap_or(&view.registration.sources)
                        .iter()
                        .filter_map(|id| state.sources.get(id).map(|s| s.handle.clone()))
                        .collect::<Vec<_>>();
                    (sources, Arc::clone(&view.cancel))
                };
                // Explicit candidates (including a restart resubmission) scan
                // a fresh snapshot. Only accepted incremental work resumes a
                // checkpoint; an unpublished candidate is never a durable base.
                prepared.remove(&(request.view_id.clone(), request.revision));
                run_query(
                    &runtime,
                    &config,
                    &mut compiler,
                    *request,
                    snapshot.0,
                    snapshot.1,
                    &tx,
                    &mut prepared,
                    Arc::clone(&budget),
                    Arc::clone(&compiler_calls),
                );
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn run_query(
    runtime: &tokio::runtime::Runtime,
    config: &ViewConfig,
    compiler: &mut Option<CompilerHost>,
    request: QueryRequest,
    sources: Vec<SourceHandle>,
    cancelled: Arc<AtomicBool>,
    tx: &mpsc::SyncSender<Update>,
    prepared: &mut HashMap<(String, u64), PreparedDefinition>,
    budget: Arc<MemoryBudget>,
    compiler_calls: Arc<AtomicU64>,
) {
    let grouping_rule = match request.constraints.grouping.as_deref() {
        Some(source) => match prepared
            .values()
            .find(|value| value.constraints.grouping.as_deref() == Some(source))
            .and_then(|value| value.grouping.clone())
            .map_or_else(|| ContinuationRule::parse(source), Ok)
        {
            Ok(rule) => Some(rule),
            Err(message) => {
                fail(
                    tx,
                    &request,
                    &cancelled,
                    QueryPurpose::Grouping,
                    &message,
                    false,
                );
                return;
            }
        },
        None => None,
    };
    if request
        .constraints
        .capture_time
        .is_some_and(|window| window.start_unix_nanos >= window.end_unix_nanos)
    {
        fail(
            tx,
            &request,
            &cancelled,
            QueryPurpose::Advanced,
            "time window start must be before end; range is [start, end)",
            false,
        );
        return;
    }
    if request.constraints.text.is_none()
        && request.constraints.advanced_polars.is_none()
        && request.constraints.enrichments.is_empty()
        && request.constraints.capture_time.is_none()
        && request.constraints.grouping.is_none()
    {
        prepared.retain(|(view_id, _), _| view_id != &request.view_id);
        let _ = send_update(
            tx,
            Update::Publish {
                request,
                published: Published::Raw,
                scanned: 0,
                watermarks: Vec::new(),
                index_bytes: 0,
                diagnostic: None,
                token: Arc::clone(&cancelled),
            },
            cancelled.as_ref(),
        );
        return;
    }
    let cache_key = (request.view_id.clone(), request.revision);
    let evaluation_provenance = (request.constraints.enrichments
        != request.base_constraints.enrichments)
        .then(|| {
            prepared
                .get(&(request.view_id.clone(), request.base_revision))
                .filter(|value| value.constraints == request.base_constraints)
                .and_then(|value| value.membership.clone())
        })
        .flatten();
    let cached = prepared
        .get(&cache_key)
        .filter(|value| {
            value.revision == request.revision && value.constraints == request.constraints
        })
        .cloned();
    let cached_refresh = cached.is_some();
    let advanced_changed =
        request.constraints.advanced_polars != request.base_constraints.advanced_polars;
    let reusable_definitions = cached
        .is_none()
        .then(|| {
            prepared
                .iter()
                .filter(|((view_id, _), value)| {
                    view_id == &request.view_id
                        && value.constraints.text == request.constraints.text
                        && value.constraints.advanced_polars == request.constraints.advanced_polars
                        && value.constraints.enrichments == request.constraints.enrichments
                })
                .max_by_key(|((_, revision), _)| *revision)
                .map(|(_, value)| value.clone())
        })
        .flatten();
    let (
        text,
        advanced,
        enrichment,
        affected_outputs,
        schema_seed,
        mut schema,
        mut checkpoints,
        prior_membership,
    ) = if let Some(cached) = cached {
        (
            cached.text,
            cached.advanced,
            cached.enrichment,
            HashSet::new(),
            cached.schema_seed,
            cached.schema,
            cached.checkpoints,
            cached.membership,
        )
    } else if let Some(reusable) = reusable_definitions {
        // A time-only revision must rescan membership so expired rows are
        // removed, but its immutable expression sources do not need Python
        // compilation again. Schema/checkpoints intentionally restart so a
        // changed journal generation or schema cannot inherit scan state.
        let schema_seed = SchemaContext::default();
        (
            reusable.text,
            reusable.advanced,
            reusable.enrichment,
            HashSet::new(),
            schema_seed.clone(),
            schema_seed,
            HashMap::new(),
            None,
        )
    } else {
        let text = match request.constraints.text.as_ref() {
            Some(value) if !value.case_insensitive => {
                fail(
                    tx,
                    &request,
                    &cancelled,
                    QueryPurpose::Search,
                    "case-sensitive literal search is not supported by the native TextSearch contract",
                    false,
                );
                return;
            }
            Some(value) => {
                let compiled = if TextSearch::is_polars(&value.literal) {
                    let Some(host) = compiler.as_mut() else {
                        fail(
                            tx,
                            &request,
                            &cancelled,
                            QueryPurpose::Search,
                            "search expression compiler is not configured",
                            false,
                        );
                        return;
                    };
                    compiler_calls.fetch_add(1, Ordering::AcqRel);
                    match host.compile(&value.literal, ExpressionKind::Filter, cancelled.as_ref()) {
                        Ok(compiled) => Some(compiled),
                        Err(error) => {
                            fail(
                                tx,
                                &request,
                                &cancelled,
                                QueryPurpose::Search,
                                &error.to_string(),
                                false,
                            );
                            return;
                        }
                    }
                } else {
                    None
                };
                match TextSearch::parse(value.literal.clone(), compiled.as_ref()) {
                    Ok(search) => Some(search),
                    Err(error) => {
                        fail(
                            tx,
                            &request,
                            &cancelled,
                            QueryPurpose::Search,
                            &error,
                            false,
                        );
                        return;
                    }
                }
            }
            None => None,
        };
        let advanced = match request.constraints.advanced_polars.as_deref() {
            Some(source) => {
                let Some(host) = compiler.as_mut() else {
                    fail(
                        tx,
                        &request,
                        &cancelled,
                        QueryPurpose::Advanced,
                        "advanced compiler is not configured",
                        false,
                    );
                    return;
                };
                compiler_calls.fetch_add(1, Ordering::AcqRel);
                match host.compile(source, ExpressionKind::Filter, cancelled.as_ref()) {
                    Ok(v) => Some(v),
                    Err(e) => {
                        fail(
                            tx,
                            &request,
                            &cancelled,
                            QueryPurpose::Advanced,
                            &e.to_string(),
                            false,
                        );
                        return;
                    }
                }
            }
            None => None,
        };
        let definitions = native_enrichment_definitions(&request.constraints);
        compiler_calls.fetch_add(
            definitions
                .iter()
                .filter(|definition| !definition.source.starts_with('/'))
                .count() as u64,
            Ordering::AcqRel,
        );
        let compiled_enrichment =
            match compile_enrichment_chain(&definitions, compiler.as_mut(), cancelled.as_ref()) {
                Ok(compiled) => compiled,
                Err(error) => {
                    fail(
                        tx,
                        &request,
                        &cancelled,
                        QueryPurpose::Enrichment,
                        &error.to_string(),
                        false,
                    );
                    return;
                }
            };
        let enrichment = compiled_enrichment
            .iter()
            .flat_map(|definition| definition.stages().iter().cloned())
            .collect::<Vec<_>>();
        let mut affected_outputs = changed_enrichment_outputs(
            &request.base_constraints.enrichments,
            &request.constraints.enrichments,
            &compiled_enrichment,
        );
        propagate_affected_outputs(&enrichment, &mut affected_outputs);
        let schema_seed = SchemaContext::default();
        (
            text,
            advanced,
            enrichment,
            affected_outputs,
            schema_seed.clone(),
            schema_seed,
            HashMap::new(),
            None,
        )
    };
    if cancelled.load(Ordering::Acquire) {
        return;
    }
    if request.constraints.capture_time.is_some()
        && request.constraints.time_basis == lvu::TimeBasis::Extracted
        && !enrichment.iter().any(|stage| stage.name == "timestamp_utc")
    {
        let purpose = if request.constraints.enrichments != request.base_constraints.enrichments {
            QueryPurpose::Enrichment
        } else {
            request.purpose
        };
        fail(
            tx,
            &request,
            &cancelled,
            purpose,
            "extracted time requires an accepted timestamp_utc enrichment; add it with e or use Alt-T in Time",
            false,
        );
        return;
    }
    let mut reservation = Reservation::new(budget);
    let mut derived = prior_membership
        .as_ref()
        .map_or_else(HashMap::new, |membership| membership.derived.clone());
    let mut evaluation_batches = prior_membership
        .as_ref()
        .map_or_else(Vec::new, |membership| {
            membership.evaluation_batches.to_vec()
        });
    if !reservation.add((sources.len() as u64).saturating_mul(SOURCE_OVERHEAD)) {
        fail(
            tx,
            &request,
            &cancelled,
            request.purpose,
            "membership memory cap reached; previous applied view preserved",
            true,
        );
        return;
    }
    let prior_sequence_bytes = prior_membership
        .as_ref()
        .map_or(0, |value| value.count.saturating_mul(SEQUENCE_BYTES));
    if !reservation.add(prior_sequence_bytes) {
        fail(
            tx,
            &request,
            &cancelled,
            request.purpose,
            "membership memory cap reached while retaining the applied snapshot",
            true,
        );
        return;
    }
    let prior_group_bytes = prior_membership.as_ref().map_or(0, |membership| {
        membership.sources.iter().fold(0_u64, |total, source| {
            source.groups.iter().fold(total, |subtotal, group| {
                subtotal.saturating_add(group_projection_bytes(group))
            })
        })
    });
    if !reservation.add(prior_group_bytes) {
        fail(
            tx,
            &request,
            &cancelled,
            QueryPurpose::Grouping,
            "display grouping projection memory cap reached while retaining the applied view",
            true,
        );
        return;
    }
    let prior_derived_bytes = derived
        .iter()
        .fold(0_u64, |total, ((source, _, field), value)| {
            total.saturating_add(
                value.as_ref().map_or(1, String::len) as u64
                    + source.len() as u64
                    + field.len() as u64
                    + 24,
            )
        });
    if !reservation.add(prior_derived_bytes) {
        fail(
            tx,
            &request,
            &cancelled,
            QueryPurpose::Enrichment,
            "derived value memory cap reached while retaining the applied snapshot",
            true,
        );
        return;
    }
    let prior_batch_bytes = evaluation_batches.iter().fold(0_u64, |total, batch| {
        total.saturating_add(evaluation_batch_bytes(batch))
    });
    if !reservation.add(prior_batch_bytes) {
        fail(
            tx,
            &request,
            &cancelled,
            request.purpose,
            "evaluation provenance memory cap reached while retaining the applied snapshot",
            true,
        );
        return;
    }
    let mut count = 0_u64;
    let mut scanned = 0u64;
    let mut runtime_diagnostic = None;
    let mut event_time_missing = prior_membership
        .as_ref()
        .map_or(0, |value| value.event_time_missing);
    let mut event_time_invalid = prior_membership
        .as_ref()
        .map_or(0, |value| value.event_time_invalid);
    let mut watermarks = Vec::new();
    let mut matched_sources = Vec::with_capacity(sources.len());
    for source in sources {
        let source_id = source.source_id().0.to_string();
        let generation = source.progress().generation;
        let prior_source_any = prior_membership.as_ref().and_then(|membership| {
            membership
                .sources
                .iter()
                .find(|item| item.source_id == source_id)
        });
        let prior_source = prior_source_any.filter(|item| item.generation == generation);
        if prior_source_any.is_some_and(|item| item.generation != generation) {
            derived.retain(|(derived_source, _, _), _| derived_source != &source_id);
            evaluation_batches.retain(|batch| batch.source_id != source_id);
        }
        let mut sequences = prior_source.map_or_else(Vec::new, |item| item.sequences.to_vec());
        let mut groups = prior_source.map_or_else(Vec::new, |item| item.groups.to_vec());
        count = count.saturating_add(sequences.len() as u64);
        let target = source.progress().high_watermark.map(|id| id.sequence);
        watermarks.push((source.source_id(), target));
        let (mut offset, mut last_sequence) = checkpoints
            .get(&source_id)
            .map(|(known_generation, offset, last)| {
                (
                    if *known_generation == generation {
                        *offset
                    } else {
                        0
                    },
                    *last,
                )
            })
            .unwrap_or((0, None));
        if target.is_none() {
            matched_sources.push(SourceMatches {
                source_id,
                generation,
                high_watermark: target,
                sequences: sequences.into(),
                groups: groups.into(),
            });
            continue;
        }
        let provenance_batches =
            evaluation_provenance
                .as_ref()
                .map_or_else(Vec::new, |membership| {
                    membership
                        .evaluation_batches
                        .iter()
                        .filter(|batch| {
                            batch.source_id == source_id && batch.generation == generation
                        })
                        .collect::<Vec<_>>()
                });
        let mut provenance_cursor = 0usize;
        loop {
            if cancelled.load(Ordering::Acquire) {
                return;
            }
            let provenance = provenance_batches.get(provenance_cursor).copied();
            let page = match runtime.block_on(
                source.read_page(
                    offset,
                    provenance.map_or(config.page_records, |batch| batch.record_count),
                    evaluation_provenance
                        .as_ref()
                        .map_or(config.page_bytes, |membership| {
                            membership.evaluation_page_bytes
                        }),
                ),
            ) {
                Ok(v) => v,
                Err(e) => {
                    fail(
                        tx,
                        &request,
                        &cancelled,
                        request.purpose,
                        &e.to_string(),
                        false,
                    );
                    return;
                }
            };
            if page.records.is_empty() {
                break;
            }
            let end_of_journal = page.end_of_journal;
            let page_start = offset;
            let saw_beyond_target = page
                .records
                .iter()
                .any(|record| target.is_some_and(|high| record.record_id.sequence > high));
            offset = if saw_beyond_target {
                page_start
            } else {
                page.next_offset
            };
            let records = page
                .records
                .into_iter()
                .filter(|record| last_sequence.is_none_or(|last| record.record_id.sequence > last))
                .take_while(|record| target.is_some_and(|high| record.record_id.sequence <= high))
                .collect::<Vec<_>>();
            if records.is_empty() {
                if end_of_journal || saw_beyond_target {
                    break;
                }
                continue;
            }
            if let Some(boundary) = provenance
                && (records.first().map(|record| record.record_id.sequence)
                    != Some(boundary.first_sequence)
                    || records.last().map(|record| record.record_id.sequence)
                        != Some(boundary.last_sequence))
            {
                fail(
                    tx,
                    &request,
                    &cancelled,
                    QueryPurpose::Enrichment,
                    "applied enrichment evaluation boundary no longer matches the journal",
                    false,
                );
                return;
            }
            let schema_before =
                provenance.map_or_else(|| schema.clone(), |batch| batch.schema_before.clone());
            let frame = if advanced.is_none()
                && enrichment.is_empty()
                && !text.as_ref().is_some_and(TextSearch::requires_projection)
            {
                literal_frame(&records)
            } else {
                let mut batch_schema = schema_before.clone();
                records_to_batch_with_context(&records, &mut batch_schema).map(|batch| {
                    schema = batch_schema;
                    batch.frame
                })
            };
            let frame = match frame {
                Ok(frame) => frame,
                Err(e) => {
                    fail(
                        tx,
                        &request,
                        &cancelled,
                        QueryPurpose::Advanced,
                        &format!("schema projection failed: {e}"),
                        false,
                    );
                    return;
                }
            };
            let result = execute_batch(
                &frame,
                BatchQuery {
                    generation: request.generation,
                    definition_generation: request.revision,
                    stages: enrichment.as_slice(),
                    filter: advanced.as_ref(),
                    text_search: text.as_ref(),
                    colors: &[],
                },
            );
            if !enrichment.is_empty()
                && let (Some(first), Some(last)) = (records.first(), records.last())
            {
                let batch = EvaluationBatch {
                    source_id: source_id.clone(),
                    generation,
                    record_count: records.len(),
                    first_sequence: first.record_id.sequence,
                    last_sequence: last.record_id.sequence,
                    schema_before,
                };
                if !reservation.add(evaluation_batch_bytes(&batch)) {
                    fail(
                        tx,
                        &request,
                        &cancelled,
                        request.purpose,
                        "evaluation provenance memory cap reached; previous applied view preserved",
                        true,
                    );
                    return;
                }
                evaluation_batches.push(batch);
            }
            let projected = enrichment
                .iter()
                .map(|stage| {
                    let stage_error = result.diagnostics.iter().find(|diagnostic| {
                        diagnostic.field.as_deref() == Some(stage.name.as_str())
                            && diagnostic.state == DerivedState::Error
                    });
                    let values = if let Some(diagnostic) = stage_error {
                        Err(diagnostic.message.clone())
                    } else {
                        scalar_projection(&result.enriched_rows, &stage.name, 512)
                    };
                    (stage, values)
                })
                .collect::<Vec<_>>();
            let candidate_projection_error = projected.iter().find_map(|(stage, values)| {
                affected_outputs
                    .contains(&stage.name)
                    .then(|| values.as_ref().err().cloned())
                    .flatten()
            });
            if let Some(message) = &candidate_projection_error {
                fail(
                    tx,
                    &request,
                    &cancelled,
                    QueryPurpose::Enrichment,
                    message,
                    false,
                );
                return;
            }
            let projection_error = projected
                .iter()
                .find_map(|(_, values)| values.as_ref().err());
            // An accepted enrichment can fail on newly appended records. In
            // that refresh case the uncertain records stay unmatched and the
            // per-stage diagnostics below remain visible; this is not a new
            // candidate failure and must not enqueue a terminal completion.
            let accepted_refresh_filter_error = cached_refresh
                && result.validity == BatchValidity::InvalidFilter
                && projection_error.is_some();
            if result.validity != BatchValidity::Valid && !accepted_refresh_filter_error {
                let message = result
                    .diagnostics
                    .iter()
                    .map(|d| d.message.as_str())
                    .collect::<Vec<_>>()
                    .join("; ");
                let advanced_depends_on_changed_enrichment =
                    advanced.as_ref().is_some_and(|filter| {
                        filter
                            .dependencies()
                            .iter()
                            .any(|dependency| affected_outputs.contains(dependency))
                    });
                let search_depends_on_changed_enrichment = text.as_ref().is_some_and(|search| {
                    search
                        .dependencies()
                        .iter()
                        .any(|field| affected_outputs.contains(field))
                });
                let purpose = if (!advanced_changed && advanced_depends_on_changed_enrichment)
                    || (request.constraints.text == request.base_constraints.text
                        && search_depends_on_changed_enrichment)
                {
                    QueryPurpose::Enrichment
                } else if advanced.is_some() {
                    QueryPurpose::Advanced
                } else {
                    QueryPurpose::Search
                };
                fail(tx, &request, &cancelled, purpose, &message, false);
                return;
            }
            for (stage, values) in projected {
                let projection = match values {
                    Err(error) => {
                        let message = bounded_text(format!("error: {error}"), 512);
                        let filter_diagnostic = (result.validity == BatchValidity::InvalidFilter)
                            .then_some("; dependent filter could not be evaluated");
                        runtime_diagnostic = Some(bounded_text(
                            format!(
                                "enrichment {} failed for new records: {}{}",
                                stage.name,
                                error,
                                filter_diagnostic.unwrap_or_default()
                            ),
                            512,
                        ));
                        records
                            .iter()
                            .map(|record| {
                                (
                                    lvu_query::StableRecordId {
                                        source_id: record.record_id.source_id.0.to_string(),
                                        sequence: record.record_id.sequence,
                                    },
                                    Some(message.clone()),
                                )
                            })
                            .collect()
                    }
                    Ok(values) => values,
                };
                for (id, value) in projection {
                    let bytes = value.as_ref().map_or(1, String::len) as u64
                        + id.source_id.len() as u64
                        + stage.name.len() as u64
                        + 24;
                    if !reservation.add(bytes) {
                        fail(
                            tx,
                            &request,
                            &cancelled,
                            QueryPurpose::Enrichment,
                            "derived value memory cap reached; previous applied view preserved",
                            true,
                        );
                        return;
                    }
                    derived.insert((id.source_id, id.sequence, stage.name.clone()), value);
                }
            }
            let mut matched_ids = if result.validity == BatchValidity::InvalidFilter {
                Vec::new()
            } else {
                result.matched_ids
            };
            // A declared field basis is read once for the whole batch rather
            // than per record, because an enriched column is only readable as a
            // compiled expression over the column as a whole.
            let selected = match (
                request.constraints.time_basis,
                request.constraints.time_field.as_deref(),
            ) {
                (lvu::TimeBasis::Selected, Some(token)) => {
                    Some(crate::time_basis::read_records(token, &records, |record| {
                        let column = match lvu_live::time::TimeFieldSelection::parse_token(token) {
                            Ok(selection) => match selection.field {
                                lvu_live::time::TimeFieldRef::Column(name) => name,
                                _ => return None,
                            },
                            Err(_) => return None,
                        };
                        derived
                            .get(&(source_id.clone(), record.record_id.sequence, column))
                            .and_then(|value| value.as_deref())
                    }))
                }
                (lvu::TimeBasis::Selected, None) => Some(crate::time_basis::SelectedTimes {
                    error: Some("no event-time field is declared for this basis".into()),
                    ..Default::default()
                }),
                _ => None,
            };
            if let Some(selected) = &selected {
                event_time_invalid += selected.invalid;
                event_time_missing += selected.missing;
            }
            if let Some(window) = request.constraints.capture_time {
                let capture_times: HashMap<_, _> = records
                    .iter()
                    .filter_map(|record| {
                        let timestamp = match request.constraints.time_basis {
                            lvu::TimeBasis::Capture => Some(record.captured_at_unix_nanos),
                            lvu::TimeBasis::Extracted => {
                                let value = derived
                                    .get(&(
                                        source_id.clone(),
                                        record.record_id.sequence,
                                        "timestamp_utc".into(),
                                    ))
                                    .and_then(|value| value.as_deref());
                                match value {
                                    Some(value) => match lvu::parse_utc_nanos(value) {
                                        Ok(timestamp) => Some(timestamp),
                                        Err(_) => {
                                            event_time_invalid += 1;
                                            None
                                        }
                                    },
                                    None => {
                                        event_time_missing += 1;
                                        None
                                    }
                                }
                            }
                            lvu::TimeBasis::Selected => selected
                                .as_ref()
                                .and_then(|selected| {
                                    selected.by_sequence.get(&record.record_id.sequence)
                                })
                                .copied(),
                            lvu::TimeBasis::Event => {
                                match lvu_live::recognize_event_time(&record.bytes) {
                                    lvu_live::EventTimeRecognition::Valid {
                                        unix_nanos, ..
                                    } => Some(unix_nanos),
                                    lvu_live::EventTimeRecognition::Invalid { .. } => {
                                        event_time_invalid += 1;
                                        None
                                    }
                                    lvu_live::EventTimeRecognition::Missing => {
                                        event_time_missing += 1;
                                        None
                                    }
                                }
                            }
                        };
                        timestamp.map(|timestamp| (record.record_id.sequence, timestamp))
                    })
                    .collect();
                matched_ids.retain(|id| {
                    capture_times.get(&id.sequence).is_some_and(|timestamp| {
                        *timestamp >= window.start_unix_nanos && *timestamp < window.end_unix_nanos
                    })
                });
            }
            let matched_sequences = matched_ids
                .iter()
                .map(|id| id.sequence)
                .collect::<std::collections::HashSet<_>>();
            let mut previous_physical_matched = last_sequence
                .zip(sequences.last().copied())
                .is_some_and(|(last, matched)| last == matched);
            for record in &records {
                if !matched_sequences.contains(&record.record_id.sequence) {
                    previous_physical_matched = false;
                    continue;
                }
                if !reservation.add(SEQUENCE_BYTES) {
                    fail(
                        tx,
                        &request,
                        &cancelled,
                        request.purpose,
                        "membership memory cap reached; previous applied view preserved",
                        true,
                    );
                    return;
                }
                let sequence_index = sequences.len();
                sequences.push(record.record_id.sequence);
                count += 1;
                if let Some(rule) = &grouping_rule {
                    let mut projection = lvu_live::display_projection(
                        record,
                        MAX_GROUP_LINE_DISPLAY_BYTES,
                        MAX_GROUP_LINE_PROJECTION_BYTES,
                    );
                    for stage in &enrichment {
                        let value = derived
                            .get(&(
                                source_id.clone(),
                                record.record_id.sequence,
                                stage.name.clone(),
                            ))
                            .and_then(Clone::clone)
                            .unwrap_or_else(|| "null".into());
                        projection.fields.retain(|(field, _)| field != &stage.name);
                        projection.fields.push((stage.name.clone(), value.clone()));
                        projection
                            .details
                            .push((format!("derived.{}", stage.name), value));
                    }
                    let projection_bytes = display_projection_bytes(&projection);
                    if !reservation.add(projection_bytes) {
                        fail(
                            tx,
                            &request,
                            &cancelled,
                            QueryPurpose::Grouping,
                            "display grouping projection memory cap reached; previous view preserved",
                            true,
                        );
                        return;
                    }
                    let continuation = rule.matches(&record.bytes);
                    let same_stream = groups
                        .last()
                        .is_some_and(|group| group.stream == record.stream);
                    let can_extend = continuation
                        && previous_physical_matched
                        && same_stream
                        && groups.last().is_some_and(|group| {
                            group.len < MAX_GROUP_LINES
                                && group.bytes.saturating_add(record.bytes.len()) <= MAX_GROUP_BYTES
                        });
                    if can_extend {
                        let group = groups.last_mut().expect("checked group");
                        group.len += 1;
                        group.bytes = group.bytes.saturating_add(record.bytes.len());
                        Arc::make_mut(&mut group.projection).push(projection);
                    } else {
                        if !reservation.add(64) {
                            fail(
                                tx,
                                &request,
                                &cancelled,
                                QueryPurpose::Grouping,
                                "display grouping index memory cap reached; previous view preserved",
                                true,
                            );
                            return;
                        }
                        groups.push(GroupRange {
                            start: sequence_index,
                            len: 1,
                            bytes: record.bytes.len(),
                            stream: record.stream,
                            orphan: continuation,
                            split: continuation && previous_physical_matched && same_stream,
                            oversized: record.bytes.len() > MAX_GROUP_BYTES,
                            projection: Arc::new(vec![projection]),
                        });
                    }
                }
                previous_physical_matched = true;
            }
            scanned = scanned.saturating_add(records.len() as u64);
            provenance_cursor += usize::from(provenance.is_some());
            last_sequence = records
                .last()
                .map(|record| record.record_id.sequence)
                .or(last_sequence);
            let _ = tx.try_send(Update::Progress {
                view_id: request.view_id.clone(),
                revision: request.revision,
                scanned,
                token: Arc::clone(&cancelled),
            });
            if end_of_journal
                || records
                    .last()
                    .is_some_and(|record| Some(record.record_id.sequence) == target)
            {
                break;
            }
        }
        checkpoints.insert(source_id.clone(), (generation, offset, last_sequence));
        matched_sources.push(SourceMatches {
            source_id,
            generation,
            high_watermark: target,
            sequences: sequences.into(),
            groups: groups.into(),
        });
    }
    if request.constraints.time_basis != lvu::TimeBasis::Capture
        && request.constraints.capture_time.is_some()
        && (event_time_missing > 0 || event_time_invalid > 0)
    {
        let event_diagnostic = format!(
            "event time: {event_time_missing} missing, {event_time_invalid} invalid/ambiguous; unmatched (no capture-time fallback; basis {:?})",
            request.constraints.time_basis
        );
        let combined = match runtime_diagnostic {
            Some(existing) => format!("{existing}; {event_diagnostic}"),
            None => event_diagnostic,
        };
        runtime_diagnostic = Some(bounded_text(combined, 512));
    }
    if cancelled.load(Ordering::Acquire) {
        return;
    }
    let index_bytes = reservation.bytes;
    let membership = reservation.finish(
        matched_sources,
        count,
        enrichment.iter().map(|stage| stage.name.clone()).collect(),
        derived,
        advanced.clone(),
        enrichment.clone(),
        config.page_bytes,
        evaluation_batches,
        event_time_missing,
        event_time_invalid,
        grouping_rule.is_some(),
    );
    prepared.insert(
        cache_key,
        PreparedDefinition {
            revision: request.revision,
            constraints: request.constraints.clone(),
            text,
            advanced,
            enrichment,
            grouping: grouping_rule,
            schema_seed,
            schema,
            checkpoints,
            membership: Some(Arc::clone(&membership)),
        },
    );
    let _ = send_update(
        tx,
        Update::Publish {
            request,
            published: Published::Filtered { membership },
            scanned,
            watermarks,
            index_bytes,
            diagnostic: runtime_diagnostic,
            token: Arc::clone(&cancelled),
        },
        cancelled.as_ref(),
    );
}

fn bounded_text(mut value: String, maximum_bytes: usize) -> String {
    if value.len() > maximum_bytes {
        let mut end = maximum_bytes;
        while !value.is_char_boundary(end) {
            end -= 1;
        }
        value.truncate(end);
    }
    value
}

fn native_enrichment_definitions(
    constraints: &lvu::QueryConstraints,
) -> Vec<NativeEnrichmentDefinition> {
    constraints
        .enrichments
        .iter()
        .map(|definition| NativeEnrichmentDefinition {
            id: NativeEnrichmentStageId(definition.id.0.clone()),
            source: definition.source.clone(),
        })
        .collect()
}

fn changed_enrichment_outputs(
    base: &[lvu::EnrichmentDefinition],
    candidate: &[lvu::EnrichmentDefinition],
    compiled: &[CompiledEnrichment],
) -> HashSet<String> {
    let unchanged = |id: &str, source: &str, definitions: &[lvu::EnrichmentDefinition]| {
        definitions
            .iter()
            .any(|item| item.id.0 == id && item.source == source)
    };
    let mut affected = HashSet::new();
    for definition in compiled {
        if !unchanged(
            &definition.definition.id.0,
            &definition.definition.source,
            base,
        ) {
            affected.extend(definition.outputs.iter().map(|output| output.name.clone()));
        }
    }
    for definition in base {
        if !unchanged(&definition.id.0, &definition.source, candidate) {
            affected.extend(enrichment_output_names(&definition.source));
        }
    }
    affected
}

fn enrichment_output_names(source: &str) -> Vec<String> {
    match parse_regex_enrichment(source) {
        Ok(Some(plan)) => plan
            .outputs()
            .iter()
            .map(|output| output.name.clone())
            .collect(),
        _ => source
            .split_once('=')
            .map(|(name, _)| vec![name.trim().to_owned()])
            .unwrap_or_default(),
    }
}

fn propagate_affected_outputs(stages: &[EnrichmentStage], affected: &mut HashSet<String>) {
    loop {
        let mut changed = false;
        for stage in stages {
            if !affected.contains(&stage.name)
                && stage
                    .definition
                    .dependencies()
                    .iter()
                    .any(|dependency| affected.contains(dependency))
            {
                changed |= affected.insert(stage.name.clone());
            }
        }
        if !changed {
            break;
        }
    }
}

fn fail(
    tx: &mpsc::SyncSender<Update>,
    request: &QueryRequest,
    token: &Arc<AtomicBool>,
    purpose: QueryPurpose,
    message: &str,
    limited: bool,
) {
    let _ = tx.send(Update::Failed {
        request: request.clone(),
        purpose,
        message: message.chars().take(512).collect(),
        limited,
        token: Arc::clone(token),
    });
}

fn send_update(tx: &mpsc::SyncSender<Update>, mut update: Update, cancelled: &AtomicBool) -> bool {
    loop {
        match tx.try_send(update) {
            Ok(()) => return true,
            Err(mpsc::TrySendError::Disconnected(_)) => return false,
            Err(mpsc::TrySendError::Full(returned)) => {
                if cancelled.load(Ordering::Acquire) {
                    return false;
                }
                update = returned;
                thread::sleep(Duration::from_millis(2));
            }
        }
    }
}

fn literal_frame(records: &[lvu_core::RawRecord]) -> polars::prelude::PolarsResult<DataFrame> {
    DataFrame::new(
        records.len(),
        vec![
            Column::from(Series::new(
                lvu_query::SOURCE_ID_COLUMN.into(),
                records
                    .iter()
                    .map(|record| record.record_id.source_id.0.to_string())
                    .collect::<Vec<_>>(),
            )),
            Column::from(Series::new(
                lvu_query::SEQUENCE_COLUMN.into(),
                records
                    .iter()
                    .map(|record| record.record_id.sequence)
                    .collect::<Vec<_>>(),
            )),
            Column::from(Series::new(
                lvu_query::RAW_COLUMN.into(),
                records
                    .iter()
                    .map(|record| String::from_utf8_lossy(&record.bytes).into_owned())
                    .collect::<Vec<_>>(),
            )),
        ],
    )
}

fn membership_ids(membership: &Membership, start: usize, len: usize) -> Vec<RowId> {
    let mut skipped = start;
    let mut result = Vec::with_capacity(len);
    for source in &membership.sources {
        if skipped >= source.sequences.len() {
            skipped -= source.sequences.len();
            continue;
        }
        for sequence in source
            .sequences
            .iter()
            .skip(skipped)
            .take(len - result.len())
        {
            result.push(RowId::new(source.source_id.clone(), *sequence));
        }
        skipped = 0;
        if result.len() == len {
            break;
        }
    }
    result
}

#[derive(Clone)]
struct DisplayGroup {
    orphan: bool,
    split: bool,
    oversized: bool,
    projection: Arc<Vec<DisplayRow>>,
}

fn membership_display_count(membership: &Membership) -> usize {
    if membership.grouped {
        membership
            .sources
            .iter()
            .map(|source| source.groups.len())
            .sum()
    } else {
        usize::try_from(membership.count).unwrap_or(usize::MAX)
    }
}

fn membership_groups(membership: &Membership, start: usize, len: usize) -> Vec<DisplayGroup> {
    let mut skipped = start;
    let mut result = Vec::with_capacity(len);
    for source in &membership.sources {
        if skipped >= source.groups.len() {
            skipped -= source.groups.len();
            continue;
        }
        for group in source.groups.iter().skip(skipped).take(len - result.len()) {
            result.push(DisplayGroup {
                orphan: group.orphan,
                split: group.split,
                oversized: group.oversized,
                projection: Arc::clone(&group.projection),
            });
        }
        skipped = 0;
        if result.len() == len {
            break;
        }
    }
    result
}

fn membership_group_index(membership: &Membership, wanted: &RowId) -> Option<usize> {
    let mut prefix = 0usize;
    for source in &membership.sources {
        if source.source_id == wanted.source_id {
            let sequence = source.sequences.binary_search(&wanted.sequence).ok()?;
            return group_index_for_sequence(&source.groups, sequence).map(|index| prefix + index);
        }
        prefix = prefix.saturating_add(source.groups.len());
    }
    None
}

fn group_index_for_sequence(groups: &[GroupRange], sequence_index: usize) -> Option<usize> {
    let boundary = groups.partition_point(|group| group.start <= sequence_index);
    let index = boundary.checked_sub(1)?;
    let group = &groups[index];
    (sequence_index < group.start.saturating_add(group.len)).then_some(index)
}

fn membership_group_for_id(membership: &Membership, wanted: &RowId) -> Option<DisplayGroup> {
    let index = membership_group_index(membership, wanted)?;
    membership_groups(membership, index, 1).pop()
}

fn project_group(
    mut members: Vec<DisplayRow>,
    orphan: bool,
    split: bool,
    oversized: bool,
) -> DisplayRow {
    let mut head = members.remove(0);
    let line_count = members.len() + 1;
    head.details.push((
        "grouping".into(),
        "display-only; physical records unchanged".into(),
    ));
    head.details
        .push(("group_line_count".into(), line_count.to_string()));
    if orphan {
        head.details
            .push(("group_boundary".into(), "orphan continuation".into()));
    }
    if split {
        head.details
            .push(("group_overflow".into(), "bounded split".into()));
    }
    if oversized {
        head.details.push((
            "group_oversized_record".into(),
            format!(
                "leading physical record exceeds the {MAX_GROUP_BYTES}-byte soft group limit; preserved alone"
            ),
        ));
    }
    let first_text = head.text.clone();
    head.details.push((
        "group_line_1".into(),
        format!("{}: {}", head.id, first_text),
    ));
    for (index, member) in members.into_iter().enumerate() {
        head.details.push((
            format!("group_line_{}", index + 2),
            format!("{}: {}", member.id, member.text),
        ));
    }
    if line_count > 1 || orphan {
        let label = if orphan {
            "orphan continuation"
        } else {
            "physical lines"
        };
        head.text = format!("{}  [{} {label}]", head.text, line_count);
    }
    head
}

fn membership_index(membership: &Membership, wanted: &RowId) -> Option<usize> {
    let mut prefix = 0usize;
    for source in &membership.sources {
        if source.source_id == wanted.source_id {
            return source
                .sequences
                .binary_search(&wanted.sequence)
                .ok()
                .map(|index| prefix + index);
        }
        prefix = prefix.saturating_add(source.sequences.len());
    }
    None
}

#[derive(Clone)]
struct ContinuationRule(regex::bytes::Regex);

impl ContinuationRule {
    fn parse(source: &str) -> Result<Self, String> {
        if source.len() > MAX_GROUP_REGEX_BYTES {
            return Err(format!(
                "display grouping regex exceeds the {MAX_GROUP_REGEX_BYTES}-byte source limit"
            ));
        }
        regex::bytes::RegexBuilder::new(source)
            .size_limit(MAX_GROUP_REGEX_COMPILED_BYTES)
            .nest_limit(MAX_GROUP_REGEX_NESTING)
            .build()
            .map(Self)
            .map_err(|error| format!("invalid display grouping regex: {error}"))
    }

    fn matches(&self, bytes: &[u8]) -> bool {
        self.0.is_match(bytes)
    }
}

#[cfg(test)]
mod grouping_tests {
    use super::*;

    #[test]
    fn rust_regex_semantics_and_complexity_limits_are_enforced() {
        let rule = ContinuationRule::parse(r"^(?:[[:space:]]+|Caused\s+by:|\[continued\]\s{1,3})")
            .unwrap();
        assert!(rule.matches(b"  at frame"));
        assert!(rule.matches(b"Caused   by: disk"));
        assert!(rule.matches(b"[continued] \xff"));
        assert!(!rule.matches(b"ordinary event"));
        assert!(ContinuationRule::parse(r"(?=lookaround)").is_err());
        assert!(ContinuationRule::parse(r"^(a)\1$").is_err());

        let nested = format!("^{}x{}", "(".repeat(65), ")".repeat(65));
        assert!(ContinuationRule::parse(&nested).is_err());
        assert!(ContinuationRule::parse(&"x".repeat(MAX_GROUP_REGEX_BYTES + 1)).is_err());
    }

    #[test]
    fn group_index_uses_ordered_boundaries_for_large_memberships() {
        let groups = (0..10_000)
            .map(|index| GroupRange {
                start: index * 3,
                len: 3,
                bytes: 0,
                stream: lvu_core::StreamKind::File,
                orphan: false,
                split: false,
                oversized: false,
                projection: Arc::new(Vec::new()),
            })
            .collect::<Vec<_>>();
        assert_eq!(group_index_for_sequence(&groups, 0), Some(0));
        assert_eq!(group_index_for_sequence(&groups, 17), Some(5));
        assert_eq!(group_index_for_sequence(&groups, 29_999), Some(9_999));
        assert_eq!(group_index_for_sequence(&groups, 30_000), None);
    }
}
