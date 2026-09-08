use crate::{
    CommandDefinition, CommandProgram, RawRecord, RecordId, SourceId, StreamKind,
    restart::{AttemptWindow, Jitter, RestartBounds, RestartDecision, decide_restart},
    source_event::{
        SourceEvent, SourceEventRecord, SourceEventSink, bounded_detail, source_event_channel,
    },
};
use crc32fast::Hasher;
use flate2::read::MultiGzDecoder;
use memchr::memchr;
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
    pub bytes: crate::RecordBytes,
    pub delimiter: crate::RecordBytes,
    pub acquisition_id: Uuid,
    pub chunk: ChunkPosition,
    /// CPU spent framing the read that produced this batch. Exactly one record
    /// in a batch carries the charge, so moving records never multiplies it.
    pub reader_cpu_nanos: u64,
}

impl CaptureEvent {
    /// The records this event carries, or nothing for the events that carry
    /// none. Lets a consumer that only wants records avoid matching the rest.
    pub fn records(&self) -> &[CapturedRecord] {
        match self {
            Self::Records(records) => records,
            _ => &[],
        }
    }

    /// The same, taking ownership.
    pub fn into_records(self) -> Vec<CapturedRecord> {
        match self {
            Self::Records(records) => records,
            _ => Vec::new(),
        }
    }
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
    /// Every record a single read produced, in order.
    ///
    /// A batch rather than one record per event because each event crosses two
    /// bounded channels and takes a semaphore permit on the way, and at a
    /// hundred bytes a record that hand-over cost more than framing and
    /// journalling put together. The framer already produces a read's records
    /// as one vector; this carries it across whole.
    ///
    /// The memory bound moves with it: a permit now covers a batch rather than
    /// a record, so what one permit can hold is a read chunk plus at most one
    /// maximum-sized record carried over from the read before it. With default
    /// limits, eight queued events therefore retain at most
    /// `8 * (256 KiB + 64 KiB) = 2.5 MiB` of payload backing per source. Record
    /// clones and siblings may outlive each other but share that same charge;
    /// they cannot increase it. The consumer may hold one additional event
    /// while processing it, for a 2.8125 MiB acquisition-side worst case.
    Records(Vec<CapturedRecord>),
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
            // A read is one hand-over, so the queue is counted in reads and
            // the bound is bytes, not records: eight reads in flight of at most
            // a chunk plus one carried-over record is about 2.5 MB per source,
            // where 128 single records of up to `maximum_record_bytes` was 8 MB.
            // Fewer, larger units cost less and hold less.
            channel_capacity: 8,
            read_chunk_bytes: 256 * 1024,
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

/// Puts a spawned command in its own process group and binds its life to ours.
///
/// The group is what makes `kill_process_group` reach a shell's own children;
/// without it, stopping a source leaves whatever it started behind. But a group
/// only helps when somebody is alive to signal it, and `SIGKILL` on lvu runs no
/// cleanup at all — that is how a day of test runs left 87 `sleep` loops on this
/// machine, and how `kill -9 lvu` would leave a user's `journalctl -f` running.
///
/// So the child also asks the kernel to kill it when the thread that spawned it
/// goes away. The two cover each other: the explicit group kill handles orderly
/// stops and the parent-death signal handles every way lvu can die without one.
#[cfg(target_os = "linux")]
fn configure_owned_process(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    command.as_std_mut().process_group(0);
    // SAFETY: async-signal-safe calls only, between fork and exec.
    unsafe {
        command.as_std_mut().pre_exec(|| {
            let parent = libc::getppid();
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            // The parent may already have died in the window before that call,
            // in which case the signal it arms will never be delivered.
            if libc::getppid() != parent {
                libc::_exit(libc::EXIT_FAILURE);
            }
            Ok(())
        });
    }
}
#[cfg(all(unix, not(target_os = "linux")))]
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
        // Read in place rather than drained into a vector first: the copy that
        // vector made was of every captured byte, to be dropped a few lines
        // later. Evidence keeps only its last `FILE_EVIDENCE_BYTES`, so only
        // that much of the tail is worth copying into it — appending a whole
        // read and then discarding the front of it was the same bytes moved
        // twice more.
        let GzipState {
            unacknowledged,
            evidence,
            content_hasher,
            acknowledged_offset,
            ..
        } = state;
        let acknowledged = &unacknowledged[..emitted];
        *acknowledged_offset += emitted as u64;
        content_hasher.update(acknowledged);
        let keep = acknowledged.len().min(FILE_EVIDENCE_BYTES);
        if keep == FILE_EVIDENCE_BYTES {
            evidence.clear();
        } else if evidence.len() + keep > FILE_EVIDENCE_BYTES {
            evidence.drain(..evidence.len() + keep - FILE_EVIDENCE_BYTES);
        }
        evidence.extend_from_slice(&acknowledged[acknowledged.len() - keep..]);
        unacknowledged.drain(..emitted);
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
    // An empty read produces no event at all rather than an empty batch, so a
    // consumer counting events still counts reads that carried something.
    if records.is_empty() {
        return true;
    }
    emit(events, cancelled, CaptureEvent::Records(records)).await
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
        // Read in place rather than drained into a vector first: the copy that
        // vector made was of every captured byte, to be dropped a few lines
        // later. Evidence keeps only its last `FILE_EVIDENCE_BYTES`, so only
        // that much of the tail is worth copying into it — appending a whole
        // read and then discarding the front of it was the same bytes moved
        // twice more.
        let FileState {
            unacknowledged,
            evidence,
            content_hasher,
            acknowledged_offset,
            ..
        } = state;
        let acknowledged = &unacknowledged[..emitted];
        *acknowledged_offset += emitted as u64;
        content_hasher.update(acknowledged);
        let keep = acknowledged.len().min(FILE_EVIDENCE_BYTES);
        if keep == FILE_EVIDENCE_BYTES {
            evidence.clear();
        } else if evidence.len() + keep > FILE_EVIDENCE_BYTES {
            evidence.drain(..evidence.len() + keep - FILE_EVIDENCE_BYTES);
        }
        evidence.extend_from_slice(&acknowledged[acknowledged.len() - keep..]);
        unacknowledged.drain(..emitted);
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
    reader_cpu_pending: u64,
    /// Payload backing allocations, separate from the output vector itself.
    /// This makes the allocation claim measurable without installing a global
    /// allocator that would count unrelated concurrent test work.
    #[cfg(test)]
    backing_allocations: u64,
}
impl Framer {
    pub(crate) fn new(maximum: usize) -> Self {
        Self {
            pending: Vec::with_capacity(maximum.min(8192)),
            fragmented: false,
            maximum,
            reader_cpu_pending: 0,
            #[cfg(test)]
            backing_allocations: 0,
        }
    }
    /// Frames a whole read rather than walking it a byte at a time.
    ///
    /// The rule is unchanged and the output is asserted identical to the
    /// byte-at-a-time original in `framing_equivalence`: a line ends at `\n`,
    /// carrying `\r\n` when the byte before it is `\r`; a run without a
    /// terminator is cut into fragments every `maximum` bytes and the source is
    /// marked fragmented until a terminator arrives. What changed is how the
    /// terminators are found — one scan per read instead of a branch per byte —
    /// and that a line lying wholly inside this read retains a range of one
    /// shared backing buffer rather than being copied through `pending`.
    pub(crate) fn push(
        &mut self,
        input: &[u8],
        stream: StreamKind,
        acquisition_id: Uuid,
    ) -> Vec<CapturedRecord> {
        let cpu = crate::ThreadCpu::start();
        // One allocation per read. Records wholly inside it retain ranges of
        // this backing through both bounded hand-overs and journal encoding.
        let backing: std::sync::Arc<[u8]> = input.into();
        #[cfg(test)]
        {
            self.backing_allocations += 1;
        }
        let mut output = Vec::new();
        let mut start = 0;
        while start < backing.len() {
            let rest = &backing[start..];
            // Fragments come first: the original emitted one as soon as a
            // non-terminator byte took `pending` past `maximum`, so any cut
            // lying before the next terminator still lies before it here.
            let terminator = memchr(b'\n', rest);
            let run = terminator.unwrap_or(rest.len());
            let mut consumed = 0;
            while self.pending.len() + (run - consumed) > self.maximum {
                let take = self.maximum - self.pending.len();
                let bytes = if self.pending.is_empty() {
                    crate::RecordBytes::from_shared(
                        std::sync::Arc::clone(&backing),
                        start + consumed..start + consumed + take,
                    )
                } else {
                    self.pending
                        .extend_from_slice(&rest[consumed..consumed + take]);
                    let bytes = self.pending[..self.maximum].to_vec().into();
                    #[cfg(test)]
                    {
                        self.backing_allocations += 1;
                    }
                    // Cleared rather than taken: the buffer keeps its capacity
                    // for the next carry-over instead of being reallocated once
                    // per record, which is what a `mem::take` here cost.
                    self.pending.clear();
                    bytes
                };
                consumed += take;
                output.push(self.record(bytes, Vec::new().into(), stream, acquisition_id, false));
                self.fragmented = true;
            }
            let Some(index) = terminator else {
                self.pending.extend_from_slice(&rest[consumed..run]);
                break;
            };
            // The terminator itself never triggers a fragment: the original
            // checked the length only on a byte that was not one.
            let tail = &rest[consumed..run];
            let carried = !self.pending.is_empty();
            let last = tail
                .last()
                .copied()
                .or_else(|| self.pending.last().copied());
            let delimiter: &[u8] = if last == Some(b'\r') { b"\r\n" } else { b"\n" };
            let body_length = self.pending.len() + tail.len() + 1 - delimiter.len();
            let bytes = if carried {
                self.pending.extend_from_slice(tail);
                let bytes = self.pending[..body_length].to_vec().into();
                #[cfg(test)]
                {
                    self.backing_allocations += 1;
                }
                self.pending.clear();
                bytes
            } else {
                crate::RecordBytes::from_shared(
                    std::sync::Arc::clone(&backing),
                    start + consumed..start + consumed + body_length,
                )
            };
            let delimiter = if delimiter.len() == 2 && tail.is_empty() {
                // CR and LF landed in different reads, so no one read backing
                // contains the delimiter as a contiguous range.
                #[cfg(test)]
                {
                    self.backing_allocations += 1;
                }
                b"\r\n".as_slice().into()
            } else {
                crate::RecordBytes::from_shared(
                    std::sync::Arc::clone(&backing),
                    start + index + 1 - delimiter.len()..start + index + 1,
                )
            };
            output.push(self.record(bytes, delimiter, stream, acquisition_id, true));
            self.fragmented = false;
            start += index + 1;
        }
        self.reader_cpu_pending = self.reader_cpu_pending.saturating_add(cpu.elapsed_nanos());
        if let Some(first) = output.first_mut() {
            first.reader_cpu_nanos = std::mem::take(&mut self.reader_cpu_pending);
        }
        output
    }
    pub(crate) fn finish(
        &mut self,
        stream: StreamKind,
        acquisition_id: Uuid,
    ) -> Vec<CapturedRecord> {
        let cpu = crate::ThreadCpu::start();
        if self.pending.is_empty() {
            if self.fragmented {
                self.fragmented = false;
                let mut records = vec![CapturedRecord {
                    captured_at_unix_nanos: now(),
                    stream,
                    bytes: Vec::new().into(),
                    delimiter: Vec::new().into(),
                    acquisition_id,
                    chunk: ChunkPosition::End,
                    reader_cpu_nanos: 0,
                }];
                self.reader_cpu_pending =
                    self.reader_cpu_pending.saturating_add(cpu.elapsed_nanos());
                records[0].reader_cpu_nanos = std::mem::take(&mut self.reader_cpu_pending);
                return records;
            }
            return Vec::new();
        }
        let bytes = std::mem::take(&mut self.pending);
        #[cfg(test)]
        {
            self.backing_allocations += 1;
        }
        let mut record = self.record(
            bytes.into(),
            Vec::new().into(),
            stream,
            acquisition_id,
            true,
        );
        self.reader_cpu_pending = self.reader_cpu_pending.saturating_add(cpu.elapsed_nanos());
        record.reader_cpu_nanos = std::mem::take(&mut self.reader_cpu_pending);
        self.fragmented = false;
        vec![record]
    }

