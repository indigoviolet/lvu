use crate::{
    catalog::{Catalog, CatalogEvent, SourceMetadata, next_generation, write_metadata},
    cursor,
    history::{SourceHistory, spawn_history},
    writer::{FileCursorSetup, PageRequest, StartupCancellation, WriterMessage, spawn_writer},
};
use fs2::FileExt;
use lvu_core::{
    Acquisition, Capture, CaptureEvent, HttpAcquisition, JournalError, JournalPage, JournalReader,
    RecordId, SourceDefinition, SourceId,
    acquisition::{
        CaptureHandle, CaptureLimits, capture_command_supervised, capture_file_auto_from,
    },
    http::capture_http,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    fs::{File, OpenOptions},
    io,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use thiserror::Error;
use tokio::io::AsyncRead;
use tokio::sync::{Notify, Semaphore, mpsc, oneshot, watch};

type OwnedReader = Pin<Box<dyn AsyncRead + Send + 'static>>;

struct StartRequest {
    definition: SourceDefinition,
    reader: Option<OwnedReader>,
    intent: StartIntent,
}

/// Why a source is being started.
///
/// This distinction is a product invariant, not a convenience: restoring a
/// workspace must never execute a remembered command or dial a remembered
/// endpoint. A restart or reconnect policy only ever applies to a source the
/// user has already started in this session.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StartIntent {
    /// The user asked for this source now. Command restart and HTTP reconnect
    /// policies apply for the life of this capture.
    UserRequested,
    /// Workspace restoration. Acquisitions whose start is an outward side
    /// effect are refused; nothing is launched, spawned or dialled.
    Restore,
}

#[derive(Clone, Debug)]
pub struct RuntimeConfig {
    pub acquisition: CaptureLimits,
    pub writer_queue_capacity: usize,
    pub batch_records: usize,
    /// Group commit: records that may accumulate before capture asks the
    /// filesystem to make them durable.
    ///
    /// This used to count *batches*, and a batch is however many messages
    /// happened to be queued when the writer woke up — so the commit rate
    /// tracked scheduling rather than data, and a producer that handed over a
    /// few records at a time produced an fsync every few records. On a volume
    /// where fsync costs tens of milliseconds that is the whole cost of
    /// capture. Counting records makes the rate a property of the data.
    pub sync_every_records: u64,
    /// The other half of group commit: however few records have arrived, they
    /// become durable within this interval of the commit that preceded them.
    /// Whichever bound is reached first commits.
    pub sync_interval: Duration,
    pub max_page_records: usize,
    pub max_page_bytes: usize,
    pub storage_limit_bytes: Option<u64>,
    pub graceful_stop_deadline: Duration,
    /// Deterministic delay hook useful for validating backpressure and deadlines.
    pub writer_delay: Duration,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            acquisition: CaptureLimits::default(),
            // Matched to the acquisition queue: both are counted in reads now
            // that a hand-over carries one, and both bound bytes rather than
            // records. See `CaptureLimits::channel_capacity`. Across the
            // default acquisition queue, writer queue, and the writer's
            // 64-record batch, the conservative retained-payload bound is
            // `(8 + 8 + 64) * (256 KiB + 64 KiB) = 25 MiB`; including the
            // producer's current read makes the capture path 25.3125 MiB. This
            // assumes the adversarial case where every batch record outlives
            // all siblings from its read; clones still share each backing and
            // add no payload bytes. Journal pages are separate compact frame
            // backings, serialized by `page_gate` and capped at 4 MiB of
            // decoded bytes plus at most 8192 58-byte frame prefixes (under
            // 4.5 MiB retained backing), making the combined bound less than
            // 29.8125 MiB; the
            // live cache keeps only owned, byte-charged display projections.
            // Page reads carry no capture payload and take no writer-queue
            // slot: the page gate bounds them to one outstanding request on
            // their own unit-capacity channel, so a flooded capture queue
            // cannot refuse a page admission.
            writer_queue_capacity: 8,
            batch_records: 64,
            // ~400 KB of a typical log line at 102 bytes, which keeps the
            // window a crash could re-read in the same order as the 256 KB
            // file-checkpoint interval it replaces, at a fortieth of the
            // commits.
            sync_every_records: 4096,
            sync_interval: Duration::from_millis(500),
            // The ceiling a caller's own page request is clamped to. It has
            // to leave room for the view scanner's page, or the scanner pays a
            // round trip per 512 records however large a page it asked for.
            max_page_records: 8192,
            max_page_bytes: 4 * 1024 * 1024,
            storage_limit_bytes: None,
            graceful_stop_deadline: Duration::from_secs(5),
            writer_delay: Duration::ZERO,
        }
    }
}

