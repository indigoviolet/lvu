use crate::index::{DiskIndex, IndexBudget};
use fs2::FileExt;
use lvu::{DisplayRow, RowId, RowPage, RowProvider, ViewportRequest};
use lvu_core::{ChunkPosition, RawRecord, SourceId};
use lvu_ingest::{RuntimeState, SourceHandle};
use std::{
    collections::{HashMap, HashSet},
    fs::{File, OpenOptions},
    io,
    path::Path,
    path::PathBuf,
    sync::{Arc, Mutex},
};
use thiserror::Error;
use tokio::{
    sync::{mpsc, oneshot, watch},
    task::JoinHandle,
};

const MAX_LOOKUP_DIAGNOSTIC_BYTES: usize = 4 * 1024;

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
        journal_identity: [u8; 16],
        budget: IndexBudget,
    ) -> Result<(Self, bool), (std::io::ErrorKind, String)> {
        let (commands, receiver) = std::sync::mpsc::sync_channel(1);
        let (opened_tx, opened_rx) = oneshot::channel();
        let task = tokio::task::spawn_blocking(move || {
            let opened = DiskIndex::open_budgeted(
                &path,
                source,
                generation,
                page_records,
                page_bytes,
                journal_identity,
                budget,
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
            .map_err(|_| {
                (
                    std::io::ErrorKind::BrokenPipe,
                    "derived index worker stopped during open".to_owned(),
                )
            })?
            .map_err(|error| (error.kind(), error.to_string()))?;
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
    pub maximum_total_index_bytes: u64,
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
            maximum_total_index_bytes: 5 * 1024 * 1024 * 1024,
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DerivedArtifactStatus {
    Missing,
    Active,
    Unused {
        bytes: u64,
        identity: DerivedArtifactIdentity,
    },
    NotOwned,
    Unverified {
        bytes: u64,
        reason: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DerivedArtifactIdentity {
    name: std::ffi::OsString,
    directory: DirectoryIdentity,
    file: FileRevisionIdentity,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DirectoryIdentity {
    device: u64,
    inode: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FileRevisionIdentity {
    device: u64,
    inode: u64,
    length: u64,
    modified_seconds: i64,
    modified_nanos: i64,
    changed_seconds: i64,
    changed_nanos: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StorageBudget {
    pub artifact_dir: PathBuf,
    pub row_cache_bytes: usize,
    pub row_cache_limit: usize,
    pub maximum_index_bytes_per_source: u64,
    pub maximum_total_index_bytes: u64,
}

pub struct LiveRowProvider {
    config: LiveConfig,
    state: Arc<Mutex<State>>,
    updates: Mutex<mpsc::Receiver<WorkerUpdate>>,
    update_tx: mpsc::Sender<WorkerUpdate>,
    workers: Mutex<Vec<WorkerSlot>>,
    artifact_ownership: Mutex<()>,
    artifact_directory: File,
    artifact_directory_identity: DirectoryIdentity,
}

impl LiveRowProvider {
    pub fn new(config: LiveConfig) -> Result<Self, AdapterError> {
        validate(&config)?;
        tokio::runtime::Handle::try_current().map_err(|_| AdapterError::NoRuntime)?;
        std::fs::create_dir_all(&config.artifact_dir).map_err(|_| AdapterError::InvalidConfig)?;
        let metadata = std::fs::symlink_metadata(&config.artifact_dir)
            .map_err(|_| AdapterError::InvalidConfig)?;
        if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
            return Err(AdapterError::InvalidConfig);
        }
        let artifact_directory = OpenOptions::new()
            .read(true)
            .open(&config.artifact_dir)
            .map_err(|_| AdapterError::InvalidConfig)?;
        let artifact_directory_identity = directory_identity(
            &artifact_directory
                .metadata()
                .map_err(|_| AdapterError::InvalidConfig)?,
        );
        let (update_tx, updates) = mpsc::channel(config.update_queue_capacity);
        Ok(Self {
            config,
            state: Arc::new(Mutex::new(State::default())),
            updates: Mutex::new(updates),
            update_tx,
            workers: Mutex::new(Vec::new()),
            artifact_ownership: Mutex::new(()),
            artifact_directory,
            artifact_directory_identity,
        })
    }

    /// Registers one runtime generation. Re-registering the same SourceId fences
    /// old worker updates and invalidates its derived cache without stopping capture.
    pub fn register_source(&self, handle: SourceHandle) -> Result<(), AdapterError> {
        let _ownership = self
            .artifact_ownership
            .lock()
            .expect("artifact ownership poisoned");
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
                    lookup_failure: None,
                    artifact_path: None,
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
        let artifact = self.config.artifact_dir.clone();
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

    pub fn storage_budget(&self) -> StorageBudget {
        StorageBudget {
            artifact_dir: self.config.artifact_dir.clone(),
            row_cache_bytes: self.stats().cached_bytes,
            row_cache_limit: self.config.cache_bytes,
            maximum_index_bytes_per_source: self.config.maximum_index_bytes_per_source,
            maximum_total_index_bytes: self.config.maximum_total_index_bytes,
        }
    }

    pub fn artifact_directory_is_current(&self) -> io::Result<bool> {
        let metadata = std::fs::symlink_metadata(&self.config.artifact_dir)?;
        Ok(metadata.file_type().is_dir()
            && !metadata.file_type().is_symlink()
            && directory_identity(&metadata) == self.artifact_directory_identity)
    }

    #[cfg(target_os = "linux")]
    pub fn derived_artifact_paths(&self, limit: usize) -> io::Result<(Vec<(PathBuf, u64)>, bool)> {
        use std::os::fd::AsRawFd;
        let directory = PathBuf::from(format!(
            "/proc/self/fd/{}",
            self.artifact_directory.as_raw_fd()
        ));
        let mut output = Vec::new();
        let mut truncated = false;
        for (position, entry) in std::fs::read_dir(directory)?.enumerate() {
            if position == limit {
                truncated = true;
                break;
            }
            let entry = entry?;
            let name = entry.file_name();
            let bytes = self
                .open_artifact(&name)
                .and_then(|file| file.metadata())
                .map_or(0, |metadata| metadata.len());
            output.push((self.config.artifact_dir.join(name), bytes));
        }
        Ok((output, truncated))
    }

    #[cfg(not(target_os = "linux"))]
    pub fn derived_artifact_paths(&self, _limit: usize) -> io::Result<(Vec<(PathBuf, u64)>, bool)> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "safe derived enumeration requires a pinned Linux directory handle",
        ))
    }

    /// Classifies a direct child of the configured derived-index directory.
    /// Symlinks and names not produced by this adapter are never owned.
    pub fn inspect_derived_artifact(&self, path: &Path) -> io::Result<DerivedArtifactStatus> {
        self.inspect_derived_artifact_cancellable(path, &std::sync::atomic::AtomicBool::new(false))
    }

    pub fn inspect_derived_artifact_cancellable(
        &self,
        path: &Path,
        cancelled: &std::sync::atomic::AtomicBool,
    ) -> io::Result<DerivedArtifactStatus> {
        let _ownership = self
            .artifact_ownership
            .lock()
            .expect("artifact ownership poisoned");
        self.inspect_derived_artifact_locked(path, cancelled)
    }

    fn inspect_derived_artifact_locked(
        &self,
        path: &Path,
        cancelled: &std::sync::atomic::AtomicBool,
    ) -> io::Result<DerivedArtifactStatus> {
        if path.parent() != Some(self.config.artifact_dir.as_path()) || !owned_index_name(path) {
            return Ok(DerivedArtifactStatus::NotOwned);
        }
        let name = path.file_name().expect("owned name");
        let mut file = match self.open_artifact(name) {
            Ok(value) => value,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(DerivedArtifactStatus::Missing);
            }
            Err(error) if error.raw_os_error() == Some(libc::ELOOP) => {
                return Ok(DerivedArtifactStatus::NotOwned);
            }
            Err(error) => return Err(error),
        };
        let metadata = file.metadata()?;
        if !metadata.file_type().is_file() {
            return Ok(DerivedArtifactStatus::NotOwned);
        }
        let active = self
            .state
            .lock()
            .expect("live state poisoned")
            .sources
            .values()
            .filter_map(|source| source.artifact_path.as_ref())
            .filter_map(|path| path.file_name())
            .any(|active_name| name == active_name);
        if active {
            return Ok(DerivedArtifactStatus::Active);
        }
        match file.try_lock_exclusive() {
            Ok(()) => {
                let opened = file.metadata()?;
                let Some(source) = source_bytes(path) else {
                    return Ok(DerivedArtifactStatus::NotOwned);
                };
                const VALIDATION_LIMIT: u64 = 16 * 1024 * 1024;
                if opened.len() > VALIDATION_LIMIT {
                    return Ok(DerivedArtifactStatus::Unverified {
                        bytes: opened.len(),
                        reason: "cleanup validation limit reached; preserved".into(),
                    });
                }
                match crate::index::validate_owned_artifact(
                    &mut file,
                    &source,
                    VALIDATION_LIMIT.min(self.config.maximum_index_bytes_per_source),
                    || cancelled.load(std::sync::atomic::Ordering::Acquire),
                ) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                        return Ok(DerivedArtifactStatus::Unverified {
                            bytes: opened.len(),
                            reason: "cleanup validation cancelled; preserved".into(),
                        });
                    }
                    Err(_) => return Ok(DerivedArtifactStatus::NotOwned),
                }
                Ok(DerivedArtifactStatus::Unused {
                    bytes: metadata.len(),
                    identity: DerivedArtifactIdentity {
                        name: name.to_os_string(),
                        directory: self.artifact_directory_identity,
                        file: file_revision_identity(&opened),
                    },
                })
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                Ok(DerivedArtifactStatus::Active)
            }
            Err(error) => Err(error),
        }
    }

    /// Deletes only an unused, exclusively locked adapter-owned index artifact.
    pub fn remove_unused_derived_artifact(
        &self,
        identity: &DerivedArtifactIdentity,
    ) -> io::Result<u64> {
        self.remove_unused_derived_artifact_cancellable(
            identity,
            &std::sync::atomic::AtomicBool::new(false),
        )
    }

    pub fn remove_unused_derived_artifact_cancellable(
        &self,
        identity: &DerivedArtifactIdentity,
        cancelled: &std::sync::atomic::AtomicBool,
    ) -> io::Result<u64> {
        let _ownership = self
            .artifact_ownership
            .lock()
            .expect("artifact ownership poisoned");
        if identity.directory != self.artifact_directory_identity
            || cancelled.load(std::sync::atomic::Ordering::Acquire)
        {
            return Ok(0);
        }
        let _cross_provider = self.ownership_file_lock()?;
        let mut file = self.open_artifact(&identity.name)?;
        file.try_lock_exclusive()?;
        if self
            .state
            .lock()
            .expect("live state poisoned")
            .sources
            .values()
            .filter_map(|source| source.artifact_path.as_ref())
            .filter_map(|path| path.file_name())
            .any(|active_name| identity.name == active_name)
        {
            return Ok(0);
        }
        let opened = file.metadata()?;
        if !opened.file_type().is_file() || file_revision_identity(&opened) != identity.file {
            return Ok(0);
        }
        let path = Path::new(&identity.name);
        let Some(source) = source_bytes(path) else {
            return Ok(0);
        };
        if crate::index::validate_owned_artifact(&mut file, &source, 16 * 1024 * 1024, || {
            cancelled.load(std::sync::atomic::Ordering::Acquire)
        })
        .is_err()
        {
            return Ok(0);
        }
        if cancelled.load(std::sync::atomic::Ordering::Acquire) {
            return Ok(0);
        }
        if self.exchange_and_unlink_reviewed(identity)? {
            crate::index::release_global_budget_locked(
                &self.config.artifact_dir.join(&identity.name),
                identity.file.length,
            )?;
            Ok(identity.file.length)
        } else {
            Ok(0)
        }
    }

    #[cfg(unix)]
    fn open_artifact(&self, name: &std::ffi::OsStr) -> io::Result<File> {
        use std::os::{
            fd::{AsRawFd, FromRawFd},
            unix::ffi::OsStrExt,
        };
        let name = std::ffi::CString::new(name.as_bytes()).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "artifact name contains NUL")
        })?;
        let fd = unsafe {
            libc::openat(
                self.artifact_directory.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDWR | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(unsafe { File::from_raw_fd(fd) })
    }

    #[cfg(not(unix))]
    fn open_artifact(&self, _name: &std::ffi::OsStr) -> io::Result<File> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "safe derived cleanup requires handle-relative Unix filesystem operations",
        ))
    }

    fn ownership_file_lock(&self) -> io::Result<File> {
        let lock = self.open_artifact(std::ffi::OsStr::new(".lvu-index-ownership.lock"))?;
        lock.lock_exclusive()?;
        Ok(lock)
    }

    #[cfg(target_os = "linux")]
    fn exchange_and_unlink_reviewed(&self, identity: &DerivedArtifactIdentity) -> io::Result<bool> {
        use std::os::{
            fd::{AsRawFd, FromRawFd},
            unix::ffi::OsStrExt,
        };
        let original = std::ffi::CString::new(identity.name.as_bytes())
            .map_err(|_| io::ErrorKind::InvalidInput)?;
        let quarantine_name = format!(".lvu-delete-{}-{}", std::process::id(), identity.file.inode);
        let quarantine =
            std::ffi::CString::new(quarantine_name.as_bytes()).expect("generated name");
        let directory = self.artifact_directory.as_raw_fd();
        let placeholder = unsafe {
            libc::openat(
                directory,
                quarantine.as_ptr(),
                libc::O_RDWR | libc::O_CLOEXEC | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW,
                0o600,
            )
        };
        if placeholder < 0 {
            return Err(io::Error::last_os_error());
        }
        drop(unsafe { File::from_raw_fd(placeholder) });
        run_before_artifact_exchange_hook();
        if unsafe {
            libc::renameat2(
                directory,
                original.as_ptr(),
                directory,
                quarantine.as_ptr(),
                libc::RENAME_EXCHANGE,
            )
        } < 0
        {
            let error = io::Error::last_os_error();
            unsafe {
                libc::unlinkat(directory, quarantine.as_ptr(), 0);
            }
            return Err(error);
        }
        let moved = self.open_artifact(std::ffi::OsStr::new(&quarantine_name))?;
        if !same_content_revision(&file_revision_identity(&moved.metadata()?), &identity.file) {
            if unsafe {
                libc::renameat2(
                    directory,
                    original.as_ptr(),
                    directory,
                    quarantine.as_ptr(),
                    libc::RENAME_EXCHANGE,
                )
            } < 0
            {
                return Err(io::Error::last_os_error());
            }
            if unsafe { libc::unlinkat(directory, quarantine.as_ptr(), 0) } < 0 {
                return Err(io::Error::last_os_error());
            }
            return Ok(false);
        }
        if unsafe { libc::unlinkat(directory, quarantine.as_ptr(), 0) } < 0
            || unsafe { libc::unlinkat(directory, original.as_ptr(), 0) } < 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(true)
    }

    #[cfg(not(target_os = "linux"))]
    fn exchange_and_unlink_reviewed(
        &self,
        _identity: &DerivedArtifactIdentity,
    ) -> io::Result<bool> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "safe derived cleanup requires Linux handle-relative exchange/unlink",
        ))
    }

    /// Returns the journal-bound artifact path once the worker has identified
    /// the journal; before then, returns the legacy SourceId-only path.
    pub fn index_path(&self, source_id: SourceId) -> PathBuf {
        if let Some(path) = self
            .state
            .lock()
            .expect("live state poisoned")
            .sources
            .get(&source_id)
            .and_then(|source| source.artifact_path.clone())
        {
            return path;
        }
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

#[cfg(unix)]
fn directory_identity(metadata: &std::fs::Metadata) -> DirectoryIdentity {
    use std::os::unix::fs::MetadataExt;
    DirectoryIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    }
}