    fn flush_partial(&mut self, stream: StreamKind, acquisition_id: Uuid) -> Vec<CapturedRecord> {
        let cpu = crate::ThreadCpu::start();
        let keep = usize::from(self.pending.last() == Some(&b'\r'));
        let emit_length = self.pending.len().saturating_sub(keep);
        if emit_length == 0 {
            return Vec::new();
        }
        let bytes: Vec<u8> = self.pending.drain(..emit_length).collect();
        #[cfg(test)]
        {
            self.backing_allocations += 1;
        }
        let record = self.record(
            bytes.into(),
            Vec::new().into(),
            stream,
            acquisition_id,
            false,
        );
        self.fragmented = true;
        let mut records = vec![record];
        self.reader_cpu_pending = self.reader_cpu_pending.saturating_add(cpu.elapsed_nanos());
        records[0].reader_cpu_nanos = std::mem::take(&mut self.reader_cpu_pending);
        records
    }

    pub(crate) fn buffered_len(&self) -> usize {
        self.pending.len()
    }

    #[cfg(test)]
    fn backing_allocations(&self) -> u64 {
        self.backing_allocations
    }
    fn record(
        &self,
        bytes: crate::RecordBytes,
        delimiter: crate::RecordBytes,
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
            reader_cpu_nanos: 0,
        }
    }
}