impl RuntimeConfig {
    fn validate(&self) -> Result<(), RuntimeError> {
        if self.writer_queue_capacity == 0
            || self.writer_queue_capacity == usize::MAX
            || self.batch_records == 0
            || self.sync_every_records == 0
            || self.sync_interval.is_zero()
            || self.max_page_records == 0
            || self.max_page_bytes == 0
            || self.graceful_stop_deadline.is_zero()
        {
            return Err(RuntimeError::InvalidConfig);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeState {
    Starting,
    Running,
    Stopping,
    Stopped,
    Aborting,
    Aborted,
    Incomplete,
    StorageBlocked,
    Error,
}

#[derive(Clone, Debug)]
pub struct SourceProgress {
    pub source_id: SourceId,
    pub generation: u64,
    pub state: RuntimeState,
    pub records: u64,
    pub high_watermark: Option<RecordId>,
    pub journal_bytes: u64,
    pub synced_records: u64,
    /// Durable commits performed for this acquisition. Capture's cost is
    /// dominated by how often it asks the filesystem for durability, and that
    /// rate is not visible in a record count or a byte count, so it is
    /// reported alongside them.
    pub syncs: u64,
    /// Capture events carrying records that the writer has taken.
    ///
    /// Each one is a hand-over across two bounded channels and a semaphore
    /// permit, and that cost is paid per event rather than per record — so the
    /// records-per-hand-over ratio, not the count, is what says whether the
    /// pipeline is moving reads or single records.
    pub handovers: u64,
    /// CPU the writer thread has spent on this source: encoding frames,
    /// appending them and committing.
    ///
    /// Per thread rather than per process, and accumulated only around capture
    /// messages. The same writer thread serves bounded journal pages for live
    /// indexing, but that query-side work is explicitly excluded. Capture's
    /// wall clock on a shared volume measures the neighbours; this measures
    /// the work.
    pub writer_cpu_nanos: u64,
    /// CPU acquisition spent framing source bytes before hand-over.
    pub reader_cpu_nanos: u64,
    pub boundaries: u64,
    pub exit_code: Option<i32>,
    pub discarded_bytes: u64,
    pub discarded_bytes_known: bool,
    pub last_error: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StopReport {
    pub complete: bool,
    pub discarded_bytes: u64,
    pub discarded_bytes_known: bool,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AbortReport {
    pub complete: bool,
    pub discarded_bytes: u64,
    pub discarded_bytes_known: bool,
}

#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error("runtime configuration contains a zero bound")]
    InvalidConfig,
    #[error("source is already running")]
    AlreadyRunning,
    #[error("source is not active")]
    NotActive,
    #[error("source definition schema is unsupported")]
    DefinitionUnsupported,
    #[error(
        "restoring a workspace cannot launch a remembered command; start the source explicitly"
    )]
    RestoreWouldLaunchCommand,
    #[error(
        "restoring a workspace cannot connect a remembered HTTP endpoint; start the source explicitly"
    )]
    RestoreWouldConnect,
    #[error("stdin cannot be restored; attach a new reader with start_with_reader")]
    RestoreCannotAttachStdin,
    #[error("stdin source requires an attached owned reader; use start_with_reader")]
    StdinNotAttached,
    #[error("an attached reader is only valid for a stdin source")]
    ReaderAttachmentMismatch,
    #[error("stdin source was already captured; create a new SourceId for a new stdin session")]
    StdinAlreadyCaptured,
    #[error("durable capture storage limit {limit} bytes reached")]
    StorageLimit { limit: u64 },
    #[error("source runtime task closed")]
    Closed,
    #[error("source shutdown exceeded its deadline")]
    StopDeadline,
    #[error("I/O: {0}")]
    Io(#[from] io::Error),
    #[error("journal: {0}")]
    Journal(#[from] JournalError),
    #[error("task: {0}")]
    Task(#[from] tokio::task::JoinError),
}

pub struct SourceManager {
    root: PathBuf,
    config: RuntimeConfig,
    active: Arc<Mutex<HashMap<SourceId, SourceHandle>>>,
    starting: Arc<Mutex<HashSet<SourceId>>>,
    starting_changed: Arc<Notify>,
    shutting_down: Arc<AtomicBool>,
}

impl SourceManager {
    pub fn new(root: impl AsRef<Path>, config: RuntimeConfig) -> Result<Self, RuntimeError> {
        config.validate()?;
        std::fs::create_dir_all(root.as_ref())?;
        Ok(Self {
            root: root.as_ref().to_owned(),
            config,
            active: Arc::new(Mutex::new(HashMap::new())),
            starting: Arc::new(Mutex::new(HashSet::new())),
            starting_changed: Arc::new(Notify::new()),
            shutting_down: Arc::new(AtomicBool::new(false)),
        })
    }

    /// Starts a source the user asked for in this session.
    pub async fn start(&self, definition: SourceDefinition) -> Result<SourceHandle, RuntimeError> {
        self.start_impl(definition, None, StartIntent::UserRequested)
            .await
    }

    pub async fn start_with_reader<R>(
        &self,
        definition: SourceDefinition,
        reader: R,
    ) -> Result<SourceHandle, RuntimeError>
    where
        R: AsyncRead + Send + 'static,
    {
        self.start_impl(
            definition,
            Some(Box::pin(reader)),
            StartIntent::UserRequested,
        )
        .await
    }

    /// Restores a remembered source when a workspace reopens.
    ///
    /// Restoration reattaches capture that has no outward side effect. A
    /// remembered command is never executed and a remembered endpoint is never
    /// contacted; both are refused with a distinct error so the application can
    /// offer an explicit start instead.
    pub async fn restore(
        &self,
        definition: SourceDefinition,
    ) -> Result<SourceHandle, RuntimeError> {
        self.start_impl(definition, None, StartIntent::Restore)
            .await
    }

    async fn start_impl(
        &self,
        definition: SourceDefinition,
        reader: Option<OwnedReader>,
        intent: StartIntent,
    ) -> Result<SourceHandle, RuntimeError> {
        if self.shutting_down.load(Ordering::Acquire) {
            return Err(RuntimeError::Closed);
        }
        if definition.schema_version != 1 {
            return Err(RuntimeError::DefinitionUnsupported);
        }
        // Admission is decided before any disk or process work: restoration may
        // never reach an acquisition that starts something outside lvu.
        if intent == StartIntent::Restore {
            match &definition.acquisition {
                Acquisition::Command { .. } => {
                    return Err(RuntimeError::RestoreWouldLaunchCommand);
                }
                Acquisition::Http { .. } => return Err(RuntimeError::RestoreWouldConnect),
                Acquisition::Stdin => return Err(RuntimeError::RestoreCannotAttachStdin),
                Acquisition::File { .. } => {}
            }
        }
        match &definition.acquisition {
            Acquisition::Stdin if reader.is_none() => return Err(RuntimeError::StdinNotAttached),
            Acquisition::Stdin => {}
            _ if reader.is_some() => return Err(RuntimeError::ReaderAttachmentMismatch),
            _ => {}
        }
        let source_id = definition.id;
        {
            let active = self.active.lock().expect("source map poisoned");
            if let Some(existing) = active.get(&source_id)
                && !existing.progress().state.is_terminal()
            {
                return Err(RuntimeError::AlreadyRunning);
            }
        }
        {
            let mut starting = self.starting.lock().expect("starting set poisoned");
            if self.shutting_down.load(Ordering::Acquire) {
                return Err(RuntimeError::Closed);
            }
            if !starting.insert(source_id) {
                return Err(RuntimeError::AlreadyRunning);
            }
        }
        let root = self.root.clone();
        let config = self.config.clone();
        let active = self.active.clone();
        let starting = self.starting.clone();
        let starting_changed = self.starting_changed.clone();
        let shutting_down = self.shutting_down.clone();
        let (reply, receive) = oneshot::channel();
        let (caller_alive, caller_status) = watch::channel(());
        let request = StartRequest {
            definition,
            reader,
            intent,
        };
        tokio::spawn(async move {
            let result = Self::start_inner(
                root,
                config,
                request,
                &starting,
                shutting_down.clone(),
                caller_status,
                &reply,
            )
            .await;
            let result = if shutting_down.load(Ordering::Acquire) || reply.is_closed() {
                if let Ok(handle) = result {
                    let _ = handle.abort().await;
                    wait_for_terminal(&handle).await;
                }
                Err(RuntimeError::Closed)
            } else {
                result
            };
            if let Ok(handle) = &result {
                active
                    .lock()
                    .expect("source map poisoned")
                    .insert(source_id, handle.clone());
            }
            starting
                .lock()
                .expect("starting set poisoned")
                .remove(&source_id);
            starting_changed.notify_waiters();
            let _ = reply.send(result);
        });
        let result = receive.await.map_err(|_| RuntimeError::Closed)?;
        drop(caller_alive);
        result
    }

    async fn start_inner(
        root: PathBuf,
        config: RuntimeConfig,
        request: StartRequest,
        starting: &Mutex<HashSet<SourceId>>,
        shutting_down: Arc<AtomicBool>,
        caller_status: watch::Receiver<()>,
        reply: &oneshot::Sender<Result<SourceHandle, RuntimeError>>,
    ) -> Result<SourceHandle, RuntimeError> {
        let StartRequest {
            definition,
            reader,
            intent,
        } = request;
        let source_id = definition.id;
        let directory = root.join(source_id.0.to_string());
        let metadata_path = directory.join("source.json");
        let catalog_path = directory.join("events.jsonl");
        let journal_path = directory.join("capture.journal");
        let cursor_path = directory.join("file-cursor.json");
        let history_path = directory.join("history.jsonl");
        let lease_path = directory.join("runtime.lock");
        let directory_for_lease = directory.clone();
        let runtime_lease = tokio::task::spawn_blocking(move || {
            std::fs::create_dir_all(directory_for_lease)?;
            let file = OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .open(lease_path)?;
            FileExt::try_lock_exclusive(&file).map_err(|error| {
                if error.kind() == io::ErrorKind::WouldBlock {
                    RuntimeError::AlreadyRunning
                } else {
                    RuntimeError::Io(error)
                }
            })?;
            Ok::<_, RuntimeError>(file)
        })
        .await??;
        let definition_for_disk = definition.clone();
        let stdin_source = matches!(&definition.acquisition, Acquisition::Stdin);
        let file_path = match &definition.acquisition {
            Acquisition::File { path, .. } => Some(path.clone()),
            _ => None,
        };
        let metadata_for_disk = metadata_path.clone();
        let catalog_for_disk = catalog_path.clone();
        let cursor_for_disk = cursor_path.clone();
        let (generation, durable_cursor) = tokio::task::spawn_blocking(move || {
            let durable_cursor = file_path
                .as_deref()
                .map(|path| cursor::load(&cursor_for_disk, source_id, path))
                .transpose()?
                .flatten();
            let generation = next_generation(&metadata_for_disk, source_id, &definition_for_disk)?;
            if stdin_source && generation > 1 {
                return Err(RuntimeError::StdinAlreadyCaptured);
            }
            write_metadata(
                &metadata_for_disk,
                &SourceMetadata {
                    schema_version: 1,
                    source_id,
                    generation,
                    definition: definition_for_disk,
                },
            )?;
            let mut catalog = Catalog::open(&catalog_for_disk)?;
            catalog.record(CatalogEvent::Starting { generation })?;
            Ok::<_, RuntimeError>((generation, durable_cursor))
        })
        .await??;

        let initial = SourceProgress {
            source_id,
            generation,
            state: RuntimeState::Starting,
            records: 0,
            high_watermark: None,
            journal_bytes: 0,
            synced_records: 0,
            syncs: 0,
            handovers: 0,
            writer_cpu_nanos: 0,
            reader_cpu_nanos: 0,
            boundaries: 0,
            exit_code: None,
            discarded_bytes: 0,
            discarded_bytes_known: true,
            last_error: None,
        };
        let (progress_tx, progress_rx) = watch::channel(initial.clone());
        let writer = spawn_writer(
            source_id,
            journal_path.clone(),
            catalog_path.clone(),
            match &definition.acquisition {
                Acquisition::File { path, .. } => Some(FileCursorSetup {
                    cursor_path,
                    source_path: path.clone(),
                    durable: durable_cursor,
                    cancellation: StartupCancellation {
                        shutting_down: shutting_down.clone(),
                        caller_status: caller_status.clone(),
                    },
                }),
                _ => None,
            },
            config.clone(),
            progress_tx.clone(),
            initial,
        )
        .await?;
        let acquisition_result = {
            // This lock is the admission barrier shared with shutdown. Disk
            // preparation may finish after the caller has gone away, but no
            // command/file acquisition is launched after admission closes.
            let _admission = starting.lock().expect("starting set poisoned");
            if shutting_down.load(Ordering::Acquire)
                || caller_status.has_changed().is_err()
                || reply.is_closed()
            {
                return Err(RuntimeError::Closed);
            }
            start_acquisition(
                &definition,
                intent,
                config.acquisition,
                writer.resume.clone(),
                reader,
            )
        };
        let capture = match acquisition_result {
            Ok(value) => value,
            Err(error) => {
                let message = error.to_string();
                let failed_catalog = catalog_path.clone();
                tokio::task::spawn_blocking(move || {
                    Catalog::open(&failed_catalog)?
                        .record(CatalogEvent::StartFailed { message: &message })
                })
                .await??;
                drop(writer.sender);
                drop(writer.page_sender);
                let _ = writer.task.await;
                return Err(error);
            }
        };
        let Capture {
            handle: acquisition,
            events: acquisition_rx,
            history: history_rx,
            history_dropped,
        } = capture;
        let (history_rx, history_task) = spawn_history(history_path, history_rx, history_dropped);
        let (control_tx, control_rx) = mpsc::channel(4);
        let page_gate = Arc::new(Semaphore::new(1));
        let handle = SourceHandle {
            source_id,
            journal_path,
            control: control_tx,
            page_sender: writer.page_sender.clone(),
            progress: progress_rx,
            history: history_rx,
            page_gate: page_gate.clone(),
            max_page_records: config.max_page_records,
            max_page_bytes: config.max_page_bytes,
        };
        tokio::spawn(supervise(Supervisor {
            acquisition,
            acquired: acquisition_rx,
            history_task,
            writer: writer.sender,
            writer_slots: writer.slots,
            writer_task: writer.task,
            runtime_lease,
            controls: control_rx,
            progress: progress_tx,
            deadline: config.graceful_stop_deadline,
        }));
        Ok(handle)
    }

    pub fn source(&self, source_id: SourceId) -> Option<SourceHandle> {
        self.active
            .lock()
            .expect("source map poisoned")
            .get(&source_id)
            .cloned()
    }

    pub async fn shutdown(&self) -> Vec<(SourceId, Result<StopReport, RuntimeError>)> {
        {
            let _admission = self.starting.lock().expect("starting set poisoned");
            self.shutting_down.store(true, Ordering::Release);
        }
        let wait_for_starts = async {
            loop {
                let notified = self.starting_changed.notified();
                if self
                    .starting
                    .lock()
                    .expect("starting set poisoned")
                    .is_empty()
                {
                    break;
                }
                notified.await;
            }
        };
        let starts_finished =
            tokio::time::timeout(self.config.graceful_stop_deadline, wait_for_starts)
                .await
                .is_ok();
        let handles: Vec<_> = self
            .active
            .lock()
            .expect("source map poisoned")
            .values()
            .filter(|handle| !handle.progress().state.is_terminal())
            .cloned()
            .collect();
        let mut reports = Vec::with_capacity(handles.len());
        if !starts_finished {
            reports.extend(
                self.starting
                    .lock()
                    .expect("starting set poisoned")
                    .iter()
                    .copied()
                    .map(|source_id| (source_id, Err(RuntimeError::StopDeadline))),
            );
        }
        for handle in handles {
            let report = stop_for_shutdown(&handle, self.config.graceful_stop_deadline).await;
            reports.push((handle.source_id(), report));
        }
        reports
    }
}

/// Stops one source during shutdown, telling a source that finished on its own
/// apart from one that failed to stop.
///
/// `shutdown` chooses what to stop from a snapshot of the sources that are not
/// yet terminal, and a source can reach its terminal state between that
/// snapshot and the stop: a command that exits, a file read to its end. By then
/// its supervisor has gone, so the stop is refused as `NotActive`, or its reply
/// is dropped as `Closed`. Neither is a failure to stop — the source had
/// already stopped — but both were reported as errors, which made quitting lvu
/// exit non-zero whenever a short-lived command happened to finish in that
/// window. The window is small on an idle machine and wide on a busy one.
///
/// The report is the source's own outcome: `Stopped` is complete, and any other
/// terminal state is reported incomplete with its discarded-byte accounting, so
/// a source that another actor stopped lossily during the window is still
/// heard. A source that never reaches a terminal state at all was genuinely
/// unreachable, and the refusal stands.
async fn stop_for_shutdown(
    handle: &SourceHandle,
    deadline: Duration,
) -> Result<StopReport, RuntimeError> {
    let refusal = match handle.stop().await {
        Ok(report) => return Ok(report),
        Err(error @ (RuntimeError::NotActive | RuntimeError::Closed)) => error,
        Err(error) => return Err(error),
    };
    // The terminal state is published as the supervisor ends, so it can trail
    // the closed channel by a moment. `wait_for_terminal` also returns when the
    // progress sender is dropped, which happens whether or not a terminal state
    // was ever published, so the state is checked rather than assumed.
    let timed_out = tokio::time::timeout(deadline, wait_for_terminal(handle))
        .await
        .is_err();
    let progress = handle.progress();
    if timed_out || !progress.state.is_terminal() {
        return Err(refusal);
    }
    Ok(StopReport {
        // `Stopped` is how an acquisition that ran to its own end finishes.
        // Any other terminal state ended for a reason the caller should still
        // hear about, and its discarded-byte accounting travels with it.
        complete: progress.state == RuntimeState::Stopped,
        discarded_bytes: progress.discarded_bytes,
        discarded_bytes_known: progress.discarded_bytes_known,
    })
}

async fn wait_for_terminal(handle: &SourceHandle) {
    let mut progress = handle.subscribe();
    while !progress.borrow().state.is_terminal() && progress.changed().await.is_ok() {}
}

#[derive(Clone)]
pub struct SourceHandle {
    source_id: SourceId,
    journal_path: PathBuf,
    control: mpsc::Sender<Control>,
    page_sender: mpsc::Sender<PageRequest>,
    progress: watch::Receiver<SourceProgress>,
    history: watch::Receiver<Arc<SourceHistory>>,
    page_gate: Arc<Semaphore>,
    max_page_records: usize,
    max_page_bytes: usize,
}

impl SourceHandle {
    pub fn source_id(&self) -> SourceId {
        self.source_id
    }
    pub fn progress(&self) -> SourceProgress {
        self.progress.borrow().clone()
    }
    pub fn subscribe(&self) -> watch::Receiver<SourceProgress> {
        self.progress.clone()
    }

    /// The bounded, published lifecycle history of this capture: connection
    /// attempts, rejections, disconnects, capture gaps and restart boundaries.
    pub fn history(&self) -> Arc<SourceHistory> {
        self.history.borrow().clone()
    }

    /// Watches lifecycle history for status surfaces.
    pub fn subscribe_history(&self) -> watch::Receiver<Arc<SourceHistory>> {
        self.history.clone()
    }

    pub async fn stop(&self) -> Result<StopReport, RuntimeError> {
        let (reply, receive) = oneshot::channel();
        self.control
            .send(Control::Stop(reply))
            .await
            .map_err(|_| RuntimeError::NotActive)?;
        receive.await.map_err(|_| RuntimeError::Closed)?
    }

    pub async fn abort(&self) -> Result<AbortReport, RuntimeError> {
        let (reply, receive) = oneshot::channel();
        self.control
            .send(Control::Abort(reply))
            .await
            .map_err(|_| RuntimeError::NotActive)?;
        receive.await.map_err(|_| RuntimeError::Closed)?
    }

    pub async fn read_page(
        &self,
        offset: u64,
        max_records: usize,
        max_bytes: usize,
    ) -> Result<JournalPage, RuntimeError> {
        // One outstanding page per source: the gate bounds page traffic without
        // letting capture's own queue slots decide whether a query may ask.
        // Capture events keep their writer-queue slots; pages carry no capture
        // payload and need none, so a flooded capture queue cannot refuse a
        // page admission. The permit travels inside the request until service
        // or drop, so cancelling this future after sending cannot free the
        // gate while the request is still queued or being served. The writer
        // serves the page between append batches, so it waits for at most the
        // batch in progress, and alternates with capture while both stay
        // queued, so sustained paging cannot stall appends either.
        let permit = self
            .page_gate
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| RuntimeError::Closed)?;
        let (reply, receive) = oneshot::channel();
        let bounded_records = max_records.min(self.max_page_records);
        let bounded_bytes = max_bytes.min(self.max_page_bytes);
        if self
            .page_sender
            .send(PageRequest {
                offset,
                max_records: bounded_records,
                max_bytes: bounded_bytes,
                reply,
                _permit: permit,
            })
            .await
            .is_ok()
        {
            match receive.await {
                Ok(result) => return result,
                Err(_) => {
                    // The writer took this request and died with its permit
                    // before answering. Fall through for a freshly permitted
                    // fallback read below.
                }
            }
        }
        // Either failure drops the request with its permit — the send error
        // carries the message back and the reply error means the writer did —
        // so this future holds no permit here by construction (the `permit`
        // binding above was moved into the send and cannot be named again),
        // and acquiring the fallback permit cannot deadlock against itself.
        // The fallback permit moves into the blocking closure and is held
        // through service or drop, so cancelling this future mid-read still
        // leaves exactly one active read per source: the detached thread
        // keeps its guard until the read finishes.
        let fallback_permit = self
            .page_gate
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| RuntimeError::Closed)?;
        let path = self.journal_path.clone();
        let source_id = self.source_id;
        tokio::task::spawn_blocking(move || {
            let _guard = fallback_permit;
            #[cfg(test)]
            fallback_probe::rendezvous(&source_id);
            JournalReader::open(path, source_id)?
                .read_page(offset, bounded_records, bounded_bytes)
                .map_err(RuntimeError::from)
        })
        .await?
    }
}

/// Test-only rendezvous for fallback reads. Production builds compile it out
/// entirely; with nothing registered the probe returns immediately, so every
/// other test behaves exactly as production. Registration is scoped per
/// source, and every test uses a fresh source identity, so parallel tests
/// never meet at the same barrier.
#[cfg(test)]
pub(crate) mod fallback_probe {
    use lvu_core::SourceId;
    use std::collections::HashMap;
    use std::sync::{Arc, Barrier, Mutex, OnceLock};