#[cfg(not(unix))]
fn directory_identity(_metadata: &std::fs::Metadata) -> DirectoryIdentity {
    DirectoryIdentity {
        device: 0,
        inode: 0,
    }
}

#[cfg(unix)]
fn file_revision_identity(metadata: &std::fs::Metadata) -> FileRevisionIdentity {
    use std::os::unix::fs::MetadataExt;
    FileRevisionIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
        length: metadata.len(),
        modified_seconds: metadata.mtime(),
        modified_nanos: metadata.mtime_nsec(),
        changed_seconds: metadata.ctime(),
        changed_nanos: metadata.ctime_nsec(),
    }
}

fn same_content_revision(left: &FileRevisionIdentity, right: &FileRevisionIdentity) -> bool {
    left.device == right.device
        && left.inode == right.inode
        && left.length == right.length
        && left.modified_seconds == right.modified_seconds
        && left.modified_nanos == right.modified_nanos
}

#[cfg(not(unix))]
fn file_revision_identity(metadata: &std::fs::Metadata) -> FileRevisionIdentity {
    FileRevisionIdentity {
        device: 0,
        inode: 0,
        length: metadata.len(),
        modified_seconds: 0,
        modified_nanos: 0,
        changed_seconds: 0,
        changed_nanos: 0,
    }
}

