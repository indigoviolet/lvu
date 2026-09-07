use crate::{
    CommandDefinition, CommandProgram, RawRecord, RecordId, SourceId, StreamKind,
    restart::{AttemptWindow, Jitter, RestartBounds, RestartDecision, decide_restart},
    source_event::{
        SourceEvent, SourceEventRecord, SourceEventSink, bounded_detail, source_event_channel,
    },
};
use crc32fast::Hasher;
use flate2::read::MultiGzDecoder;
use std::{
    fs::File as StdFile,
    io::{self, Read, Seek, SeekFrom as StdSeekFrom},
    path::PathBuf,
    process::ExitStatus,
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::{
    fs::File,
    io::{AsyncRead, AsyncReadExt, AsyncSeekExt, SeekFrom},
    process::{Child, Command},
    sync::{mpsc, watch},
    task::JoinHandle,
};
use uuid::Uuid;

const FILE_EVIDENCE_BYTES: usize = 4096;
const FILE_CHECKPOINT_INTERVAL_BYTES: u64 = 256 * 1024;
const GZIP_RUNNING: u8 = 0;
const GZIP_STOPPING: u8 = 1;
const GZIP_ABORTING: u8 = 2;

#[derive(Clone)]
pub struct FileContentHasher(Hasher);

impl FileContentHasher {
    pub fn new() -> Self {
        Self(Hasher::new())
    }
    pub fn update(&mut self, bytes: &[u8]) {
        self.0.update(bytes);
    }
    pub fn finalize(self) -> u32 {
        self.0.finalize()
    }
    pub fn checksum(&self) -> u32 {
        self.0.clone().finalize()
    }
}

impl Default for FileContentHasher {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FileIdentity {
    pub device: u64,
    pub inode: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FileResumeCursor {
    pub offset: u64,
    pub identity: Option<FileIdentity>,
    pub evidence: Vec<u8>,
    pub content_crc32: u32,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FileEncoding {
    #[default]
    Plain,
    Gzip {
        compressed_size: u64,
        compressed_crc32: u32,
        compressed_evidence: Vec<u8>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileCaptureResume {
    pub cursor: FileResumeCursor,
    pub encoding: FileEncoding,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChunkPosition {
    Complete,
    Start,
    Continue,
    End,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CapturedRecord {
    pub captured_at_unix_nanos: i64,
    pub stream: StreamKind,
    pub bytes: Vec<u8>,
    pub delimiter: Vec<u8>,
    pub acquisition_id: Uuid,
    pub chunk: ChunkPosition,
}

impl CapturedRecord {
    pub fn into_raw(self, source_id: SourceId) -> RawRecord {
        RawRecord {
            record_id: RecordId {
                source_id,
                sequence: 0,
            },
            captured_at_unix_nanos: self.captured_at_unix_nanos,
            stream: self.stream,
            bytes: self.bytes,
            delimiter: self.delimiter,
            acquisition_id: self.acquisition_id,
            chunk: self.chunk,
        }
    }
}

#[derive(Clone, Debug)]
pub enum CaptureEvent {
    Boundary {
        acquisition_id: Uuid,
        reason: BoundaryReason,
    },
    Record(CapturedRecord),
    FileCheckpoint {
        acquisition_id: Uuid,
        cursor: FileResumeCursor,
        encoding: FileEncoding,
    },
    CommandExit {
        acquisition_id: Uuid,
        status: ExitStatus,
    },
    Error {
        acquisition_id: Uuid,
        message: String,
    },
    Stopped {
        acquisition_id: Uuid,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BoundaryReason {
    Started,
    Rotated,
    Truncated,
}

#[derive(Clone, Copy, Debug)]
pub struct CaptureLimits {
    pub channel_capacity: usize,
    pub read_chunk_bytes: usize,
    pub maximum_record_bytes: usize,
    pub poll_interval: Duration,
    pub partial_flush_interval: Duration,
    /// Bound on undelivered source-history entries. Saturation drops entries
    /// and reports the count; it never blocks or grows capture.
    pub history_capacity: usize,
    /// Runtime bounds applied to command restarts. The restart *policy* is
    /// persisted with the source; these bounds are runtime configuration so no
    /// stored definition can describe an unbounded restart loop.
    pub restart: RestartBounds,
}
impl Default for CaptureLimits {
    fn default() -> Self {
        Self {
            channel_capacity: 128,
            read_chunk_bytes: 16 * 1024,
            maximum_record_bytes: 64 * 1024,
            poll_interval: Duration::from_millis(50),
            partial_flush_interval: Duration::from_millis(100),
            history_capacity: crate::source_event::DEFAULT_SOURCE_EVENT_CAPACITY,
            restart: RestartBounds::default(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CaptureCompletion {
    pub aborted: bool,
    pub discarded_buffered_bytes: usize,
}

pub struct CaptureHandle {
    abort: watch::Sender<bool>,
    stop: watch::Sender<bool>,
    task: Option<JoinHandle<CaptureCompletion>>,
}
impl CaptureHandle {
    pub fn stop(&mut self) {
        let _ = self.stop.send(true);
    }
    pub fn abort(&mut self) {
        let _ = self.abort.send(true);
    }
    pub fn cancel(&mut self) {
        self.abort();
    }
    pub async fn join(&mut self) -> Result<CaptureCompletion, tokio::task::JoinError> {
        self.task.take().expect("capture task exists").await
    }
    pub async fn wait(mut self) -> Result<CaptureCompletion, tokio::task::JoinError> {
        self.join().await
    }
}
impl Drop for CaptureHandle {
    fn drop(&mut self) {
        let _ = self.abort.send(true);
    }
}

/// A started acquisition: its control handle, its record/boundary events, and
/// its visible lifecycle history.
pub struct Capture {
    pub handle: CaptureHandle,
    pub events: mpsc::Receiver<CaptureEvent>,
    pub history: mpsc::Receiver<SourceEventRecord>,
    /// Counts history entries dropped because a consumer fell behind. Holding
    /// the counter rather than a sender keeps the history channel closable.
    pub history_dropped: std::sync::Arc<std::sync::atomic::AtomicU64>,
}

/// Creates the bounded channels and cancellation signals every acquisition
/// shares, then spawns `run` as the capture task.
pub(crate) fn spawn_capture<F, Fut>(limits: CaptureLimits, run: F) -> io::Result<Capture>
where
    F: FnOnce(
        mpsc::Sender<CaptureEvent>,
        SourceEventSink,
        watch::Receiver<bool>,
        watch::Receiver<bool>,
    ) -> Fut,
    Fut: std::future::Future<Output = CaptureCompletion> + Send + 'static,
{
    validate(&limits)?;
    let (events, receiver) = mpsc::channel(limits.channel_capacity);
    let (sink, history) = source_event_channel(limits.history_capacity);
    let (abort, aborted) = watch::channel(false);
    let (stop, stopped) = watch::channel(false);
    let history_dropped = sink.dropped_counter();
    let task = tokio::spawn(run(events, sink, aborted, stopped));
    Ok(Capture {
        handle: CaptureHandle {
            abort,
            stop,
            task: Some(task),
        },
        events: receiver,
        history,
        history_dropped,
    })
}

/// Captures a command, honouring its persisted restart policy.
///
/// A restart policy applies to a source the caller has already chosen to start;
/// it is never a reason to launch a remembered command on its own. Ingest
/// enforces that distinction (see `SourceManager::restore`).
pub fn capture_command_supervised(
    definition: CommandDefinition,
    limits: CaptureLimits,
) -> io::Result<Capture> {
    validate(&limits)?;
    let child = spawn_child(&definition)?;
    spawn_capture(limits, move |events, history, cancelled, stopped| {
        run_command_supervised(
            child, definition, limits, events, history, cancelled, stopped,
        )
    })
}

/// Captures a command without observing its lifecycle history. Restart policy
/// still applies; history entries are simply not retained.
pub fn capture_command(
    definition: CommandDefinition,
    limits: CaptureLimits,
) -> io::Result<(CaptureHandle, mpsc::Receiver<CaptureEvent>)> {
    let capture = capture_command_supervised(definition, limits)?;
    Ok((capture.handle, capture.events))
}

fn spawn_child(definition: &CommandDefinition) -> io::Result<Child> {
    let mut command = build_command(definition.clone());
    configure_owned_process(&mut command);
    command.spawn()
}

pub fn capture_reader<R>(
    reader: R,
    limits: CaptureLimits,
) -> io::Result<(CaptureHandle, mpsc::Receiver<CaptureEvent>)>
where
    R: AsyncRead + Unpin + Send + 'static,
{
    validate(&limits)?;
    let (events, receiver) = mpsc::channel(limits.channel_capacity);
    let (abort, aborted) = watch::channel(false);
    let (stop, stopped) = watch::channel(false);
    let task = tokio::spawn(run_reader(reader, events, aborted, stopped, limits));
    Ok((
        CaptureHandle {
            abort,
            stop,
            task: Some(task),
        },
        receiver,
    ))
}

async fn run_reader<R: AsyncRead + Unpin>(
    mut reader: R,
    events: mpsc::Sender<CaptureEvent>,
    mut cancelled: watch::Receiver<bool>,
    mut stopped: watch::Receiver<bool>,
    limits: CaptureLimits,
) -> CaptureCompletion {
    let acquisition_id = Uuid::new_v4();
    if !emit(
        &events,
        &mut cancelled,
        CaptureEvent::Boundary {
            acquisition_id,
            reason: BoundaryReason::Started,
        },
    )
    .await
    {
        return CaptureCompletion {
            aborted: true,
            discarded_buffered_bytes: 0,
        };
    }
    let mut framer = Framer::new(limits.maximum_record_bytes);
    let mut buffer = vec![0; limits.read_chunk_bytes];
    let mut partial_tick = tokio::time::interval(limits.partial_flush_interval);
    partial_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    partial_tick.tick().await;
    loop {
        let read = tokio::select! {
            biased;
            _ = cancelled.changed() => return CaptureCompletion { aborted: true, discarded_buffered_bytes: framer.buffered_len() },
            _ = stopped.changed() => {
                let records = framer.finish(StreamKind::Stdin, acquisition_id);
                let _ = emit_records(&events, &mut cancelled, records).await;
                return CaptureCompletion::default();
            },
            _ = events.closed() => return CaptureCompletion { aborted: true, discarded_buffered_bytes: framer.buffered_len() },
            _ = partial_tick.tick() => {
                if !emit_records(&events, &mut cancelled, framer.flush_partial(StreamKind::Stdin, acquisition_id)).await {
                    return CaptureCompletion { aborted: true, discarded_buffered_bytes: framer.buffered_len() };
                }
                continue;
            },
            result = reader.read(&mut buffer) => result,
        };
        match read {
            Ok(0) => {
                let records = framer.finish(StreamKind::Stdin, acquisition_id);
                let _ = emit_records(&events, &mut cancelled, records).await;
                return CaptureCompletion::default();
            }
            Ok(count) => {
                if !emit_records(
                    &events,
                    &mut cancelled,
                    framer.push(&buffer[..count], StreamKind::Stdin, acquisition_id),
                )
                .await
                {
                    return CaptureCompletion {
                        aborted: true,
                        discarded_buffered_bytes: framer.buffered_len(),
                    };
                }
            }
            Err(error) => {
                let records = framer.finish(StreamKind::Stdin, acquisition_id);
                let discarded = records
                    .iter()
                    .map(|record| record.bytes.len() + record.delimiter.len())
                    .sum();
                if !emit_records(&events, &mut cancelled, records).await {
                    return CaptureCompletion {
                        aborted: true,
                        discarded_buffered_bytes: discarded,
                    };
                }
                if !emit(
                    &events,
                    &mut cancelled,
                    capture_error(acquisition_id, error),
                )
                .await
                {
                    return CaptureCompletion {
                        aborted: true,
                        discarded_buffered_bytes: 0,
                    };
                }
                return CaptureCompletion::default();
            }
        }
    }
}

fn build_command(definition: CommandDefinition) -> Command {
    let mut command = match definition.program {
        CommandProgram::Shell { text } => {
            let mut value = Command::new("sh");
            value.arg("-c").arg(text);
            value
        }
        CommandProgram::Exec { executable, args } => {
            let mut value = Command::new(executable);
            value.args(args);
            value
        }
    };
    command
        .kill_on_drop(true)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    if let Some(cwd) = definition.cwd {
        command.current_dir(cwd);
    }
    command.envs(definition.environment);
    command
}

#[cfg(unix)]
fn configure_owned_process(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    command.as_std_mut().process_group(0);
}
#[cfg(not(unix))]
fn configure_owned_process(_: &mut Command) {}

/// How one command run ended. Only a natural exit is eligible for restart.
enum InstanceOutcome {
    Aborted {
        discarded: usize,
    },
    /// The owner asked capture to stop, or the consumer went away. A restart
    /// policy must never fight an explicit stop.
    Halted {
        discarded: usize,
    },
    Exited {
        status: Option<ExitStatus>,
        discarded: usize,
    },
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum EndedBy {
    Natural,
    Cancelled,
    Halted,
}

/// Runs one command process to completion and reports how it ended.
async fn run_command_instance(
    mut child: Child,
    acquisition_id: Uuid,
    events: &mpsc::Sender<CaptureEvent>,
    cancelled: &mut watch::Receiver<bool>,
    stopped: &mut watch::Receiver<bool>,
    limits: CaptureLimits,
) -> InstanceOutcome {
    if !emit(
        events,
        cancelled,
        CaptureEvent::Boundary {
            acquisition_id,
            reason: BoundaryReason::Started,
        },
    )
    .await
    {
        let _ = terminate_child_tree(&mut child).await;
        return InstanceOutcome::Aborted { discarded: 0 };
    }
    let stdout_task = child.stdout.take().map(|pipe| {
        tokio::spawn(read_stream(
            pipe,
            StreamKind::Stdout,
            acquisition_id,
            events.clone(),
            cancelled.clone(),
            limits,
        ))
    });
    let stderr_task = child.stderr.take().map(|pipe| {
        tokio::spawn(read_stream(
            pipe,
            StreamKind::Stderr,
            acquisition_id,
            events.clone(),
            cancelled.clone(),
            limits,
        ))
    });
    let process_id = child.id();
    let mut ended_by = EndedBy::Natural;
    let outcome = tokio::select! {
        biased;
        changed = cancelled.changed() => {
            let _ = changed;
            ended_by = EndedBy::Cancelled;
            terminate_child_tree(&mut child).await
        }
        changed = stopped.changed() => {
            let _ = changed;
            ended_by = EndedBy::Halted;
            terminate_child_tree(&mut child).await
        }
        _ = events.closed() => {
            ended_by = EndedBy::Halted;
            terminate_child_tree(&mut child).await
        }
        status = child.wait() => {
            kill_process_group(process_id);
            status
        }
    };
    let aborted = *cancelled.borrow();
    if aborted {
        let discarded = join_reader(stdout_task).await + join_reader(stderr_task).await;
        if let Ok(status) = outcome {
            let _ = events.try_send(CaptureEvent::CommandExit {
                acquisition_id,
                status,
            });
        }
        let _ = events.try_send(CaptureEvent::Stopped { acquisition_id });
        return InstanceOutcome::Aborted { discarded };
    }
    let discarded = join_reader(stdout_task).await + join_reader(stderr_task).await;
    let status = match outcome {
        Ok(status) => {
            let _ = emit(
                events,
                cancelled,
                CaptureEvent::CommandExit {
                    acquisition_id,
                    status,
                },
            )
            .await;
            Some(status)
        }
        Err(error) => {
            let _ = emit(events, cancelled, capture_error(acquisition_id, error)).await;
            None
        }
    };
    match ended_by {
        EndedBy::Halted | EndedBy::Cancelled => InstanceOutcome::Halted { discarded },
        EndedBy::Natural => InstanceOutcome::Exited { status, discarded },
    }
}

/// Runs a command and applies its restart policy with bounded backoff and a
/// bounded restart budget. Every start, exit, refusal and restart boundary is
/// published as source history, and each run gets a fresh acquisition identity
/// so stored records never imply one uninterrupted run.
#[allow(clippy::too_many_arguments)]
async fn run_command_supervised(
    first: Child,
    definition: CommandDefinition,
    limits: CaptureLimits,
    events: mpsc::Sender<CaptureEvent>,
    history: SourceEventSink,
    mut cancelled: watch::Receiver<bool>,
    mut stopped: watch::Receiver<bool>,
) -> CaptureCompletion {
    let policy = definition.restart;
    let bounds = limits.restart;
    let mut window = AttemptWindow::new(bounds.maximum_restarts, bounds.window);
    let mut jitter = Jitter::from_entropy();
    let mut child = first;
    let mut run = 0_u32;
    let mut discarded_total = 0_usize;
    loop {
        run = run.saturating_add(1);
        let acquisition_id = Uuid::new_v4();
        history.emit(SourceEvent::CommandStarted {
            acquisition_id,
            run,
        });
        let outcome = run_command_instance(
            child,
            acquisition_id,
            &events,
            &mut cancelled,
            &mut stopped,
            limits,
        )
        .await;
        let (status, discarded) = match outcome {
            InstanceOutcome::Aborted { discarded } => {
                return CaptureCompletion {
                    aborted: true,
                    discarded_buffered_bytes: discarded_total + discarded,
                };
            }
            InstanceOutcome::Halted { discarded } => {
                return CaptureCompletion {
                    aborted: false,
                    discarded_buffered_bytes: discarded_total + discarded,
                };
            }
            InstanceOutcome::Exited { status, discarded } => (status, discarded),
        };
        discarded_total += discarded;
        history.emit(SourceEvent::CommandExited {
            acquisition_id,
            code: status.and_then(|status| status.code()),
            success: status.is_some_and(|status| status.success()),
        });

        // A run that could not be observed counts as a failure, and a restart
        // that cannot be launched re-enters the same bounded decision.
        let mut success = status.map(|status| status.success());
        let next = loop {
            match decide_restart(
                policy,
                success,
                &bounds,
                &mut window,
                &mut jitter,
                Instant::now(),
            ) {
                RestartDecision::PolicyDeclines => {
                    history.emit(SourceEvent::RestartDeclined { policy, success });
                    return CaptureCompletion {
                        aborted: false,
                        discarded_buffered_bytes: discarded_total,
                    };
                }
                RestartDecision::BudgetExhausted { used, .. } => {
                    history.emit(SourceEvent::RestartsExhausted {
                        policy,
                        attempts: used,
                        window_millis: bounds.window.as_millis().min(u128::from(u64::MAX)) as u64,
                    });
                    let _ = emit(
                        &events,
                        &mut cancelled,
                        capture_error(
                            acquisition_id,
                            format!("command restart budget exhausted after {used} restarts"),
                        ),
                    )
                    .await;
                    return CaptureCompletion {
                        aborted: false,
                        discarded_buffered_bytes: discarded_total,
                    };
                }
                RestartDecision::Restart { attempt, delay } => {
                    history.emit(SourceEvent::RestartScheduled {
                        policy,
                        attempt,
                        maximum_restarts: bounds.maximum_restarts,
                        delay_millis: delay.as_millis().min(u128::from(u64::MAX)) as u64,
                    });
                    if !wait_for_restart(delay, &mut cancelled, &mut stopped).await {
                        return CaptureCompletion {
                            aborted: *cancelled.borrow(),
                            discarded_buffered_bytes: discarded_total,
                        };
                    }
                    match spawn_child(&definition) {
                        Ok(next) => break next,
                        Err(error) => {
                            let detail = bounded_detail(error.to_string());
                            history.emit(SourceEvent::RestartFailed {
                                detail: detail.clone(),
                            });
                            let _ = emit(
                                &events,
                                &mut cancelled,
                                capture_error(acquisition_id, detail),
                            )
                            .await;
                            success = Some(false);
                        }
                    }
                }
            }
        };
        child = next;
    }
}

async fn wait_for_restart(
    delay: Duration,
    cancelled: &mut watch::Receiver<bool>,
    stopped: &mut watch::Receiver<bool>,
) -> bool {
    tokio::select! {
        biased;
        _ = cancelled.changed() => false,
        _ = stopped.changed() => false,
        _ = tokio::time::sleep(delay) => true,
    }
}

async fn join_reader(task: Option<JoinHandle<usize>>) -> usize {
    if let Some(task) = task {
        task.await.unwrap_or(0)
    } else {
        0
    }
}

#[cfg(unix)]
fn kill_process_group(process_id: Option<u32>) {
    if let Some(id) = process_id {
        unsafe {
            libc::kill(-(id as i32), libc::SIGKILL);
        }
    }
}
#[cfg(not(unix))]
fn kill_process_group(_: Option<u32>) {}

async fn terminate_child_tree(child: &mut Child) -> io::Result<ExitStatus> {
    kill_process_group(child.id());
    #[cfg(not(unix))]
    {
        let _ = child.kill().await;
    }
    child.wait().await
}

async fn read_stream<R: AsyncRead + Unpin>(
    mut reader: R,
    stream: StreamKind,
    acquisition_id: Uuid,
    events: mpsc::Sender<CaptureEvent>,
    mut cancelled: watch::Receiver<bool>,
    limits: CaptureLimits,
) -> usize {
    let mut framer = Framer::new(limits.maximum_record_bytes);
    let mut buffer = vec![0; limits.read_chunk_bytes];
    let mut partial_tick = tokio::time::interval(limits.partial_flush_interval);
    partial_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    partial_tick.tick().await;
    loop {
        let read = tokio::select! {
            biased;
            _ = cancelled.changed() => return framer.buffered_len(),
            _ = partial_tick.tick() => {
                if !emit_records(&events, &mut cancelled, framer.flush_partial(stream, acquisition_id)).await { return framer.buffered_len(); }
                continue;
            }
            value = reader.read(&mut buffer) => value,
        };
        match read {
            Ok(0) => {
                emit_records(
                    &events,
                    &mut cancelled,
                    framer.finish(stream, acquisition_id),
                )
                .await;
                return 0;
            }
            Ok(count) => {
                if !emit_records(
                    &events,
                    &mut cancelled,
                    framer.push(&buffer[..count], stream, acquisition_id),
                )
                .await
                {
                    return framer.buffered_len();
                }
            }
            Err(error) => {
                let _ = emit(
                    &events,
                    &mut cancelled,
                    capture_error(acquisition_id, error),
                )
                .await;
                return framer.buffered_len();
            }
        }
    }
}

pub fn capture_file(
    path: PathBuf,
    follow: bool,
    limits: CaptureLimits,
) -> io::Result<(CaptureHandle, mpsc::Receiver<CaptureEvent>)> {
    capture_file_from(path, follow, limits, None)
}

pub fn capture_file_from(
    path: PathBuf,
    follow: bool,
    limits: CaptureLimits,
    resume: Option<FileResumeCursor>,
) -> io::Result<(CaptureHandle, mpsc::Receiver<CaptureEvent>)> {
    capture_file_auto_from(
        path,
        follow,
        limits,
        resume.map(|cursor| FileCaptureResume {
            cursor,
            encoding: FileEncoding::Plain,
        }),
    )
}

pub fn capture_file_auto_from(
    path: PathBuf,
    follow: bool,
    limits: CaptureLimits,
    resume: Option<FileCaptureResume>,
) -> io::Result<(CaptureHandle, mpsc::Receiver<CaptureEvent>)> {
    validate(&limits)?;
    let (events, receiver) = mpsc::channel(limits.channel_capacity);
    let (abort, aborted) = watch::channel(false);
    let (stop, stopped) = watch::channel(false);
    let task = tokio::spawn(run_file(
        path, follow, limits, resume, events, aborted, stopped,
    ));
    Ok((
        CaptureHandle {
            abort,
            stop,
            task: Some(task),
        },
        receiver,
    ))
}

async fn open_with_magic(path: &PathBuf) -> io::Result<(File, bool)> {
    let mut file = File::open(path).await?;
    if !file.metadata().await?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "file capture requires a regular file",
        ));
    }
    let mut magic = [0_u8; 2];
    let mut read = 0;
    while read < magic.len() {
        let count = file.read(&mut magic[read..]).await?;
        if count == 0 {
            break;
        }
        read += count;
    }
    file.seek(SeekFrom::Start(0)).await?;
    Ok((file, read == magic.len() && magic == [0x1f, 0x8b]))
}

enum GzipOutput {
    Ready {
        identity: Option<FileIdentity>,
        encoding: FileEncoding,
    },
    Bytes(Vec<u8>),
    Complete,
    Stopped,
    Error(io::Error),
}

fn spawn_gzip_decoder(
    file: StdFile,
    chunk_bytes: usize,
    capacity: usize,
) -> (mpsc::Receiver<GzipOutput>, JoinHandle<()>, Arc<AtomicU8>) {
    let (sender, receiver) = mpsc::channel(capacity);
    let signal = Arc::new(AtomicU8::new(GZIP_RUNNING));
    let worker_signal = signal.clone();
    let task = tokio::task::spawn_blocking(move || {
        let result = (|| -> io::Result<()> {
            let mut file = file;
            let metadata = file.metadata()?;
            if !metadata.is_file() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "gzip capture requires a regular file",
                ));
            }
            let identity = metadata_identity(&metadata);
            let mut verification_file = file.try_clone()?;
            let encoding = fingerprint_std(&mut verification_file, &worker_signal)?;
            file.seek(StdSeekFrom::Start(0))?;
            if worker_signal.load(Ordering::Acquire) != GZIP_RUNNING {
                let _ = send_gzip(&sender, GzipOutput::Stopped, &worker_signal, true);
                return Ok(());
            }
            if !send_gzip(
                &sender,
                GzipOutput::Ready {
                    identity,
                    encoding: encoding.clone(),
                },
                &worker_signal,
                false,
            ) {
                return Ok(());
            }
            let mut decoder = MultiGzDecoder::new(CancellableRead {
                inner: file,
                signal: worker_signal.clone(),
            });
            let mut buffer = vec![0_u8; chunk_bytes];
            loop {
                let count = match decoder.read(&mut buffer) {
                    Ok(count) => count,
                    Err(_) if worker_signal.load(Ordering::Acquire) != GZIP_RUNNING => {
                        let _ = send_gzip(&sender, GzipOutput::Stopped, &worker_signal, true);
                        return Ok(());
                    }
                    Err(error) => return Err(error),
                };
                if count == 0 {
                    let final_encoding = fingerprint_std(&mut verification_file, &worker_signal)?;
                    if final_encoding != encoding {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "gzip archive changed while it was being decoded",
                        ));
                    }
                    let _ = send_gzip(&sender, GzipOutput::Complete, &worker_signal, false);
                    return Ok(());
                }
                if !send_gzip(
                    &sender,
                    GzipOutput::Bytes(buffer[..count].to_vec()),
                    &worker_signal,
                    true,
                ) {
                    return Ok(());
                }
                if worker_signal.load(Ordering::Acquire) != GZIP_RUNNING {
                    let _ = send_gzip(&sender, GzipOutput::Stopped, &worker_signal, true);
                    return Ok(());
                }
            }
        })();
        if let Err(error) = result {
            if worker_signal.load(Ordering::Acquire) == GZIP_RUNNING {
                let _ = send_gzip(&sender, GzipOutput::Error(error), &worker_signal, false);
            } else {
                let _ = send_gzip(&sender, GzipOutput::Stopped, &worker_signal, true);
            }
        }
    });
    (receiver, task, signal)
}

struct CancellableRead {
    inner: StdFile,
    signal: Arc<AtomicU8>,
}

impl Read for CancellableRead {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.signal.load(Ordering::Acquire) != GZIP_RUNNING {
            return Err(io::Error::other("gzip decoding stopped"));
        }
        // Keep parser work between signal checks bounded as well as raw I/O.
        // This matters for gzip headers and chains of empty members that can
        // consume input without producing a decoded output chunk.
        let limit = buffer.len().min(1024);
        self.inner.read(&mut buffer[..limit])
    }
}

fn fingerprint_std(file: &mut StdFile, signal: &AtomicU8) -> io::Result<FileEncoding> {
    file.seek(StdSeekFrom::Start(0))?;
    let metadata = file.metadata()?;
    let mut hasher = Hasher::new();
    let mut evidence = Vec::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        if signal.load(Ordering::Acquire) != GZIP_RUNNING {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "gzip fingerprint stopped",
            ));
        }
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
        evidence.extend_from_slice(&buffer[..count]);
        if evidence.len() > FILE_EVIDENCE_BYTES {
            evidence.drain(..evidence.len() - FILE_EVIDENCE_BYTES);
        }
    }
    Ok(FileEncoding::Gzip {
        compressed_size: metadata.len(),
        compressed_crc32: hasher.finalize(),
        compressed_evidence: evidence,
    })
}

fn send_gzip(
    sender: &mpsc::Sender<GzipOutput>,
    mut output: GzipOutput,
    signal: &AtomicU8,
    deliver_when_stopping: bool,
) -> bool {
    loop {
        match sender.try_send(output) {
            Ok(()) => return true,
            Err(mpsc::error::TrySendError::Closed(_)) => return false,
            Err(mpsc::error::TrySendError::Full(value)) => output = value,
        }
        let state = signal.load(Ordering::Acquire);
        if state == GZIP_ABORTING || (state == GZIP_STOPPING && !deliver_when_stopping) {
            return false;
        }
        thread::sleep(Duration::from_millis(1));
    }
}

struct GzipState {
    identity: Option<FileIdentity>,
    encoding: FileEncoding,
    acquisition_id: Uuid,
    framer: Framer,
    acknowledged_offset: u64,
    unacknowledged: Vec<u8>,
    evidence: Vec<u8>,
    checkpointed_offset: u64,
    content_hasher: Hasher,
}

async fn run_gzip_file(
    file: File,
    limits: CaptureLimits,
    resume: Option<FileCaptureResume>,
    events: mpsc::Sender<CaptureEvent>,
    mut cancelled: watch::Receiver<bool>,
    mut stopped: watch::Receiver<bool>,
) -> CaptureCompletion {
    let file = file.into_std().await;
    let (mut decoded, worker, worker_signal) =
        spawn_gzip_decoder(file, limits.read_chunk_bytes, limits.channel_capacity);
    let mut stopping = false;
    let ready = tokio::select! {
        biased;
        _ = cancelled.changed() => {
            worker_signal.store(GZIP_ABORTING, Ordering::Release);
            drop(decoded);
            let _ = worker.await;
            return CaptureCompletion { aborted: true, discarded_buffered_bytes: 0 };
        },
        _ = stopped.changed() => {
            stopping = true;
            worker_signal.store(GZIP_STOPPING, Ordering::Release);
            decoded.recv().await
        },
        output = decoded.recv() => output,
    };
    let (identity, encoding) = match ready {
        Some(GzipOutput::Ready { identity, encoding }) => (identity, encoding),
        Some(GzipOutput::Error(error)) => {
            let _ = emit(
                &events,
                &mut cancelled,
                capture_error(Uuid::new_v4(), error),
            )
            .await;
            let _ = worker.await;
            return CaptureCompletion::default();
        }
        Some(GzipOutput::Stopped) | None => {
            let _ = worker.await;
            return CaptureCompletion::default();
        }
        Some(GzipOutput::Bytes(_)) | Some(GzipOutput::Complete) => {
            worker_signal.store(GZIP_ABORTING, Ordering::Release);
            drop(decoded);
            let _ = worker.await;
            let _ = events.try_send(capture_error(
                Uuid::new_v4(),
                "gzip decoder did not publish its fingerprint first",
            ));
            return CaptureCompletion::default();
        }
    };
    let resume = match resume {
        Some(value)
            if value.encoding == encoding
                && value.cursor.identity.as_ref() == identity.as_ref() =>
        {
            Some(value.cursor)
        }
        Some(_) => {
            worker_signal.store(GZIP_ABORTING, Ordering::Release);
            drop(decoded);
            let _ = worker.await;
            let _ = emit(
                &events,
                &mut cancelled,
                capture_error(
                    Uuid::new_v4(),
                    "gzip archive changed; create a new source identity to capture the replacement",
                ),
            )
            .await;
            return CaptureCompletion::default();
        }
        None => None,
    };
    let acquisition_id = Uuid::new_v4();
    let resume_offset = resume.as_ref().map_or(0, |cursor| cursor.offset);
    let mut state = GzipState {
        identity,
        encoding,
        acquisition_id,
        framer: Framer::new(limits.maximum_record_bytes),
        acknowledged_offset: resume_offset,
        unacknowledged: Vec::new(),
        evidence: resume
            .as_ref()
            .map_or_else(Vec::new, |cursor| cursor.evidence.clone()),
        checkpointed_offset: resume_offset,
        content_hasher: Hasher::new(),
    };
    if !emit(
        &events,
        &mut cancelled,
        CaptureEvent::Boundary {
            acquisition_id,
            reason: BoundaryReason::Started,
        },
    )
    .await
    {
        return CaptureCompletion {
            aborted: true,
            discarded_buffered_bytes: 0,
        };
    }
    if resume_offset == 0 && !emit_gzip_checkpoint(&events, &mut cancelled, &state).await {
        return CaptureCompletion {
            aborted: true,
            discarded_buffered_bytes: 0,
        };
    }
    let mut skipped = 0_u64;
    let mut validation_hasher = Hasher::new();
    let mut validation_tail = Vec::new();
    let mut resume_validated = resume_offset == 0;
    let mut partial_tick = tokio::time::interval(limits.partial_flush_interval);
    partial_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    partial_tick.tick().await;
    let mut terminal_error = None;
    loop {
        let output = tokio::select! {
            biased;
            _ = cancelled.changed() => {
                worker_signal.store(GZIP_ABORTING, Ordering::Release);
                drop(decoded);
                let _ = worker.await;
                return CaptureCompletion { aborted: true, discarded_buffered_bytes: state.framer.buffered_len() };
            },
            _ = stopped.changed(), if !stopping => {
                stopping = true;
                worker_signal.store(GZIP_STOPPING, Ordering::Release);
                continue;
            },
            _ = events.closed() => {
                worker_signal.store(GZIP_ABORTING, Ordering::Release);
                drop(decoded);
                let _ = worker.await;
                return CaptureCompletion { aborted: true, discarded_buffered_bytes: state.framer.buffered_len() };
            },
            _ = partial_tick.tick(), if resume_validated => {
                if !emit_records(&events, &mut cancelled, state.framer.flush_partial(StreamKind::File, acquisition_id)).await
                    || !acknowledge_gzip(&events, &mut cancelled, &mut state, true).await
                {
                    worker_signal.store(GZIP_ABORTING, Ordering::Release);
                    drop(decoded);
                    let _ = worker.await;
                    return CaptureCompletion { aborted: true, discarded_buffered_bytes: state.framer.buffered_len() };
                }
                continue;
            },
            value = decoded.recv() => value,
        };
        match output {
            Some(GzipOutput::Bytes(mut bytes)) => {
                if !resume_validated {
                    let remaining = (resume_offset - skipped) as usize;
                    let take = remaining.min(bytes.len());
                    validation_hasher.update(&bytes[..take]);
                    validation_tail.extend_from_slice(&bytes[..take]);
                    if validation_tail.len() > FILE_EVIDENCE_BYTES {
                        validation_tail.drain(..validation_tail.len() - FILE_EVIDENCE_BYTES);
                    }
                    skipped += take as u64;
                    bytes.drain(..take);
                    if skipped == resume_offset {
                        let cursor = resume.as_ref().expect("nonzero resume has cursor");
                        if validation_hasher.clone().finalize() != cursor.content_crc32
                            || validation_tail != cursor.evidence
                        {
                            terminal_error = Some(io::Error::new(
                                io::ErrorKind::InvalidData,
                                "decoded gzip prefix no longer matches durable cursor",
                            ));
                            break;
                        }
                        state.content_hasher = validation_hasher.clone();
                        resume_validated = true;
                        if !emit_gzip_checkpoint(&events, &mut cancelled, &state).await {
                            worker_signal.store(GZIP_ABORTING, Ordering::Release);
                            drop(decoded);
                            let _ = worker.await;
                            return CaptureCompletion {
                                aborted: true,
                                discarded_buffered_bytes: 0,
                            };
                        }
                    }
                }
                if resume_validated && !bytes.is_empty() {
                    state.unacknowledged.extend_from_slice(&bytes);
                    if !emit_records(
                        &events,
                        &mut cancelled,
                        state.framer.push(&bytes, StreamKind::File, acquisition_id),
                    )
                    .await
                        || !acknowledge_gzip(&events, &mut cancelled, &mut state, false).await
                    {
                        worker_signal.store(GZIP_ABORTING, Ordering::Release);
                        drop(decoded);
                        let _ = worker.await;
                        return CaptureCompletion {
                            aborted: true,
                            discarded_buffered_bytes: state.framer.buffered_len(),
                        };
                    }
                }
            }
            Some(GzipOutput::Error(error)) => {
                terminal_error = Some(error);
                break;
            }
            Some(GzipOutput::Complete) | Some(GzipOutput::Stopped) => break,
            Some(GzipOutput::Ready { .. }) => {
                terminal_error = Some(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "gzip decoder published duplicate fingerprint metadata",
                ));
                break;
            }
            None => break,
        }
    }
    drop(decoded);
    let _ = worker.await;
    if !resume_validated && !stopping {
        terminal_error = Some(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "gzip stream ended before durable decoded offset",
        ));
    }
    let pending = state.framer.buffered_len();
    if !emit_records(
        &events,
        &mut cancelled,
        state.framer.finish(StreamKind::File, acquisition_id),
    )
    .await
    {
        return CaptureCompletion {
            aborted: true,
            discarded_buffered_bytes: pending,
        };
    }
    if !acknowledge_gzip(&events, &mut cancelled, &mut state, true).await {
        return CaptureCompletion {
            aborted: true,
            discarded_buffered_bytes: 0,
        };
    }
    if let Some(error) = terminal_error {
        if !emit(
            &events,
            &mut cancelled,
            capture_error(acquisition_id, error),
        )
        .await
        {
            return CaptureCompletion {
                aborted: true,
                discarded_buffered_bytes: 0,
            };
        }
    } else if stopping {
        let _ = events.try_send(CaptureEvent::Stopped { acquisition_id });
    }
    CaptureCompletion::default()
}

async fn acknowledge_gzip(
    events: &mpsc::Sender<CaptureEvent>,
    cancelled: &mut watch::Receiver<bool>,
    state: &mut GzipState,
    force: bool,
) -> bool {
    let emitted = state
        .unacknowledged
        .len()
        .saturating_sub(state.framer.buffered_len());
    if emitted > 0 {
        let acknowledged: Vec<_> = state.unacknowledged.drain(..emitted).collect();
        state.acknowledged_offset += acknowledged.len() as u64;
        state.evidence.extend_from_slice(&acknowledged);
        state.content_hasher.update(&acknowledged);
        if state.evidence.len() > FILE_EVIDENCE_BYTES {
            state
                .evidence
                .drain(..state.evidence.len() - FILE_EVIDENCE_BYTES);
        }
    }
    if state.acknowledged_offset == state.checkpointed_offset
        || (!force
            && state.acknowledged_offset - state.checkpointed_offset
                < FILE_CHECKPOINT_INTERVAL_BYTES)
    {
        return true;
    }
    if emit_gzip_checkpoint(events, cancelled, state).await {
        state.checkpointed_offset = state.acknowledged_offset;
        true
    } else {
        false
    }
}

async fn emit_gzip_checkpoint(
    events: &mpsc::Sender<CaptureEvent>,
    cancelled: &mut watch::Receiver<bool>,
    state: &GzipState,
) -> bool {
    emit(
        events,
        cancelled,
        CaptureEvent::FileCheckpoint {
            acquisition_id: state.acquisition_id,
            cursor: FileResumeCursor {
                offset: state.acknowledged_offset,
                identity: state.identity.clone(),
                evidence: state.evidence.clone(),
                content_crc32: state.content_hasher.clone().finalize(),
            },
            encoding: state.encoding.clone(),
        },
    )
    .await
}

struct FileState {
    file: File,
    identity: Option<FileIdentity>,
    offset: u64,
    acquisition_id: Uuid,
    framer: Framer,
    acknowledged_offset: u64,
    unacknowledged: Vec<u8>,
    evidence: Vec<u8>,
    checkpointed_offset: u64,
    content_hasher: Hasher,
}

async fn run_file(
    path: PathBuf,
    follow: bool,
    limits: CaptureLimits,
    resume: Option<FileCaptureResume>,
    events: mpsc::Sender<CaptureEvent>,
    mut cancelled: watch::Receiver<bool>,
    mut stopped: watch::Receiver<bool>,
) -> CaptureCompletion {
    let (opened_file, gzip) = match open_with_magic(&path).await {
        Ok(value) => value,
        Err(error) => {
            let _ = events.try_send(capture_error(Uuid::new_v4(), error));
            return CaptureCompletion::default();
        }
    };
    if gzip {
        return run_gzip_file(opened_file, limits, resume, events, cancelled, stopped).await;
    }
    drop(opened_file);
    let resume = match resume {
        Some(FileCaptureResume {
            cursor,
            encoding: FileEncoding::Plain,
        }) => Some(cursor),
        Some(_) => {
            let _ = events.try_send(capture_error(
                Uuid::new_v4(),
                "file encoding changed from gzip to plain; create a new source identity",
            ));
            return CaptureCompletion::default();
        }
        None => None,
    };
    let (mut state, initial_reason) = match open_file_state(
        &path,
        limits.maximum_record_bytes,
        resume,
        Some((&mut cancelled, &mut stopped)),
    )
    .await
    {
        Ok(state) => state,
        Err(error) => {
            let _ = events.try_send(capture_error(Uuid::new_v4(), error));
            return CaptureCompletion {
                aborted: false,
                discarded_buffered_bytes: 0,
            };
        }
    };
    if !emit(
        &events,
        &mut cancelled,
        CaptureEvent::Boundary {
            acquisition_id: state.acquisition_id,
            reason: initial_reason,
        },
    )
    .await
    {
        return CaptureCompletion {
            aborted: true,
            discarded_buffered_bytes: state.framer.buffered_len(),
        };
    }
    if !emit_checkpoint(&events, &mut cancelled, &state).await {
        return CaptureCompletion {
            aborted: true,
            discarded_buffered_bytes: 0,
        };
    }
    let mut buffer = vec![0; limits.read_chunk_bytes];
    let mut partial_tick = tokio::time::interval(limits.partial_flush_interval);
    partial_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    partial_tick.tick().await;
    loop {
        let read = tokio::select! {
            biased;
            _ = cancelled.changed() => { let discarded = state.framer.buffered_len(); finish_file(&mut state, &events, &mut cancelled, true).await; return CaptureCompletion { aborted: true, discarded_buffered_bytes: discarded }; },
            _ = stopped.changed() => { finish_file(&mut state, &events, &mut cancelled, true).await; return CaptureCompletion::default(); },
            _ = events.closed() => return CaptureCompletion { aborted: true, discarded_buffered_bytes: state.framer.buffered_len() },
            _ = partial_tick.tick() => {
                let records = state.framer.flush_partial(StreamKind::File, state.acquisition_id);
                if !emit_records(&events, &mut cancelled, records).await { return CaptureCompletion { aborted: true, discarded_buffered_bytes: state.framer.buffered_len() }; }
                if !acknowledge_file(&events, &mut cancelled, &mut state, true).await { return CaptureCompletion { aborted: true, discarded_buffered_bytes: state.framer.buffered_len() }; }
                continue;
            }
            value = state.file.read(&mut buffer) => value,
        };
        match read {
            Ok(count) if count > 0 => {
                state.offset += count as u64;
                state.unacknowledged.extend_from_slice(&buffer[..count]);
                let records =
                    state
                        .framer
                        .push(&buffer[..count], StreamKind::File, state.acquisition_id);
                if !emit_records(&events, &mut cancelled, records).await {
                    return CaptureCompletion {
                        aborted: true,
                        discarded_buffered_bytes: state.framer.buffered_len(),
                    };
                }
                if !acknowledge_file(&events, &mut cancelled, &mut state, false).await {
                    return CaptureCompletion {
                        aborted: true,
                        discarded_buffered_bytes: state.framer.buffered_len(),
                    };
                }
            }
            Ok(_) if !follow => {
                finish_file(&mut state, &events, &mut cancelled, false).await;
                return CaptureCompletion::default();
            }
            Ok(_) => {
                if !wait_poll(limits.poll_interval, &mut cancelled).await {
                    finish_file(&mut state, &events, &mut cancelled, true).await;
                    return CaptureCompletion {
                        aborted: true,
                        discarded_buffered_bytes: state.framer.buffered_len(),
                    };
                }
                let Err(error) =
                    update_follow_state(&path, &limits, &events, &mut cancelled, &mut state).await
                else {
                    continue;
                };
                if !emit(
                    &events,
                    &mut cancelled,
                    capture_error(state.acquisition_id, error),
                )
                .await
                {
                    return CaptureCompletion {
                        aborted: true,
                        discarded_buffered_bytes: state.framer.buffered_len(),
                    };
                }
            }
            Err(error) => {
                let _ = emit(
                    &events,
                    &mut cancelled,
                    capture_error(state.acquisition_id, error),
                )
                .await;
                return CaptureCompletion {
                    aborted: false,
                    discarded_buffered_bytes: state.framer.buffered_len(),
                };
            }
        }
    }
}

async fn open_file_state(
    path: &PathBuf,
    maximum_record_bytes: usize,
    resume: Option<FileResumeCursor>,
    signals: Option<(&mut watch::Receiver<bool>, &mut watch::Receiver<bool>)>,
) -> io::Result<(FileState, BoundaryReason)> {
    let mut file = File::open(path).await?;
    let identity = metadata_identity(&file.metadata().await?);
    let (offset, evidence, content_hasher, reason) =
        validate_resume(&mut file, identity.as_ref(), resume, signals).await?;
    file.seek(SeekFrom::Start(offset)).await?;
    Ok((
        FileState {
            file,
            identity,
            offset,
            acquisition_id: Uuid::new_v4(),
            framer: Framer::new(maximum_record_bytes),
            acknowledged_offset: offset,
            unacknowledged: Vec::new(),
            evidence,
            checkpointed_offset: offset,
            content_hasher,
        },
        reason,
    ))
}

async fn update_follow_state(
    path: &PathBuf,
    limits: &CaptureLimits,
    events: &mpsc::Sender<CaptureEvent>,
    cancelled: &mut watch::Receiver<bool>,
    state: &mut FileState,
) -> io::Result<()> {
    let path_metadata = tokio::fs::metadata(path).await?;
    let path_identity = metadata_identity(&path_metadata);
    if state.identity.is_some() && path_identity != state.identity {
        if state.file.metadata().await?.len() > state.offset {
            return Ok(());
        }
        if !emit_records(
            events,
            cancelled,
            state.framer.finish(StreamKind::File, state.acquisition_id),
        )
        .await
        {
            return Ok(());
        }
        if !acknowledge_file(events, cancelled, state, true).await {
            return Ok(());
        }
        let (new_state, _) = open_file_state(path, limits.maximum_record_bytes, None, None).await?;
        *state = new_state;
        let _ = emit(
            events,
            cancelled,
            CaptureEvent::Boundary {
                acquisition_id: state.acquisition_id,
                reason: BoundaryReason::Rotated,
            },
        )
        .await;
        let _ = emit_checkpoint(events, cancelled, state).await;
    } else if path_metadata.len() < state.offset {
        if !emit_records(
            events,
            cancelled,
            state.framer.finish(StreamKind::File, state.acquisition_id),
        )
        .await
        {
            return Ok(());
        }
        if !acknowledge_file(events, cancelled, state, true).await {
            return Ok(());
        }
        state.file.seek(SeekFrom::Start(0)).await?;
        state.offset = 0;
        state.acquisition_id = Uuid::new_v4();
        state.framer = Framer::new(limits.maximum_record_bytes);
        state.acknowledged_offset = 0;
        state.unacknowledged.clear();
        state.evidence.clear();
        state.checkpointed_offset = 0;
        state.content_hasher = Hasher::new();
        let _ = emit(
            events,
            cancelled,
            CaptureEvent::Boundary {
                acquisition_id: state.acquisition_id,
                reason: BoundaryReason::Truncated,
            },
        )
        .await;
        let _ = emit_checkpoint(events, cancelled, state).await;
    }
    Ok(())
}

async fn finish_file(
    state: &mut FileState,
    events: &mpsc::Sender<CaptureEvent>,
    cancelled: &mut watch::Receiver<bool>,
    stopped: bool,
) {
    let records = state.framer.finish(StreamKind::File, state.acquisition_id);
    if !*cancelled.borrow() && emit_records(events, cancelled, records).await {
        let _ = acknowledge_file(events, cancelled, state, true).await;
    }
    if stopped {
        let _ = events.try_send(CaptureEvent::Stopped {
            acquisition_id: state.acquisition_id,
        });
    }
}

async fn wait_poll(duration: Duration, cancelled: &mut watch::Receiver<bool>) -> bool {
    tokio::select! { biased; _ = cancelled.changed() => false, _ = tokio::time::sleep(duration) => true }
}

pub(crate) async fn emit_records(
    events: &mpsc::Sender<CaptureEvent>,
    cancelled: &mut watch::Receiver<bool>,
    records: Vec<CapturedRecord>,
) -> bool {
    for record in records {
        if !emit(events, cancelled, CaptureEvent::Record(record)).await {
            return false;
        }
    }
    true
}

pub(crate) async fn emit(
    events: &mpsc::Sender<CaptureEvent>,
    cancelled: &mut watch::Receiver<bool>,
    event: CaptureEvent,
) -> bool {
    if *cancelled.borrow() || events.is_closed() {
        return false;
    }
    tokio::select! { biased; _ = cancelled.changed() => false, _ = events.closed() => false, result = events.send(event) => result.is_ok() }
}

fn validate(limits: &CaptureLimits) -> io::Result<()> {
    if limits.channel_capacity == 0
        || limits.read_chunk_bytes == 0
        || limits.maximum_record_bytes == 0
        || limits.partial_flush_interval.is_zero()
        || limits.poll_interval.is_zero()
    {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "capture limits must be nonzero",
        ))
    } else {
        Ok(())
    }
}

