use crate::{
    catalog::{Catalog, CatalogEvent, SourceMetadata, next_generation, write_metadata},
    cursor,
    history::{SourceHistory, spawn_history},
    writer::{FileCursorSetup, StartupCancellation, WriterMessage, spawn_writer},
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
    pub sync_every_batches: usize,
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
            writer_queue_capacity: 128,
            batch_records: 64,
            sync_every_batches: 8,
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
            || self.sync_every_batches == 0
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
            writer: writer.sender.clone(),
            writer_slots: writer.slots.clone(),
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
    writer: mpsc::Sender<WriterMessage>,
    writer_slots: Arc<Semaphore>,
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
        let _permit = self
            .page_gate
            .acquire()
            .await
            .map_err(|_| RuntimeError::Closed)?;
        let (reply, receive) = oneshot::channel();
        let bounded_records = max_records.min(self.max_page_records);
        let bounded_bytes = max_bytes.min(self.max_page_bytes);
        let writer_permit = self
            .writer_slots
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| RuntimeError::Closed)?;
        if self
            .writer
            .send(WriterMessage::Page {
                offset,
                max_records: bounded_records,
                max_bytes: bounded_bytes,
                reply,
                _permit: writer_permit,
            })
            .await
            .is_ok()
        {
            match receive.await {
                Ok(result) => return result,
                Err(_) => {
                    // The writer may accept this page immediately before a
                    // terminal storage/error transition drops its queue.
                    // Once that owner is gone, the read-only journal path is
                    // the authoritative bounded fallback.
                }
            }
        }
        let path = self.journal_path.clone();
        let source_id = self.source_id;
        tokio::task::spawn_blocking(move || {
            JournalReader::open(path, source_id)?
                .read_page(offset, bounded_records, bounded_bytes)
                .map_err(RuntimeError::from)
        })
        .await?
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
        if let CaptureEvent::Record(record) = event {
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