fn source_bytes(path: &Path) -> Option<[u8; 16]> {
    let name = path.file_name()?.to_str()?.strip_suffix(".rows.idx")?;
    let name = name.split('.').next()?;
    let compact = name
        .bytes()
        .filter(|byte| *byte != b'-')
        .collect::<Vec<_>>();
    if compact.len() != 32 {
        return None;
    }
    let mut output = [0; 16];
    for (index, byte) in output.iter_mut().enumerate() {
        *byte = (hex(compact[index * 2])? << 4) | hex(compact[index * 2 + 1])?;
    }
    Some(output)
}

fn hex(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
static BEFORE_ARTIFACT_EXCHANGE: std::sync::Mutex<Option<Box<dyn FnOnce() + Send>>> =
    std::sync::Mutex::new(None);

#[cfg(test)]
fn run_before_artifact_exchange_hook() {
    if let Some(hook) = BEFORE_ARTIFACT_EXCHANGE
        .lock()
        .expect("hook poisoned")
        .take()
    {
        hook();
    }
}

#[cfg(not(test))]
fn run_before_artifact_exchange_hook() {}

fn owned_index_name(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let Some(stem) = name.strip_suffix(".rows.idx") else {
        return false;
    };
    let mut parts = stem.split('.');
    let Some(source) = parts.next() else {
        return false;
    };
    let journal = parts.next();
    if parts.next().is_some() {
        return false;
    }
    uuid_name(source) && journal.is_none_or(uuid_name)
}

fn uuid_name(stem: &str) -> bool {
    stem.len() == 36
        && stem.bytes().enumerate().all(|(index, byte)| {
            matches!(index, 8 | 13 | 18 | 23) && byte == b'-'
                || !matches!(index, 8 | 13 | 18 | 23) && byte.is_ascii_hexdigit()
        })
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

    fn context_page(
        &self,
        view_id: &str,
        anchor: &RowId,
        offset: isize,
        len: usize,
    ) -> lvu::ContextPage {
        let mut result = lvu::ContextPage {
            anchor_position: None,
            start: 0,
            total: 0,
            rows: Vec::new(),
            pending: false,
            diagnostic: None,
        };
        let mut state = self.state.lock().expect("live state poisoned");
        let Some(source_id) = state.source_in_view(view_id, &anchor.source_id) else {
            result.diagnostic = Some("context source is no longer registered".into());
            return result;
        };
        let source = &state.sources[&source_id];
        result.total = usize_from_u64(source.indexed_records);
        let key = IdKey {
            source_id,
            generation: source.generation,
            sequence: anchor.sequence,
        };
        let Some(position) = state.id_positions.get(&key).copied() else {
            if matches!(source.index, IndexState::Error | IndexState::Shutdown)
                || (matches!(source.index, IndexState::Ready | IndexState::Limited)
                    && source
                        .high_watermark
                        .is_none_or(|high| anchor.sequence > high))
            {
                result.diagnostic =
                    Some(source.last_error.clone().unwrap_or_else(|| {
                        "record is outside the available indexed history".into()
                    }));
                return result;
            }
            state.enqueue(source_id, Request::Sequence(anchor.sequence));
            result.pending = true;
            return result;
        };
        result.anchor_position = Some(usize_from_u64(position));
        result.start = usize_from_u64(position)
            .saturating_add_signed(offset)
            .min(result.total.saturating_sub(1));
        let len = len
            .min(32)
            .min(self.config.maximum_request_rows)
            .min((self.config.cache_rows / 2).max(1))
            .min(result.total.saturating_sub(result.start));
        let prefix = state.views[view_id]
            .sources
            .iter()
            .take_while(|id| **id != source_id)
            .filter_map(|id| state.sources.get(id))
            .map(|source| usize_from_u64(source.indexed_records))
            .fold(0usize, usize::saturating_add);
        drop(state);
        result.rows = self
            .page(
                view_id,
                ViewportRequest {
                    start: prefix.saturating_add(result.start),
                    len,
                },
            )
            .rows;
        // A concurrently replaced registration must never expose another source.
        result
            .rows
            .retain(|row| row.id.source_id == anchor.source_id);
        result.pending = result.rows.len() < len;
        result
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
            WorkerUpdate::Artifact {
                source_id,
                generation: _,
                epoch: _,
                path,
            } => {
                self.sources
                    .get_mut(&source_id)
                    .expect("generation checked")
                    .artifact_path = Some(path);
            }
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
                    request: request.clone(),
                });
                if let Some(source) = self.sources.get_mut(&source_id)
                    && source
                        .lookup_failure
                        .as_ref()
                        .is_some_and(|failure| failure.request == request)
                {
                    source.lookup_failure = None;
                }
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
                mut message,
            } => {
                self.pending.remove(&RequestKey {
                    source_id,
                    generation,
                    epoch,
                    request: request.clone(),
                });
                if let Some(source) = self.sources.get_mut(&source_id) {
                    // Serving a cached-row lookup and maintaining the derived
                    // index are separate operations. Keep the request failure
                    // visible without falsely declaring indexing terminal, and
                    // clear it only when that same request later succeeds.
                    truncate_utf8(&mut message, MAX_LOOKUP_DIAGNOSTIC_BYTES);
                    source.lookup_failure = Some(LookupFailure { request, message });
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
            last_error: source
                .lookup_failure
                .as_ref()
                .map(|failure| failure.message.clone())
                .or_else(|| source.last_error.clone()),
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
    lookup_failure: Option<LookupFailure>,
    artifact_path: Option<PathBuf>,
    requests: mpsc::Sender<Request>,
}
struct LookupFailure {
    request: Request,
    message: String,
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
    Artifact {
        source_id: SourceId,
        generation: u64,
        epoch: u64,
        path: PathBuf,
    },
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
            Self::Artifact {
                source_id,
                generation,
                epoch,
                ..
            }
            | Self::Progress {
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
    artifact_dir: PathBuf,
    config: LiveConfig,
    mut requests: mpsc::Receiver<Request>,
    updates: mpsc::Sender<WorkerUpdate>,
    mut cancelled: watch::Receiver<bool>,
) {
    let generation = token.generation;
    let epoch = token.epoch;
    let source_id = handle.source_id();
    // The source/generation pair is scoped to one capture root. Bind derived
    // offsets to the durable acquisition UUID in the journal itself so a
    // global cache directory cannot alias another capture root's generation 1.
    let journal_identity = loop {
        if *cancelled.borrow() {
            return;
        }
        let progress = handle.progress();
        if progress.records > 0 {
            let page = tokio::select! {
                biased;
                changed = cancelled.changed() => {
                    if changed.is_err() || *cancelled.borrow() { return; }
                    continue;
                }
                result = handle.read_page(0, 1, config.index_page_bytes) => result,
            };
            match page {
                Ok(page) => {
                    if let Some(record) = page.records.first() {
                        break record.acquisition_id;
                    }
                }
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
                            Some(format!("cannot identify backing journal: {error}")),
                        ),
                    )
                    .await;
                    return;
                }
            }
        } else {
            let state = if matches!(
                progress.state,
                RuntimeState::Stopped
                    | RuntimeState::Aborted
                    | RuntimeState::Incomplete
                    | RuntimeState::StorageBlocked
                    | RuntimeState::Error
            ) {
                IndexState::Ready
            } else {
                IndexState::Indexing
            };
            if !emit(
                &updates,
                &mut cancelled,
                progress_update(&handle, generation, epoch, 0, None, state, None),
            )
            .await
            {
                return;
            }
        }
        tokio::select! {
            biased;
            changed = cancelled.changed() => {
                if changed.is_err() || *cancelled.borrow() { return; }
            }
            _ = tokio::time::sleep(std::time::Duration::from_millis(20)) => {}
        }
    };
    let artifact = artifact_dir.join(format!("{}.{}.rows.idx", source_id.0, journal_identity));
    if !emit(
        &updates,
        &mut cancelled,
        WorkerUpdate::Artifact {
            source_id,
            generation,
            epoch,
            path: artifact.clone(),
        },
    )
    .await
    {
        return;
    }
    let (mut disk, rebuilt) = match DiskService::open(
        artifact,
        source_id,
        generation,
        config.index_page_records,
        config.index_page_bytes,
        *journal_identity.as_bytes(),
        IndexBudget {
            per_source: config.maximum_index_bytes_per_source,
            total: config.maximum_total_index_bytes,
            reconciliation_limit: config.maximum_sources.saturating_mul(4).clamp(64, 4096),
        },
    )
    .await
    {
        Ok(value) => value,
        Err((kind, error)) => {
            let _ = emit(
                &updates,
                &mut cancelled,
                progress_update(
                    &handle,
                    generation,
                    epoch,
                    0,
                    None,
                    if kind == std::io::ErrorKind::WriteZero {
                        IndexState::Limited
                    } else {
                        IndexState::Error
                    },
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
    display_projection(record, config.maximum_display_bytes, config.cache_bytes)
}

/// Builds the bounded display projection used by the live provider.
///
/// View adapters may retain this projection under their own explicit memory
/// budget when one logical display row needs several physical records at once.
pub fn display_projection(
    record: &RawRecord,
    maximum_display_bytes: usize,
    maximum_row_bytes: usize,
) -> DisplayRow {
    let fragment = record.chunk != ChunkPosition::Complete;
    let projection_len = record.bytes.len().min(maximum_display_bytes);
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
        if bytes <= maximum_row_bytes || text.is_empty() {
            return row;
        }
        let target = text.len().saturating_sub(bytes - maximum_row_bytes);
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
    let fields = recognized_fields(&text);
    let event_time = recognize_event_time(&record.bytes);
    let severity = fields
        .iter()
        .find(|(key, _)| matches!(key.as_str(), "level" | "severity" | "lvl"))
        .map(|(_, value)| normalize_severity(value))
        .unwrap_or_default();
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
    match event_time {
        EventTimeRecognition::Valid { field, unix_nanos } => {
            details.push(("event_time_field".into(), field));
            details.push(("event_time_utc".into(), format_rfc3339_utc(unix_nanos)));
            details.push(("event_time_utc_nanos".into(), unix_nanos.to_string()));
            details.push((
                "event_time_note".into(),
                "RFC3339 offset normalized to UTC".into(),
            ));
        }
        EventTimeRecognition::Invalid { field, diagnostic } => {
            details.push(("event_time_field".into(), field));
            details.push(("event_time_invalid".into(), diagnostic));
        }
        EventTimeRecognition::Missing => {}
    }
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
        captured_at_unix_nanos: Some(record.captured_at_unix_nanos),
        level: if fragment {
            "fragment".into()
        } else if !severity.is_empty() {
            severity
        } else {
            String::new()
        },
        text,
        details,
        fields,
    }
}

const MAX_EVENT_TIME_RECORD_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EventTimeRecognition {
    Valid { field: String, unix_nanos: i64 },
    Invalid { field: String, diagnostic: String },
    Missing,
}

pub fn recognize_event_time(bytes: &[u8]) -> EventTimeRecognition {
    if bytes.len() > MAX_EVENT_TIME_RECORD_BYTES {
        return EventTimeRecognition::Invalid {
            field: "timestamp/time/ts".into(),
            diagnostic: "record exceeds bounded event-time recognition limit".into(),
        };
    }
    let text = String::from_utf8_lossy(bytes);
    let candidate = if let Ok(serde_json::Value::Object(object)) = serde_json::from_str(&text) {
        ["timestamp", "time", "ts"].into_iter().find_map(|key| {
            object.get(key).map(|value| {
                (
                    key.to_owned(),
                    match value {
                        serde_json::Value::String(value) => value.clone(),
                        other => other.to_string(),
                    },
                    !value.is_string(),
                )
            })
        })
    } else {
        match event_time_logfmt_candidate(&text) {
            Ok(candidate) => candidate,
            Err(diagnostic) => {
                return EventTimeRecognition::Invalid {
                    field: "timestamp/time/ts".into(),
                    diagnostic,
                };
            }
        }
    };
    let Some((field, value, non_string)) = candidate else {
        return EventTimeRecognition::Missing;
    };
    if non_string || looks_numeric(&value) || looks_timezone_less(&value) {
        return EventTimeRecognition::Invalid {
            field,
            diagnostic:
                "ambiguous timestamp requires explicit RFC3339 timezone/epoch interpretation".into(),
        };
    }
    match parse_rfc3339_nanos(&value) {
        Ok(unix_nanos) => EventTimeRecognition::Valid { field, unix_nanos },
        Err(diagnostic) => EventTimeRecognition::Invalid { field, diagnostic },
    }
}

/// Traverses the full bounded record independently of the clipped display
/// projection. Candidate precedence is `timestamp`, then `time`, then `ts`.
fn event_time_logfmt_candidate(text: &str) -> Result<Option<(String, String, bool)>, String> {
    let bytes = text.as_bytes();
    let mut candidates: [Option<String>; 3] = [None, None, None];
    let mut cursor = 0usize;
    while cursor < bytes.len() {
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if cursor == bytes.len() {
            break;
        }
        let key_start = cursor;
        while cursor < bytes.len() && !bytes[cursor].is_ascii_whitespace() && bytes[cursor] != b'='
        {
            cursor += 1;
        }
        if cursor == bytes.len() || bytes[cursor] != b'=' {
            while cursor < bytes.len() && !bytes[cursor].is_ascii_whitespace() {
                cursor += 1;
            }
            continue;
        }
        let key = &text[key_start..cursor];
        cursor += 1;
        let value = if cursor < bytes.len() && bytes[cursor] == b'"' {
            cursor += 1;
            let mut value = String::new();
            let mut closed = false;
            while cursor < bytes.len() {
                match bytes[cursor] {
                    b'"' => {
                        cursor += 1;
                        closed = true;
                        break;
                    }
                    b'\\' => {
                        cursor += 1;
                        if cursor == bytes.len() {
                            break;
                        }
                        let escaped_start = cursor;
                        let escaped = text[escaped_start..]
                            .chars()
                            .next()
                            .expect("cursor is within text");
                        value.push(escaped);
                        cursor += escaped.len_utf8();
                    }
                    _ => {
                        let character = text[cursor..]
                            .chars()
                            .next()
                            .expect("cursor is within text");
                        value.push(character);
                        cursor += character.len_utf8();
                    }
                }
            }
            if !closed {
                return Err("malformed logfmt: unterminated quoted value".into());
            }
            value
        } else {
            let value_start = cursor;
            while cursor < bytes.len() && !bytes[cursor].is_ascii_whitespace() {
                cursor += 1;
            }
            text[value_start..cursor].to_owned()
        };
        if let Some(index) = ["timestamp", "time", "ts"]
            .iter()
            .position(|candidate| *candidate == key)
            && candidates[index].is_none()
        {
            candidates[index] = Some(value);
        }
    }
    Ok(candidates
        .into_iter()
        .enumerate()
        .find_map(|(index, value)| {
            value.map(|value| (["timestamp", "time", "ts"][index].to_owned(), value, false))
        }))
}

fn looks_numeric(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'+' | b'-' | b'.'))
}