    struct Probe {
        entered: Arc<Barrier>,
        release: Mutex<Option<std::sync::mpsc::Receiver<()>>>,
    }

    fn registry() -> &'static Mutex<HashMap<SourceId, Arc<Probe>>> {
        static REGISTRY: OnceLock<Mutex<HashMap<SourceId, Arc<Probe>>>> = OnceLock::new();
        REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
    }

    /// Arm the rendezvous for one source: the next fallback read to arrive
    /// parks inside its blocking closure until the test arrives and releases
    /// it. Returns the barrier the test waits on and the release sender.
    /// Later reads find the release taken and proceed unimpeded.
    pub(crate) fn arm(source: SourceId) -> (Arc<Barrier>, std::sync::mpsc::Sender<()>) {
        let entered = Arc::new(Barrier::new(2));
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        registry().lock().expect("probe registry poisoned").insert(
            source,
            Arc::new(Probe {
                entered: entered.clone(),
                release: Mutex::new(Some(release_rx)),
            }),
        );
        (entered, release_tx)
    }

    /// Withdraw the rendezvous, e.g. at test end. A leaked registration only
    /// affects its own fresh source identity, never another test.
    pub(crate) fn disarm(source: &SourceId) {
        registry()
            .lock()
            .expect("probe registry poisoned")
            .remove(source);
    }

