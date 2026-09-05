use crate::{
    catalog::{Catalog, CatalogEvent, SourceMetadata, next_generation, write_metadata},
    cursor,
    writer::{WriterMessage, spawn_writer},
};
use fs2::FileExt;
use lvu_core::{
    Acquisition, CaptureEvent, JournalError, JournalPage, JournalReader, RecordId,
    SourceDefinition, SourceId,
    acquisition::{CaptureHandle, CaptureLimits, capture_command, capture_file_from},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    fs::{File, OpenOptions},
    io,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use thiserror::Error;
use tokio::sync::{Notify, Semaphore, mpsc, oneshot, watch};

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
            max_page_records: 512,
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
    #[error("HTTP acquisition is not implemented")]
    HttpUnsupported,
    #[error("command restart execution is not implemented")]
    RestartUnsupported,
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

    pub async fn start(&self, definition: SourceDefinition) -> Result<SourceHandle, RuntimeError> {
        if self.shutting_down.load(Ordering::Acquire) {
            return Err(RuntimeError::Closed);
        }
        if definition.schema_version != 1 {
            return Err(RuntimeError::DefinitionUnsupported);
        }
        match &definition.acquisition {
            Acquisition::Http { .. } => return Err(RuntimeError::HttpUnsupported),
            Acquisition::Command { command }
                if command.restart != lvu_core::RestartPolicy::Never =>
            {
                return Err(RuntimeError::RestartUnsupported);
            }
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
        tokio::spawn(async move {
            let result =
                Self::start_inner(root, config, definition, &starting, &shutting_down, &reply)
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
        receive.await.map_err(|_| RuntimeError::Closed)?
    }

    async fn start_inner(
        root: PathBuf,
        config: RuntimeConfig,
        definition: SourceDefinition,
        starting: &Mutex<HashSet<SourceId>>,
        shutting_down: &AtomicBool,
        reply: &oneshot::Sender<Result<SourceHandle, RuntimeError>>,
    ) -> Result<SourceHandle, RuntimeError> {
        let source_id = definition.id;
        let directory = root.join(source_id.0.to_string());
        let metadata_path = directory.join("source.json");
        let catalog_path = directory.join("events.jsonl");
        let journal_path = directory.join("capture.journal");
        let cursor_path = directory.join("file-cursor.json");
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
                Acquisition::File { path, .. } => Some((cursor_path, path.clone(), durable_cursor)),
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
            if shutting_down.load(Ordering::Acquire) || reply.is_closed() {
                return Err(RuntimeError::Closed);
            }
            start_acquisition(&definition, config.acquisition, writer.resume.clone())
        };
        let (acquisition, acquisition_rx) = match acquisition_result {
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
        let (control_tx, control_rx) = mpsc::channel(4);
        let page_gate = Arc::new(Semaphore::new(1));
        let handle = SourceHandle {
            source_id,
            journal_path,
            control: control_tx,
            writer: writer.sender.clone(),
            writer_slots: writer.slots.clone(),
            progress: progress_rx,
            page_gate: page_gate.clone(),
            max_page_records: config.max_page_records,
            max_page_bytes: config.max_page_bytes,
        };
        tokio::spawn(supervise(Supervisor {
            acquisition,
            acquired: acquisition_rx,
            writer: writer.sender,
            writer_slots: writer.slots,
            writer_task: writer.task,
            _runtime_lease: runtime_lease,
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
            reports.push((handle.source_id(), handle.stop().await));
        }
        reports
    }
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
    fn is_terminal(self) -> bool {
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
    Natural(bool),
    Incomplete(StopReport),
    IncompleteAbort(AbortReport),
    None,
}

fn start_acquisition(
    definition: &SourceDefinition,
    limits: CaptureLimits,
    resume: Option<lvu_core::FileResumeCursor>,
) -> Result<(CaptureHandle, mpsc::Receiver<CaptureEvent>), RuntimeError> {
    match &definition.acquisition {
        Acquisition::File { path, follow } => {
            Ok(capture_file_from(path.clone(), *follow, limits, resume)?)
        }
        Acquisition::Command { command } => Ok(capture_command(command.clone(), limits)?),
        Acquisition::Http { .. } => Err(RuntimeError::HttpUnsupported),
    }
}

struct Supervisor {
    acquisition: CaptureHandle,
    acquired: mpsc::Receiver<CaptureEvent>,
    writer: mpsc::Sender<WriterMessage>,
    writer_slots: Arc<Semaphore>,
    writer_task: tokio::task::JoinHandle<()>,
    _runtime_lease: File,
    controls: mpsc::Receiver<Control>,
    progress: watch::Sender<SourceProgress>,
    deadline: Duration,
}

async fn supervise(supervisor: Supervisor) {
    let Supervisor {
        mut acquisition,
        mut acquired,
        writer,
        writer_slots,
        writer_task,
        _runtime_lease,
        mut controls,
        progress,
        deadline,
    } = supervisor;
    let mut writer_status = progress.subscribe();
    let completion = loop {
        tokio::select! {
            control = controls.recv() => match control {
                Some(Control::Stop(reply)) => { let result = graceful_stop(&mut acquisition, &mut acquired, &writer, &writer_slots, &progress, deadline).await; break CompletionReply::Stop(reply, result); }
                Some(Control::Abort(reply)) => { let result = abort_source(&mut acquisition, &mut acquired, &writer, &progress, deadline).await; break CompletionReply::Abort(reply, result); }
                None => { acquisition.abort(); break CompletionReply::None; }
            },
            event = next_acquired(&mut acquired, &writer_slots) => match event {
                Ok(Some((event, permit))) => if writer.send(WriterMessage::Event { event, _permit: permit }).await.is_err() { acquisition.abort(); break CompletionReply::None; },
                Err(_) => { acquisition.abort(); break CompletionReply::None; },
                Ok(None) => { let finished = finish(&writer, RuntimeState::Stopped, 0, true).await.is_ok(); break CompletionReply::Natural(finished); }
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
    let _ = writer_task.await;
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
        CompletionReply::Natural(true) => update_state(&progress, RuntimeState::Stopped, None),
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
        CompletionReply::Natural(false) | CompletionReply::None => {}
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