/// The framing rule, pinned against the implementation it replaced.
///
/// `Framer::push` used to walk a read a byte at a time. It now scans for
/// terminators and copies whole lines, which is a different shape for the same
/// rule — so the rule is asserted rather than assumed: the reference below is
/// the original loop, and the two must agree record for record over reads split
/// at every awkward place.
#[cfg(test)]
mod framing_equivalence {
    use super::*;

    #[test]
    fn complete_records_retain_one_read_backing() {
        let mut framer = Framer::new(64 * 1024);
        let records = framer.push(b"alpha\r\nbeta\ngamma\n", StreamKind::File, Uuid::nil());

        assert_eq!(records.len(), 3);
        assert!(records[0].bytes.shares_backing_with(&records[0].delimiter));
        assert!(records[0].bytes.shares_backing_with(&records[1].bytes));
        assert!(records[1].delimiter.shares_backing_with(&records[2].bytes));
        assert_eq!(&*records[0].bytes, b"alpha");
        assert_eq!(&*records[0].delimiter, b"\r\n");
        assert_eq!(&*records[1].bytes, b"beta");
        assert_eq!(&*records[2].bytes, b"gamma");
    }

    #[test]
    fn retained_record_survives_siblings_and_later_reads_byte_identically() {
        use crate::{Journal, SourceId};

        let mut framer = Framer::new(64 * 1024);
        let acquisition = Uuid::new_v4();
        let first_read = b"keep-this\r\ndrop-one\ndrop-two\n";
        let mut siblings = framer.push(first_read, StreamKind::File, acquisition);
        let retained = siblings.remove(0);
        drop(siblings);

        let later = framer.push(b"a-later-read\n", StreamKind::File, acquisition);
        drop(later);
        assert_eq!(&*retained.bytes, b"keep-this");
        assert_eq!(&*retained.delimiter, b"\r\n");
        assert_eq!(retained.bytes.retained_payload_bytes(), first_read.len());

        let directory = tempfile::tempdir().expect("temporary journal root");
        let source_id = SourceId::new();
        let (mut journal, _) =
            Journal::open(directory.path().join("capture.journal"), source_id).expect("journal");
        let expected = retained.clone().into_raw(source_id);
        journal
            .append(retained.into_raw(source_id))
            .expect("append");
        journal.flush().expect("flush");
        let actual = journal
            .read_page(0, 1, 1024)
            .expect("read page")
            .records
            .remove(0);

        assert_eq!(actual, expected);
        assert_eq!(actual.bytes.retained_payload_bytes(), 58 + 9 + 2);
    }