fn looks_timezone_less(value: &str) -> bool {
    value.len() >= 19
        && matches!(value.as_bytes().get(10), Some(b'T' | b't'))
        && !value.ends_with('Z')
        && !value.ends_with('z')
        && value
            .get(19..)
            .is_none_or(|tail| !tail.contains('+') && !tail.contains('-'))
}

fn parse_rfc3339_nanos(value: &str) -> Result<i64, String> {
    let (date_time, offset_seconds) = if let Some(body) =
        value.strip_suffix('Z').or_else(|| value.strip_suffix('z'))
    {
        (body, 0_i64)
    } else {
        let offset_index = value
            .get(19..)
            .and_then(|tail| tail.rfind(['+', '-']).map(|index| index + 19))
            .ok_or_else(|| "RFC3339 timestamp requires Z or an explicit UTC offset".to_owned())?;
        let (body, offset) = value.split_at(offset_index);
        let bytes = offset.as_bytes();
        if bytes.len() != 6
            || !matches!(bytes[0], b'+' | b'-')
            || bytes[3] != b':'
            || !bytes[1..3].iter().all(u8::is_ascii_digit)
            || !bytes[4..6].iter().all(u8::is_ascii_digit)
        {
            return Err("invalid RFC3339 UTC offset".into());
        }
        let hours = offset[1..3].parse::<i64>().map_err(|_| "invalid offset")?;
        let minutes = offset[4..6].parse::<i64>().map_err(|_| "invalid offset")?;
        if hours > 23 || minutes > 59 {
            return Err("invalid RFC3339 UTC offset".into());
        }
        let sign = if bytes[0] == b'+' { 1 } else { -1 };
        (body, sign * (hours * 3600 + minutes * 60))
    };
    let (whole, fraction) = date_time.split_once('.').unwrap_or((date_time, ""));
    let bytes = whole.as_bytes();
    if bytes.len() != 19
        || [(4, b'-'), (7, b'-'), (13, b':'), (16, b':')]
            .iter()
            .any(|&(index, expected)| bytes[index] != expected)
        || !matches!(bytes[10], b'T' | b't')
        || bytes
            .iter()
            .enumerate()
            .any(|(index, byte)| !matches!(index, 4 | 7 | 10 | 13 | 16) && !byte.is_ascii_digit())
        || fraction.is_empty() && date_time.contains('.')
        || fraction.len() > 9
        || !fraction.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err("invalid RFC3339 timestamp".into());
    }
    let number = |range: std::ops::Range<usize>| {
        whole[range]
            .parse::<i64>()
            .map_err(|_| "invalid RFC3339 number".to_owned())
    };
    let (year, month, day, hour, minute, second) = (
        number(0..4)?,
        number(5..7)?,
        number(8..10)?,
        number(11..13)?,
        number(14..16)?,
        number(17..19)?,
    );
    if year < 1 || !(1..=12).contains(&month) || hour > 23 || minute > 59 || second > 59 {
        return Err("invalid RFC3339 date/time".into());
    }
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days_in_month = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    if day < 1 || day > days_in_month[(month - 1) as usize] {
        return Err("invalid RFC3339 calendar date".into());
    }
    let y = year - i64::from(month <= 2);
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = month + if month > 2 { -3 } else { 9 };
    let doy = (153 * mp + 2) / 5 + day - 1;
    let days = era * 146097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719468;
    let seconds = days
        .checked_mul(86400)
        .and_then(|base| base.checked_add(hour * 3600 + minute * 60 + second))
        .and_then(|base| base.checked_sub(offset_seconds))
        .ok_or_else(|| "RFC3339 timestamp overflows supported range".to_owned())?;
    let nanos = if fraction.is_empty() {
        0
    } else {
        format!("{fraction:0<9}")
            .parse::<i64>()
            .map_err(|_| "invalid RFC3339 fraction".to_owned())?
    };
    seconds
        .checked_mul(1_000_000_000)
        .and_then(|base| base.checked_add(nanos))
        .ok_or_else(|| "RFC3339 timestamp overflows supported range".to_owned())
}

