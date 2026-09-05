use crate::{
    CommandDefinition, CommandProgram, RawRecord, RecordId, RestartPolicy, SourceId, StreamKind,
};
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
}
impl Default for CaptureLimits {
    fn default() -> Self {
        Self {
            channel_capacity: 128,
            read_chunk_bytes: 16 * 1024,
            maximum_record_bytes: 64 * 1024,
            poll_interval: Duration::from_millis(50),
        }
    }
}

pub struct CaptureHandle {
    cancel: watch::Sender<bool>,
    task: Option<JoinHandle<()>>,
}
impl CaptureHandle {
    pub fn cancel(&mut self) {
        let _ = self.cancel.send(true);
    }
    pub async fn wait(mut self) -> Result<(), tokio::task::JoinError> {
        self.task.take().expect("capture task exists").await
    }
}
impl Drop for CaptureHandle {
    fn drop(&mut self) {
        let _ = self.cancel.send(true);
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
    let (cancel, cancelled) = watch::channel(false);
    let task = tokio::spawn(run_command(child, events, cancelled, limits));
    Ok((
        CaptureHandle {
            cancel,
            task: Some(task),
        },
        receiver,
    ))
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
    limits: CaptureLimits,
) {
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
        return;
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
        _ = events.closed() => terminate_child_tree(&mut child).await,
        status = child.wait() => {
            kill_process_group(process_id);
            status
        }
    };
    if *cancelled.borrow() {
        abort_reader(stdout_task).await;
        abort_reader(stderr_task).await;
        if let Ok(status) = outcome {
            let _ = events.try_send(CaptureEvent::CommandExit {
                acquisition_id,
                status,
            });
        }
        let _ = events.try_send(CaptureEvent::Stopped { acquisition_id });
        return;
    }
    join_reader(stdout_task).await;
    join_reader(stderr_task).await;
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
}

async fn abort_reader(task: Option<JoinHandle<()>>) {
    if let Some(task) = task {
        task.abort();
        let _ = task.await;
    }
}
async fn join_reader(task: Option<JoinHandle<()>>) {
    if let Some(task) = task {
        let _ = task.await;
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
) {
    let mut framer = Framer::new(limits.maximum_record_bytes);
    let mut buffer = vec![0; limits.read_chunk_bytes];
    loop {
        let read = tokio::select! { biased; _ = cancelled.changed() => return, value = reader.read(&mut buffer) => value };
        match read {
            Ok(0) => {
                emit_records(
                    &events,
                    &mut cancelled,
                    framer.finish(stream, acquisition_id),
                )
                .await;
                return;
            }
            Ok(count) => {
                if !emit_records(
                    &events,
                    &mut cancelled,
                    framer.push(&buffer[..count], stream, acquisition_id),
                )
                .await
                {
                    return;
                }
            }
            Err(error) => {
                let _ = emit(
                    &events,
                    &mut cancelled,
                    capture_error(acquisition_id, error),
                )
                .await;
                return;
            }
        }
    }
}

pub fn capture_file(
    path: PathBuf,
    follow: bool,
    limits: CaptureLimits,
) -> io::Result<(CaptureHandle, mpsc::Receiver<CaptureEvent>)> {
    validate(&limits)?;
    let (events, receiver) = mpsc::channel(limits.channel_capacity);
    let (cancel, cancelled) = watch::channel(false);
    let task = tokio::spawn(run_file(path, follow, limits, events, cancelled));
    Ok((
        CaptureHandle {
            cancel,
            task: Some(task),
        },
        receiver,
    ))
}

struct FileState {
    file: File,
    identity: Option<(u64, u64)>,
    offset: u64,
    acquisition_id: Uuid,
    framer: Framer,
}

async fn run_file(
    path: PathBuf,
    follow: bool,
    limits: CaptureLimits,
    events: mpsc::Sender<CaptureEvent>,
    mut cancelled: watch::Receiver<bool>,
) {
    let mut state = match open_file_state(&path, limits.maximum_record_bytes).await {
        Ok(state) => state,
        Err(error) => {
            let _ = events.try_send(capture_error(Uuid::new_v4(), error));
            return;
        }
    };
    if !emit(
        &events,
        &mut cancelled,
        CaptureEvent::Boundary {
            acquisition_id: state.acquisition_id,
            reason: BoundaryReason::Started,
        },
    )
    .await
    {
        return;
    }
    let mut buffer = vec![0; limits.read_chunk_bytes];
    loop {
        let read = tokio::select! {
            biased;
            _ = cancelled.changed() => { finish_file(&mut state, &events, &mut cancelled, true).await; return; },
            _ = events.closed() => return,
            value = state.file.read(&mut buffer) => value,
        };
        match read {
            Ok(count) if count > 0 => {
                state.offset += count as u64;
                let records =
                    state
                        .framer
                        .push(&buffer[..count], StreamKind::File, state.acquisition_id);
                if !emit_records(&events, &mut cancelled, records).await {
                    return;
                }
            }
            Ok(_) if !follow => {
                finish_file(&mut state, &events, &mut cancelled, false).await;
                return;
            }
            Ok(_) => {
                if !wait_poll(limits.poll_interval, &mut cancelled).await {
                    finish_file(&mut state, &events, &mut cancelled, true).await;
                    return;
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
                    return;
                }
            }
            Err(error) => {
                let _ = emit(
                    &events,
                    &mut cancelled,
                    capture_error(state.acquisition_id, error),
                )
                .await;
                return;
            }
        }
    }
}

async fn open_file_state(path: &PathBuf, maximum_record_bytes: usize) -> io::Result<FileState> {
    let file = File::open(path).await?;
    let identity = metadata_identity(&file.metadata().await?);
    Ok(FileState {
        file,
        identity,
        offset: 0,
        acquisition_id: Uuid::new_v4(),
        framer: Framer::new(maximum_record_bytes),
    })
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
        *state = open_file_state(path, limits.maximum_record_bytes).await?;
        let _ = emit(
            events,
            cancelled,
            CaptureEvent::Boundary {
                acquisition_id: state.acquisition_id,
                reason: BoundaryReason::Rotated,
            },
        )
        .await;
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
        state.file.seek(SeekFrom::Start(0)).await?;
        state.offset = 0;
        state.acquisition_id = Uuid::new_v4();
        state.framer = Framer::new(limits.maximum_record_bytes);
        let _ = emit(
            events,
            cancelled,
            CaptureEvent::Boundary {
                acquisition_id: state.acquisition_id,
                reason: BoundaryReason::Truncated,
            },
        )
        .await;
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
    if !*cancelled.borrow() {
        let _ = emit_records(events, cancelled, records).await;
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
fn metadata_identity(metadata: &std::fs::Metadata) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    Some((metadata.dev(), metadata.ino()))
}
#[cfg(not(unix))]
fn metadata_identity(_: &std::fs::Metadata) -> Option<(u64, u64)> {
    None
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
            return Vec::new();
        }
        let bytes = std::mem::take(&mut self.pending);
        let record = self.record(bytes, Vec::new(), stream, acquisition_id, true);
        self.fragmented = false;
        vec![record]
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
