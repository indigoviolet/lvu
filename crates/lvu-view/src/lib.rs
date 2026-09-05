//! Native, bounded live-view query adapter.

mod export;
pub use export::*;

use lvu::{
    DisplayRow, QueryCompletion, QueryFailure, QueryPurpose, QueryRequest, RowId, RowPage,
    RowProvider, ViewportRequest, terminal::QueryDispatcher,
};
use lvu_core::SourceId;
use lvu_ingest::SourceHandle;
use lvu_live::LiveRowProvider;
use lvu_query::{
    BatchQuery, BatchValidity, CompilerHost, CompilerHostConfig, DerivedState, EnrichmentStage,
    ExpressionKind, SchemaContext, TextSearch, execute_batch, records_to_batch_with_context,
    scalar_projection,
};
use polars::prelude::{Column, DataFrame, NamedFrom, Series};
use std::{
    collections::{HashMap, VecDeque},
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
    enrichment: Option<EnrichmentStage>,
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
}

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
    enrichment_name: Option<String>,
    derived: HashMap<(String, u64), Option<String>>,
    advanced: Option<lvu_query::CompiledDefinition>,
    enrichment: Option<EnrichmentStage>,
    evaluation_page_bytes: usize,
    evaluation_batches: Arc<[EvaluationBatch]>,
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
        enrichment_name: Option<String>,
        derived: HashMap<(String, u64), Option<String>>,
        advanced: Option<lvu_query::CompiledDefinition>,
        enrichment: Option<EnrichmentStage>,
        evaluation_page_bytes: usize,
        evaluation_batches: Vec<EvaluationBatch>,
    ) -> Arc<Membership> {
        self.committed = true;
        Arc::new(Membership {
            sources,
            count,
            bytes: self.bytes,
            budget: Arc::clone(&self.budget),
            enrichment_name,
            derived,
            advanced,
            enrichment,
            evaluation_page_bytes,
            evaluation_batches: evaluation_batches.into(),
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

#[derive(Clone)]
enum Published {
    Raw,
    Filtered { membership: Arc<Membership> },
}

struct ViewState {
    registration: ViewRegistration,
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
}

struct Shared {
    accepting: bool,
    sources: HashMap<SourceId, SourceRegistration>,
    views: HashMap<String, ViewState>,
}

enum Work {
    Query(QueryRequest),
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
    raw: Arc<LiveRowProvider>,
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
}

/// Cloneable read-only half for terminal composition. Keep the adapter itself as
/// the mutable `QueryDispatcher` and pass this handle as the `RowProvider`.
#[derive(Clone)]
pub struct NativeViewRows {
    raw: Arc<LiveRowProvider>,
    config: ViewConfig,
    shared: Arc<Mutex<Shared>>,
}

impl NativeViewAdapter {
    pub fn new(raw: Arc<LiveRowProvider>, config: ViewConfig) -> Result<Self, ViewError> {
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
        let worker = thread::Builder::new()
            .name("lvu-view-query".into())
            .spawn(move || {
                worker_loop(
                    worker_config,
                    worker_shared,
                    work_rx,
                    update_tx,
                    worker_budget,
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
        })
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
            .map_err(|e| ViewError::Live(e.to_string()))?;
        let mut shared = self.shared.lock().expect("view state poisoned");
        let source_id = handle.source_id();
        shared
            .sources
            .insert(source_id, SourceRegistration { handle });
        for view in shared
            .views
            .values_mut()
            .filter(|view| view.registration.sources.contains(&source_id))
        {
            view.cancel.store(true, Ordering::Release);
            view.cancel = Arc::new(AtomicBool::new(false));
            view.refreshing = false;
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
        if shared
            .views
            .values()
            .all(|view| !view.registration.sources.contains(&source_id))
        {
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
                },
            );
        }
        self.raw
            .register_raw_view(raw_view, deduped)
            .map_err(|e| ViewError::Live(e.to_string()))
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

impl QueryDispatcher for NativeViewAdapter {
    fn submit(&mut self, request: QueryRequest) -> Result<(), String> {
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
            view.cancel = Arc::new(AtomicBool::new(false));
            view.desired_revision = request.revision;
            view.status.state = ScanState::Pending;
            view.status.revision = request.revision;
            view.status.diagnostic = None;
            if tx.try_send(Work::Query(request.clone())).is_err() {
                view.cancel = old_cancel;
                view.desired_revision = old_desired;
                view.status = old_status;
                return Err("query queue is full".into());
            }
            old_cancel.store(true, Ordering::Release);
        }
        self.admitted
            .insert(request.view_id.clone(), request.revision);
        Ok(())
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

    fn revision(&self, view_id: &str) -> u64 {
        self.rows().revision(view_id)
    }
}

impl RowProvider for NativeViewRows {
    fn page(&self, view_id: &str, request: ViewportRequest) -> RowPage {
        let shared = self.shared.lock().expect("view state poisoned");
        let Some(view) = shared.views.get(view_id) else {
            return RowPage {
                total: 0,
                rows: Vec::new(),
            };
        };
        match &view.published {
            Published::Raw => self.raw.page(&view.registration.raw_view, request),
            Published::Filtered { membership } => {
                let raw_view = &view.registration.raw_view;
                let total = usize::try_from(membership.count).unwrap_or(usize::MAX);
                let len = request
                    .len
                    .min(self.config.maximum_viewport_rows)
                    .min(total.saturating_sub(request.start));
                let ids = membership_ids(membership, request.start, len);
                let mut rows = Vec::with_capacity(ids.len());
                for id in ids {
                    let row = self.raw.row_by_id(raw_view, &id);
                    let Some(row) = row else { break };
                    rows.push(with_enrichment(row, membership));
                }
                RowPage { total, rows }
            }
        }
    }

    fn row_by_id(&self, view_id: &str, id: &RowId) -> Option<DisplayRow> {
        let shared = self.shared.lock().expect("view state poisoned");
        let view = shared.views.get(view_id)?;
        match &view.published {
            Published::Raw => self.raw.row_by_id(&view.registration.raw_view, id),
            Published::Filtered { membership } => {
                membership_index(membership, id)?;
                self.raw
                    .row_by_id(&view.registration.raw_view, id)
                    .map(|row| with_enrichment(row, membership))
            }
        }
    }

    fn index_of_id(&self, view_id: &str, id: &RowId) -> Option<usize> {
        let shared = self.shared.lock().expect("view state poisoned");
        let view = shared.views.get(view_id)?;
        match &view.published {
            Published::Raw => self.raw.index_of_id(&view.registration.raw_view, id),
            Published::Filtered { membership } => membership_index(membership, id),
        }
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
            })
    }
}

fn with_enrichment(mut row: DisplayRow, membership: &Membership) -> DisplayRow {
    if let Some(name) = &membership.enrichment_name {
        let value = membership
            .derived
            .get(&(row.id.source_id.clone(), row.id.sequence))
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
                        .registration
                        .sources
                        .iter()
                        .filter_map(|id| state.sources.get(id).map(|s| s.handle.clone()))
                        .collect::<Vec<_>>();
                    (sources, Arc::clone(&view.cancel))
                };
                run_query(
                    &runtime,
                    &config,
                    &mut compiler,
                    request,
                    snapshot.0,
                    snapshot.1,
                    &tx,
                    &mut prepared,
                    Arc::clone(&budget),
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
) {
    if request.constraints.text.is_none()
        && request.constraints.advanced_polars.is_none()
        && request.constraints.enrichment.is_none()
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
    let cached = prepared
        .get(&cache_key)
        .filter(|value| {
            value.revision == request.revision && value.constraints == request.constraints
        })
        .cloned();
    let candidate_enrichment =
        cached.is_none() && request.constraints.enrichment != request.base_constraints.enrichment;
    let (text, advanced, enrichment, schema_seed, mut schema, mut checkpoints, prior_membership) =
        if let Some(cached) = cached {
            (
                cached.text,
                cached.advanced,
                cached.enrichment,
                cached.schema_seed,
                cached.schema,
                cached.checkpoints,
                cached.membership,
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
                Some(value) => match TextSearch::new(value.literal.clone()) {
                    Ok(v) => Some(v),
                    Err(e) => {
                        fail(
                            tx,
                            &request,
                            &cancelled,
                            QueryPurpose::Search,
                            &e.to_string(),
                            false,
                        );
                        return;
                    }
                },
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
            let enrichment = match request.constraints.enrichment.as_deref() {
                Some(source) => {
                    let Some((name, expression)) = source.split_once('=') else {
                        fail(
                            tx,
                            &request,
                            &cancelled,
                            QueryPurpose::Enrichment,
                            "use: name = Polars expression",
                            false,
                        );
                        return;
                    };
                    let name = name.trim();
                    if name.is_empty()
                        || name.len() > 64
                        || name == "raw"
                        || name.starts_with("_lvu_")
                        || !name.chars().all(|c| c == '_' || c.is_alphanumeric())
                    {
                        fail(
                            tx,
                            &request,
                            &cancelled,
                            QueryPurpose::Enrichment,
                            "invalid or protected enrichment name (maximum 64 UTF-8 bytes)",
                            false,
                        );
                        return;
                    }
                    let Some(host) = compiler.as_mut() else {
                        fail(
                            tx,
                            &request,
                            &cancelled,
                            QueryPurpose::Enrichment,
                            "enrichment compiler is not configured",
                            false,
                        );
                        return;
                    };
                    match host.compile(
                        expression.trim(),
                        ExpressionKind::Enrichment,
                        cancelled.as_ref(),
                    ) {
                        Ok(definition) => Some(EnrichmentStage {
                            name: name.into(),
                            definition,
                        }),
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
                    }
                }
                None => None,
            };
            let schema_seed = SchemaContext::default();
            (
                text,
                advanced,
                enrichment,
                schema_seed.clone(),
                schema_seed,
                HashMap::new(),
                None,
            )
        };
    if cancelled.load(Ordering::Acquire) {
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
    let prior_derived_bytes = derived.values().fold(0_u64, |total, value| {
        total.saturating_add(value.as_ref().map_or(1, String::len) as u64 + 24)
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
    let mut count = prior_membership.as_ref().map_or(0, |value| value.count);
    let mut scanned = 0u64;
    let mut runtime_diagnostic = None;
    let mut watermarks = Vec::new();
    let mut matched_sources = Vec::with_capacity(sources.len());
    for source in sources {
        let source_id = source.source_id().0.to_string();
        let mut sequences = prior_membership
            .as_ref()
            .and_then(|membership| {
                membership
                    .sources
                    .iter()
                    .find(|item| item.source_id == source_id)
            })
            .map_or_else(Vec::new, |item| item.sequences.to_vec());
        let target = source.progress().high_watermark.map(|id| id.sequence);
        watermarks.push((source.source_id(), target));
        let generation = source.progress().generation;
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
            });
            continue;
        }
        loop {
            if cancelled.load(Ordering::Acquire) {
                return;
            }
            let page = match runtime.block_on(source.read_page(
                offset,
                config.page_records,
                config.page_bytes,
            )) {
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
                break;
            }
            let schema_before = schema.clone();
            let frame = if advanced.is_none() && enrichment.is_none() {
                literal_frame(&records)
            } else {
                records_to_batch_with_context(&records, &mut schema).map(|batch| batch.frame)
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
            if enrichment.is_some()
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
            let stage_error = enrichment.as_ref().and_then(|stage| {
                result.diagnostics.iter().find(|diagnostic| {
                    diagnostic.field.as_deref() == Some(stage.name.as_str())
                        && diagnostic.state == DerivedState::Error
                })
            });
            let projected = enrichment.as_ref().map(|stage| {
                if let Some(diagnostic) = stage_error {
                    Err(diagnostic.message.clone())
                } else {
                    scalar_projection(&result.enriched_rows, &stage.name, 512)
                }
            });
            let projection_error = projected
                .as_ref()
                .and_then(|value| value.as_ref().err())
                .cloned();
            if candidate_enrichment && let Some(message) = &projection_error {
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
            if result.validity != BatchValidity::Valid && projection_error.is_none() {
                let message = result
                    .diagnostics
                    .iter()
                    .map(|d| d.message.as_str())
                    .collect::<Vec<_>>()
                    .join("; ");
                let purpose = if advanced.is_some() {
                    QueryPurpose::Advanced
                } else {
                    QueryPurpose::Search
                };
                fail(tx, &request, &cancelled, purpose, &message, false);
                return;
            }
            if let Some(stage) = enrichment.as_ref() {
                let projection = if let Some(error) = &projection_error {
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
                } else {
                    projected
                        .expect("enrichment projection exists")
                        .expect("checked above")
                };
                for (id, value) in projection {
                    let bytes = value.as_ref().map_or(1, String::len) as u64 + 24;
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
                    derived.insert((id.source_id, id.sequence), value);
                }
            }
            let matched_ids = if result.validity == BatchValidity::InvalidFilter {
                Vec::new()
            } else {
                result.matched_ids
            };
            for id in matched_ids {
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
                sequences.push(id.sequence);
                count += 1;
            }
            scanned = scanned.saturating_add(records.len() as u64);
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
        });
    }
    if cancelled.load(Ordering::Acquire) {
        return;
    }
    let index_bytes = reservation.bytes;
    let membership = reservation.finish(
        matched_sources,
        count,
        enrichment.as_ref().map(|stage| stage.name.clone()),
        derived,
        advanced.clone(),
        enrichment.clone(),
        config.page_bytes,
        evaluation_batches,
    );
    prepared.insert(
        cache_key,
        PreparedDefinition {
            revision: request.revision,
            constraints: request.constraints.clone(),
            text,
            advanced,
            enrichment,
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