fn format_rfc3339_utc(value: i64) -> String {
    lvu::format_utc_nanos(value)
}

const MAX_DISPLAY_FIELDS: usize = 32;
const MAX_FIELD_KEY_BYTES: usize = 64;
const MAX_FIELD_VALUE_BYTES: usize = 512;

fn recognized_fields(text: &str) -> Vec<(String, String)> {
    if let Ok(serde_json::Value::Object(values)) = serde_json::from_str(text) {
        return values
            .into_iter()
            .filter_map(|(key, value)| {
                let value = match value {
                    serde_json::Value::Null => "null".into(),
                    serde_json::Value::Bool(value) => value.to_string(),
                    serde_json::Value::Number(value) => value.to_string(),
                    serde_json::Value::String(value) => value,
                    _ => return None,
                };
                bounded_field(key, value)
            })
            .take(MAX_DISPLAY_FIELDS)
            .collect();
    }
    logfmt_fields(text)
}

fn logfmt_fields(text: &str) -> Vec<(String, String)> {
    let mut fields = Vec::new();
    let mut rest = text.trim_start();
    while !rest.is_empty() && fields.len() < MAX_DISPLAY_FIELDS {
        let token_end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        let Some(equal) = rest[..token_end].find('=') else {
            rest = rest[token_end..].trim_start();
            continue;
        };
        let key = &rest[..equal];
        let after = &rest[equal + 1..];
        let (value, consumed) = if let Some(quoted) = after.strip_prefix('"') {
            let closing = quoted.find('"');
            let end = closing.unwrap_or(quoted.len());
            (
                &quoted[..end],
                equal + 2 + end + usize::from(closing.is_some()),
            )
        } else {
            let end = after.find(char::is_whitespace).unwrap_or(after.len());
            (&after[..end], equal + 1 + end)
        };
        if let Some(field) = bounded_field(key.to_owned(), value.to_owned()) {
            fields.push(field);
        }
        rest = rest[consumed.min(rest.len())..].trim_start();
    }
    fields
}