/// Capture timestamp source shared by every acquisition kind.
pub(crate) fn capture_now() -> i64 {
    now()
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_nanos().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}
pub(crate) fn capture_error(acquisition_id: Uuid, error: impl std::fmt::Display) -> CaptureEvent {
    CaptureEvent::Error {
        acquisition_id,
        message: error.to_string(),
    }
}

#[cfg(unix)]
fn metadata_identity(metadata: &std::fs::Metadata) -> Option<FileIdentity> {
    use std::os::unix::fs::MetadataExt;
    Some(FileIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}
#[cfg(not(unix))]
fn metadata_identity(_: &std::fs::Metadata) -> Option<FileIdentity> {
    None
}

async fn validate_resume(
    file: &mut File,
    identity: Option<&FileIdentity>,
    resume: Option<FileResumeCursor>,
    mut signals: Option<(&mut watch::Receiver<bool>, &mut watch::Receiver<bool>)>,
) -> io::Result<(u64, Vec<u8>, Hasher, BoundaryReason)> {
    let Some(resume) = resume else {
        return Ok((0, Vec::new(), Hasher::new(), BoundaryReason::Started));
    };
    if resume.identity.as_ref() != identity {
        return Ok((0, Vec::new(), Hasher::new(), BoundaryReason::Rotated));
    }
    if resume.evidence.len() > FILE_EVIDENCE_BYTES || resume.evidence.len() as u64 > resume.offset {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid file resume evidence",
        ));
    }
    if file.metadata().await?.len() < resume.offset {
        return Ok((0, Vec::new(), Hasher::new(), BoundaryReason::Truncated));
    }
    file.seek(SeekFrom::Start(0)).await?;
    let mut remaining = resume.offset;
    let mut buffer = vec![0_u8; 64 * 1024];
    let mut hasher = Hasher::new();
    let mut tail = Vec::new();
    while remaining > 0 {
        let limit = remaining.min(buffer.len() as u64) as usize;
        let count = if let Some((cancelled, stopped)) = signals.as_mut() {
            tokio::select! {
                biased;
                _ = cancelled.changed() => return Err(io::Error::new(io::ErrorKind::Interrupted, "file resume validation aborted")),
                _ = stopped.changed() => return Err(io::Error::new(io::ErrorKind::Interrupted, "file resume validation stopped")),
                result = file.read(&mut buffer[..limit]) => result?,
            }
        } else {
            file.read(&mut buffer[..limit]).await?
        };
        if count == 0 {
            return Ok((0, Vec::new(), Hasher::new(), BoundaryReason::Truncated));
        }
        hasher.update(&buffer[..count]);
        tail.extend_from_slice(&buffer[..count]);
        if tail.len() > FILE_EVIDENCE_BYTES {
            tail.drain(..tail.len() - FILE_EVIDENCE_BYTES);
        }
        remaining -= count as u64;
    }
    if tail != resume.evidence || hasher.clone().finalize() != resume.content_crc32 {
        return Ok((0, Vec::new(), Hasher::new(), BoundaryReason::Truncated));
    }
    Ok((
        resume.offset,
        resume.evidence,
        hasher,
        BoundaryReason::Started,
    ))
}

