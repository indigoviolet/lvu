use crate::index::DiskIndex;
use lvu::{DisplayRow, RowId, RowPage, RowProvider, ViewportRequest};
use lvu_core::{ChunkPosition, RawRecord, SourceId};
use lvu_ingest::{RuntimeState, SourceHandle};
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{Arc, Mutex},
};
use thiserror::Error;
use tokio::{
    sync::{mpsc, oneshot, watch},
    task::JoinHandle,
};

#[derive(Clone, Copy)]
struct DiskMeta {
    count: u64,
    next_offset: u64,
    high_sequence: Option<u64>,
}

enum DiskCommand {
    Append {
        page_offset: u64,
        next_offset: u64,
        records: Vec<RawRecord>,
        maximum_bytes: u64,
        reply: oneshot::Sender<std::io::Result<DiskMeta>>,
    },
    Entries {
        start: u64,
        len: usize,
        reply: oneshot::Sender<std::io::Result<Vec<crate::index::IndexEntry>>>,
    },
    Find {
        sequence: u64,
        reply: oneshot::Sender<std::io::Result<Option<crate::index::IndexEntry>>>,
    },
}

struct DiskService {
    commands: Option<std::sync::mpsc::SyncSender<DiskCommand>>,
    task: JoinHandle<()>,
    meta: DiskMeta,
}

impl DiskService {
    async fn open(
        path: PathBuf,
        source: SourceId,
        generation: u64,
        page_records: usize,
        page_bytes: usize,
        maximum_bytes: u64,
    ) -> Result<(Self, bool), String> {
        let (commands, receiver) = std::sync::mpsc::sync_channel(1);
        let (opened_tx, opened_rx) = oneshot::channel();
        let task = tokio::task::spawn_blocking(move || {
            let opened = DiskIndex::open(
                &path,
                source,
                generation,
                page_records,
                page_bytes,
                maximum_bytes,
            );
            let Ok((mut index, rebuilt)) = opened else {
                let _ = opened_tx.send(opened.map(|_| unreachable!()));
                return;
            };
            let meta = disk_meta(&index);
            if opened_tx.send(Ok((meta, rebuilt))).is_err() {
                return;
            }
            while let Ok(command) = receiver.recv() {
                match command {
                    DiskCommand::Append {
                        page_offset,
                        next_offset,
                        records,
                        maximum_bytes,
                        reply,
                    } => {
                        let result = index
                            .append_page(page_offset, next_offset, &records, maximum_bytes)
                            .map(|()| disk_meta(&index));
                        let _ = reply.send(result);
                    }
                    DiskCommand::Entries { start, len, reply } => {
                        let _ = reply.send(index.entries(start, len));
                    }
                    DiskCommand::Find { sequence, reply } => {
                        let _ = reply.send(index.find_sequence(sequence));
                    }
                }
            }
        });
        let (meta, rebuilt) = opened_rx
            .await
            .map_err(|_| "derived index worker stopped during open".to_owned())?
            .map_err(|error| error.to_string())?;
        Ok((
            Self {
                commands: Some(commands),
                task,
                meta,
            },
            rebuilt,
        ))
    }

    async fn append(
        &mut self,
        page_offset: u64,
        next_offset: u64,
        records: Vec<RawRecord>,
        maximum_bytes: u64,
    ) -> Result<(), (std::io::ErrorKind, String)> {
        let (reply, receive) = oneshot::channel();
        self.send(DiskCommand::Append {
            page_offset,
            next_offset,
            records,
            maximum_bytes,
            reply,
        })
        .map_err(|message| (std::io::ErrorKind::BrokenPipe, message))?;
        self.meta = receive
            .await
            .map_err(|_| {
                (
                    std::io::ErrorKind::BrokenPipe,
                    "derived index worker stopped during append".to_owned(),
                )
            })?
            .map_err(|error| (error.kind(), error.to_string()))?;
        Ok(())
    }

    async fn entries(
        &self,
        start: u64,
        len: usize,
    ) -> Result<Vec<crate::index::IndexEntry>, String> {
        let (reply, receive) = oneshot::channel();
        self.send(DiskCommand::Entries { start, len, reply })?;
        receive
            .await
            .map_err(|_| "derived index worker stopped during read".to_owned())?
            .map_err(|error| error.to_string())
    }

    async fn find(&self, sequence: u64) -> Result<Option<crate::index::IndexEntry>, String> {
        let (reply, receive) = oneshot::channel();
        self.send(DiskCommand::Find { sequence, reply })?;
        receive
            .await
            .map_err(|_| "derived index worker stopped during lookup".to_owned())?
            .map_err(|error| error.to_string())
    }

    fn send(&self, command: DiskCommand) -> Result<(), String> {
        self.commands
            .as_ref()
            .ok_or_else(|| "derived index worker is closed".to_owned())?
            .try_send(command)
            .map_err(|_| "derived index worker command queue is unavailable".to_owned())
    }

    async fn close(mut self) {
        self.commands.take();
        let _ = self.task.await;
    }
}