    /// The original, byte at a time.
    fn reference(
        pending: &mut Vec<u8>,
        fragmented: &mut bool,
        maximum: usize,
        input: &[u8],
    ) -> Vec<(Vec<u8>, Vec<u8>, bool)> {
        let mut output = Vec::new();
        for &byte in input {
            pending.push(byte);
            if byte == b'\n' {
                let delimiter = if pending.len() >= 2 && pending[pending.len() - 2] == b'\r' {
                    vec![b'\r', b'\n']
                } else {
                    vec![b'\n']
                };
                let body_length = pending.len() - delimiter.len();
                let bytes: Vec<u8> = pending.drain(..body_length).collect();
                pending.clear();
                output.push((bytes, delimiter, true));
                *fragmented = false;
            } else if pending.len() > maximum {
                let bytes: Vec<u8> = pending.drain(..maximum).collect();
                output.push((bytes, Vec::new(), false));
                *fragmented = true;
            }
        }
        output
    }

    fn observed(framer: &mut Framer, input: &[u8]) -> Vec<(Vec<u8>, Vec<u8>, bool)> {
        framer
            .push(input, StreamKind::File, Uuid::nil())
            .into_iter()
            .map(|record| {
                (
                    record.bytes.to_vec(),
                    record.delimiter.to_vec(),
                    record.chunk == ChunkPosition::Complete || record.chunk == ChunkPosition::End,
                )
            })
            .collect()
    }