    /// Park a fallback read until its test rendezvouses and releases it. The
    /// registry lock is never held across the wait; the release is taken under
    /// it so exactly one read parks per arming.
    pub(crate) fn rendezvous(source: &SourceId) {
        let (entered, release) = {
            let registry = registry().lock().expect("probe registry poisoned");
            match registry.get(source) {
                None => return,
                Some(probe) => (
                    probe.entered.clone(),
                    probe.release.lock().expect("probe release poisoned").take(),
                ),
            }
        };
        if let Some(release) = release {
            entered.wait();
            let _ = release.recv();
        }
    }
}

impl RuntimeState {
    /// Whether this state denotes a finished or blocked acquisition.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Stopped | Self::Aborted | Self::Incomplete | Self::StorageBlocked | Self::Error
        )
    }
}

enum Control {
    Stop(oneshot::Sender<Result<StopReport, RuntimeError>>),
    Abort(oneshot::Sender<Result<AbortReport, RuntimeError>>),
}

enum CompletionReply {
    Stop(
        oneshot::Sender<Result<StopReport, RuntimeError>>,
        Result<StopReport, RuntimeError>,
    ),
    Abort(
        oneshot::Sender<Result<AbortReport, RuntimeError>>,
        Result<AbortReport, RuntimeError>,
    ),
    Natural {
        finished: bool,
        state: RuntimeState,
    },
    Incomplete(StopReport),
    IncompleteAbort(AbortReport),
    None,
}