async fn acknowledge_file(
    events: &mpsc::Sender<CaptureEvent>,
    cancelled: &mut watch::Receiver<bool>,
    state: &mut FileState,
    force: bool,
) -> bool {
    let emitted = state
        .unacknowledged
        .len()
        .saturating_sub(state.framer.buffered_len());
    if emitted > 0 {
        let acknowledged: Vec<_> = state.unacknowledged.drain(..emitted).collect();
        state.acknowledged_offset += acknowledged.len() as u64;
        state.evidence.extend_from_slice(&acknowledged);
        state.content_hasher.update(&acknowledged);
        if state.evidence.len() > FILE_EVIDENCE_BYTES {
            state
                .evidence
                .drain(..state.evidence.len() - FILE_EVIDENCE_BYTES);
        }
    }
    if state.acknowledged_offset == state.checkpointed_offset
        || (!force
            && state.acknowledged_offset - state.checkpointed_offset
                < FILE_CHECKPOINT_INTERVAL_BYTES)
    {
        return true;
    }
    if emit_checkpoint(events, cancelled, state).await {
        state.checkpointed_offset = state.acknowledged_offset;
        true
    } else {
        false
    }
}

async fn emit_checkpoint(
    events: &mpsc::Sender<CaptureEvent>,
    cancelled: &mut watch::Receiver<bool>,
    state: &FileState,
) -> bool {
    emit(
        events,
        cancelled,
        CaptureEvent::FileCheckpoint {
            acquisition_id: state.acquisition_id,
            cursor: FileResumeCursor {
                offset: state.acknowledged_offset,
                identity: state.identity.clone(),
                evidence: state.evidence.clone(),
                content_crc32: state.content_hasher.clone().finalize(),
            },
            encoding: FileEncoding::Plain,
        },
    )
    .await
}