fn bounded_field(mut key: String, mut value: String) -> Option<(String, String)> {
    if key.is_empty() || key.len() > MAX_FIELD_KEY_BYTES {
        return None;
    }
    truncate_utf8(&mut key, MAX_FIELD_KEY_BYTES);
    truncate_utf8(&mut value, MAX_FIELD_VALUE_BYTES);
    Some((key, value))
}

fn normalize_severity(value: &str) -> String {
    match value.to_ascii_lowercase().as_str() {
        "trace" => "TRACE",
        "debug" => "DEBUG",
        "info" | "information" => "INFO",
        "warn" | "warning" => "WARN",
        "error" | "err" => "ERROR",
        "fatal" | "critical" => "FATAL",
        _ => "",
    }
    .into()
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
        .saturating_add(
            row.fields
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
        || config.maximum_index_bytes_per_source < 100
        || config.maximum_total_index_bytes < 100
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

#[cfg(test)]
mod diagnostic_state_tests {
    use super::*;

    fn state_with_source(source_id: SourceId, generation: u64, epoch: u64) -> State {
        let (requests, _request_rx) = mpsc::channel(1);
        let mut state = State::default();
        state.sources.insert(
            source_id,
            SourceState {
                generation,
                epoch,
                acquisition: RuntimeState::Stopped,
                reported_records: 3,
                indexed_records: 3,
                high_watermark: Some(2),
                index: IndexState::Ready,
                last_error: None,
                lookup_failure: None,
                artifact_path: None,
                requests,
            },
        );
        state
    }

    fn progress(source_id: SourceId, generation: u64, epoch: u64) -> WorkerUpdate {
        WorkerUpdate::Progress {
            source_id,
            generation,
            epoch,
            acquisition: RuntimeState::Stopped,
            reported_records: 3,
            indexed_records: 3,
            high_watermark: Some(2),
            index: IndexState::Ready,
            error: None,
        }
    }

    fn row(source_id: SourceId, sequence: u64) -> DisplayRow {
        DisplayRow {
            id: RowId::new(source_id.0.to_string(), sequence),
            timestamp: String::new(),
            captured_at_unix_nanos: None,
            level: String::new(),
            text: format!("row {sequence}"),
            details: Vec::new(),
            fields: Vec::new(),
        }
    }

    #[test]
    fn lookup_failure_survives_progress_and_only_matching_success_clears_it() {
        let source_id = SourceId::new();
        let generation = 4;
        let epoch = 9;
        let failed = Request::Sequence(2);
        let mut state = state_with_source(source_id, generation, epoch);
        let config = LiveConfig::new("unused-test-artifacts");

        state.apply(
            WorkerUpdate::Failed {
                source_id,
                generation,
                epoch,
                request: failed.clone(),
                message: "controlled lookup failure".into(),
            },
            &config,
        );
        let status = state.source_status(source_id).unwrap();
        assert_eq!(
            status.index,
            IndexState::Ready,
            "lookup is not index failure"
        );
        assert_eq!(
            status.last_error.as_deref(),
            Some("controlled lookup failure")
        );

        state.apply(progress(source_id, generation, epoch), &config);
        assert_eq!(
            state
                .source_status(source_id)
                .unwrap()
                .last_error
                .as_deref(),
            Some("controlled lookup failure"),
            "unrelated index progress must not erase lookup evidence"
        );

        state.apply(
            WorkerUpdate::Rows {
                source_id,
                generation,
                epoch,
                request: Request::Sequence(1),
                rows: vec![(1, row(source_id, 1))],
            },
            &config,
        );
        assert_eq!(
            state
                .source_status(source_id)
                .unwrap()
                .last_error
                .as_deref(),
            Some("controlled lookup failure"),
            "a different successful lookup is not recovery"
        );

        state.apply(
            WorkerUpdate::Rows {
                source_id,
                generation,
                epoch,
                request: failed,
                rows: vec![(2, row(source_id, 2))],
            },
            &config,
        );
        assert_eq!(state.source_status(source_id).unwrap().last_error, None);
    }

    #[test]
    fn stale_generation_updates_cannot_set_or_clear_current_lookup_failure() {
        let source_id = SourceId::new();
        let generation = 8;
        let epoch = 13;
        let request = Request::Positions { start: 0, len: 1 };
        let mut state = state_with_source(source_id, generation, epoch);
        let config = LiveConfig::new("unused-test-artifacts");

        state.apply(
            WorkerUpdate::Failed {
                source_id,
                generation,
                epoch,
                request: request.clone(),
                message: "current lookup failure".into(),
            },
            &config,
        );
        state.apply(
            WorkerUpdate::Rows {
                source_id,
                generation: generation - 1,
                epoch: epoch - 1,
                request: request.clone(),
                rows: Vec::new(),
            },
            &config,
        );
        assert_eq!(
            state
                .source_status(source_id)
                .unwrap()
                .last_error
                .as_deref(),
            Some("current lookup failure")
        );

        state.apply(
            WorkerUpdate::Failed {
                source_id,
                generation: generation - 1,
                epoch: epoch - 1,
                request: Request::Sequence(99),
                message: "stale replacement".into(),
            },
            &config,
        );
        assert_eq!(
            state
                .source_status(source_id)
                .unwrap()
                .last_error
                .as_deref(),
            Some("current lookup failure")
        );

        state.apply(
            WorkerUpdate::Rows {
                source_id,
                generation,
                epoch,
                request,
                rows: vec![(0, row(source_id, 0))],
            },
            &config,
        );
        assert_eq!(state.source_status(source_id).unwrap().last_error, None);
    }

    #[test]
    fn retained_lookup_diagnostic_is_utf8_bounded() {
        let source_id = SourceId::new();
        let generation = 2;
        let epoch = 3;
        let mut state = state_with_source(source_id, generation, epoch);
        let config = LiveConfig::new("unused-test-artifacts");

        state.apply(
            WorkerUpdate::Failed {
                source_id,
                generation,
                epoch,
                request: Request::Sequence(0),
                message: "é".repeat(MAX_LOOKUP_DIAGNOSTIC_BYTES),
            },
            &config,
        );

        let diagnostic = state.source_status(source_id).unwrap().last_error.unwrap();
        assert!(diagnostic.len() <= MAX_LOOKUP_DIAGNOSTIC_BYTES);
        assert!(diagnostic.is_char_boundary(diagnostic.len()));
    }
}

#[cfg(test)]
mod presentation_tests {
    use super::{
        EventTimeRecognition, MAX_EVENT_TIME_RECORD_BYTES, normalize_severity,
        recognize_event_time, recognized_fields,
    };

    #[test]
    fn recognizes_bounded_json_and_quoted_logfmt_without_changing_raw() {
        let json = r#"{"level":"error","service":"api","missing":null,"nested":{"x":1}}"#;
        let fields = recognized_fields(json);
        assert!(fields.contains(&("service".into(), "api".into())));
        assert!(fields.contains(&("missing".into(), "null".into())));
        assert!(fields.iter().all(|(key, _)| key != "nested"));
        assert_eq!(normalize_severity("Critical"), "FATAL");

        let fields = recognized_fields(r#"level=warn service=worker message="two words""#);
        assert!(fields.contains(&("message".into(), "two words".into())));
        assert!(recognized_fields("malformed raw text").is_empty());
    }

    #[test]
    fn recognizes_only_explicit_rfc3339_event_times_and_normalizes_offsets() {
        assert!(matches!(
            recognize_event_time(br#"{"timestamp":"2026-09-05T12:30:45.123456789Z"}"#),
            EventTimeRecognition::Valid {
                unix_nanos: 1_788_611_445_123_456_789,
                ..
            }
        ));
        assert!(matches!(
            recognize_event_time(b"time=2026-09-05T14:30:45+02:00 level=info"),
            EventTimeRecognition::Valid {
                unix_nanos: 1_788_611_445_000_000_000,
                ..
            }
        ));
        for raw in [
            br#"{"ts":"2026-09-05T12:30:45"}"#.as_slice(),
            br#"{"ts":1788611445}"#.as_slice(),
            br#"{"timestamp":"nope"}"#.as_slice(),
        ] {
            assert!(matches!(
                recognize_event_time(raw),
                EventTimeRecognition::Invalid { .. }
            ));
        }
        assert_eq!(
            recognize_event_time(b"plain raw"),
            EventTimeRecognition::Missing
        );
    }

    #[test]
    fn event_time_logfmt_scan_is_full_escaped_and_key_targeted() {
        let prefix = (0..40)
            .map(|index| format!("k{index}=v"))
            .collect::<Vec<_>>()
            .join(" ");
        let after_display_limit = format!("{prefix} ts=2026-09-05T12:30:45Z");
        assert!(matches!(
            recognize_event_time(after_display_limit.as_bytes()),
            EventTimeRecognition::Valid { field, .. } if field == "ts"
        ));

        let message_only = r#"message="said \"timestamp=2026-09-05T12:30:45Z\" only" service=api"#;
        assert_eq!(
            recognize_event_time(message_only.as_bytes()),
            EventTimeRecognition::Missing
        );
        let quoted_then_time = format!(
            r#"message="{} \"quoted\" text" time="2026-09-05T14:30:45+02:00""#,
            "x".repeat(600)
        );
        assert!(matches!(
            recognize_event_time(quoted_then_time.as_bytes()),
            EventTimeRecognition::Valid {
                unix_nanos: 1_788_611_445_000_000_000,
                ..
            }
        ));
        assert!(matches!(
            recognize_event_time(br#"message="unterminated timestamp=2026-09-05T12:30:45Z"#),
            EventTimeRecognition::Invalid { diagnostic, .. }
                if diagnostic.contains("unterminated")
        ));

        let clipped_candidate = format!("timestamp=2026-09-05T12:30:45Z{}", "x".repeat(600));
        assert!(matches!(
            recognize_event_time(clipped_candidate.as_bytes()),
            EventTimeRecognition::Invalid { .. }
        ));
        assert!(matches!(
            recognize_event_time(
                b"time=2026-09-05T12:30:45Z timestamp=invalid ts=2026-09-05T12:30:45Z"
            ),
            EventTimeRecognition::Invalid { field, .. } if field == "timestamp"
        ));
        assert!(matches!(
            recognize_event_time(&vec![b'x'; MAX_EVENT_TIME_RECORD_BYTES + 1]),
            EventTimeRecognition::Invalid { diagnostic, .. }
                if diagnostic.contains("bounded event-time recognition limit")
        ));
    }
}

#[cfg(test)]
mod artifact_tests {
    use super::*;
    use crate::index::DiskIndex;
    use std::io::Write;
    use tempfile::tempdir;

    #[tokio::test]
    async fn only_reviewed_valid_index_identity_is_removed_and_it_rebuilds() {
        let temp = tempdir().unwrap();
        let derived = temp.path().join("derived");
        let config = LiveConfig::new(&derived);
        let provider = LiveRowProvider::new(config.clone()).unwrap();
        let source = SourceId::new();
        let path = provider.index_path(source);
        let (index, _) = DiskIndex::open(
            &path,
            source,
            1,
            128,
            1024 * 1024,
            config.maximum_index_bytes_per_source,
        )
        .unwrap();
        drop(index);
        let identity = match provider.inspect_derived_artifact(&path).unwrap() {
            DerivedArtifactStatus::Unused { identity, .. } => identity,
            status => panic!("expected unused valid index, got {status:?}"),
        };
        let displaced = derived.join("reviewed-index-moved-by-race");
        let hook_path = path.clone();
        let hook_displaced = displaced.clone();
        *BEFORE_ARTIFACT_EXCHANGE.lock().unwrap() = Some(Box::new(move || {
            std::fs::rename(&hook_path, &hook_displaced).unwrap();
            std::fs::write(&hook_path, b"same-name replacement").unwrap();
        }));
        assert_eq!(
            provider.remove_unused_derived_artifact(&identity).unwrap(),
            0
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"same-name replacement");
        assert!(displaced.exists());

        std::fs::remove_file(&path).unwrap();
        std::fs::write(&path, b"replacement sentinel").unwrap();
        assert_eq!(
            provider.remove_unused_derived_artifact(&identity).unwrap(),
            0
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"replacement sentinel");

        std::fs::remove_file(&path).unwrap();
        let (index, _) = DiskIndex::open(
            &path,
            source,
            1,
            128,
            1024 * 1024,
            config.maximum_index_bytes_per_source,
        )
        .unwrap();
        drop(index);
        let identity = match provider.inspect_derived_artifact(&path).unwrap() {
            DerivedArtifactStatus::Unused { identity, .. } => identity,
            status => panic!("expected unused valid index, got {status:?}"),
        };
        let second_source = SourceId::new();
        let second_path = provider.index_path(second_source);
        let (second, _) = DiskIndex::open(
            &second_path,
            second_source,
            1,
            128,
            1024 * 1024,
            config.maximum_index_bytes_per_source,
        )
        .unwrap();
        drop(second);
        let second_identity = match provider.inspect_derived_artifact(&second_path).unwrap() {
            DerivedArtifactStatus::Unused { identity, .. } => identity,
            status => panic!("expected second unused valid index, got {status:?}"),
        };
        assert!(provider.remove_unused_derived_artifact(&identity).unwrap() > 0);
        assert!(provider.artifact_directory_is_current().unwrap());
        assert!(
            provider
                .remove_unused_derived_artifact(&second_identity)
                .unwrap()
                > 0
        );
        let (rebuilt, did_rebuild) = DiskIndex::open(
            &path,
            source,
            1,
            128,
            1024 * 1024,
            config.maximum_index_bytes_per_source,
        )
        .unwrap();
        assert!(did_rebuild);
        drop(rebuilt);

        let unknown = derived.join(format!("{}.rows.idx", SourceId::new().0));
        std::fs::write(&unknown, b"uuid-named unknown sentinel").unwrap();
        assert_eq!(
            provider.inspect_derived_artifact(&unknown).unwrap(),
            DerivedArtifactStatus::NotOwned
        );
        assert_eq!(
            std::fs::read(unknown).unwrap(),
            b"uuid-named unknown sentinel"
        );
    }

    #[tokio::test]
    async fn changed_parent_future_header_and_concurrent_owner_are_preserved() {
        let temp = tempdir().unwrap();
        let derived = temp.path().join("derived");
        let config = LiveConfig::new(&derived);
        let provider = LiveRowProvider::new(config.clone()).unwrap();
        let source = SourceId::new();
        let path = provider.index_path(source);
        let (index, _) = DiskIndex::open(
            &path,
            source,
            1,
            128,
            1024 * 1024,
            config.maximum_index_bytes_per_source,
        )
        .unwrap();
        drop(index);
        let reviewed = match provider.inspect_derived_artifact(&path).unwrap() {
            DerivedArtifactStatus::Unused { identity, .. } => identity,
            status => panic!("expected unused index, got {status:?}"),
        };
        let (owner, _) = DiskIndex::open(
            &path,
            source,
            1,
            128,
            1024 * 1024,
            config.maximum_index_bytes_per_source,
        )
        .unwrap();
        let other_provider = LiveRowProvider::new(config.clone()).unwrap();
        assert_eq!(
            other_provider.inspect_derived_artifact(&path).unwrap(),
            DerivedArtifactStatus::Active
        );
        assert!(provider.remove_unused_derived_artifact(&reviewed).is_err());
        drop(owner);

        let mut future = OpenOptions::new().write(true).open(&path).unwrap();
        future.write_all(b"FUTURE!!").unwrap();
        drop(future);
        assert_eq!(
            provider.inspect_derived_artifact(&path).unwrap(),
            DerivedArtifactStatus::NotOwned
        );

        let original = temp.path().join("derived-original");
        std::fs::rename(&derived, &original).unwrap();
        let external = temp.path().join("external");
        std::fs::create_dir(&external).unwrap();
        let external_file = external.join(path.file_name().unwrap());
        std::fs::write(&external_file, b"outside sentinel").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&external, &derived).unwrap();
        assert!(!provider.artifact_directory_is_current().unwrap());
        assert_eq!(
            provider.inspect_derived_artifact(&external_file).unwrap(),
            DerivedArtifactStatus::NotOwned
        );
        assert_eq!(std::fs::read(external_file).unwrap(), b"outside sentinel");
    }
}