fn start_acquisition(
    definition: &SourceDefinition,
    intent: StartIntent,
    limits: CaptureLimits,
    resume: Option<lvu_core::FileCaptureResume>,
    reader: Option<OwnedReader>,
) -> Result<Capture, RuntimeError> {
    // The intent check is repeated at the launch point itself, inside the
    // admission lock, so no future caller can reach a side effect by taking a
    // different route to this function.
    match (&definition.acquisition, intent) {
        (Acquisition::Command { .. }, StartIntent::Restore) => {
            return Err(RuntimeError::RestoreWouldLaunchCommand);
        }
        (Acquisition::Http { .. }, StartIntent::Restore) => {
            return Err(RuntimeError::RestoreWouldConnect);
        }
        (Acquisition::Stdin, StartIntent::Restore) => {
            return Err(RuntimeError::RestoreCannotAttachStdin);
        }
        _ => {}
    }
    match &definition.acquisition {
        Acquisition::Stdin => Ok(into_capture(lvu_core::acquisition::capture_reader(
            reader.ok_or(RuntimeError::StdinNotAttached)?,
            limits,
        )?)),
        Acquisition::File { path, follow } => Ok(into_capture(capture_file_auto_from(
            path.clone(),
            *follow,
            limits,
            resume,
        )?)),
        Acquisition::Command { command } => {
            Ok(capture_command_supervised(command.clone(), limits)?)
        }
        Acquisition::Http {
            url,
            framing,
            reconnect,
            headers,
            limits: http,
        } => Ok(capture_http(
            HttpAcquisition {
                url: url.clone(),
                framing: *framing,
                reconnect: reconnect.clone(),
                headers: headers.clone(),
                limits: *http,
            },
            limits,
        )?),
    }
}