    #[test]
    fn scanning_frames_exactly_as_the_byte_loop_did() {
        // Terminators at the start, at the end and nowhere; bare `\r`; `\r\n`
        // split across reads; runs longer than the maximum; empty lines.
        let corpus: &[&[u8]] = &[
            b"one\ntwo\nthree\n",
            b"no terminator at all",
            b"\n\n\n",
            b"crlf\r\nmixed\nlast\r\n",
            b"trailing cr\r",
            b"\rleading cr\n",
            b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            b"exceeds\nthe\nmaximum\naaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\nend\n",
            b"",
        ];
        for maximum in [1_usize, 2, 3, 7, 8, 64] {
            for split in [1_usize, 2, 3, 5, 16, usize::MAX] {
                let mut reference_pending = Vec::new();
                let mut reference_fragmented = false;
                let mut reference_output = Vec::new();
                let mut framer = Framer::new(maximum);
                let mut observed_output = Vec::new();
                for piece in corpus {
                    for window in piece.chunks(split.min(piece.len().max(1))) {
                        reference_output.extend(reference(
                            &mut reference_pending,
                            &mut reference_fragmented,
                            maximum,
                            window,
                        ));
                        observed_output.extend(observed(&mut framer, window));
                    }
                }
                assert_eq!(
                    observed_output, reference_output,
                    "maximum {maximum}, reads split every {split}"
                );
                assert_eq!(
                    framer.pending, reference_pending,
                    "leftover differs at maximum {maximum}, split {split}"
                );
                assert_eq!(
                    framer.fragmented, reference_fragmented,
                    "fragment flag differs at maximum {maximum}, split {split}"
                );
            }
        }
    }

    /// The same, over pseudo-random bytes rather than hand-picked ones, so a
    /// case nobody thought of still has to agree.
    #[test]
    fn scanning_agrees_with_the_byte_loop_on_random_reads() {
        let mut state = 0x5eed_1234_u64;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for maximum in [1_usize, 4, 16] {
            let mut input = Vec::new();
            for _ in 0..20_000 {
                // Terminators and carriage returns over-represented on purpose.
                input.push(match next() % 8 {
                    0 | 1 => b'\n',
                    2 => b'\r',
                    value => b'a' + (value as u8),
                });
            }
            let mut reference_pending = Vec::new();
            let mut reference_fragmented = false;
            let mut reference_output = Vec::new();
            let mut framer = Framer::new(maximum);
            let mut observed_output = Vec::new();
            let mut offset = 0;
            while offset < input.len() {
                let take = ((next() % 37) as usize + 1).min(input.len() - offset);
                let window = &input[offset..offset + take];
                reference_output.extend(reference(
                    &mut reference_pending,
                    &mut reference_fragmented,
                    maximum,
                    window,
                ));
                observed_output.extend(observed(&mut framer, window));
                offset += take;
            }
            assert_eq!(observed_output, reference_output, "maximum {maximum}");
            assert_eq!(framer.pending, reference_pending, "maximum {maximum}");
            assert_eq!(framer.fragmented, reference_fragmented, "maximum {maximum}");
        }
    }
}

