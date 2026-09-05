use crate::{COMPATIBILITY_ID, CompiledDefinition, ExpressionKind, PYTHON_POLARS_VERSION};
use serde::{Deserialize, Serialize};
use std::{
    collections::VecDeque,
    io::{BufRead, BufReader, Read, Write},
    os::unix::process::CommandExt,
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use thiserror::Error;
const SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug)]
pub struct CompilerHostConfig {
    pub executable: String,
    pub args: Vec<String>,
    pub request_limit: usize,
    pub output_limit: usize,
    pub stderr_limit: usize,
    pub timeout: Duration,
}
impl CompilerHostConfig {
    pub fn python_module(executable: impl Into<String>, module: impl Into<String>) -> Self {
        Self {
            executable: executable.into(),
            args: vec!["-m".into(), module.into()],
            request_limit: 64 * 1024,
            output_limit: 384 * 1024,
            stderr_limit: 32 * 1024,
            timeout: Duration::from_secs(3),
        }
    }
}
#[derive(Debug, Error)]
pub enum HostError {
    #[error("compiler host I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("compiler request exceeds configured limit")]
    RequestTooLarge,
    #[error("compiler timed out")]
    Timeout,
    #[error("compiler request cancelled")]
    Cancelled,
    #[error("compiler exited or closed stdout")]
    Exited,
    #[error("compiler response exceeds configured limit")]
    OutputTooLarge,
    #[error("malformed compiler response: {0}")]
    Malformed(String),
    #[error("stale/mismatched compiler response")]
    MismatchedResponse,
    #[error("compiler rejected expression ({code}): {message}")]
    Rejected { code: String, message: String },
    #[error("incompatible compiler metadata: {0}")]
    Incompatible(String),
    #[error("native expression validation failed: {0}")]
    NativeValidation(String),
}
#[derive(Serialize)]
struct CompileRequest<'a> {
    schema_version: u32,
    request_id: &'a str,
    operation: &'static str,
    kind: ExpressionKind,
    expression: &'a str,
}
#[derive(Deserialize)]
struct CompileResponse {
    schema_version: u32,
    request_id: Option<String>,
    ok: bool,
    expression_json: Option<String>,
    python_polars_version: Option<String>,
    compatibility_id: Option<String>,
    error: Option<ResponseError>,
}
#[derive(Deserialize)]
struct ResponseError {
    code: String,
    message: String,
}
enum ReaderMessage {
    Line(Vec<u8>),
    Oversize,
    Eof,
    Error(String),
}
struct WriteRequest {
    bytes: Vec<u8>,
    ack: mpsc::SyncSender<Result<(), String>>,
}
struct RunningChild {
    child: Child,
    pgid: i32,
    writer: mpsc::SyncSender<WriteRequest>,
    rx: mpsc::Receiver<ReaderMessage>,
    stderr: Arc<Mutex<VecDeque<u8>>>,
    threads: Vec<JoinHandle<()>>,
}
pub struct CompilerHost {
    config: CompilerHostConfig,
    child: Option<RunningChild>,
    next_id: AtomicU64,
    last_stderr: Vec<u8>,
}
impl CompilerHost {
    pub fn new(config: CompilerHostConfig) -> Self {
        Self {
            config,
            child: None,
            next_id: AtomicU64::new(1),
            last_stderr: Vec::new(),
        }
    }
    pub fn compile(
        &mut self,
        source: &str,
        kind: ExpressionKind,
        cancelled: &AtomicBool,
    ) -> Result<CompiledDefinition, HostError> {
        if cancelled.load(Ordering::Acquire) {
            return Err(HostError::Cancelled);
        }
        let request_id = format!("compile-{}", self.next_id.fetch_add(1, Ordering::Relaxed));
        let mut bytes = serde_json::to_vec(&CompileRequest {
            schema_version: SCHEMA_VERSION,
            request_id: &request_id,
            operation: "compile",
            kind,
            expression: source,
        })
        .map_err(|e| HostError::Malformed(e.to_string()))?;
        bytes.push(b'\n');
        if bytes.len() > self.config.request_limit {
            return Err(HostError::RequestTooLarge);
        }
        let deadline = Instant::now() + self.config.timeout;
        self.ensure_child()?;
        if Instant::now() >= deadline {
            self.restart();
            return Err(HostError::Timeout);
        }
        let (ack_tx, ack_rx) = mpsc::sync_channel(1);
        if self
            .child
            .as_ref()
            .expect("child ensured")
            .writer
            .send(WriteRequest { bytes, ack: ack_tx })
            .is_err()
        {
            self.restart();
            return Err(HostError::Exited);
        }
        let mut wrote = false;
        loop {
            if cancelled.load(Ordering::Acquire) {
                self.restart();
                return Err(HostError::Cancelled);
            }
            if Instant::now() >= deadline {
                self.restart();
                return Err(HostError::Timeout);
            }
            if !wrote {
                match ack_rx.try_recv() {
                    Ok(Ok(())) => wrote = true,
                    Ok(Err(e)) => {
                        self.restart();
                        return Err(HostError::Malformed(e));
                    }
                    Err(mpsc::TryRecvError::Disconnected) => {
                        self.restart();
                        return Err(HostError::Exited);
                    }
                    Err(mpsc::TryRecvError::Empty) => {}
                }
            }
            match self
                .child
                .as_ref()
                .expect("child ensured")
                .rx
                .recv_timeout(Duration::from_millis(5))
            {
                Ok(ReaderMessage::Line(line)) => {
                    return self.parse_response(source, kind, &request_id, &line);
                }
                Ok(ReaderMessage::Oversize) => {
                    self.restart();
                    return Err(HostError::OutputTooLarge);
                }
                Ok(ReaderMessage::Eof) => {
                    self.restart();
                    return Err(HostError::Exited);
                }
                Ok(ReaderMessage::Error(e)) => {
                    self.restart();
                    return Err(HostError::Malformed(e));
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    self.restart();
                    return Err(HostError::Exited);
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
        }
    }
    fn parse_response(
        &mut self,
        source: &str,
        kind: ExpressionKind,
        request_id: &str,
        line: &[u8],
    ) -> Result<CompiledDefinition, HostError> {
        let response: CompileResponse = match serde_json::from_slice(line) {
            Ok(v) => v,
            Err(e) => return self.poison(HostError::Malformed(e.to_string())),
        };
        if response.schema_version != SCHEMA_VERSION
            || response.request_id.as_deref() != Some(request_id)
        {
            return self.poison(HostError::MismatchedResponse);
        }
        if !response.ok {
            if response.expression_json.is_some()
                || response.python_polars_version.is_some()
                || response.compatibility_id.is_some()
            {
                return self.poison(HostError::Malformed(
                    "error response contains success payload".into(),
                ));
            }
            return match response.error {
                Some(e) => Err(HostError::Rejected {
                    code: e.code,
                    message: e.message,
                }),
                None => self.poison(HostError::Malformed(
                    "error response omitted error body".into(),
                )),
            };
        }
        if response.error.is_some() {
            return self.poison(HostError::Malformed(
                "successful response contains error body".into(),
            ));
        }
        if response.python_polars_version.as_deref() != Some(PYTHON_POLARS_VERSION)
            || response.compatibility_id.as_deref() != Some(COMPATIBILITY_ID)
        {
            return self.poison(HostError::Incompatible(
                "unexpected Python Polars version or compatibility ID".into(),
            ));
        }
        let json = match response.expression_json {
            Some(v) => v,
            None => {
                return self.poison(HostError::Malformed(
                    "successful response omitted expression_json".into(),
                ));
            }
        };
        match CompiledDefinition::compile(source.into(), &json, kind) {
            Ok(v) => Ok(v),
            Err(e) => self.poison(HostError::NativeValidation(e.to_string())),
        }
    }
    fn poison<T>(&mut self, error: HostError) -> Result<T, HostError> {
        self.restart();
        Err(error)
    }
    pub fn stderr_snapshot(&self) -> Vec<u8> {
        self.child
            .as_ref()
            .map(|c| {
                c.stderr
                    .lock()
                    .expect("stderr mutex poisoned")
                    .iter()
                    .copied()
                    .collect()
            })
            .unwrap_or_else(|| self.last_stderr.clone())
    }
    fn ensure_child(&mut self) -> Result<(), HostError> {
        if self
            .child
            .as_mut()
            .is_some_and(|r| r.child.try_wait().ok().flatten().is_some())
        {
            self.restart()
        };
        if self.child.is_some() {
            return Ok(());
        }
        let mut command = Command::new(&self.config.executable);
        command
            .args(&self.config.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        unsafe {
            command.pre_exec(|| {
                if libc::setpgid(0, 0) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = command.spawn()?;
        let pgid = child.id() as i32;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| std::io::Error::other("compiler stdin unavailable"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| std::io::Error::other("compiler stdout unavailable"))?;
        let stderr_pipe = child
            .stderr
            .take()
            .ok_or_else(|| std::io::Error::other("compiler stderr unavailable"))?;
        let (writer_tx, writer_rx) = mpsc::sync_channel::<WriteRequest>(1);
        let writer_thread = thread::spawn(move || writer_loop(stdin, writer_rx));
        let (tx, rx) = mpsc::sync_channel(1);
        let limit = self.config.output_limit;
        let stdout_thread = thread::spawn(move || read_stdout(stdout, limit, tx));
        let stderr = Arc::new(Mutex::new(VecDeque::new()));
        let copy = Arc::clone(&stderr);
        let stderr_limit = self.config.stderr_limit;
        let stderr_thread = thread::spawn(move || drain_stderr(stderr_pipe, copy, stderr_limit));
        self.child = Some(RunningChild {
            child,
            pgid,
            writer: writer_tx,
            rx,
            stderr,
            threads: vec![writer_thread, stdout_thread, stderr_thread],
        });
        Ok(())
    }
    fn restart(&mut self) {
        if let Some(running) = self.child.take() {
            let RunningChild {
                mut child,
                pgid,
                writer,
                rx,
                stderr,
                threads,
            } = running;
            unsafe {
                libc::kill(-pgid, libc::SIGKILL);
            }
            _ = child.wait();
            drop(writer);
            drop(rx);
            for thread in threads {
                _ = thread.join();
            }
            self.last_stderr = stderr
                .lock()
                .expect("stderr mutex poisoned")
                .iter()
                .copied()
                .collect();
        }
    }
}
impl Drop for CompilerHost {
    fn drop(&mut self) {
        self.restart()
    }
}
fn writer_loop(mut stdin: impl Write, rx: mpsc::Receiver<WriteRequest>) {
    for request in rx {
        let result = stdin
            .write_all(&request.bytes)
            .and_then(|_| stdin.flush())
            .map_err(|e| e.to_string());
        _ = request.ack.send(result);
    }
}
fn read_stdout(stdout: impl Read, limit: usize, tx: mpsc::SyncSender<ReaderMessage>) {
    let mut reader = BufReader::new(stdout);
    loop {
        let mut line = Vec::new();
        loop {
            let available = match reader.fill_buf() {
                Ok(v) => v,
                Err(e) => {
                    _ = tx.send(ReaderMessage::Error(e.to_string()));
                    return;
                }
            };
            if available.is_empty() {
                _ = tx.send(if line.is_empty() {
                    ReaderMessage::Eof
                } else {
                    ReaderMessage::Line(line)
                });
                return;
            }
            let newline = available.iter().position(|b| *b == b'\n');
            let consumed = newline.map_or(available.len(), |p| p + 1);
            if line.len().saturating_add(consumed) > limit {
                _ = tx.send(ReaderMessage::Oversize);
                return;
            }
            line.extend_from_slice(&available[..consumed]);
            reader.consume(consumed);
            if newline.is_some() {
                line.pop();
                if tx.send(ReaderMessage::Line(line)).is_err() {
                    return;
                }
                break;
            }
        }
    }
}
fn drain_stderr(mut input: impl Read, target: Arc<Mutex<VecDeque<u8>>>, limit: usize) {
    let mut chunk = [0u8; 4096];
    while let Ok(count) = input.read(&mut chunk) {
        if count == 0 {
            break;
        }
        let mut bytes = target.lock().expect("stderr mutex poisoned");
        bytes.extend(&chunk[..count]);
        while bytes.len() > limit {
            bytes.pop_front();
        }
    }
}