/// Adapts the acquisition kinds that publish no lifecycle history of their own.
fn into_capture(parts: (CaptureHandle, mpsc::Receiver<CaptureEvent>)) -> Capture {
    let (handle, events) = parts;
    let (sink, history) = lvu_core::source_event::source_event_channel(1);
    let history_dropped = sink.dropped_counter();
    drop(sink);
    Capture {
        handle,
        events,
        history,
        history_dropped,
    }
}

struct Supervisor {
    acquisition: CaptureHandle,
    acquired: mpsc::Receiver<CaptureEvent>,
    history_task: tokio::task::JoinHandle<()>,
    writer: mpsc::Sender<WriterMessage>,
    writer_slots: Arc<Semaphore>,
    writer_task: tokio::task::JoinHandle<()>,
    runtime_lease: File,
    controls: mpsc::Receiver<Control>,
    progress: watch::Sender<SourceProgress>,
    deadline: Duration,
}

async fn supervise(supervisor: Supervisor) {
    let Supervisor {
        mut acquisition,
        mut acquired,
        history_task,
        writer,
        writer_slots,
        writer_task,
        runtime_lease,
        mut controls,
        progress,
        deadline,
    } = supervisor;
    let mut writer_status = progress.subscribe();
    let mut acquisition_failed = false;
    let completion = loop {
        tokio::select! {
            control = controls.recv() => match control {
                Some(Control::Stop(reply)) => { let result = graceful_stop(&mut acquisition, &mut acquired, &writer, &writer_slots, &progress, deadline).await; break CompletionReply::Stop(reply, result); }
                Some(Control::Abort(reply)) => { let result = abort_source(&mut acquisition, &mut acquired, &writer, &progress, deadline).await; break CompletionReply::Abort(reply, result); }
                None => { acquisition.abort(); break CompletionReply::None; }
            },
            event = next_acquired(&mut acquired, &writer_slots) => match event {
                Ok(Some((event, permit))) => {
                    acquisition_failed |= matches!(event, CaptureEvent::Error { .. });
                    if writer.send(WriterMessage::Event { event, _permit: permit }).await.is_err() { acquisition.abort(); break CompletionReply::None; }
                },
                Err(_) => { acquisition.abort(); break CompletionReply::None; },
                Ok(None) => {
                    let state = if acquisition_failed { RuntimeState::Error } else { RuntimeState::Stopped };
                    let finished = finish(&writer, state, 0, true).await.is_ok();
                    break CompletionReply::Natural { finished, state };
                }
            },
            changed = writer_status.changed() => {
                if changed.is_err() || matches!(writer_status.borrow().state, RuntimeState::StorageBlocked | RuntimeState::Error) {
                    acquisition.abort();
                    let _ = acquisition.join().await;
                    break CompletionReply::None;
                }
            },
        }
    };
    let completion = match completion {
        CompletionReply::Stop(reply, Ok(report)) if !report.complete => {
            let _ = reply.send(Ok(report));
            CompletionReply::Incomplete(report)
        }
        CompletionReply::Abort(reply, Ok(report)) if !report.complete => {
            let _ = reply.send(Ok(report));
            CompletionReply::IncompleteAbort(report)
        }
        other => other,
    };
    drop(writer);
    lvu_core::journal::trace::record(|| "source_task JOINING writer".to_owned());
    let joined = writer_task.await;
    lvu_core::journal::trace::record(|| format!("source_task JOINED writer ok={}", joined.is_ok()));
    // The history sender lives in the capture task; once that ends the drain
    // finishes and publishes any final drop count.
    let _ = history_task.await;
    // Terminal progress is the public restart-admission boundary. Release the
    // cross-manager lease first so observing Stopped/Aborted/Incomplete cannot
    // race a subsequent start into a transient AlreadyRunning result.
    if let Err(error) = release_runtime_lease(runtime_lease) {
        update_state(
            &progress,
            RuntimeState::Error,
            Some(format!("runtime lease release failed: {error}")),
        );
        match completion {
            CompletionReply::Stop(reply, _) => {
                let _ = reply.send(Err(RuntimeError::Io(error)));
            }
            CompletionReply::Abort(reply, _) => {
                let _ = reply.send(Err(RuntimeError::Io(error)));
            }
            _ => {}
        }
        return;
    }
    match completion {
        CompletionReply::Stop(reply, result) => {
            if let Ok(report) = &result {
                update_completion_state(
                    &progress,
                    if report.complete {
                        RuntimeState::Stopped
                    } else {
                        RuntimeState::Incomplete
                    },
                    report.discarded_bytes,
                    report.discarded_bytes_known,
                );
            }
            lvu_core::journal::trace::record(|| "source_task STOP REPLY".to_owned());
            let _ = reply.send(result);
        }
        CompletionReply::Abort(reply, result) => {
            if let Ok(report) = &result {
                update_completion_state(
                    &progress,
                    RuntimeState::Aborted,
                    report.discarded_bytes,
                    report.discarded_bytes_known,
                );
            }
            let _ = reply.send(result);
        }
        CompletionReply::Natural {
            finished: true,
            state,
        } => update_state(&progress, state, None),
        CompletionReply::Incomplete(report) => update_completion_state(
            &progress,
            RuntimeState::Incomplete,
            report.discarded_bytes,
            report.discarded_bytes_known,
        ),
        CompletionReply::IncompleteAbort(report) => update_completion_state(
            &progress,
            RuntimeState::Incomplete,
            report.discarded_bytes,
            report.discarded_bytes_known,
        ),
        CompletionReply::Natural {
            finished: false, ..
        }
        | CompletionReply::None => {}
    }
}