fn disk_meta(index: &DiskIndex) -> DiskMeta {
    DiskMeta {
        count: index.count,
        next_offset: index.next_offset,
        high_sequence: index.high_sequence,
    }
}

#[derive(Clone, Debug)]
pub struct LiveConfig {
    pub artifact_dir: PathBuf,
    pub index_page_records: usize,
    pub index_page_bytes: usize,
    pub maximum_request_rows: usize,
    pub request_queue_capacity: usize,
    pub update_queue_capacity: usize,
    pub cache_rows: usize,
    pub cache_bytes: usize,
    pub maximum_display_bytes: usize,
    pub maximum_index_bytes_per_source: u64,
    pub maximum_sources: usize,
    pub maximum_view_sources: usize,
}

impl LiveConfig {
    pub fn new(artifact_dir: impl Into<PathBuf>) -> Self {
        Self {
            artifact_dir: artifact_dir.into(),
            index_page_records: 128,
            index_page_bytes: 1024 * 1024,
            maximum_request_rows: 256,
            request_queue_capacity: 32,
            update_queue_capacity: 64,
            cache_rows: 1024,
            cache_bytes: 4 * 1024 * 1024,
            maximum_display_bytes: 64 * 1024,
            maximum_index_bytes_per_source: 256 * 1024 * 1024,
            maximum_sources: 128,
            maximum_view_sources: 32,
        }
    }
}