pub(crate) struct Framer {
    pending: Vec<u8>,
    fragmented: bool,
    maximum: usize,
}
impl Framer {
    pub(crate) fn new(maximum: usize) -> Self {
        Self {
            pending: Vec::with_capacity(maximum.min(8192)),
            fragmented: false,
            maximum,
        }
    }
    pub(crate) fn push(
        &mut self,
        input: &[u8],
        stream: StreamKind,
        acquisition_id: Uuid,
    ) -> Vec<CapturedRecord> {
        let mut output = Vec::new();
        for &byte in input {
            self.pending.push(byte);
            if byte == b'\n' {
                let delimiter =
                    if self.pending.len() >= 2 && self.pending[self.pending.len() - 2] == b'\r' {
                        vec![b'\r', b'\n']
                    } else {
                        vec![b'\n']
                    };
                let body_length = self.pending.len() - delimiter.len();
                let bytes = self.pending.drain(..body_length).collect();
                self.pending.clear();
                output.push(self.record(bytes, delimiter, stream, acquisition_id, true));
                self.fragmented = false;
            } else if self.pending.len() > self.maximum {
                let bytes = self.pending.drain(..self.maximum).collect();
                output.push(self.record(bytes, Vec::new(), stream, acquisition_id, false));
                self.fragmented = true;
            }
        }
        output
    }
    pub(crate) fn finish(
        &mut self,
        stream: StreamKind,
        acquisition_id: Uuid,
    ) -> Vec<CapturedRecord> {
        if self.pending.is_empty() {
            if self.fragmented {
                self.fragmented = false;
                return vec![CapturedRecord {
                    captured_at_unix_nanos: now(),
                    stream,
                    bytes: Vec::new(),
                    delimiter: Vec::new(),
                    acquisition_id,
                    chunk: ChunkPosition::End,
                }];
            }
            return Vec::new();
        }
        let bytes = std::mem::take(&mut self.pending);
        let record = self.record(bytes, Vec::new(), stream, acquisition_id, true);
        self.fragmented = false;
        vec![record]
    }

    fn flush_partial(&mut self, stream: StreamKind, acquisition_id: Uuid) -> Vec<CapturedRecord> {
        let keep = usize::from(self.pending.last() == Some(&b'\r'));
        let emit_length = self.pending.len().saturating_sub(keep);
        if emit_length == 0 {
            return Vec::new();
        }
        let bytes = self.pending.drain(..emit_length).collect();
        let record = self.record(bytes, Vec::new(), stream, acquisition_id, false);
        self.fragmented = true;
        vec![record]
    }

    pub(crate) fn buffered_len(&self) -> usize {
        self.pending.len()
    }
    fn record(
        &self,
        bytes: Vec<u8>,
        delimiter: Vec<u8>,
        stream: StreamKind,
        acquisition_id: Uuid,
        end: bool,
    ) -> CapturedRecord {
        let chunk = match (self.fragmented, end) {
            (false, true) => ChunkPosition::Complete,
            (false, false) => ChunkPosition::Start,
            (true, false) => ChunkPosition::Continue,
            (true, true) => ChunkPosition::End,
        };
        CapturedRecord {
            captured_at_unix_nanos: now(),
            stream,
            bytes,
            delimiter,
            acquisition_id,
            chunk,
        }
    }
}