async fn next_acquired(
    acquired: &mut mpsc::Receiver<CaptureEvent>,
    slots: &Arc<Semaphore>,
) -> Result<Option<(CaptureEvent, tokio::sync::OwnedSemaphorePermit)>, RuntimeError> {
    let permit = slots
        .clone()
        .acquire_owned()
        .await
        .map_err(|_| RuntimeError::Closed)?;
    Ok(acquired.recv().await.map(|event| (event, permit)))
}

async fn graceful_stop(
    acquisition: &mut CaptureHandle,
    acquired: &mut mpsc::Receiver<CaptureEvent>,
    writer: &mpsc::Sender<WriterMessage>,
    writer_slots: &Arc<Semaphore>,
    progress: &watch::Sender<SourceProgress>,
    deadline: Duration,
) -> Result<StopReport, RuntimeError> {
    update_state(progress, RuntimeState::Stopping, None);
    acquisition.stop();
    let started = Instant::now();
    let drain = async {
        while let Some((event, permit)) = next_acquired(acquired, writer_slots).await? {
            writer
                .send(WriterMessage::Event {
                    event,
                    _permit: permit,
                })
                .await
                .map_err(|_| RuntimeError::Closed)?;
        }
        let completion = acquisition.join().await?;
        if completion.aborted {
            return Err(RuntimeError::Closed);
        }
        finish(writer, RuntimeState::Stopped, 0, true).await
    };
    match tokio::time::timeout(deadline, drain).await {
        Ok(result) => {
            result?;
            Ok(StopReport {
                complete: true,
                discarded_bytes: 0,
                discarded_bytes_known: true,
            })
        }
        Err(_) => {
            acquisition.abort();
            let discarded = drain_discarded(acquired) as u64;
            update_state(
                progress,
                RuntimeState::Stopping,
                Some("graceful stop deadline exceeded".into()),
            );
            let remaining = deadline
                .saturating_sub(started.elapsed())
                .max(Duration::from_millis(1));
            let _ = tokio::time::timeout(
                remaining,
                finish(writer, RuntimeState::Incomplete, discarded, false),
            )
            .await;
            Ok(StopReport {
                complete: false,
                discarded_bytes: discarded,
                discarded_bytes_known: false,
            })
        }
    }
}

async fn abort_source(
    acquisition: &mut CaptureHandle,
    acquired: &mut mpsc::Receiver<CaptureEvent>,
    writer: &mpsc::Sender<WriterMessage>,
    progress: &watch::Sender<SourceProgress>,
    deadline: Duration,
) -> Result<AbortReport, RuntimeError> {
    update_state(progress, RuntimeState::Aborting, None);
    acquisition.abort();
    let started = Instant::now();
    let completion = tokio::time::timeout(deadline, acquisition.join()).await;
    let buffered = match completion {
        Ok(Ok(value)) => value.discarded_buffered_bytes,
        _ => 0,
    };
    let discarded = buffered as u64 + drain_discarded(acquired) as u64;
    let remaining = deadline.saturating_sub(started.elapsed());
    let complete = if remaining.is_zero() {
        false
    } else {
        matches!(
            tokio::time::timeout(
                remaining,
                finish(writer, RuntimeState::Aborted, discarded, false),
            )
            .await,
            Ok(Ok(()))
        )
    };
    if !complete {
        update_state(
            progress,
            RuntimeState::Aborting,
            Some("abort cleanup exceeded its deadline".into()),
        );
    }
    Ok(AbortReport {
        complete,
        discarded_bytes: discarded,
        discarded_bytes_known: false,
    })
}

fn drain_discarded(receiver: &mut mpsc::Receiver<CaptureEvent>) -> usize {
    let mut bytes = 0;
    while let Ok(event) = receiver.try_recv() {
        for record in event.records() {
            bytes += record.bytes.len() + record.delimiter.len();
        }
    }
    bytes
}