#[derive(Debug, Error)]
pub enum AdapterError {
    #[error("live adapter requires a Tokio runtime")]
    NoRuntime,
    #[error("adapter configuration contains a zero bound")]
    InvalidConfig,
    #[error("view references an unregistered source")]
    UnknownSource,
    #[error("view contains too many sources")]
    TooManySources,
    #[error("adapter source limit reached")]
    SourceLimit,
    #[error("live adapter is shut down")]
    Closed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IndexState {
    Opening,
    Rebuilding,
    Indexing,
    Ready,
    /// The configured derived-index disk budget was reached. Captured data is intact.
    Limited,
    Error,
    Shutdown,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceViewStatus {
    pub source_id: SourceId,
    pub generation: u64,
    pub acquisition: RuntimeState,
    pub reported_records: u64,
    /// Count of indexed physical records. Fragment grouping is not applied.
    pub indexed_records: u64,
    pub high_watermark: Option<u64>,
    pub index: IndexState,
    pub pending_requests: usize,
    pub last_error: Option<String>,
    pub fragments_are_physical_records: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ViewStatus {
    pub view_id: String,
    pub indexed_physical_records: usize,
    pub indexing: bool,
    pub loading_requests: usize,
    pub sources: Vec<SourceViewStatus>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AdapterStats {
    pub cached_rows: usize,
    pub cached_bytes: usize,
    pub pending_requests: usize,
    pub dropped_requests: u64,
    pub completed_requests: u64,
}

pub struct LiveRowProvider {
    config: LiveConfig,
    state: Arc<Mutex<State>>,
    updates: Mutex<mpsc::Receiver<WorkerUpdate>>,
    update_tx: mpsc::Sender<WorkerUpdate>,
    workers: Mutex<Vec<WorkerSlot>>,
}

impl LiveRowProvider {
    pub fn new(config: LiveConfig) -> Result<Self, AdapterError> {
        validate(&config)?;
        tokio::runtime::Handle::try_current().map_err(|_| AdapterError::NoRuntime)?;
        let (update_tx, updates) = mpsc::channel(config.update_queue_capacity);
        Ok(Self {
            config,
            state: Arc::new(Mutex::new(State::default())),
            updates: Mutex::new(updates),
            update_tx,
            workers: Mutex::new(Vec::new()),
        })
    }

    /// Registers one runtime generation. Re-registering the same SourceId fences
    /// old worker updates and invalidates its derived cache without stopping capture.
    pub fn register_source(&self, handle: SourceHandle) -> Result<(), AdapterError> {
        let progress = handle.progress();
        let source_id = handle.source_id();
        let generation = progress.generation;
        let (epoch, request_rx) = {
            let mut state = self.state.lock().expect("live state poisoned");
            if !state.accepting {
                return Err(AdapterError::Closed);
            }
            if !state.sources.contains_key(&source_id)
                && state.sources.len() >= self.config.maximum_sources
            {
                return Err(AdapterError::SourceLimit);
            }
            if let Some(current) = state.sources.get_mut(&source_id)
                && current.generation == generation
            {
                current.acquisition = progress.state;
                current.reported_records = progress.records;
                return Ok(());
            }
            state.next_epoch = state.next_epoch.wrapping_add(1).max(1);
            let epoch = state.next_epoch;
            let (requests, request_rx) = mpsc::channel(self.config.request_queue_capacity);
            if let Some(previous) = state.sources.insert(
                source_id,
                SourceState {
                    generation,
                    epoch,
                    acquisition: progress.state,
                    reported_records: progress.records,
                    indexed_records: 0,
                    high_watermark: None,
                    index: IndexState::Opening,
                    last_error: None,
                    requests,
                },
            ) {
                state.invalidate_source(source_id, previous.generation);
            }
            state.bump_views_for(source_id);
            (epoch, request_rx)
        };
        let (cancel, cancelled) = watch::channel(false);
        let mut workers = self.workers.lock().expect("worker list poisoned");
        let previous = workers
            .iter()
            .position(|worker| worker.source_id == source_id)
            .map(|position| workers.remove(position));
        workers.retain(|worker| !worker.task.is_finished());
        let artifact = self.index_path(source_id);
        let config = self.config.clone();
        let updates = self.update_tx.clone();
        let task = tokio::spawn(async move {
            if let Some(previous) = previous {
                let _ = previous.cancel.send(true);
                let _ = previous.task.await;
            }
            source_worker(
                handle,
                WorkerToken { generation, epoch },
                artifact,
                config,
                request_rx,
                updates,
                cancelled,
            )
            .await;
        });
        workers.push(WorkerSlot {
            source_id,
            cancel,
            task,
        });
        Ok(())
    }

    /// Defines an unfiltered raw view. Multiple views share source workers,
    /// indexes and caches; no acquisition is started here.
    pub fn register_raw_view(
        &self,
        view_id: impl Into<String>,
        sources: Vec<SourceId>,
    ) -> Result<(), AdapterError> {
        if sources.len() > self.config.maximum_view_sources {
            return Err(AdapterError::TooManySources);
        }
        let mut seen = HashSet::new();
        let sources: Vec<_> = sources.into_iter().filter(|id| seen.insert(*id)).collect();
        let mut state = self.state.lock().expect("live state poisoned");
        if !state.accepting {
            return Err(AdapterError::Closed);
        }
        if sources
            .iter()
            .any(|source| !state.sources.contains_key(source))
        {
            return Err(AdapterError::UnknownSource);
        }
        let view_id = view_id.into();
        let revision = state
            .views
            .get(&view_id)
            .map_or(1, |view| view.revision + 1);
        state.views.insert(view_id, ViewState { sources, revision });
        Ok(())
    }

    /// Applies at most `maximum` background updates. Call this from the terminal
    /// tick before `App::sync_provider`; it performs no disk I/O and never awaits.
    pub fn drain_ready_updates(&self, maximum: usize) -> usize {
        let mut receiver = self.updates.lock().expect("update receiver poisoned");
        let mut state = self.state.lock().expect("live state poisoned");
        let mut drained = 0;
        while drained < maximum {
            let Ok(update) = receiver.try_recv() else {
                break;
            };
            drained += 1;
            state.apply(update, &self.config);
        }
        drained
    }

    pub fn source_status(&self, source_id: SourceId) -> Option<SourceViewStatus> {
        let state = self.state.lock().expect("live state poisoned");
        state.source_status(source_id)
    }

    pub fn view_status(&self, view_id: &str) -> Option<ViewStatus> {
        let state = self.state.lock().expect("live state poisoned");
        let view = state.views.get(view_id)?;
        let sources: Vec<_> = view
            .sources
            .iter()
            .filter_map(|source| state.source_status(*source))
            .collect();
        Some(ViewStatus {
            view_id: view_id.to_owned(),
            indexed_physical_records: sources
                .iter()
                .map(|source| usize_from_u64(source.indexed_records))
                .fold(0usize, usize::saturating_add),
            indexing: sources.iter().any(|source| {
                matches!(
                    source.index,
                    IndexState::Opening | IndexState::Rebuilding | IndexState::Indexing
                )
            }),
            loading_requests: sources.iter().map(|source| source.pending_requests).sum(),
            sources,
        })
    }

    pub fn stats(&self) -> AdapterStats {
        self.state.lock().expect("live state poisoned").stats()
    }

    pub fn index_path(&self, source_id: SourceId) -> PathBuf {
        self.config
            .artifact_dir
            .join(format!("{}.rows.idx", source_id.0))
    }

    pub async fn shutdown(&self) {
        {
            let mut state = self.state.lock().expect("live state poisoned");
            state.accepting = false;
            state.pending.clear();
        }
        let workers = {
            let mut workers = self.workers.lock().expect("worker list poisoned");
            for worker in workers.iter() {
                let _ = worker.cancel.send(true);
            }
            std::mem::take(&mut *workers)
        };
        for worker in workers {
            let _ = worker.task.await;
        }
        let mut state = self.state.lock().expect("live state poisoned");
        for source in state.sources.values_mut() {
            source.index = IndexState::Shutdown;
        }
    }
}

impl RowProvider for LiveRowProvider {
    fn page(&self, view_id: &str, request: ViewportRequest) -> RowPage {
        let mut state = self.state.lock().expect("live state poisoned");
        let Some(view) = state.views.get(view_id).cloned() else {
            return RowPage {
                total: 0,
                rows: Vec::new(),
            };
        };
        let total = state.view_total(&view);
        let len = request
            .len
            .min(self.config.maximum_request_rows)
            .min(self.config.cache_rows)
            .min(total.saturating_sub(request.start));
        if len == 0 {
            return RowPage {
                total,
                rows: Vec::new(),
            };
        }
        let mut rows = Vec::with_capacity(len.min(self.config.cache_rows));
        let mut contiguous = true;
        let end = request.start.saturating_add(len);
        let mut base = 0usize;
        for source_id in &view.sources {
            let Some((generation, indexed_records)) = state
                .sources
                .get(source_id)
                .map(|source| (source.generation, source.indexed_records))
            else {
                continue;
            };
            let count = usize_from_u64(indexed_records);
            let source_end = base.saturating_add(count);
            let overlap_start = request.start.max(base);
            let overlap_end = end.min(source_end);
            if overlap_start < overlap_end {
                let local_start = overlap_start - base;
                let local_len = overlap_end - overlap_start;
                let mut missing = false;
                for position in local_start..local_start + local_len {
                    let key = PositionKey {
                        source_id: *source_id,
                        generation,
                        position: position as u64,
                    };
                    self_touch(&mut state, key);
                    if let Some(entry) = state.cache.get(&key) {
                        if contiguous {
                            rows.push(entry.row.clone());
                        }
                    } else {
                        missing = true;
                        contiguous = false;
                    }
                }
                if missing {
                    state.enqueue(
                        *source_id,
                        Request::Positions {
                            start: local_start as u64,
                            len: local_len,
                        },
                    );
                }
            }
            base = source_end;
        }
        RowPage { total, rows }
    }

    fn row_by_id(&self, view_id: &str, id: &RowId) -> Option<DisplayRow> {
        let mut state = self.state.lock().expect("live state poisoned");
        let source_id = state.source_in_view(view_id, &id.source_id)?;
        let generation = state.sources.get(&source_id)?.generation;
        let id_key = IdKey {
            source_id,
            generation,
            sequence: id.sequence,
        };
        if let Some(position) = state.id_positions.get(&id_key).copied() {
            let key = PositionKey {
                source_id,
                generation,
                position,
            };
            self_touch(&mut state, key);
            if let Some(entry) = state.cache.get(&key) {
                return Some(entry.row.clone());
            }
        }
        state.enqueue(source_id, Request::Sequence(id.sequence));
        None
    }

    fn index_of_id(&self, view_id: &str, id: &RowId) -> Option<usize> {
        let mut state = self.state.lock().expect("live state poisoned");
        let view = state.views.get(view_id)?.clone();
        let source_id = state.source_in_view(view_id, &id.source_id)?;
        let source = state.sources.get(&source_id)?;
        let key = IdKey {
            source_id,
            generation: source.generation,
            sequence: id.sequence,
        };
        let local = state.id_positions.get(&key).copied();
        if local.is_none() {
            state.enqueue(source_id, Request::Sequence(id.sequence));
        }
        let local = usize_from_u64(local?);
        let prefix = view
            .sources
            .iter()
            .take_while(|source| **source != source_id)
            .filter_map(|source| state.sources.get(source))
            .map(|source| usize_from_u64(source.indexed_records))
            .fold(0usize, usize::saturating_add);
        Some(prefix.saturating_add(local))
    }

    fn revision(&self, view_id: &str) -> u64 {
        self.state
            .lock()
            .expect("live state poisoned")
            .views
            .get(view_id)
            .map_or(0, |view| view.revision)
    }
}

fn self_touch(state: &mut State, key: PositionKey) {
    state.clock = state.clock.wrapping_add(1);
    if let Some(entry) = state.cache.get_mut(&key) {
        entry.touched = state.clock;
    }
}

impl Drop for LiveRowProvider {
    fn drop(&mut self) {
        if let Ok(workers) = self.workers.get_mut() {
            for worker in workers.iter() {
                let _ = worker.cancel.send(true);
            }
        }
    }
}

struct State {
    sources: HashMap<SourceId, SourceState>,
    views: HashMap<String, ViewState>,
    cache: HashMap<PositionKey, CacheEntry>,
    id_positions: HashMap<IdKey, u64>,
    pending: HashSet<RequestKey>,
    cached_bytes: usize,
    clock: u64,
    dropped_requests: u64,
    completed_requests: u64,
    next_epoch: u64,
    accepting: bool,
}

impl Default for State {
    fn default() -> Self {
        Self {
            sources: HashMap::new(),
            views: HashMap::new(),
            cache: HashMap::new(),
            id_positions: HashMap::new(),
            pending: HashSet::new(),
            cached_bytes: 0,
            clock: 0,
            dropped_requests: 0,
            completed_requests: 0,
            next_epoch: 0,
            accepting: true,
        }
    }
}

impl State {
    fn apply(&mut self, update: WorkerUpdate, config: &LiveConfig) {
        let (source_id, generation, epoch) = update.identity();
        if self
            .sources
            .get(&source_id)
            .map(|source| (source.generation, source.epoch))
            != Some((generation, epoch))
        {
            return;
        }
        match update {
            WorkerUpdate::Progress {
                source_id,
                generation: _,
                epoch: _,
                acquisition,
                reported_records,
                indexed_records,
                high_watermark,
                index,
                error,
            } => {
                let source = self
                    .sources
                    .get_mut(&source_id)
                    .expect("generation checked");
                let membership_changed = source.indexed_records != indexed_records;
                source.acquisition = acquisition;
                source.reported_records = reported_records;
                source.indexed_records = indexed_records;
                source.high_watermark = high_watermark;
                source.index = index;
                source.last_error = error;
                if matches!(index, IndexState::Error | IndexState::Shutdown) {
                    self.pending.retain(|key| {
                        key.source_id != source_id
                            || key.generation != generation
                            || key.epoch != epoch
                    });
                }
                if membership_changed {
                    self.bump_views_for(source_id);
                }
            }
            WorkerUpdate::Rows {
                source_id,
                generation,
                epoch,
                request,
                rows,
            } => {
                self.pending.remove(&RequestKey {
                    source_id,
                    generation,
                    epoch,
                    request,
                });
                self.completed_requests = self.completed_requests.saturating_add(1);
                for (position, row) in rows {
                    self.insert_cache(source_id, generation, position, row, config);
                }
                self.bump_views_for(source_id);
            }
            WorkerUpdate::Failed {
                source_id,
                generation,
                epoch,
                request,
                message,
            } => {
                self.pending.remove(&RequestKey {
                    source_id,
                    generation,
                    epoch,
                    request,
                });
                if let Some(source) = self.sources.get_mut(&source_id) {
                    source.index = IndexState::Error;
                    source.last_error = Some(message);
                }
                self.bump_views_for(source_id);
            }
        }
    }

    fn insert_cache(
        &mut self,
        source_id: SourceId,
        generation: u64,
        position: u64,
        row: DisplayRow,
        config: &LiveConfig,
    ) {
        let key = PositionKey {
            source_id,
            generation,
            position,
        };
        let bytes = row_bytes(&row);
        self.clock = self.clock.wrapping_add(1);
        if let Some(old) = self.cache.insert(
            key,
            CacheEntry {
                row: row.clone(),
                bytes,
                touched: self.clock,
            },
        ) {
            self.cached_bytes = self.cached_bytes.saturating_sub(old.bytes);
        }
        self.cached_bytes = self.cached_bytes.saturating_add(bytes);
        self.id_positions.insert(
            IdKey {
                source_id,
                generation,
                sequence: row.id.sequence,
            },
            position,
        );
        while self.cache.len() > config.cache_rows || self.cached_bytes > config.cache_bytes {
            let Some(oldest) = self
                .cache
                .iter()
                .min_by_key(|(_, entry)| entry.touched)
                .map(|(key, _)| *key)
            else {
                break;
            };
            if let Some(entry) = self.cache.remove(&oldest) {
                self.cached_bytes = self.cached_bytes.saturating_sub(entry.bytes);
                self.id_positions.remove(&IdKey {
                    source_id: oldest.source_id,
                    generation: oldest.generation,
                    sequence: entry.row.id.sequence,
                });
            }
        }
    }

    fn enqueue(&mut self, source_id: SourceId, request: Request) {
        if !self.accepting {
            return;
        }
        let Some(source) = self.sources.get(&source_id) else {
            return;
        };
        let key = RequestKey {
            source_id,
            generation: source.generation,
            epoch: source.epoch,
            request: request.clone(),
        };
        if self.pending.contains(&key) {
            return;
        }
        match source.requests.try_send(request) {
            Ok(()) => {
                self.pending.insert(key);
            }
            Err(_) => {
                self.dropped_requests = self.dropped_requests.saturating_add(1);
            }
        }
    }

    fn source_in_view(&self, view_id: &str, text: &str) -> Option<SourceId> {
        self.views
            .get(view_id)?
            .sources
            .iter()
            .copied()
            .find(|source| source.0.to_string() == text)
    }
    fn view_total(&self, view: &ViewState) -> usize {
        view.sources
            .iter()
            .filter_map(|id| self.sources.get(id))
            .map(|source| usize_from_u64(source.indexed_records))
            .fold(0, usize::saturating_add)
    }
    fn bump_views_for(&mut self, source_id: SourceId) {
        for view in self
            .views
            .values_mut()
            .filter(|view| view.sources.contains(&source_id))
        {
            view.revision = view.revision.wrapping_add(1);
        }
    }
    fn invalidate_source(&mut self, source_id: SourceId, generation: u64) {
        self.cache.retain(|key, entry| {
            let keep = key.source_id != source_id || key.generation != generation;
            if !keep {
                self.cached_bytes = self.cached_bytes.saturating_sub(entry.bytes);
            }
            keep
        });
        self.id_positions
            .retain(|key, _| key.source_id != source_id || key.generation != generation);
        self.pending
            .retain(|key| key.source_id != source_id || key.generation != generation);
    }
    fn source_status(&self, source_id: SourceId) -> Option<SourceViewStatus> {
        let source = self.sources.get(&source_id)?;
        Some(SourceViewStatus {
            source_id,
            generation: source.generation,
            acquisition: source.acquisition,
            reported_records: source.reported_records,
            indexed_records: source.indexed_records,
            high_watermark: source.high_watermark,
            index: source.index,
            pending_requests: self
                .pending
                .iter()
                .filter(|key| key.source_id == source_id && key.generation == source.generation)
                .count(),
            last_error: source.last_error.clone(),
            fragments_are_physical_records: true,
        })
    }
    fn stats(&self) -> AdapterStats {
        AdapterStats {
            cached_rows: self.cache.len(),
            cached_bytes: self.cached_bytes,
            pending_requests: self.pending.len(),
            dropped_requests: self.dropped_requests,
            completed_requests: self.completed_requests,
        }
    }
}

struct SourceState {
    generation: u64,
    epoch: u64,
    acquisition: RuntimeState,
    reported_records: u64,
    indexed_records: u64,
    high_watermark: Option<u64>,
    index: IndexState,
    last_error: Option<String>,
    requests: mpsc::Sender<Request>,
}
#[derive(Clone)]
struct ViewState {
    sources: Vec<SourceId>,
    revision: u64,
}
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct PositionKey {
    source_id: SourceId,
    generation: u64,
    position: u64,
}
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct IdKey {
    source_id: SourceId,
    generation: u64,
    sequence: u64,
}
struct CacheEntry {
    row: DisplayRow,
    bytes: usize,
    touched: u64,
}
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
enum Request {
    Positions { start: u64, len: usize },
    Sequence(u64),
}
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct RequestKey {
    source_id: SourceId,
    generation: u64,
    epoch: u64,
    request: Request,
}
struct WorkerSlot {
    source_id: SourceId,
    cancel: watch::Sender<bool>,
    task: JoinHandle<()>,
}

#[derive(Clone, Copy)]
struct WorkerToken {
    generation: u64,
    epoch: u64,
}

enum WorkerUpdate {
    Progress {
        source_id: SourceId,
        generation: u64,
        epoch: u64,
        acquisition: RuntimeState,
        reported_records: u64,
        indexed_records: u64,
        high_watermark: Option<u64>,
        index: IndexState,
        error: Option<String>,
    },
    Rows {
        source_id: SourceId,
        generation: u64,
        epoch: u64,
        request: Request,
        rows: Vec<(u64, DisplayRow)>,
    },
    Failed {
        source_id: SourceId,
        generation: u64,
        epoch: u64,
        request: Request,
        message: String,
    },
}
impl WorkerUpdate {
    fn identity(&self) -> (SourceId, u64, u64) {
        match self {
            Self::Progress {
                source_id,
                generation,
                epoch,
                ..
            }
            | Self::Rows {
                source_id,
                generation,
                epoch,
                ..
            }
            | Self::Failed {
                source_id,
                generation,
                epoch,
                ..
            } => (*source_id, *generation, *epoch),
        }
    }
}

async fn source_worker(
    handle: SourceHandle,
    token: WorkerToken,
    artifact: PathBuf,
    config: LiveConfig,
    mut requests: mpsc::Receiver<Request>,
    updates: mpsc::Sender<WorkerUpdate>,
    mut cancelled: watch::Receiver<bool>,
) {
    let generation = token.generation;
    let epoch = token.epoch;
    let source_id = handle.source_id();
    let (mut disk, rebuilt) = match DiskService::open(
        artifact,
        source_id,
        generation,
        config.index_page_records,
        config.index_page_bytes,
        config.maximum_index_bytes_per_source,
    )
    .await
    {
        Ok(value) => value,
        Err(error) => {
            let _ = emit(
                &updates,
                &mut cancelled,
                progress_update(
                    &handle,
                    generation,
                    epoch,
                    0,
                    None,
                    IndexState::Error,
                    Some(error),
                ),
            )
            .await;
            return;
        }
    };
    let initial_state = if rebuilt {
        IndexState::Rebuilding
    } else {
        IndexState::Indexing
    };
    if !emit(
        &updates,
        &mut cancelled,
        progress_update(
            &handle,
            generation,
            epoch,
            disk.meta.count,
            disk.meta.high_sequence,
            initial_state,
            None,
        ),
    )
    .await
    {
        disk.close().await;
        return;
    }
    let mut limited = false;
    let mut limited_error = None;
    loop {
        if *cancelled.borrow() {
            break;
        }
        // Serve at most one queued viewport request, then perform at most one
        // indexing page. This bounded alternation prevents either workload from
        // starving the other.
        if let Ok(request) = requests.try_recv()
            && !serve_and_emit(
                &handle,
                token,
                &config,
                &disk,
                request,
                &updates,
                &mut cancelled,
            )
            .await
        {
            break;
        }
        let progress = handle.progress();
        if !limited && disk.meta.count < progress.records {
            let page_offset = disk.meta.next_offset;
            let page_result = tokio::select! {
                biased;
                changed = cancelled.changed() => {
                    if changed.is_err() || *cancelled.borrow() { break; }
                    continue;
                }
                result = handle.read_page(
                    page_offset,
                    config.index_page_records,
                    config.index_page_bytes,
                ) => result,
            };
            match page_result {
                Ok(page) if !page.records.is_empty() => {
                    if let Err((kind, message)) = disk
                        .append(
                            page_offset,
                            page.next_offset,
                            page.records,
                            config.maximum_index_bytes_per_source,
                        )
                        .await
                    {
                        let index_state = if kind == std::io::ErrorKind::WriteZero {
                            limited = true;
                            limited_error = Some(message.clone());
                            IndexState::Limited
                        } else {
                            IndexState::Error
                        };
                        let _ = emit(
                            &updates,
                            &mut cancelled,
                            progress_update(
                                &handle,
                                generation,
                                epoch,
                                disk.meta.count,
                                disk.meta.high_sequence,
                                index_state,
                                Some(message),
                            ),
                        )
                        .await;
                        if !limited {
                            break;
                        }
                    } else if !emit(
                        &updates,
                        &mut cancelled,
                        progress_update(
                            &handle,
                            generation,
                            epoch,
                            disk.meta.count,
                            disk.meta.high_sequence,
                            if disk.meta.count < handle.progress().records {
                                IndexState::Indexing
                            } else {
                                IndexState::Ready
                            },
                            None,
                        ),
                    )
                    .await
                    {
                        break;
                    } else {
                        tokio::task::yield_now().await;
                        continue;
                    }
                }
                Ok(_) => {}
                Err(error) => {
                    let _ = emit(
                        &updates,
                        &mut cancelled,
                        progress_update(
                            &handle,
                            generation,
                            epoch,
                            disk.meta.count,
                            disk.meta.high_sequence,
                            IndexState::Error,
                            Some(error.to_string()),
                        ),
                    )
                    .await;
                    break;
                }
            }
        }
        tokio::select! {
            biased;
            changed = cancelled.changed() => { if changed.is_err() || *cancelled.borrow() { break; } }
            request = requests.recv() => {
                let Some(request) = request else { break };
                if !serve_and_emit(&handle, token, &config, &disk, request, &updates, &mut cancelled).await { break; }
            }
            _ = tokio::time::sleep(std::time::Duration::from_millis(20)) => {
                let state = if limited { IndexState::Limited } else if disk.meta.count < handle.progress().records { IndexState::Indexing } else { IndexState::Ready };
                let update = progress_update(&handle, generation, epoch, disk.meta.count, disk.meta.high_sequence, state, limited_error.clone());
                if !emit(&updates, &mut cancelled, update).await { break; }
            }
        }
    }
    disk.close().await;
}

async fn serve_and_emit(
    handle: &SourceHandle,
    token: WorkerToken,
    config: &LiveConfig,
    disk: &DiskService,
    request: Request,
    updates: &mpsc::Sender<WorkerUpdate>,
    cancelled: &mut watch::Receiver<bool>,
) -> bool {
    let generation = token.generation;
    let epoch = token.epoch;
    let result = tokio::select! {
        biased;
        changed = cancelled.changed() => {
            if changed.is_err() || *cancelled.borrow() { return false; }
            return true;
        }
        result = serve_request(handle, disk, config, &request) => result,
    };
    let update = match result {
        Ok(rows) => WorkerUpdate::Rows {
            source_id: handle.source_id(),
            generation,
            epoch,
            request,
            rows,
        },
        Err(message) => WorkerUpdate::Failed {
            source_id: handle.source_id(),
            generation,
            epoch,
            request,
            message,
        },
    };
    emit(updates, cancelled, update).await
}

async fn emit(
    updates: &mpsc::Sender<WorkerUpdate>,
    cancelled: &mut watch::Receiver<bool>,
    update: WorkerUpdate,
) -> bool {
    tokio::select! {
        biased;
        changed = cancelled.changed() => changed.is_ok() && !*cancelled.borrow(),
        result = updates.send(update) => result.is_ok(),
    }
}

async fn serve_request(
    handle: &SourceHandle,
    disk: &DiskService,
    config: &LiveConfig,
    request: &Request,
) -> Result<Vec<(u64, DisplayRow)>, String> {
    let entries = match request {
        Request::Positions { start, len } => {
            disk.entries(*start, (*len).min(config.maximum_request_rows))
                .await
        }
        Request::Sequence(sequence) => disk
            .find(*sequence)
            .await
            .map(|entry| entry.into_iter().collect()),
    }?;
    let mut rows = Vec::with_capacity(entries.len());
    let mut current_page: Option<(u64, Vec<RawRecord>)> = None;
    for entry in entries {
        if current_page.as_ref().map(|(offset, _)| *offset) != Some(entry.page_offset) {
            let page = handle
                .read_page(
                    entry.page_offset,
                    config
                        .index_page_records
                        .max(entry.within_page as usize + 1),
                    config.index_page_bytes,
                )
                .await
                .map_err(|error| error.to_string())?;
            current_page = Some((entry.page_offset, page.records));
        }
        let record = current_page
            .as_ref()
            .and_then(|(_, records)| records.get(entry.within_page as usize))
            .ok_or_else(|| "derived index points outside journal page".to_owned())?;
        if record.record_id.sequence != entry.sequence {
            return Err("derived index sequence mismatch".into());
        }
        rows.push((entry.position, display(record, config)));
    }
    Ok(rows)
}

fn progress_update(
    handle: &SourceHandle,
    generation: u64,
    epoch: u64,
    indexed_records: u64,
    high_watermark: Option<u64>,
    index: IndexState,
    error: Option<String>,
) -> WorkerUpdate {
    let progress = handle.progress();
    WorkerUpdate::Progress {
        source_id: handle.source_id(),
        generation,
        epoch,
        acquisition: progress.state,
        reported_records: progress.records,
        indexed_records,
        high_watermark,
        index,
        error,
    }
}

fn display(record: &RawRecord, config: &LiveConfig) -> DisplayRow {
    let fragment = record.chunk != ChunkPosition::Complete;
    let projection_len = record.bytes.len().min(config.maximum_display_bytes);
    let mut text = String::from_utf8_lossy(&record.bytes[..projection_len]).into_owned();
    let decoded_bytes = text.len();
    loop {
        let row = build_display(
            record,
            fragment,
            projection_len,
            text.clone(),
            decoded_bytes.saturating_sub(text.len()),
        );
        let bytes = row_bytes(&row);
        if bytes <= config.cache_bytes || text.is_empty() {
            return row;
        }
        let target = text.len().saturating_sub(bytes - config.cache_bytes);
        truncate_utf8(&mut text, target);
    }
}

fn truncate_utf8(text: &mut String, mut maximum: usize) {
    maximum = maximum.min(text.len());
    while !text.is_char_boundary(maximum) {
        maximum -= 1;
    }
    text.truncate(maximum);
}

fn build_display(
    record: &RawRecord,
    fragment: bool,
    projection_len: usize,
    text: String,
    decoded_truncated: usize,
) -> DisplayRow {
    let mut details = vec![
        (
            "stream".into(),
            format!("{:?}", record.stream).to_ascii_lowercase(),
        ),
        ("sequence".into(), record.record_id.sequence.to_string()),
        (
            "captured_unix_nanos".into(),
            record.captured_at_unix_nanos.to_string(),
        ),
    ];
    if fragment {
        details.push((
            "fragment".into(),
            format!("{:?}", record.chunk).to_ascii_lowercase(),
        ));
    }
    if projection_len < record.bytes.len() {
        details.push((
            "display_truncated_bytes".into(),
            (record.bytes.len() - projection_len).to_string(),
        ));
    }
    if decoded_truncated > 0 {
        details.push((
            "display_truncated_decoded_bytes".into(),
            decoded_truncated.to_string(),
        ));
    }
    DisplayRow {
        id: RowId::new(
            record.record_id.source_id.0.to_string(),
            record.record_id.sequence,
        ),
        timestamp: display_timestamp(record.captured_at_unix_nanos),
        level: if fragment {
            "fragment".into()
        } else {
            String::new()
        },
        text,
        details,
    }
}

fn display_timestamp(unix_nanos: i64) -> String {
    let seconds = unix_nanos.div_euclid(1_000_000_000);
    let millis = unix_nanos.rem_euclid(1_000_000_000) / 1_000_000;
    let day_seconds = seconds.rem_euclid(86_400);
    let hour = day_seconds / 3_600;
    let minute = day_seconds % 3_600 / 60;
    let second = day_seconds % 60;
    format!("{hour:02}:{minute:02}:{second:02}.{millis:03}Z")
}

fn row_bytes(row: &DisplayRow) -> usize {
    row.id
        .source_id
        .len()
        .saturating_add(row.timestamp.len())
        .saturating_add(row.level.len())
        .saturating_add(row.text.len())
        .saturating_add(
            row.details
                .iter()
                .map(|(key, value)| key.len().saturating_add(value.len()))
                .sum::<usize>(),
        )
}
fn usize_from_u64(value: u64) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
}
fn validate(config: &LiveConfig) -> Result<(), AdapterError> {
    if config.index_page_records == 0
        || config.index_page_bytes == 0
        || config.maximum_request_rows == 0
        || config.request_queue_capacity == 0
        || config.update_queue_capacity == 0
        || config.cache_rows == 0
        || config.cache_bytes < 256
        || config.maximum_display_bytes == 0
        || config.maximum_index_bytes_per_source < 84
        || config.index_page_records > u32::MAX as usize
        || config.index_page_bytes > u32::MAX as usize
        || config.maximum_sources == 0
        || config.maximum_view_sources == 0
    {
        Err(AdapterError::InvalidConfig)
    } else {
        Ok(())
    }
}