/// What framing costs on its own, with no channel, no journal and no
/// durability under it.
///
/// The pipeline phase table can only say what framing, the hand-over and the
/// journal cost *together*; this separates the first of the three. It reports
/// bytes per CPU-second rather than a wall time, for the reason every other
/// measurement here does: wall clock on a shared build box measures the
/// neighbours.
#[cfg(test)]
mod framing_throughput {
    use super::*;

    /// Lines in the shape `tests/soak/generate.py` writes: mixed lengths, one
    /// long record, so the framer sees realistic work rather than one uniform
    /// size that would flatter it.
    fn fixture(target_bytes: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(target_bytes + 4096);
        let mut index = 0_u64;
        while out.len() < target_bytes {
            let line = match index % 3 {
                0 => format!(
                    "{{\"level\":\"INFO\",\"service\":\"api\",\"seq\":{index},\"request_id\":\"req-{:05}\",\"duration_ms\":1234,\"message\":\"handled request\"}}",
                    index % 5000
                ),
                1 => format!(
                    "INFO service=worker seq={index} request_id=req-{:05} duration_ms=91 message=handled request",
                    index % 5000
                ),
                _ => format!("WARN service=ingest seq={index} message=short"),
            };
            out.extend_from_slice(line.as_bytes());
            out.push(b'\n');
            index += 1;
        }
        out
    }