async fn finish(
    writer: &mpsc::Sender<WriterMessage>,
    state: RuntimeState,
    discarded_bytes: u64,
    discarded_bytes_known: bool,
) -> Result<(), RuntimeError> {
    let (reply, receive) = oneshot::channel();
    writer
        .send(WriterMessage::Finish {
            state,
            discarded_bytes,
            discarded_bytes_known,
            reply,
        })
        .await
        .map_err(|_| RuntimeError::Closed)?;
    receive.await.map_err(|_| RuntimeError::Closed)??;
    Ok(())
}

fn update_state(
    progress: &watch::Sender<SourceProgress>,
    state: RuntimeState,
    error: Option<String>,
) {
    let mut value = progress.borrow().clone();
    value.state = state;
    if error.is_some() {
        value.last_error = error;
    }
    let _ = progress.send(value);
}

fn update_completion_state(
    progress: &watch::Sender<SourceProgress>,
    state: RuntimeState,
    discarded_bytes: u64,
    discarded_bytes_known: bool,
) {
    let mut value = progress.borrow().clone();
    value.state = state;
    value.discarded_bytes = discarded_bytes;
    value.discarded_bytes_known = discarded_bytes_known;
    let _ = progress.send(value);
}

// Explicit unlock also releases the shared open-file-description lock when a
// concurrent fork briefly inherits a descriptor before its CLOEXEC takes effect.
fn release_runtime_lease(lease: File) -> io::Result<()> {
    FileExt::unlock(&lease)
}

#[cfg(test)]
mod lease_tests {
    use super::*;
    #[test]
    fn stopped_lease_releases_even_with_an_inherited_open_description() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("runtime.lock");
        let owner = File::create(&path).unwrap();
        FileExt::try_lock_exclusive(&owner).unwrap();
        let inherited = owner.try_clone().unwrap();
        release_runtime_lease(owner).unwrap();
        let next = File::open(path).unwrap();
        FileExt::try_lock_exclusive(&next).unwrap();
        drop(inherited);
    }
}

#[cfg(test)]
mod fallback_guard_tests {
    //! The fallback read holds the gate through service or drop.
    //!
    //! Cancelling a caller mid-read must not free the gate early (the
    //! detached thread keeps its guard), and the acquire/drop/reacquire dance
    //! must never self-deadlock: a reacquire attempted while still holding
    //! the failed request's permit would hang against the unit-capacity gate.
    //! Every verdict below is barrier- or content-ordered; timeouts guard
    //! against hangs only, which is exactly what a deadlock would look like.
    use super::*;
    use std::collections::BTreeMap;

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn cancelled_fallback_read_stays_guarded_and_later_reads_complete() {
        let root = tempfile::tempdir().unwrap();
        let input = root.path().join("input.log");
        let mut fixture = String::new();
        for index in 0..10 {
            fixture.push_str(&format!("row-{index:02}\n"));
        }
        std::fs::write(&input, fixture.as_bytes()).unwrap();
        let source_id = SourceId::new();
        let manager =
            SourceManager::new(root.path().join("capture"), RuntimeConfig::default()).unwrap();
        let handle = manager
            .start(SourceDefinition {
                schema_version: 1,
                id: source_id,
                name: "fallback guard".into(),
                acquisition: Acquisition::File {
                    path: input.clone(),
                    follow: false,
                },
                identity_hints: BTreeMap::new(),
                retention: None,
            })
            .await
            .unwrap();
        // Barrier, not timing: the terminal state is published only after the
        // supervisor joins the writer, so the page channel is deterministically
        // closed and every read below takes the fallback path.
        let mut progress = handle.subscribe();
        tokio::time::timeout(Duration::from_secs(60), async {
            loop {
                if progress.borrow_and_update().state.is_terminal() {
                    return;
                }
                progress.changed().await.expect("progress channel");
            }
        })
        .await
        .expect("source stopped without hanging");
        assert_eq!(handle.progress().state, RuntimeState::Stopped);

        // Arm the rendezvous: the next fallback read parks inside its blocking
        // closure, past the point where it acquired its guard.
        let (entered, release) = fallback_probe::arm(source_id);
        let reader = handle.clone();
        let parked = tokio::spawn(async move { reader.read_page(0, 3, 4096).await });
        // The blocking rendezvous must not run on a runtime worker.
        tokio::task::spawn_blocking(move || entered.wait())
            .await
            .expect("rendezvous");
        // The parked read provably holds its guard here. Cancel its caller and
        // wait until the cancellation is processed: the detached thread stays
        // parked holding the guard.
        parked.abort();
        let aborted = parked
            .await
            .expect_err("abort must cancel the parked caller");
        assert!(aborted.is_cancelled());
        // A replacement caller must not pass while the detached guard lives.
        // Without the fallback-held guard this succeeds and the test fails:
        // the bound becomes deterministic cancellation evidence, not a
        // structural claim.
        assert!(
            handle.page_gate.clone().try_acquire_owned().is_err(),
            "gate must stay held by the detached parked read after its caller is gone"
        );
        // Free the parked thread so it completes detached and releases. The
        // release sender is owned by this scope, so even a panic above drops
        // it and unblocks the parked thread via a recv error instead of
        // leaking a blocked thread.
        release.send(()).expect("release parked read");
        // The next read must complete correctly: no deadlock from the
        // acquire/drop dance, no corruption from the cancelled predecessor.
        let page = tokio::time::timeout(Duration::from_secs(60), handle.read_page(0, 3, 4096))
            .await
            .expect("second read completes, not deadlocked")
            .expect("page read");
        assert_eq!(page.records.len(), 3);
        assert_eq!(page.records[0].bytes.as_slice(), b"row-00".as_slice());
        assert_eq!(page.records[2].bytes.as_slice(), b"row-02".as_slice());
        fallback_probe::disarm(&source_id);
    }
}
