use crate::{
    CommandDefinition, CommandProgram, RawRecord, RecordId, RestartPolicy, SourceId, StreamKind,
};
use crc32fast::Hasher;
use std::{
    io,
    path::PathBuf,
    process::ExitStatus,
    time::{Duration, SystemTime, UNIX_EPOCH},
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
}
impl Default for CaptureLimits {
    fn default() -> Self {
        Self {
            channel_capacity: 128,
            read_chunk_bytes: 16 * 1024,
            maximum_record_bytes: 64 * 1024,
            poll_interval: Duration::from_millis(50),
            partial_flush_interval: Duration::from_millis(100),
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

pub fn capture_command(
    definition: CommandDefinition,
    limits: CaptureLimits,
) -> io::Result<(CaptureHandle, mpsc::Receiver<CaptureEvent>)> {
    validate(&limits)?;
    if definition.restart != RestartPolicy::Never {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "command restart policies are not implemented",
        ));
    }
    let mut command = build_command(definition);
    configure_owned_process(&mut command);
    let child = command.spawn()?;
    let (events, receiver) = mpsc::channel(limits.channel_capacity);
    let (abort, aborted) = watch::channel(false);
    let (stop, stopped) = watch::channel(false);
    let task = tokio::spawn(run_command(child, events, aborted, stopped, limits));
    Ok((
        CaptureHandle {
            abort,
            stop,
            task: Some(task),
        },
        receiver,
    ))
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

async fn run_command(
    mut child: Child,
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
        let _ = terminate_child_tree(&mut child).await;
        return CaptureCompletion {
            aborted: true,
            discarded_buffered_bytes: 0,
        };
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
    let outcome = tokio::select! {
        biased;
        changed = cancelled.changed() => {
            let _ = changed;
            terminate_child_tree(&mut child).await
        }
        changed = stopped.changed() => {
            let _ = changed;
            terminate_child_tree(&mut child).await
        }
        _ = events.closed() => terminate_child_tree(&mut child).await,
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
        return CaptureCompletion {
            aborted: true,
            discarded_buffered_bytes: discarded,
        };
    }
    let discarded = join_reader(stdout_task).await + join_reader(stderr_task).await;
    match outcome {
        Ok(status) => {
            let _ = emit(
                &events,
                &mut cancelled,
                CaptureEvent::CommandExit {
                    acquisition_id,
                    status,
                },
            )
            .await;
        }
        Err(error) => {
            let _ = emit(
                &events,
                &mut cancelled,
                capture_error(acquisition_id, error),
            )
            .await;
        }
    }
    CaptureCompletion {
        aborted: false,
        discarded_buffered_bytes: discarded,
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
    resume: Option<FileResumeCursor>,
    events: mpsc::Sender<CaptureEvent>,
    mut cancelled: watch::Receiver<bool>,
    mut stopped: watch::Receiver<bool>,
) -> CaptureCompletion {
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

async fn emit_records(
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

async fn emit(
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

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_nanos().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}
fn capture_error(acquisition_id: Uuid, error: impl std::fmt::Display) -> CaptureEvent {
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
        },
    )
    .await
}

struct Framer {
    pending: Vec<u8>,
    fragmented: bool,
    maximum: usize,
}
impl Framer {
    fn new(maximum: usize) -> Self {
        Self {
            pending: Vec::with_capacity(maximum.min(8192)),
            fragmented: false,
            maximum,
        }
    }
    fn push(
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
    fn finish(&mut self, stream: StreamKind, acquisition_id: Uuid) -> Vec<CapturedRecord> {
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

    fn buffered_len(&self) -> usize {
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