    fn cpu_seconds() -> f64 {
        let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
        // SAFETY: getrusage writes a whole `rusage` through this pointer and
        // reports failure through its return value.
        if unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) } != 0 {
            return 0.0;
        }
        // SAFETY: getrusage returned success, so the value is initialised.
        let usage = unsafe { usage.assume_init() };
        let seconds = |value: libc::timeval| {
            value.tv_sec.max(0) as f64 + (value.tv_usec.max(0) as f64) / 1_000_000.0
        };
        seconds(usage.ru_utime) + seconds(usage.ru_stime)
    }

    /// The journal's share: encode each framed record and append it, with no
    /// durability, no channel and no acquisition under it.
    #[test]
    fn journal_append_reports_its_bytes_per_cpu_second() {
        use crate::{Journal, RawRecord, SourceId};
        let bytes = fixture(32 * 1024 * 1024);
        let acquisition = Uuid::new_v4();
        let mut framer = Framer::new(64 * 1024);
        let mut framed = Vec::new();
        for window in bytes.chunks(256 * 1024) {
            framed.extend(framer.push(window, StreamKind::File, acquisition));
        }
        framed.extend(framer.finish(StreamKind::File, acquisition));

        let directory = tempfile::tempdir().expect("temporary journal root");
        let source_id = SourceId::new();
        let (mut journal, _) =
            Journal::open(directory.path().join("capture.journal"), source_id).expect("journal");
        let records: Vec<RawRecord> = framed
            .into_iter()
            .map(|record| record.into_raw(source_id))
            .collect();
        let count = records.len() as u64;
        let started = cpu_seconds();
        for record in records {
            journal.append(record).expect("append");
        }
        journal.flush().expect("flush");
        let cpu = cpu_seconds() - started;
        println!(
            "journal append: {:.1} MB, {count} records, {:.2} CPU-s, {:.1} MB per CPU-second",
            bytes.len() as f64 / 1_048_576.0,
            cpu,
            (bytes.len() as f64 / 1_048_576.0) / cpu.max(f64::EPSILON),
        );
        assert!(count > 0);
    }

    /// What it costs to turn framed records into the form the writer receives,
    /// including how many payload backing allocations framing retained.
    #[test]
    fn record_ownership_reports_its_bytes_per_cpu_second() {
        use crate::SourceId;
        let bytes = fixture(32 * 1024 * 1024);
        let acquisition = Uuid::new_v4();
        let mut framer = Framer::new(64 * 1024);
        let mut framed = Vec::new();
        for window in bytes.chunks(256 * 1024) {
            framed.extend(framer.push(window, StreamKind::File, acquisition));
        }
        let backing_allocations = framer.backing_allocations();
        let source_id = SourceId::new();
        let count = framed.len() as u64;
        let started = cpu_seconds();
        let owned: Vec<_> = framed
            .into_iter()
            .map(|record| record.into_raw(source_id))
            .collect();
        let cpu = cpu_seconds() - started;
        println!(
            "into_raw: {:.1} MB, {count} records, {backing_allocations} payload backing allocations ({:.1}/MB), {:.2} CPU-s, {:.1} MB per CPU-second",
            bytes.len() as f64 / 1_048_576.0,
            backing_allocations as f64 / (bytes.len() as f64 / 1_048_576.0),
            cpu,
            (bytes.len() as f64 / 1_048_576.0) / cpu.max(f64::EPSILON),
        );
        assert_eq!(owned.len() as u64, count);
        assert!(
            backing_allocations * 64 < count,
            "{backing_allocations} backing allocations for {count} records lost read sharing"
        );
    }

    /// The hand-over's share: every record crosses two bounded channels and
    /// takes a semaphore permit on the way — acquisition to supervisor, then
    /// supervisor to writer. This reproduces that shape with the same record
    /// count and nothing else in it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn channel_handover_reports_its_bytes_per_cpu_second() {
        use std::sync::Arc;
        use tokio::sync::Semaphore;

        let bytes = fixture(32 * 1024 * 1024);
        let acquisition = Uuid::new_v4();
        let mut framer = Framer::new(64 * 1024);
        let mut framed = Vec::new();
        for window in bytes.chunks(256 * 1024) {
            framed.extend(framer.push(window, StreamKind::File, acquisition));
        }
        let count = framed.len() as u64;

        let slots = Arc::new(Semaphore::new(128));
        let (first_tx, mut first_rx) = mpsc::channel::<CapturedRecord>(128);
        let (second_tx, mut second_rx) = mpsc::channel::<CapturedRecord>(128);
        let started = cpu_seconds();
        let producer = tokio::spawn(async move {
            for record in framed {
                if first_tx.send(record).await.is_err() {
                    break;
                }
            }
        });
        let supervisor = tokio::spawn(async move {
            while let Some(record) = first_rx.recv().await {
                let permit = Arc::clone(&slots).acquire_owned().await.expect("permit");
                if second_tx.send(record).await.is_err() {
                    break;
                }
                drop(permit);
            }
        });
        let mut seen = 0_u64;
        while second_rx.recv().await.is_some() {
            seen += 1;
        }
        producer.await.expect("producer");
        supervisor.await.expect("supervisor");
        let cpu = cpu_seconds() - started;
        println!(
            "handover: {:.1} MB, {seen} records, {:.2} CPU-s, {:.1} MB per CPU-second",
            bytes.len() as f64 / 1_048_576.0,
            cpu,
            (bytes.len() as f64 / 1_048_576.0) / cpu.max(f64::EPSILON),
        );
        assert_eq!(seen, count);
    }

    /// `cargo test -p lvu-core framing_throughput -- --nocapture`
    #[test]
    fn framing_reports_its_bytes_per_cpu_second() {
        let bytes = fixture(32 * 1024 * 1024);
        let chunk = 256 * 1024;
        let acquisition = Uuid::new_v4();
        let mut framer = Framer::new(64 * 1024);
        let started = cpu_seconds();
        let mut records = 0_u64;
        for window in bytes.chunks(chunk) {
            records += framer.push(window, StreamKind::File, acquisition).len() as u64;
        }
        records += framer.finish(StreamKind::File, acquisition).len() as u64;
        let cpu = cpu_seconds() - started;
        println!(
            "framing: {:.1} MB, {records} records, {:.2} CPU-s, {:.1} MB per CPU-second",
            bytes.len() as f64 / 1_048_576.0,
            cpu,
            (bytes.len() as f64 / 1_048_576.0) / cpu.max(f64::EPSILON),
        );
        assert!(records > 0);
    }
}
