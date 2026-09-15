//! Bounded, UI-independent host for the local TypeScript Paseo bridge.

use std::{
    collections::HashMap,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError},
    },
    thread,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const PROTOCOL_VERSION: u32 = 1;
const DEFAULT_MAX_LINE_BYTES: usize = 262_144;
const DEFAULT_MAX_PENDING: usize = 32;
const DEFAULT_EVENT_CAPACITY: usize = 128;

#[derive(Clone, Debug)]
pub struct AgentBridgeConfig {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub environment: Vec<(String, String)>,
    pub max_line_bytes: usize,
    pub max_pending: usize,
    pub event_capacity: usize,
    pub request_timeout: Duration,
    pub shutdown_timeout: Duration,
}

impl AgentBridgeConfig {
    /// Runs the already-built bridge through the repository's pinned mise Node.
    pub fn mise_bridge(repository: &Path) -> Self {
        Self {
            program: "mise".into(),
            args: vec![
                "exec".into(),
                "node@26.8.1".into(),
                "--".into(),
                "node".into(),
                "dist/cli.js".into(),
            ],
            cwd: repository.join("bridge"),
            environment: Vec::new(),
            max_line_bytes: DEFAULT_MAX_LINE_BYTES,
            max_pending: DEFAULT_MAX_PENDING,
            event_capacity: DEFAULT_EVENT_CAPACITY,
            request_timeout: Duration::from_secs(125),
            shutdown_timeout: Duration::from_secs(2),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ProposalKind {
    Source,
    Sources,
    Filter,
    Enrichment,
    View,
}

/// Bound on one multi-source proposal. This is a per-proposal bound, not proof
/// that admission has room: pending starts and the source cap share slots, so
/// Apply still admits each item against the live limits and reports shortfalls
/// per source. Kept equal to the bridge's MAX_SOURCES_PER_PROPOSAL and to
/// `MAX_PENDING_STARTS` in main.rs, so one Apply can never overflow the start
/// queue by itself.
pub const MAX_SOURCES_PER_PROPOSAL: usize = 8;

/// Managed session intent; omitted intent retains the legacy resumable protocol.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionPurpose {
    Ask,
    SourceAssistance,
    Investigation,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct OriginatingRevision {
    pub data: String,
    pub definition: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProposalContext {
    pub manifest_path: PathBuf,
    pub dataset_paths: Vec<PathBuf>,
    pub inline_context: Option<Value>,
    pub inspection_command: Option<Vec<String>>,
    /// Byte ceiling for `inline_context`, from the sample tier the request was
    /// prepared at (`docs/larger-ask-sample.md`). The wider tier legitimately
    /// exceeds the standard 32 KiB.
    pub inline_context_limit: usize,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct ProposalEnvelope {
    pub kind: ProposalKind,
    pub definition: Value,
    pub explanation: String,
    pub originating_revision: OriginatingRevision,
    /// The agent's own report that the bounded sample it was given was not
    /// enough to answer with. Absent from older bridges, so it defaults false.
    #[serde(default)]
    pub needs_more_data: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BridgeFailure {
    pub code: String,
    pub message: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HostError {
    NotRunning(String),
    Capacity,
    Timeout,
    Io(String),
    Protocol(String),
    Bridge(BridgeFailure),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HostState {
    Starting,
    Running,
    Disconnected,
    Faulted,
    Stopped,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostStatus {
    pub state: HostState,
    pub generation: u64,
    pub pending: usize,
    pub dropped_events: u64,
    pub diagnostic: Option<String>,
    /// The bridge process's own last words. Kept apart from `diagnostic`
    /// because the lifecycle message that replaces it ("stdout reached EOF")
    /// describes the symptom, while this describes the cause.
    pub stderr: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct BridgeEvent {
    pub session_id: String,
    pub kind: String,
    pub payload: Value,
}

pub struct Request<T> {
    pub request_id: String,
    rx: Receiver<Result<T, HostError>>,
}

impl<T> Request<T> {
    pub fn try_result(&self) -> Option<Result<T, HostError>> {
        match self.rx.try_recv() {
            Ok(value) => Some(value),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(Err(HostError::NotRunning(
                "bridge response channel disconnected".into(),
            ))),
        }
    }

    pub fn recv_timeout(self, timeout: Duration) -> Result<T, HostError> {
        self.rx
            .recv_timeout(timeout)
            .unwrap_or(Err(HostError::Timeout))
    }
}

type Parser = Box<dyn FnOnce(Result<Value, HostError>) + Send>;
struct Pending {
    generation: u64,
    deadline: Instant,
    parse: Parser,
}
struct StatusData {
    state: HostState,
    diagnostic: Option<String>,
    stderr: Option<String>,
    dropped_events: u64,
}
struct Inner {
    config: AgentBridgeConfig,
    generation: AtomicU64,
    next_id: AtomicU64,
    closed: AtomicBool,
    writer: Mutex<Option<SyncSender<Vec<u8>>>>,
    child: Mutex<Option<(u64, Child)>>,
    workers: Mutex<Vec<(u64, &'static str, thread::JoinHandle<()>)>>,
    lifecycle: Mutex<()>,
    pending: Mutex<HashMap<String, Pending>>,
    events_tx: SyncSender<BridgeEvent>,
    critical_events_tx: SyncSender<BridgeEvent>,
    status: Mutex<StatusData>,
}

pub struct AgentBridgeHost {
    inner: Arc<Inner>,
    events_rx: Receiver<BridgeEvent>,
    critical_events_rx: Receiver<BridgeEvent>,
}

impl AgentBridgeHost {
    pub fn launch(config: AgentBridgeConfig) -> Result<Self, HostError> {
        if config.max_line_bytes == 0 || config.max_pending == 0 || config.event_capacity == 0 {
            return Err(HostError::Protocol("bridge limits must be positive".into()));
        }
        let (events_tx, events_rx) = mpsc::sync_channel(config.event_capacity);
        let (critical_events_tx, critical_events_rx) = mpsc::sync_channel(config.event_capacity);
        let inner = Arc::new(Inner {
            config,
            generation: AtomicU64::new(0),
            next_id: AtomicU64::new(1),
            closed: AtomicBool::new(false),
            writer: Mutex::new(None),
            child: Mutex::new(None),
            workers: Mutex::new(Vec::new()),
            lifecycle: Mutex::new(()),
            pending: Mutex::new(HashMap::new()),
            events_tx,
            critical_events_tx,
            status: Mutex::new(StatusData {
                state: HostState::Starting,
                diagnostic: None,
                stderr: None,
                dropped_events: 0,
            }),
        });
        launch_generation(&inner)?;
        Ok(Self {
            inner,
            events_rx,
            critical_events_rx,
        })
    }

    /// A `Send + Sync` handle to the request side of the bridge.
    ///
    /// The host itself is neither: it owns the single-consumer event receivers,
    /// which belong to the thread that drains them. Issuing a request touches
    /// only `Inner`, which is `Mutex`/`Atomic`/`SyncSender` throughout, so the
    /// two halves can be separated. Shutdown settles several independent
    /// subsystems concurrently and each needs to cancel its own session
    /// (`main.rs`), which is what this exists for.
    pub fn handle(&self) -> AgentHandle {
        AgentHandle {
            inner: Arc::clone(&self.inner),
        }
    }

    pub fn status(&self) -> HostStatus {
        status_on(&self.inner)
    }

    pub fn poll_event(&self) -> Option<BridgeEvent> {
        self.critical_events_rx
            .try_recv()
            .ok()
            .or_else(|| self.events_rx.try_recv().ok())
    }

    pub fn capabilities(&self) -> Result<Request<Value>, HostError> {
        self.submit(json!({ "method": "capabilities" }), Ok)
    }

    pub fn start_session(
        &self,
        provider: &str,
        cwd: &Path,
        mode_id: Option<&str>,
        thinking_option_id: Option<&str>,
        title: Option<&str>,
    ) -> Result<Request<String>, HostError> {
        self.start_session_with_purpose(provider, cwd, mode_id, thinking_option_id, title, None)
    }

    pub fn start_session_with_purpose(
        &self,
        provider: &str,
        cwd: &Path,
        mode_id: Option<&str>,
        thinking_option_id: Option<&str>,
        title: Option<&str>,
        purpose: Option<SessionPurpose>,
    ) -> Result<Request<String>, HostError> {
        let mut request = json!({
            "method": "start_session", "provider": provider,
            "cwd": path_text(cwd)?,
        });
        if let Some(purpose) = purpose {
            request["purpose"] = json!(purpose);
        }
        insert_optional(&mut request, "mode_id", mode_id);
        insert_optional(&mut request, "thinking_option_id", thinking_option_id);
        insert_optional(&mut request, "title", title);
        self.submit(request, |value| required_string(&value, "session_id"))
    }

    pub fn resume_session(&self, session_id: &str) -> Result<Request<String>, HostError> {
        self.resume_session_with_purpose(session_id, None)
    }

    pub fn resume_session_with_purpose(
        &self,
        session_id: &str,
        purpose: Option<SessionPurpose>,
    ) -> Result<Request<String>, HostError> {
        let mut request = json!({ "method": "resume_session", "session_id": session_id });
        if let Some(purpose) = purpose {
            request["purpose"] = json!(purpose);
        }
        self.submit(request, |value| required_string(&value, "session_id"))
    }

    pub fn send_prompt(&self, session_id: &str, prompt: &str) -> Result<Request<Value>, HostError> {
        self.submit(
            json!({ "method": "send_prompt", "session_id": session_id, "prompt": prompt }),
            Ok,
        )
    }

    pub fn cancel(&self, session_id: &str) -> Result<Request<Value>, HostError> {
        self.submit(json!({ "method": "cancel", "session_id": session_id }), Ok)
    }

    pub fn propose(
        &self,
        session_id: &str,
        kind: ProposalKind,
        instruction: &str,
        revision: OriginatingRevision,
        context: ProposalContext,
    ) -> Result<Request<ProposalEnvelope>, HostError> {
        if context.dataset_paths.len() > 64 {
            return Err(HostError::Protocol(
                "at most 64 dataset paths are allowed".into(),
            ));
        }
        let manifest_path = path_text(&context.manifest_path)?;
        let dataset_paths = context
            .dataset_paths
            .iter()
            .map(|path| path_text(path))
            .collect::<Result<Vec<_>, _>>()?;
        let expected_kind = kind;
        let expected_revision = revision.clone();
        let mut wire_context =
            json!({ "manifest_path": manifest_path, "dataset_paths": dataset_paths });
        if let Some(inline) = context.inline_context {
            let limit = context.inline_context_limit;
            if !inline.is_object()
                || serde_json::to_vec(&inline)
                    .map_err(|error| HostError::Protocol(error.to_string()))?
                    .len()
                    > limit
            {
                return Err(HostError::Protocol(format!(
                    "prepared assistance context must be an object within {} KiB",
                    limit / 1024
                )));
            }
            wire_context["inline_context"] = inline;
        }
        if let Some(command) = context.inspection_command {
            if command.is_empty()
                || command.len() > 16
                || command
                    .iter()
                    .any(|argument| argument.is_empty() || argument.len() > 4096)
            {
                return Err(HostError::Protocol(
                    "invalid inspection command arguments".into(),
                ));
            }
            wire_context["inspection_command"] = json!(command);
        }
        self.submit(
            json!({
                "method": "request_proposal", "session_id": session_id,
                "kind": kind, "instruction": instruction,
                "originating_revision": revision,
                "context": wire_context,
            }),
            move |value| validate_proposal(value, expected_kind, &expected_revision),
        )
    }

    pub fn restart(&self) -> Result<(), HostError> {
        terminate_current(
            &self.inner,
            HostError::NotRunning("bridge restarted".into()),
        )?;
        self.inner.closed.store(false, Ordering::Release);
        launch_generation(&self.inner)
    }

    pub fn shutdown(&self) -> Result<(), HostError> {
        self.inner.closed.store(true, Ordering::Release);
        let cleanup = shutdown_current_gracefully(
            &self.inner,
            HostError::NotRunning("bridge shut down".into()),
        );
        self.inner.status.lock().expect("status lock").state = HostState::Stopped;
        cleanup
    }

    fn submit<T: Send + 'static>(
        &self,
        body: Value,
        parse_result: impl FnOnce(Value) -> Result<T, HostError> + Send + 'static,
    ) -> Result<Request<T>, HostError> {
        submit_on(&self.inner, body, parse_result)
    }
}

/// See `AgentBridgeHost::handle`. Everything here is request issuing, which is
/// `Inner`-only and therefore shareable; the event receivers stay with the host.
#[derive(Clone)]
pub struct AgentHandle {
    inner: Arc<Inner>,
}

impl AgentHandle {
    pub fn cancel(&self, session_id: &str) -> Result<Request<Value>, HostError> {
        submit_on(
            &self.inner,
            json!({ "method": "cancel", "session_id": session_id }),
            Ok,
        )
    }
}

fn status_on(inner: &Arc<Inner>) -> HostStatus {
    let status = inner.status.lock().expect("status lock");
    HostStatus {
        state: status.state.clone(),
        generation: inner.generation.load(Ordering::Acquire),
        pending: inner.pending.lock().expect("pending lock").len(),
        dropped_events: status.dropped_events,
        diagnostic: status.diagnostic.clone(),
        stderr: status.stderr.clone(),
    }
}

fn submit_on<T: Send + 'static>(
    inner: &Arc<Inner>,
    mut body: Value,
    parse_result: impl FnOnce(Value) -> Result<T, HostError> + Send + 'static,
) -> Result<Request<T>, HostError> {
    let status = status_on(inner);
    if status.state != HostState::Running {
        return Err(HostError::NotRunning(not_running_reason(&status)));
    }
    let request_id = format!("rust-{}", inner.next_id.fetch_add(1, Ordering::Relaxed));
    let object = body
        .as_object_mut()
        .ok_or_else(|| HostError::Protocol("request must be an object".into()))?;
    object.insert("schema_version".into(), json!(PROTOCOL_VERSION));
    object.insert("request_id".into(), json!(request_id));
    let mut encoded = CappedBuffer::new(inner.config.max_line_bytes);
    serde_json::to_writer(&mut encoded, &body)
        .map_err(|_| HostError::Protocol("request exceeds JSONL byte limit".into()))?;
    let mut encoded = encoded.into_inner();
    encoded.push(b'\n');
    if encoded.len() > inner.config.max_line_bytes {
        return Err(HostError::Protocol(
            "request exceeds JSONL byte limit".into(),
        ));
    }
    let (tx, rx) = mpsc::sync_channel(1);
    let callback: Parser = Box::new(move |value| {
        let _ = tx.send(value.and_then(parse_result));
    });
    let generation = inner.generation.load(Ordering::Acquire);
    {
        let mut pending = inner.pending.lock().expect("pending lock");
        if pending.len() >= inner.config.max_pending {
            return Err(HostError::Capacity);
        }
        pending.insert(
            request_id.clone(),
            Pending {
                generation,
                deadline: Instant::now() + inner.config.request_timeout,
                parse: callback,
            },
        );
    }
    let writer = inner.writer.lock().expect("writer lock").clone();
    match writer.map(|writer| writer.try_send(encoded)) {
        Some(Ok(())) => {}
        Some(Err(TrySendError::Full(_))) => {
            fail_one(inner, &request_id, HostError::Capacity);
        }
        Some(Err(TrySendError::Disconnected(_))) | None => {
            fail_one(
                inner,
                &request_id,
                HostError::NotRunning("bridge stdin disconnected".into()),
            );
        }
    }
    Ok(Request { request_id, rx })
}

struct CappedBuffer {
    bytes: Vec<u8>,
    limit: usize,
}

impl CappedBuffer {
    fn new(limit: usize) -> Self {
        Self {
            bytes: Vec::with_capacity(limit.min(8192)),
            limit,
        }
    }

    fn into_inner(self) -> Vec<u8> {
        self.bytes
    }
}

impl Write for CappedBuffer {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
            return Err(std::io::Error::other("JSONL byte limit exceeded"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Drop for AgentBridgeHost {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

fn launch_generation(inner: &Arc<Inner>) -> Result<(), HostError> {
    let _lifecycle = inner.lifecycle.lock().expect("lifecycle lock");
    let generation = inner.generation.fetch_add(1, Ordering::AcqRel) + 1;
    let mut command = Command::new(&inner.config.program);
    command
        .args(&inner.config.args)
        .current_dir(&inner.config.cwd)
        .envs(inner.config.environment.iter().cloned())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            // Name the program: `No such file or directory` on its own cannot
            // tell the user that Node, not lvu, is what is missing.
            let message = format!("{}: {error}", inner.config.program.display());
            let mut status = inner.status.lock().expect("status lock");
            status.state = HostState::Faulted;
            status.diagnostic = Some(message.clone());
            return Err(HostError::Io(message));
        }
    };
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| HostError::Io("missing child stdin".into()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| HostError::Io("missing child stdout".into()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| HostError::Io("missing child stderr".into()))?;
    *inner.child.lock().expect("child lock") = Some((generation, child));
    let (writer_tx, writer_rx) = mpsc::sync_channel(inner.config.max_pending);
    *inner.writer.lock().expect("writer lock") = Some(writer_tx);
    {
        let mut status = inner.status.lock().expect("status lock");
        status.state = HostState::Running;
        status.diagnostic = None;
        status.stderr = None;
    }
    let mut workers = inner.workers.lock().expect("workers lock");
    let weak = Arc::downgrade(inner);
    workers.push((
        generation,
        "stdin writer",
        thread::spawn(move || writer_loop(stdin, writer_rx, weak, generation)),
    ));
    let weak = Arc::downgrade(inner);
    let max_line = inner.config.max_line_bytes;
    workers.push((
        generation,
        "stdout reader",
        thread::spawn(move || stdout_loop(stdout, weak, generation, max_line)),
    ));
    let weak = Arc::downgrade(inner);
    workers.push((
        generation,
        "stderr reader",
        thread::spawn(move || stderr_loop(stderr, weak, generation)),
    ));
    let weak = Arc::downgrade(inner);
    workers.push((
        generation,
        "timeout monitor",
        thread::spawn(move || timeout_loop(weak, generation)),
    ));
    Ok(())
}

fn timeout_loop(weak: std::sync::Weak<Inner>, generation: u64) {
    loop {
        thread::sleep(Duration::from_millis(10));
        let Some(inner) = weak.upgrade() else { break };
        if inner.closed.load(Ordering::Acquire)
            || inner.generation.load(Ordering::Acquire) != generation
        {
            break;
        }
        let now = Instant::now();
        let expired = inner
            .pending
            .lock()
            .expect("pending lock")
            .iter()
            .find(|(_, pending)| pending.generation == generation && pending.deadline <= now)
            .map(|(id, _)| id.clone());
        if let Some(id) = expired {
            fail_one(&inner, &id, HostError::Timeout);
            fault_generation(&inner, generation, "bridge request timed out");
            break;
        }
    }
}

fn writer_loop(
    mut stdin: impl Write,
    rx: Receiver<Vec<u8>>,
    weak: std::sync::Weak<Inner>,
    generation: u64,
) {
    while let Ok(bytes) = rx.recv() {
        if let Err(error) = stdin.write_all(&bytes).and_then(|_| stdin.flush()) {
            if let Some(inner) = weak.upgrade() {
                fault_generation(&inner, generation, &format!("bridge stdin: {error}"));
            }
            break;
        }
    }
}

fn stdout_loop(mut stdout: impl Read, weak: std::sync::Weak<Inner>, generation: u64, max: usize) {
    let mut chunk = [0_u8; 8192];
    let mut line = Vec::new();
    loop {
        match stdout.read(&mut chunk) {
            Ok(0) => {
                if let Some(inner) = weak.upgrade() {
                    disconnect_generation(&inner, generation, "bridge stdout reached EOF");
                }
                break;
            }
            Ok(count) => {
                for &byte in &chunk[..count] {
                    if byte == b'\n' {
                        if !line.is_empty()
                            && let Some(inner) = weak.upgrade()
                        {
                            process_line(&inner, generation, &line);
                        }
                        line.clear();
                    } else {
                        if line.len() == max {
                            if let Some(inner) = weak.upgrade() {
                                fault_generation(
                                    &inner,
                                    generation,
                                    "bridge emitted oversized JSONL",
                                );
                            }
                            return;
                        } else {
                            line.push(byte);
                        }
                    }
                }
            }
            Err(error) => {
                if let Some(inner) = weak.upgrade() {
                    fault_generation(&inner, generation, &format!("bridge stdout: {error}"));
                }
                break;
            }
        }
    }
}

fn stderr_loop(mut stderr: impl Read, weak: std::sync::Weak<Inner>, generation: u64) {
    let mut buffer = [0_u8; 8192];
    loop {
        match stderr.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(count) => {
                if let Some(inner) = weak.upgrade() {
                    if inner.generation.load(Ordering::Acquire) != generation {
                        break;
                    }
                    let text = String::from_utf8_lossy(&buffer[..count]);
                    let mut status = inner.status.lock().expect("status lock");
                    status.stderr = Some(tail(&text, 4096));
                } else {
                    break;
                }
            }
        }
    }
}

fn process_line(inner: &Arc<Inner>, generation: u64, bytes: &[u8]) {
    if inner.generation.load(Ordering::Acquire) != generation {
        return;
    }
    let value: Value = match serde_json::from_slice(bytes) {
        Ok(value) => value,
        Err(error) => {
            return fault_generation(inner, generation, &format!("invalid bridge JSON: {error}"));
        }
    };
    if value.get("schema_version").and_then(Value::as_u64) != Some(PROTOCOL_VERSION.into()) {
        return fault_generation(inner, generation, "unsupported bridge protocol version");
    }
    if let (Some(request_id), Some(ok)) = (
        value.get("request_id").and_then(Value::as_str),
        value.get("ok").and_then(Value::as_bool),
    ) {
        let pending = inner
            .pending
            .lock()
            .expect("pending lock")
            .remove(request_id);
        let Some(pending) = pending else {
            record_diagnostic(inner, format!("ignored stale response id {request_id}"));
            return;
        };
        if pending.generation != generation {
            return;
        }
        if ok {
            (pending.parse)(Ok(value.get("result").cloned().unwrap_or(Value::Null)));
        } else {
            let failure = BridgeFailure {
                code: value
                    .pointer("/error/code")
                    .and_then(Value::as_str)
                    .unwrap_or("PROTOCOL_ERROR")
                    .to_owned(),
                message: value
                    .pointer("/error/message")
                    .and_then(Value::as_str)
                    .unwrap_or("bridge returned an invalid error")
                    .to_owned(),
            };
            deliver_error(pending, HostError::Bridge(failure));
        }
        return;
    }
    let (Some(session_id), Some(kind)) = (
        value.get("session_id").and_then(Value::as_str),
        value.get("kind").and_then(Value::as_str),
    ) else {
        return fault_generation(
            inner,
            generation,
            "bridge message is neither response nor event",
        );
    };
    let critical = matches!(kind, "session_archived" | "archive_failed");
    let event = BridgeEvent {
        session_id: session_id.to_owned(),
        kind: kind.to_owned(),
        payload: value,
    };
    if critical {
        if let Err(TrySendError::Full(event)) = inner.critical_events_tx.try_send(event) {
            fault_generation(
                inner,
                generation,
                &format!(
                    "critical agent lifecycle event queue is full; retained ownership requires restart recovery (session {}, event {})",
                    event.session_id, event.kind
                ),
            );
        }
    } else if let Err(TrySendError::Full(_)) = inner.events_tx.try_send(event) {
        inner.status.lock().expect("status lock").dropped_events += 1;
    }
}

fn fail_one(inner: &Arc<Inner>, id: &str, error: HostError) -> bool {
    let pending = inner.pending.lock().expect("pending lock").remove(id);
    if let Some(pending) = pending {
        deliver_error(pending, error);
        true
    } else {
        false
    }
}

fn deliver_error(pending: Pending, error: HostError) {
    (pending.parse)(Err(error));
}

/// The lifecycle message plus the process's own last words. A bridge that
/// could not reach the daemon exits, and all lvu sees on its own is EOF.
fn failure_reason(status: &StatusData, message: &str) -> String {
    match status
        .stderr
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        Some(stderr) => format!("{message}; {stderr}"),
        None => message.to_owned(),
    }
}

fn fault_generation(inner: &Arc<Inner>, generation: u64, message: &str) {
    if inner.generation.load(Ordering::Acquire) != generation {
        return;
    }
    let lifecycle = inner.lifecycle.lock().expect("lifecycle lock");
    if inner.generation.load(Ordering::Acquire) != generation {
        return;
    }
    let reason = {
        let mut status = inner.status.lock().expect("status lock");
        status.state = HostState::Faulted;
        status.diagnostic = Some(tail(message, 4096));
        failure_reason(&status, message)
    };
    *inner.writer.lock().expect("writer lock") = None;
    let pending = take_pending_generation(inner, generation);
    let child = take_child(inner, generation);
    drop(lifecycle);
    for pending in pending {
        deliver_error(pending, HostError::NotRunning(reason.clone()));
    }
    terminate_child(inner, child, generation);
}

fn disconnect_generation(inner: &Arc<Inner>, generation: u64, message: &str) {
    if inner.generation.load(Ordering::Acquire) != generation {
        return;
    }
    let lifecycle = inner.lifecycle.lock().expect("lifecycle lock");
    if inner.generation.load(Ordering::Acquire) != generation {
        return;
    }
    let reason = {
        let mut status = inner.status.lock().expect("status lock");
        status.state = HostState::Disconnected;
        status.diagnostic = Some(tail(message, 4096));
        failure_reason(&status, message)
    };
    *inner.writer.lock().expect("writer lock") = None;
    let pending = take_pending_generation(inner, generation);
    let child = take_child(inner, generation);
    drop(lifecycle);
    for pending in pending {
        deliver_error(pending, HostError::NotRunning(reason.clone()));
    }
    terminate_child(inner, child, generation);
}

fn terminate_current(inner: &Arc<Inner>, error: HostError) -> Result<(), HostError> {
    let lifecycle = inner.lifecycle.lock().expect("lifecycle lock");
    let generation = inner.generation.load(Ordering::Acquire);
    inner.generation.fetch_add(1, Ordering::AcqRel);
    *inner.writer.lock().expect("writer lock") = None;
    let pending = take_pending_generation(inner, generation);
    let child = take_child(inner, generation);
    drop(lifecycle);
    for pending in pending {
        deliver_error(pending, error.clone());
    }
    terminate_child(inner, child, generation);
    join_retired_workers(inner, generation)
}

fn shutdown_current_gracefully(inner: &Arc<Inner>, error: HostError) -> Result<(), HostError> {
    let deadline = Instant::now() + inner.config.shutdown_timeout;
    let lifecycle = inner.lifecycle.lock().expect("lifecycle lock");
    let generation = inner.generation.load(Ordering::Acquire);
    inner.generation.fetch_add(1, Ordering::AcqRel);
    // Dropping the host's only sender lets the writer drain already-admitted
    // messages and then close child stdin. The bridge CLI treats EOF as a
    // normal stop and releases its owned-session lease in Bridge.close().
    *inner.writer.lock().expect("writer lock") = None;
    let pending = take_pending_generation(inner, generation);
    let child = take_child(inner, generation);
    drop(lifecycle);
    for pending in pending {
        deliver_error(pending, error.clone());
    }
    finish_child_gracefully(inner, child, generation, deadline);
    join_retired_workers_until(inner, generation, deadline)
}

fn take_pending_generation(inner: &Arc<Inner>, generation: u64) -> Vec<Pending> {
    let mut pending = inner.pending.lock().expect("pending lock");
    let ids: Vec<_> = pending
        .iter()
        .filter(|(_, value)| value.generation == generation)
        .map(|(id, _)| id.clone())
        .collect();
    ids.into_iter()
        .filter_map(|id| pending.remove(&id))
        .collect()
}

fn take_child(inner: &Arc<Inner>, generation: u64) -> Option<Child> {
    let mut slot = inner.child.lock().expect("child lock");
    if slot.as_ref().is_some_and(|(owned, _)| *owned == generation) {
        slot.take().map(|(_, child)| child)
    } else {
        None
    }
}

fn terminate_child(inner: &Arc<Inner>, child: Option<Child>, generation: u64) {
    if let Some(mut child) = child {
        kill_owned_process(&mut child);
        let deadline = std::time::Instant::now() + inner.config.shutdown_timeout;
        loop {
            match child.try_wait() {
                Ok(Some(_)) | Err(_) => break,
                Ok(None) if std::time::Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(2));
                }
                Ok(None) => {
                    record_diagnostic(inner, "bridge child reap exceeded shutdown deadline".into());
                    let reaper = thread::spawn(move || {
                        let _ = child.wait();
                    });
                    inner.workers.lock().expect("workers lock").push((
                        generation,
                        "child reaper",
                        reaper,
                    ));
                    break;
                }
            }
        }
    }
}

fn finish_child_gracefully(
    inner: &Arc<Inner>,
    child: Option<Child>,
    generation: u64,
    deadline: Instant,
) {
    let Some(mut child) = child else { return };
    // Pipe readers still need to observe EOF and be scheduled after the child
    // exits. Keep that work inside the existing total timeout instead of
    // allowing child reaping to consume the workers' entire deadline.
    let worker_reserve = inner.config.shutdown_timeout / 8;
    let child_deadline = deadline.checked_sub(worker_reserve).unwrap_or(deadline);
    let reserve = inner.config.shutdown_timeout / 4;
    let graceful_deadline = child_deadline
        .checked_sub(reserve)
        .unwrap_or(child_deadline);
    loop {
        match owned_leader_exited(&mut child) {
            Ok(true) => {
                // The unreaped leader still reserves its PID, so targeting the
                // process group cannot hit an unrelated reused PID. Clean up any
                // descendants that inherited bridge pipes, then reap the leader.
                kill_owned_process(&mut child);
                let _ = child.wait();
                return;
            }
            Ok(false) if Instant::now() < graceful_deadline => {
                thread::sleep(Duration::from_millis(2));
            }
            Ok(false) | Err(_) => break,
        }
    }
    kill_owned_process(&mut child);
    loop {
        match child.try_wait() {
            Ok(Some(_)) | Err(_) => return,
            Ok(None) if Instant::now() < child_deadline => thread::sleep(Duration::from_millis(2)),
            Ok(None) => {
                record_diagnostic(inner, "bridge child reap exceeded shutdown deadline".into());
                let reaper = thread::spawn(move || {
                    let _ = child.wait();
                });
                inner.workers.lock().expect("workers lock").push((
                    generation,
                    "child reaper",
                    reaper,
                ));
                return;
            }
        }
    }
}

#[cfg(unix)]
fn owned_leader_exited(child: &mut Child) -> io::Result<bool> {
    let pid = i32::try_from(child.id())
        .map_err(|_| io::Error::other("bridge child PID exceeds platform range"))?;
    // SAFETY: `pid` names our unreaped child. WNOWAIT observes terminal state
    // without releasing that PID for reuse before owned-group cleanup.
    unsafe {
        let mut status: libc::siginfo_t = std::mem::zeroed();
        if libc::waitid(
            libc::P_PID,
            pid as libc::id_t,
            &mut status,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        ) == -1
        {
            return Err(io::Error::last_os_error());
        }
        Ok(status.si_pid() != 0)
    }
}

#[cfg(not(unix))]
fn owned_leader_exited(child: &mut Child) -> io::Result<bool> {
    // Non-Unix targets do not create or signal a process group.
    child.try_wait().map(|status| status.is_some())
}

#[cfg(unix)]
fn kill_owned_process(child: &mut Child) {
    if let Ok(pid) = i32::try_from(child.id()) {
        // SAFETY: this host created `pid` as a new process-group leader.
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
    }
    let _ = child.kill();
}

#[cfg(not(unix))]
fn kill_owned_process(child: &mut Child) {
    let _ = child.kill();
}

fn join_retired_workers(inner: &Arc<Inner>, generation: u64) -> Result<(), HostError> {
    join_retired_workers_until(
        inner,
        generation,
        Instant::now() + inner.config.shutdown_timeout,
    )
}

fn join_retired_workers_until(
    inner: &Arc<Inner>,
    generation: u64,
    deadline: Instant,
) -> Result<(), HostError> {
    let mut owned = Vec::new();
    {
        let mut workers = inner.workers.lock().expect("workers lock");
        let mut index = 0;
        while index < workers.len() {
            if workers[index].0 <= generation {
                owned.push(workers.swap_remove(index));
            } else {
                index += 1;
            }
        }
    }
    while owned.iter().any(|(_, _, worker)| !worker.is_finished()) && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(2));
    }
    let mut incomplete = Vec::new();
    for worker in owned {
        if worker.2.is_finished() {
            let _ = worker.2.join();
        } else {
            incomplete.push(worker);
        }
    }
    if incomplete.is_empty() {
        Ok(())
    } else {
        let count = incomplete.len();
        let names = incomplete
            .iter()
            .map(|(_, name, _)| *name)
            .collect::<Vec<_>>()
            .join(", ");
        inner
            .workers
            .lock()
            .expect("workers lock")
            .extend(incomplete);
        let message = format!(
            "{} bridge worker thread(s) exceeded shutdown deadline: {names}",
            count
        );
        record_diagnostic(inner, message.clone());
        Err(HostError::Io(message))
    }
}

fn validate_proposal(
    value: Value,
    kind: ProposalKind,
    revision: &OriginatingRevision,
) -> Result<ProposalEnvelope, HostError> {
    let mut proposal: ProposalEnvelope = serde_json::from_value(
        value
            .get("proposal")
            .cloned()
            .ok_or_else(|| HostError::Protocol("missing proposal".into()))?,
    )
    .map_err(|error| HostError::Protocol(error.to_string()))?;
    if kind == ProposalKind::Sources && proposal.kind == ProposalKind::Source {
        // A singular legacy agent answers a plural request with one source.
        // Validate the inner definition first, then normalize to a
        // single-element batch so the application only sees one shape.
        validate_definition(ProposalKind::Source, &proposal.definition)?;
        proposal.definition = json!({"schema_version": 1, "sources": [proposal.definition]});
        proposal.kind = ProposalKind::Sources;
    }
    if proposal.kind != kind || &proposal.originating_revision != revision {
        return Err(HostError::Protocol(
            "proposal kind or originating revision mismatch".into(),
        ));
    }
    if proposal.explanation.is_empty() || proposal.explanation.len() > 16_384 {
        return Err(HostError::Protocol(
            "proposal explanation is invalid".into(),
        ));
    }
    validate_definition(kind, &proposal.definition)?;
    Ok(proposal)
}

fn validate_definition(kind: ProposalKind, value: &Value) -> Result<(), HostError> {
    let object = value
        .as_object()
        .ok_or_else(|| HostError::Protocol("proposal definition must be an object".into()))?;
    if object.get("schema_version").and_then(Value::as_u64) != Some(1) {
        return Err(HostError::Protocol(
            "proposal definition schema_version must be 1".into(),
        ));
    }
    let valid = match kind {
        ProposalKind::Filter => {
            exact_fields(object, &["schema_version", "expression"])
                && bounded_field(object, "expression", 131_072)
        }
        ProposalKind::Source => validate_source_definition(object),
        ProposalKind::Sources => validate_sources_definition(object),
        ProposalKind::Enrichment => validate_enrichment_definition(object),
        ProposalKind::View => validate_view_definition(object),
    };
    valid
        .then_some(())
        .ok_or_else(|| HostError::Protocol("proposal definition does not match its kind".into()))
}

fn bounded_field(object: &serde_json::Map<String, Value>, key: &str, max: usize) -> bool {
    object
        .get(key)
        .and_then(Value::as_str)
        .is_some_and(|value| !value.is_empty() && value.len() <= max)
}

fn uuid_field(object: &serde_json::Map<String, Value>, key: &str) -> bool {
    object
        .get(key)
        .and_then(Value::as_str)
        .is_some_and(|value| uuid::Uuid::parse_str(value).is_ok())
}

fn exact_fields(object: &serde_json::Map<String, Value>, fields: &[&str]) -> bool {
    object.len() == fields.len() && fields.iter().all(|field| object.contains_key(*field))
}

fn validate_source_definition(object: &serde_json::Map<String, Value>) -> bool {
    if !uuid_field(object, "id")
        || !bounded_field(object, "name", 256)
        || !object.get("identity_hints").is_some_and(Value::is_object)
        || !object
            .get("retention")
            .is_some_and(|value| value.is_null() || value.is_object())
    {
        return false;
    }
    match object.get("kind").and_then(Value::as_str) {
        Some("file") => {
            exact_fields(
                object,
                &[
                    "schema_version",
                    "id",
                    "name",
                    "identity_hints",
                    "retention",
                    "kind",
                    "path",
                    "follow",
                ],
            ) && bounded_field(object, "path", 4096)
                && object.get("follow").is_some_and(Value::is_boolean)
        }
        Some("command") => {
            exact_fields(
                object,
                &[
                    "schema_version",
                    "id",
                    "name",
                    "identity_hints",
                    "retention",
                    "kind",
                    "command",
                ],
            ) && object.get("command").is_some_and(Value::is_object)
        }
        Some("http") => {
            exact_fields(
                object,
                &[
                    "schema_version",
                    "id",
                    "name",
                    "identity_hints",
                    "retention",
                    "kind",
                    "url",
                    "framing",
                    "reconnect",
                ],
            ) && bounded_field(object, "url", 8192)
                && matches!(
                    object.get("framing").and_then(Value::as_str),
                    Some("newline" | "sse")
                )
                && object.get("reconnect").is_some_and(Value::is_object)
        }
        _ => false,
    }
}

fn validate_sources_definition(object: &serde_json::Map<String, Value>) -> bool {
    if !exact_fields(object, &["schema_version", "sources"]) {
        return false;
    }
    let Some(sources) = object.get("sources").and_then(Value::as_array) else {
        return false;
    };
    if sources.is_empty() || sources.len() > MAX_SOURCES_PER_PROPOSAL {
        return false;
    }
    let mut ids = std::collections::HashSet::new();
    sources.iter().all(|source| {
        source.as_object().is_some_and(|definition| {
            validate_source_definition(definition)
                && definition
                    .get("id")
                    .and_then(Value::as_str)
                    .is_some_and(|id| ids.insert(id.to_owned()))
        })
    })
}

fn validate_enrichment_definition(object: &serde_json::Map<String, Value>) -> bool {
    exact_fields(object, &["schema_version", "stages"])
        && object
            .get("stages")
            .and_then(Value::as_array)
            .is_some_and(|stages| {
                !stages.is_empty()
                    && stages.len() <= 64
                    && stages.iter().all(|stage| {
                        let Some(stage) = stage.as_object() else {
                            return false;
                        };
                        exact_fields(stage, &["id", "name", "expressions"])
                            && uuid_field(stage, "id")
                            && bounded_field(stage, "name", 256)
                            && stage
                                .get("expressions")
                                .and_then(Value::as_object)
                                .is_some_and(|expressions| {
                                    !expressions.is_empty()
                                        && expressions.len() <= 128
                                        && expressions.iter().all(|(name, expression)| {
                                            !name.is_empty()
                                                && name.len() <= 256
                                                && expression.as_str().is_some_and(|value| {
                                                    !value.is_empty() && value.len() <= 131_072
                                                })
                                        })
                                })
                    })
            })
}

fn validate_view_definition(object: &serde_json::Map<String, Value>) -> bool {
    let mut fields = vec![
        "schema_version",
        "id",
        "name",
        "source_ids",
        "filter",
        "recipe_stage_revisions",
    ];
    if object.contains_key("enrichments") {
        fields.push("enrichments");
    }
    let valid_chain = object.get("enrichments").is_none_or(|value| {
        value.as_array().is_some_and(|stages| {
            let mut ids = std::collections::HashSet::new();
            stages.len() <= 32
                && stages.iter().all(|stage| {
                    stage.as_object().is_some_and(|stage| {
                        exact_fields(stage, &["id", "source"])
                            && bounded_field(stage, "id", 128)
                            && bounded_field(stage, "source", 16_384)
                            && ids.insert(stage["id"].as_str())
                    })
                })
        })
    });
    valid_chain
        && exact_fields(object, &fields)
        && uuid_field(object, "id")
        && bounded_field(object, "name", 256)
        && object
            .get("source_ids")
            .and_then(Value::as_array)
            .is_some_and(|ids| {
                !ids.is_empty()
                    && ids.len() <= 64
                    && ids.iter().all(|id| {
                        id.as_str()
                            .is_some_and(|value| uuid::Uuid::parse_str(value).is_ok())
                    })
            })
        && object.get("filter").is_some_and(|filter| {
            filter.is_null()
                || filter.as_object().is_some_and(|value| {
                    value.get("schema_version").and_then(Value::as_u64) == Some(1)
                        && exact_fields(value, &["schema_version", "expression"])
                        && bounded_field(value, "expression", 131_072)
                })
        })
        && object
            .get("recipe_stage_revisions")
            .and_then(Value::as_array)
            .is_some_and(|revisions| {
                revisions.len() <= 256
                    && revisions.iter().all(|revision| {
                        revision
                            .as_str()
                            .is_some_and(|value| !value.is_empty() && value.len() <= 256)
                    })
            })
}

fn required_string(value: &Value, key: &str) -> Result<String, HostError> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| HostError::Protocol(format!("missing {key}")))
}

fn insert_optional(object: &mut Value, key: &str, value: Option<&str>) {
    if let Some(value) = value {
        object
            .as_object_mut()
            .expect("object")
            .insert(key.into(), json!(value));
    }
}

fn path_text(path: &Path) -> Result<String, HostError> {
    path.to_str()
        .filter(|value| !value.is_empty())
        .filter(|value| value.len() <= 4096)
        .map(str::to_owned)
        .ok_or_else(|| HostError::Protocol("bridge paths must be 1..4096 UTF-8 bytes".into()))
}

fn record_diagnostic(inner: &Arc<Inner>, message: String) {
    inner.status.lock().expect("status lock").diagnostic = Some(tail(&message, 4096));
}

/// One actionable sentence for a failed 🧠 request: what failed, and what the
/// user can do about it.
///
/// The bridge is a child process that talks to a separate Paseo daemon, so a
/// single `bridge is not running` covered a missing Node, an unbuilt bridge, a
/// process that died on startup, a daemon that was never running and a
/// provider nobody had authenticated. Each of those has a different remedy, so
/// each gets its own message. Everything outside 🧠 — capture, search, native
/// filtering, enrichment, export — is unaffected either way.
pub fn diagnose(error: &HostError) -> String {
    match error {
        HostError::Bridge(failure) => bridge_failure_message(failure),
        HostError::Io(message) => launch_message(message),
        HostError::Capacity => {
            "the agent bridge already has as many requests in flight as it accepts; \
             wait for the current 🧠 request to finish and try again"
                .to_owned()
        }
        HostError::Timeout => {
            "the agent bridge did not answer within its deadline; the request was \
             abandoned and the bridge restarts on the next 🧠 request"
                .to_owned()
        }
        HostError::Protocol(message) => {
            format!(
                "the agent bridge sent a response lvu cannot use ({message}); this is a version mismatch — rebuild it with `npm --prefix bridge ci && npm --prefix bridge run build`"
            )
        }
        HostError::NotRunning(reason) => not_running_message(reason),
    }
}

fn launch_message(message: &str) -> String {
    let program = message
        .split_once(':')
        .map_or(message, |(program, _)| program);
    if message.contains("os error 2") || message.contains("No such file or directory") {
        return format!(
            "the agent bridge could not be started because `{program}` is not installed or not on PATH; \
             install Node (`mise install node@26.8.1` in the checkout, or your system package manager) and reopen 🧠"
        );
    }
    if message.contains("os error 13") || message.contains("Permission denied") {
        return format!(
            "the agent bridge could not be started: `{program}` is not executable ({message})"
        );
    }
    format!("the agent bridge could not be started ({message})")
}

/// `reason` already carries the bridge's own last words: `terminate_generation`
/// and `submit` fold `stderr` into it, because the lifecycle message alone
/// ("stdout reached EOF") is the symptom and never the cause.
///
/// Classification is by stable marker, not wrapper prose. The CLI prefixes
/// every startup failure with `bridge connection failed`, so that wrapper
/// alone never implies a daemon failure. Owned-root busy is checked first so
/// it is never reported as daemon unreachable when both appear together.
fn not_running_message(reason: &str) -> String {
    let detail = reason;
    let text = reason;
    if text.contains("MODULE_NOT_FOUND")
        || text.contains("ERR_MODULE_NOT_FOUND")
        || text.contains("Cannot find module")
    {
        return format!(
            "the agent bridge process started but could not load its own code ({}); \
             build it with `npm --prefix bridge ci && npm --prefix bridge run build`",
            tail(detail, 400)
        );
    }
    if looks_like_owned_root_busy(text) {
        // EEXIST alone cannot prove the lock is stale versus a live competing
        // bridge, and PID existence cannot prove ownership across races or PID
        // reuse, so never unlink automatically. Instruct safe exact-path
        // recovery instead, and never recommend deleting the assistance root
        // or capture data.
        let lock = owned_lock_path(text).unwrap_or_else(|| "the reported bridge.lock".into());
        return format!(
            "the owned assistance route is busy — another lvu window already holds the 🧠 assistance lease ({}); \
             close all lvu windows using this capture root and wait for any agent request to finish. \
             If no lvu window remains, verify no lvu or bridge process still owns it, then remove only the exact lock file at {} — never delete the assistance root or capture data. \
             EEXIST alone cannot prove the lock is stale. Capture, search and native filtering stay usable",
            tail(detail, 400),
            lock,
        );
    }
    if looks_like_daemon_failure(text) {
        return format!(
            "the agent bridge started but could not reach the Paseo daemon ({}); \
             start Paseo (or point LVU_PASEO_URL at it) and reopen 🧠. \
             Localhost here is the machine/container running lvu, not a remote client: establish topology (Desktop-managed, standalone, or Docker), \
             check Settings → host → Overview → Full status and `paseo daemon status --json`; a remotely connected client does not imply localhost has a daemon. \
             See https://paseo.sh/docs/troubleshooting and https://paseo.sh/docs/connectivity. Capture, search and native filtering stay usable",
            tail(detail, 400)
        );
    }
    format!(
        "the agent bridge is not available ({}); it restarts on the next 🧠 request",
        tail(detail, 400)
    )
}

/// What `submit` reports when the host is not in `Running`: the state plus
/// whatever the process last said, so the classifier has a cause to work with.
fn not_running_reason(status: &HostStatus) -> String {
    let state = match status.state {
        HostState::Starting => "starting",
        HostState::Running => "running",
        HostState::Disconnected => "disconnected",
        HostState::Faulted => "faulted",
        HostState::Stopped => "stopped",
    };
    match (
        status
            .stderr
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty()),
        status.diagnostic.as_deref(),
    ) {
        (Some(stderr), Some(lifecycle)) => format!("bridge {state}: {lifecycle}; {stderr}"),
        (Some(stderr), None) => format!("bridge {state}: {stderr}"),
        (None, Some(lifecycle)) => format!("bridge {state}: {lifecycle}"),
        (None, None) => format!("bridge {state}"),
    }
}

/// Stable owned-route signal. `OWNED_ROOT_BUSY` is the structured marker new
/// bridges emit; `bridge.lock`, `owned assistance root/route` and
/// `assistance lease` cover legacy bridges whose EEXIST message has neither
/// the marker nor the word lease. Checked before any daemon test so a combined
/// stderr containing both the generic `bridge connection failed` wrapper and
/// an owned busy line is never reported as daemon unreachable. The bare
/// substring `lease` is deliberately absent: it matches unrelated words such
/// as `please` and `released`.
fn looks_like_owned_root_busy(text: &str) -> bool {
    text.contains("OWNED_ROOT_BUSY")
        || text.contains("bridge.lock")
        || text.contains("owned assistance root")
        || text.contains("owned assistance route")
        || text.contains("assistance lease")
}

/// Bounded extraction of the exact reported `bridge.lock` path for safe
/// recovery guidance. Returns the path characters only, without surrounding
/// quotes or trailing punctuation, capped so the diagnostic stays bounded.
// The engine computes; the app names and presents. This hand-rolled scan is
// presentation-only folding of an already-reported path (raw-bytes/stable
// identity invariant): Polars cannot express it.
fn owned_lock_path(text: &str) -> Option<String> {
    // The bridge emits one JSON-quoted exact path (`at "/tmp/My
    // Logs/.../bridge.lock";`) amid bare prose mentions (`stale bridge.lock`,
    // `that exact bridge.lock`). The exact path is the only quoted one, so
    // find it by its closing quote rather than by position: prose mentions
    // never end in `bridge.lock"`.
    let mut offset = 0;
    while let Some(rel) = text[offset..].find("bridge.lock\"") {
        let end = offset + rel + "bridge.lock".len();
        if let Some(open) = text[..end].rfind('"') {
            let after = &text[end..end + 1];
            if after == "\"" {
                let token = &text[open..=end];
                if let Ok(decoded) = serde_json::from_str::<String>(token)
                    && decoded.ends_with("bridge.lock")
                    && decoded.len() <= 1024
                    && decoded.contains('/')
                {
                    return Some(decoded);
                }
                let raw = text[open + 1..end].to_owned();
                if raw.ends_with("bridge.lock") && raw.len() <= 1024 && raw.contains('/') {
                    return Some(raw);
                }
            }
        }
        offset = end + 1;
        if offset >= text.len() {
            break;
        }
    }
    // Legacy quoting and unquoted paths: try every mention from last to first
    // and keep the first token containing a separator, so a trailing bare
    // prose mention never shadows an earlier exact path.
    let mut mentions: Vec<usize> = Vec::new();
    let mut off = 0;
    while let Some(rel) = text[off..].find("bridge.lock") {
        mentions.push(off + rel);
        off += rel + "bridge.lock".len();
        if off >= text.len() {
            break;
        }
    }
    if mentions.is_empty() {
        return None;
    }
    for &mention in mentions.iter().rev() {
        let end = mention + "bridge.lock".len();
        // Legacy single-quote/backtick quoting (hand-written, never JSON).
        for quote in ['\'', '`'] {
            if let Some(close) = text[end..].find(quote)
                && let Some(open) = text[..end].rfind(quote)
            {
                let after = &text[end..end + close];
                if after.len() <= 2
                    && after
                        .chars()
                        .all(|c| matches!(c, ';' | ',' | '.' | ':' | ' ' | '\n' | '\t'))
                {
                    let quoted = text[open + 1..end].to_owned();
                    if quoted.ends_with("bridge.lock")
                        && quoted.len() <= 1024
                        && quoted.contains('/')
                    {
                        return Some(quoted);
                    }
                }
            }
        }
        if let Some(path) = unquoted_token_path(text, end)
            && path.contains('/')
        {
            return Some(path);
        }
    }
    // No exact path anywhere: name the file so recovery still points at it.
    if text.contains("bridge.lock") {
        return Some("bridge.lock".to_owned());
    }
    None
}

/// Walk back over unquoted path bytes to the start of the token. Spaces end
/// the token (quoted paths are handled above); non-ASCII bytes are kept so
/// Unicode roots survive.
fn unquoted_token_path(text: &str, end: usize) -> Option<String> {
    let bytes = text.as_bytes();
    if end > text.len() || !text[..end].ends_with("bridge.lock") {
        return None;
    }
    let mut start = end - "bridge.lock".len();
    while start > 0 {
        let byte = bytes[start - 1];
        let is_path = byte.is_ascii_alphanumeric()
            || byte >= 128
            || matches!(byte, b'/' | b'.' | b'_' | b'-' | b'~' | b'+');
        if !is_path {
            break;
        }
        start -= 1;
        if end - start > 1024 {
            break;
        }
    }
    let mut path = text[start..end].to_owned();
    path = path
        .trim_matches(|character: char| character == '"' || character == '\'' || character == '`')
        .to_owned();
    if path.len() > 1024 || !path.ends_with("bridge.lock") {
        return None;
    }
    if path.contains('/') {
        if let Some(slash) = path.find('/') {
            path = path[slash..].to_owned();
        }
        Some(path)
    } else if path == "bridge.lock" {
        Some(path)
    } else {
        None
    }
}

fn looks_like_daemon_failure(text: &str) -> bool {
    // Stable daemon markers and transport errors only. The generic CLI wrapper
    // `bridge connection failed` and bare `connect ` are deliberately absent:
    // they prefix every startup failure, including owned-root busy, and must
    // never alone imply the daemon is unreachable.
    const MARKERS: [&str; 12] = [
        "DAEMON_UNREACHABLE",
        "DAEMON_TIMEOUT",
        "ECONNREFUSED",
        "ECONNRESET",
        "EHOSTUNREACH",
        "ENETUNREACH",
        "ETIMEDOUT",
        "CONNECT_TIMEOUT",
        "Daemon",
        "daemon",
        "WebSocket",
        "websocket",
    ];
    MARKERS.iter().any(|marker| text.contains(marker))
}

fn bridge_failure_message(failure: &BridgeFailure) -> String {
    match failure.code.as_str() {
        "DAEMON_UNREACHABLE" | "DAEMON_TIMEOUT" => format!(
            "{}; start Paseo (or point LVU_PASEO_URL at it) and reopen 🧠. \
             Localhost is the machine/container running lvu; a remotely connected client does not imply it has a daemon. \
             See https://paseo.sh/docs/troubleshooting and https://paseo.sh/docs/connectivity",
            failure.message
        ),
        "PROVIDER_UNKNOWN" | "PROVIDER_UNAVAILABLE" | "PROVIDERS_UNAVAILABLE" => format!(
            "{}; authenticate a provider in Paseo, or choose an authenticated one in Settings",
            failure.message
        ),
        "OWNED_ROOT_UNAVAILABLE" | "OWNED_ROOT_BUSY" => format!(
            "{}; another lvu window may already hold the 🧠 assistance lease — close all lvu windows for this capture root; \
             if none remain, remove only the exact reported bridge.lock after verifying no lvu/bridge process owns it, never the assistance root",
            failure.message
        ),
        "LIMIT_EXCEEDED" => format!(
            "{}; finish or cancel an open 🧠 request first",
            failure.message
        ),
        _ => format!(
            "the agent bridge rejected the request ({}: {})",
            failure.code, failure.message
        ),
    }
}

fn tail(value: &str, max: usize) -> String {
    if value.len() <= max {
        return value.to_owned();
    }
    let mut start = value.len() - max;
    while !value.is_char_boundary(start) {
        start += 1;
    }
    value[start..].to_owned()
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::{fs, os::unix::fs::PermissionsExt, time::Instant};
    use tempfile::TempDir;

    fn fake(script_body: &str, timeout: Duration) -> (TempDir, AgentBridgeHost) {
        let temp = TempDir::new().unwrap();
        let script = temp.path().join("fake-bridge");
        fs::write(&script, format!("#!/bin/sh\nset -eu\n{script_body}\n")).unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
        let host = AgentBridgeHost::launch(AgentBridgeConfig {
            program: "/bin/sh".into(),
            args: vec![script.to_string_lossy().into_owned()],
            cwd: temp.path().to_path_buf(),
            environment: Vec::new(),
            max_line_bytes: 262_144,
            max_pending: 4,
            event_capacity: 2,
            request_timeout: timeout,
            shutdown_timeout: Duration::from_millis(500),
        })
        .unwrap();
        (temp, host)
    }

    fn wait_for_fixture_file(path: &Path) {
        let deadline = Instant::now() + Duration::from_secs(1);
        while !path.exists() {
            assert!(
                Instant::now() < deadline,
                "fixture did not publish handshake {}",
                path.display()
            );
            thread::yield_now();
        }
    }

    const LOOP: &str = r#"
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"request_id":"\([^"]*\)".*/\1/p')
  case "$line" in
    *'"method":"start_session"'*) result='{"session_id":"session-1"}' ;;
    *'"method":"resume_session"'*) result='{"session_id":"session-1","resumed":true}' ;;
    *'"method":"request_proposal"'*) printf '{"schema_version":1,"session_id":"session-1","kind":"proposal_started","request_id":"%s"}\n' "$id"; result='{"proposal":{"kind":"filter","definition":{"schema_version":1,"expression":"x"},"explanation":"synthetic","originating_revision":{"data":"data-1","definition":"definition-1"}}}' ;;
    *) result='{"accepted":true}' ;;
  esac
  printf '{"schema_version":1,"request_id":"%s","ok":true,"result":%s}\n' "$id" "$result"
done
"#;

    /// The user report: every one of these arrived as
    /// `local agent service: bridge is not running`.
    #[test]
    fn every_bridge_failure_mode_names_what_failed_and_what_to_do() {
        // The launcher itself is missing (no node, or no mise in a checkout).
        let missing = AgentBridgeHost::launch(AgentBridgeConfig {
            program: "lvu-no-such-node".into(),
            args: Vec::new(),
            cwd: std::env::temp_dir(),
            environment: Vec::new(),
            max_line_bytes: 4096,
            max_pending: 2,
            event_capacity: 2,
            request_timeout: Duration::from_millis(200),
            shutdown_timeout: Duration::from_millis(200),
        });
        let Err(missing) = missing else {
            panic!("a missing launcher cannot start")
        };
        let message = diagnose(&missing);
        assert!(message.contains("lvu-no-such-node"), "{message}");
        assert!(
            message.contains("not installed or not on PATH"),
            "{message}"
        );
        assert!(message.contains("mise install node@26.8.1"), "{message}");

        // The bridge starts, fails to reach the daemon and exits. lvu sees EOF;
        // the cause is only on the process's stderr.
        let (_temp, host) = fake(
            "echo 'bridge connection failed: Error: connect ECONNREFUSED 127.0.0.1:6767' >&2\nexit 1",
            Duration::from_secs(1),
        );
        let error = wait_for_failure(&host);
        let message = diagnose(&error);
        assert!(
            message.contains("could not reach the Paseo daemon"),
            "{message}"
        );
        assert!(message.contains("ECONNREFUSED"), "{message}");
        assert!(message.contains("start Paseo"), "{message}");

        // The bridge directory exists but nobody built it.
        let (_temp, host) = fake(
            "echo \"node:internal/modules/esm/resolve: Cannot find module 'dist/cli.js'\" >&2\nexit 1",
            Duration::from_secs(1),
        );
        let message = diagnose(&wait_for_failure(&host));
        assert!(message.contains("could not load its own code"), "{message}");
        assert!(
            message.contains("npm --prefix bridge run build"),
            "{message}"
        );

        // The bridge runs, reaches the daemon, and the daemon says why not.
        for (code, needle) in [
            ("DAEMON_UNREACHABLE", "start Paseo"),
            ("PROVIDER_UNAVAILABLE", "authenticate a provider in Paseo"),
            ("PROVIDER_UNKNOWN", "authenticate a provider in Paseo"),
            ("OWNED_ROOT_BUSY", "already hold the 🧠 assistance lease"),
        ] {
            let message = diagnose(&HostError::Bridge(BridgeFailure {
                code: code.to_owned(),
                message: "the agent bridge is running but cannot reach the Paseo daemon (Daemon client closed)".to_owned(),
            }));
            assert!(message.contains(needle), "{code}: {message}");
            assert!(
                !message.contains("bridge is not running"),
                "{code}: {message}"
            );
        }
    }

    /// The reported field failure: startup stderr carries both the generic
    /// `bridge connection failed` wrapper (plus EOF/disconnect lifecycle) and
    /// an owned-route busy line. It must never be reported as daemon
    /// unreachable, with or without the stable marker.
    #[test]
    fn owned_route_busy_is_never_reported_as_daemon_unreachable() {
        let lock = "/tmp/lvu-muse-bridge-route-combined/capture/assistance/bridge.lock";
        let lock_json = serde_json::to_string(lock).expect("lock path quotes");
        // New bridge: stable marker plus the JSON-quoted exact lock path.
        let combined_new = format!(
            "bridge disconnected: bridge stdout reached EOF; bridge connection failed [OWNED_ROOT_BUSY]: \
             Error: OWNED_ROOT_BUSY: owned assistance root is busy or contains a stale bridge.lock at {lock_json}; \
             automatic stale-lock removal is intentionally refused"
        );
        // Legacy bridge: no marker, no lease word — only the busy/stale prose.
        let combined_legacy =
            "bridge disconnected: bridge stdout reached EOF; bridge connection failed: \
             Error: owned assistance root is busy or contains a stale bridge.lock; \
             automatic stale-lock removal is intentionally refused"
                .to_owned();
        // User paraphrase from the field report, concatenating every observed line.
        let combined_user =
            "Agent bridge started but could not reach the Paseo daemon; bridge disconnected; \
             bridge stdout reached EOF; bridge connection failed; \
             owned assistance route is busy or contains a stale bridge.lock"
                .to_owned();
        for combined in [&combined_new, &combined_legacy, &combined_user] {
            let message = not_running_message(combined);
            assert!(
                message.starts_with("the owned assistance route is busy"),
                "owned cause lost (wrong classifier) in: {message}"
            );
            assert!(
                message.contains("assistance lease"),
                "owned cause lost in: {message}"
            );
            assert!(
                message.contains("close all lvu windows"),
                "missing competing-window guidance in: {message}"
            );
            assert!(
                message.contains("remove only")
                    && (message.contains("bridge.lock") || message.contains("exact lock")),
                "missing safe exact-path recovery in: {message}"
            );
            // The daemon classifier must not win: its distinctive topology
            // guidance belongs only to daemon failures, even when the input
            // echoes an older daemon line alongside the owned busy line.
            assert!(
                !message.contains("machine/container running lvu")
                    && !message.contains("paseo daemon status --json"),
                "owned busy misreported as daemon in: {message}"
            );
            assert!(!message.contains("bridge is not running"), "{message}");
        }
        // The exact bounded lock path is preserved in the diagnostic.
        let message = not_running_message(&combined_new);
        assert!(message.contains(lock), "exact lock path lost in: {message}");
        assert_eq!(owned_lock_path(&combined_new).as_deref(), Some(lock));
        assert_eq!(
            owned_lock_path("noise \"/tmp/a b/assistance/bridge.lock\"; more").as_deref(),
            Some("/tmp/a b/assistance/bridge.lock"),
            "quoted paths with spaces must survive intact"
        );
        assert_eq!(
            owned_lock_path("stale bridge.lock with no directory"),
            Some("bridge.lock".to_owned())
        );
    }

    /// Daemon failures stay separate and explain topology: localhost is the
    /// lvu machine/container and LVU_PASEO_URL selects the endpoint.
    #[test]
    fn daemon_failures_name_topology_not_a_competing_window() {
        for stderr in [
            "bridge connection failed [DAEMON_UNREACHABLE]: Error: DAEMON_UNREACHABLE: could not reach the Paseo daemon (Error: connect ECONNREFUSED 127.0.0.1:6767)".to_owned(),
            "bridge connection failed: Error: connect ECONNREFUSED 127.0.0.1:6767".to_owned(),
            "bridge connection failed [DAEMON_TIMEOUT]: Error: DAEMON_TIMEOUT: could not reach the Paseo daemon (Error: operation exceeded 10000ms)".to_owned(),
            "bridge disconnected: bridge stdout reached EOF; bridge connection failed [DAEMON_UNREACHABLE]: Error: Daemon client closed".to_owned(),
            "bridge disconnected: bridge stdout reached EOF; WebSocket connection to ws://127.0.0.1:6767/ws failed".to_owned(),
        ] {
            let message = not_running_message(&stderr);
            assert!(
                message.contains("could not reach the Paseo daemon"),
                "daemon cause lost in: {message}"
            );
            assert!(message.contains("LVU_PASEO_URL"), "{message}");
            assert!(message.contains("machine/container running lvu"), "{message}");
            assert!(
                message.contains("paseo daemon status --json"),
                "missing authoritative status guidance in: {message}"
            );
            assert!(
                message.contains("https://paseo.sh/docs/troubleshooting"),
                "{message}"
            );
            assert!(
                !message.contains("assistance lease"),
                "daemon misreported as owned busy in: {message}"
            );
        }
        // The generic wrapper alone is not daemon evidence.
        let generic = not_running_message(
            "bridge disconnected: bridge stdout reached EOF; bridge connection failed: Error: boom",
        );
        assert!(
            !generic.contains("could not reach the Paseo daemon"),
            "generic wrapper misreported as daemon: {generic}"
        );
        assert!(
            generic.contains("not available"),
            "generic startup failure needs a neutral message: {generic}"
        );
    }

    /// Negative control: unrelated words containing the `lease` substring
    /// (`please`, `released`) must not classify as owned-route busy.
    #[test]
    fn generic_please_is_not_owned_busy() {
        let text = "bridge disconnected: please try again; bridge connection failed: Error: released probe boom";
        assert!(
            !looks_like_owned_root_busy(text),
            "generic 'please/released' misclassified as owned busy"
        );
        assert!(!looks_like_daemon_failure(text));
        let message = not_running_message(text);
        assert!(
            message.starts_with("the agent bridge is not available"),
            "generic failure needs a neutral message: {message}"
        );
        assert!(!message.contains("assistance lease"), "{message}");
        assert!(!message.contains("bridge.lock"), "{message}");
    }

    /// End to end: the actual `OwnedSessionLedger` error for a root containing
    /// spaces, run through the stderr classifier. Guards the real quoting
    /// contract, not just a hand-authored string.
    #[test]
    fn spaced_root_ledger_error_classifies_with_exact_path() {
        let dist =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../bridge/dist/owned_sessions.js");
        if !dist.exists() {
            eprintln!("SKIPPED spaced-root ledger probe: bridge/dist is not built");
            return;
        }
        let temp = TempDir::new().expect("temp root");
        let root = temp
            .path()
            .join("My Logs")
            .join("capture")
            .join("assistance");
        std::fs::create_dir_all(&root).expect("spaced root");
        let lock_path = root.join("bridge.lock");
        // Force EEXIST with a stale well-formed lock the probe must not remove.
        std::fs::write(&lock_path, "{\"pid\":1,\"nonce\":\"stale-probe\"}\n").expect("stale lock");
        let probe = temp.path().join("spaced-lease-probe.mjs");
        let spec = format!("file://{}", dist.display());
        std::fs::write(
            &probe,
            format!(
                "import {{ OwnedSessionLedger }} from {spec:?};\n\
                 const root = process.argv[2];\n\
                 const ledger = new OwnedSessionLedger(root);\n\
                 await ledger.initialize();\n\
                 try {{ await ledger.acquireLease(); console.log(\"UNEXPECTED_SUCCESS\"); process.exit(2); }}\n\
                 catch (e) {{ console.log(`CODE:${{e?.code ?? \"\"}}`); console.log(`MESSAGE:${{e?.message ?? String(e)}}`); }}\n"
            ),
        )
        .expect("probe script");
        let output = std::process::Command::new("node")
            .arg(&probe)
            .arg(&root)
            .output();
        let output = match output {
            Ok(output) => output,
            Err(error) => {
                eprintln!("SKIPPED spaced-root ledger probe: node unavailable ({error})");
                return;
            }
        };
        assert!(
            output.status.success(),
            "ledger probe failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        let code = stdout
            .lines()
            .find_map(|line| line.strip_prefix("CODE:"))
            .unwrap_or("");
        let error_message = stdout
            .lines()
            .find_map(|line| line.strip_prefix("MESSAGE:"))
            .unwrap_or("");
        assert_eq!(code, "OWNED_ROOT_BUSY", "probe stdout:\n{stdout}");
        // The exact lock path is JSON-quoted in the real error.
        let expected_json =
            serde_json::to_string(&lock_path.to_string_lossy()).expect("quote lock");
        assert!(
            error_message.contains(&expected_json),
            "real ledger error lost the quoted path:\n{error_message}"
        );
        // Through the CLI wrapper and the Rust classifier.
        let stderr = format!(
            "bridge disconnected: bridge stdout reached EOF; bridge connection failed [{code}]: Error: {error_message}"
        );
        assert!(looks_like_owned_root_busy(&stderr));
        assert!(!looks_like_daemon_failure(&stderr));
        let expected = lock_path.to_string_lossy().into_owned();
        assert_eq!(owned_lock_path(&stderr).as_deref(), Some(expected.as_str()));
        let message = not_running_message(&stderr);
        assert!(
            message.starts_with("the owned assistance route is busy"),
            "spaced-root error misclassified: {message}"
        );
        assert!(
            message.contains(expected.as_str()),
            "exact spaced path lost: {message}"
        );
        // The stale probe lock was refused, never silently removed.
        assert_eq!(
            std::fs::read_to_string(&lock_path).expect("lock survives"),
            "{\"pid\":1,\"nonce\":\"stale-probe\"}\n"
        );
    }

    /// Submit until the host has observed the child's exit, then return the
    /// error the user's request would have received.
    fn wait_for_failure(host: &AgentBridgeHost) -> HostError {
        // Wait for the exit *and* for stderr to be drained. The first request
        // can lose the race against the write side and see only a broken pipe;
        // the cause is on stderr, so the classifier needs it to have arrived.
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let status = host.status();
            if status.state != HostState::Running && status.stderr.is_some() {
                break;
            }
            // Keep poking it: the host only notices the exit through its pipes.
            if let Ok(request) = host.capabilities() {
                let _ = request.recv_timeout(Duration::from_millis(50));
            }
            assert!(
                Instant::now() < deadline,
                "bridge failure was never observed"
            );
            thread::sleep(Duration::from_millis(20));
        }
        match host.capabilities() {
            Ok(request) => request
                .recv_timeout(Duration::from_millis(200))
                .expect_err("a dead bridge cannot answer"),
            Err(error) => error,
        }
    }

    #[test]
    fn production_command_uses_pinned_mise_node_and_built_bridge() {
        let config = AgentBridgeConfig::mise_bridge(Path::new("/workspace/lvu"));
        assert_eq!(config.program, PathBuf::from("mise"));
        assert_eq!(
            config.args,
            ["exec", "node@26.8.1", "--", "node", "dist/cli.js"]
        );
        assert_eq!(config.cwd, PathBuf::from("/workspace/lvu/bridge"));
    }

    #[test]
    fn session_purpose_is_explicit_on_wire_and_legacy_omits_it() {
        let body = LOOP.replace(
            "result='{\"session_id\":\"session-1\"}'",
            r#"case "$line" in
                *'"purpose":"ask"'*) result='{"session_id":"ask"}' ;;
                *'"purpose":"source_assistance"'*) result='{"session_id":"source_assistance"}' ;;
                *'"purpose":"investigation"'*) result='{"session_id":"investigation"}' ;;
                *'"purpose"'*) result='{"session_id":"invalid"}' ;;
                *) result='{"session_id":"legacy"}' ;;
              esac"#,
        );
        let (_temp, host) = fake(&body, Duration::from_secs(1));
        for (purpose, expected) in [
            (Some(SessionPurpose::Ask), "ask"),
            (Some(SessionPurpose::SourceAssistance), "source_assistance"),
            (Some(SessionPurpose::Investigation), "investigation"),
            (None, "legacy"),
        ] {
            let session = host
                .start_session_with_purpose("fixture", Path::new("/tmp"), None, None, None, purpose)
                .unwrap()
                .recv_timeout(Duration::from_secs(1))
                .unwrap();
            assert_eq!(session, expected);
        }
    }

    #[test]
    fn resume_purpose_is_explicit_on_wire_and_legacy_wrapper_omits_it() {
        let body = LOOP.replace(
            r#"*'"method":"resume_session"'*) result='{"session_id":"session-1","resumed":true}' ;;"#,
            r#"*'"method":"resume_session"'*'"purpose":"investigation"'*) result='{"session_id":"managed-investigation","resumed":true}' ;;
    *'"method":"resume_session"'*'"purpose"'*) result='{"session_id":"invalid","resumed":true}' ;;
    *'"method":"resume_session"'*) result='{"session_id":"legacy","resumed":true}' ;;"#,
        );
        let (_temp, host) = fake(&body, Duration::from_secs(1));
        assert_eq!(
            host.resume_session_with_purpose(
                "managed-investigation",
                Some(SessionPurpose::Investigation),
            )
            .unwrap()
            .recv_timeout(Duration::from_secs(1))
            .unwrap(),
            "managed-investigation"
        );
        assert_eq!(
            host.resume_session("legacy")
                .unwrap()
                .recv_timeout(Duration::from_secs(1))
                .unwrap(),
            "legacy"
        );
    }

    #[test]
    fn actual_wire_smoke_correlates_sessions_cancel_resume_and_typed_proposal() {
        let (_temp, host) = fake(LOOP, Duration::from_secs(1));
        let session = host
            .start_session(
                "codex/gpt-5.6-sol",
                Path::new("/tmp"),
                Some("full-access"),
                Some("medium"),
                None,
            )
            .unwrap()
            .recv_timeout(Duration::from_secs(1))
            .unwrap();
        assert_eq!(session, "session-1");
        host.cancel(&session)
            .unwrap()
            .recv_timeout(Duration::from_secs(1))
            .unwrap();
        assert_eq!(
            host.resume_session(&session)
                .unwrap()
                .recv_timeout(Duration::from_secs(1))
                .unwrap(),
            session
        );
        let revision = OriginatingRevision {
            data: "data-1".into(),
            definition: "definition-1".into(),
        };
        let proposal = host
            .propose(
                &session,
                ProposalKind::Filter,
                "keep records",
                revision.clone(),
                ProposalContext {
                    inline_context: None,
                    inline_context_limit: 32 * 1024,
                    inspection_command: None,
                    manifest_path: "/tmp/snapshot/manifest.json".into(),
                    dataset_paths: vec!["/tmp/snapshot/data.parquet".into()],
                },
            )
            .unwrap()
            .recv_timeout(Duration::from_secs(1))
            .unwrap();
        assert_eq!(proposal.kind, ProposalKind::Filter);
        assert_eq!(proposal.originating_revision, revision);
        assert_eq!(host.poll_event().unwrap().kind, "proposal_started");
        host.shutdown().unwrap();
    }

    #[test]
    fn prepared_context_is_bounded_before_delivery_and_retains_complete_wire_values() {
        let script = LOOP.replace(
            "  case",
            "  printf '%s\\n' \"$line\" > request.json\n  case",
        );
        let (temp, host) = fake(&script, Duration::from_secs(1));
        let revision = OriginatingRevision {
            data: "data-1".into(),
            definition: "definition-1".into(),
        };
        // Sized off the ceiling rather than a literal: the wider sample tier
        // moved it once (`docs/larger-ask-sample.md`) and may move it again.
        // A NUL serialises as the six bytes `\u0000`.
        let limit = 96 * 1024;
        let mut context = ProposalContext {
            inline_context_limit: limit,
            manifest_path: "/tmp/context.json".into(),
            dataset_paths: Vec::new(),
            inline_context: Some(json!({"wide_schema": "\u{0}".repeat(limit / 6 + 1_000)})),
            inspection_command: None,
        };
        assert!(matches!(
            host.propose(
                "session-1",
                ProposalKind::Filter,
                "test",
                revision.clone(),
                context.clone()
            ),
            Err(HostError::Protocol(_))
        ));
        assert!(!temp.path().join("request.json").exists());
        let timestamp = "2026-09-06T12:34:56.123456789+02:00";
        let inline = json!({
            "schemas": {"s1": [{"name": "observed_at", "dtype": "String"}]},
            "rows": [{"observed_at": timestamp}, {"observed_at": null}],
            "coverage": {"sampled": 2, "available": 500}
        });
        context.inline_context = Some(inline.clone());
        context.inspection_command = Some(vec![
            "/usr/bin/python".into(),
            "/tmp/inspect context.py".into(),
        ]);
        host.propose("session-1", ProposalKind::Filter, "test", revision, context)
            .unwrap()
            .recv_timeout(Duration::from_secs(1))
            .unwrap();
        let wire: Value =
            serde_json::from_slice(&fs::read(temp.path().join("request.json")).unwrap()).unwrap();
        assert_eq!(wire["context"]["inline_context"], inline);
        assert_eq!(
            wire["context"]["inspection_command"][1],
            "/tmp/inspect context.py"
        );
        host.shutdown().unwrap();
    }

    #[test]
    fn stale_ids_and_noisy_stderr_do_not_break_correlation() {
        let script = r#"
head -c 131072 /dev/zero >&2
IFS= read -r line
printf '%s\n' '{"schema_version":1,"request_id":"stale","ok":true,"result":{}}'
printf '%s\n' '{"schema_version":1,"session_id":"session-1","kind":"stream","payload":"event"}'
printf '%s\n' '{"schema_version":1,"request_id":"rust-1","ok":true,"result":{"ok":true}}'
sleep 1
"#;
        let (_temp, host) = fake(script, Duration::from_secs(1));
        assert!(
            host.capabilities()
                .unwrap()
                .recv_timeout(Duration::from_secs(1))
                .is_ok()
        );
        assert_eq!(host.poll_event().unwrap().kind, "stream");
        let status = host.status();
        assert!(
            status
                .diagnostic
                .as_deref()
                .is_some_and(|value| !value.is_empty())
        );
    }

    #[test]
    fn critical_lifecycle_event_is_prioritized_when_ordinary_queue_is_saturated() {
        let script = r#"
IFS= read -r line
printf '%s\n' '{"schema_version":1,"session_id":"session-1","kind":"stream","payload":"one"}'
printf '%s\n' '{"schema_version":1,"session_id":"session-1","kind":"stream","payload":"two"}'
printf '%s\n' '{"schema_version":1,"session_id":"session-1","kind":"stream","payload":"dropped"}'
printf '%s\n' '{"schema_version":1,"session_id":"session-1","kind":"session_archived","activity_path":"/activity/session-1.jsonl"}'
id=$(printf '%s' "$line" | sed -n 's/.*"request_id":"\([^"]*\)".*/\1/p')
printf '{"schema_version":1,"request_id":"%s","ok":true,"result":{}}\n' "$id"
sleep 1
"#;
        let (_temp, host) = fake(script, Duration::from_secs(1));
        host.capabilities()
            .unwrap()
            .recv_timeout(Duration::from_secs(1))
            .unwrap();
        let event = host.poll_event().expect("critical lifecycle event");
        assert_eq!(event.kind, "session_archived");
        assert_eq!(event.session_id, "session-1");
        assert_eq!(host.status().dropped_events, 1);
        host.shutdown().unwrap();
    }

    #[test]
    fn critical_lifecycle_queue_overflow_faults_host_instead_of_dropping_ack() {
        // The bridge stays alive past the assertions. A script that exits
        // faults the host for a different reason — stdin disconnected — which
        // would mask the overflow this is about.
        let script = r#"
IFS= read -r line
printf '%s\n' '{"schema_version":1,"session_id":"session-1","kind":"session_archived"}'
printf '%s\n' '{"schema_version":1,"session_id":"session-2","kind":"archive_failed"}'
printf '%s\n' '{"schema_version":1,"session_id":"session-3","kind":"session_archived"}'
sleep 30
"#;
        // Long enough that the reaper can never be what resolves the request:
        // the fault has to be.
        let (_temp, host) = fake(script, Duration::from_secs(120));
        let request = host.capabilities().unwrap();

        // Three critical events into a queue of two faults the host, and
        // `fault_generation` publishes the state and the diagnostic *before* it
        // fails the pending requests. So waiting on the request is a
        // happens-after edge for the state: when this returns, the status is
        // already committed and there is nothing to poll for. Asserting on a
        // deadline instead — one second in the original, a ten-second poll in
        // the first attempt at this fix — was a bet on when a thread ran.
        let error = request
            .recv_timeout(Duration::from_secs(30))
            .expect_err("the fault fails every pending request");
        assert!(
            matches!(error, HostError::NotRunning(_)),
            "pending requests fail with the fault's reason: {error:?}"
        );

        let status = host.status();
        assert_eq!(
            status.state,
            HostState::Faulted,
            "the request failed, so something ended the generation; diagnostic: {:?}",
            status.diagnostic
        );
        assert_eq!(status.dropped_events, 0, "a critical ack is never dropped");
        assert!(
            status.diagnostic.as_deref().is_some_and(|message| {
                message.contains("critical agent lifecycle event queue is full")
                    && message.contains("session-3")
            }),
            "the diagnostic names the event that could not be queued: {:?}",
            status.diagnostic
        );

        // The two that fitted are still there, in arrival order.
        assert_eq!(host.poll_event().unwrap().session_id, "session-1");
        assert_eq!(host.poll_event().unwrap().session_id, "session-2");
        host.shutdown().unwrap();
    }

    #[test]
    fn oversized_output_faults_host_and_settles_pending() {
        let script =
            "IFS= read -r line\nhead -c 2048 /dev/zero | tr '\\000' x\nprintf '\\n'\nsleep 1";
        let (temp, host) = fake(script, Duration::from_secs(1));
        // Re-launch with a deliberately small protocol cap.
        host.shutdown().unwrap();
        let script = temp.path().join("fake-bridge");
        let host = AgentBridgeHost::launch(AgentBridgeConfig {
            program: "/bin/sh".into(),
            args: vec![script.to_string_lossy().into_owned()],
            cwd: temp.path().into(),
            environment: vec![],
            max_line_bytes: 128,
            max_pending: 4,
            event_capacity: 2,
            request_timeout: Duration::from_secs(1),
            shutdown_timeout: Duration::from_millis(500),
        })
        .unwrap();
        assert!(
            host.capabilities()
                .unwrap()
                .recv_timeout(Duration::from_secs(1))
                .is_err()
        );
        assert_eq!(host.status().state, HostState::Faulted);
    }

    #[test]
    fn stalled_stdin_times_out_without_blocking_and_restart_recovers() {
        let large_loop = format!("if [ -e recover ]; then\n{LOOP}\nelse\nsleep 60\nfi");
        let (temp, host) = fake(&large_loop, Duration::from_millis(40));
        let started = Instant::now();
        let request = host.send_prompt("session", &"x".repeat(200_000)).unwrap();
        assert!(started.elapsed() < Duration::from_millis(20));
        assert_eq!(
            request.recv_timeout(Duration::from_secs(1)),
            Err(HostError::Timeout)
        );
        fs::write(temp.path().join("recover"), b"").unwrap();
        host.restart().unwrap();
        assert!(
            host.capabilities()
                .unwrap()
                .recv_timeout(Duration::from_secs(1))
                .is_ok()
        );
    }

    #[test]
    fn eof_settles_request_and_shutdown_reaps_child() {
        let script = r#"
printf '%s' $$ > child.pid
: > ready
while [ ! -e release ]; do sleep 0.001; done
IFS= read -r line
: > admitted
exit 0
"#;
        let (temp, host) = fake(script, Duration::from_secs(1));
        wait_for_fixture_file(&temp.path().join("ready"));
        let request = host.capabilities().unwrap();
        fs::write(temp.path().join("release"), b"").unwrap();
        wait_for_fixture_file(&temp.path().join("admitted"));
        assert!(request.recv_timeout(Duration::from_secs(1)).is_err());
        let pid = fs::read_to_string(temp.path().join("child.pid")).unwrap();
        host.shutdown().unwrap();
        assert!(!Path::new(&format!("/proc/{pid}")).exists());
    }

    #[test]
    fn normal_shutdown_closes_stdin_releases_lease_and_settles_pending_request() {
        let script = r#"
printf '%s' $$ > child.pid
printf 'owned' > bridge.lock
: > ready
IFS= read -r line
: > admitted
while IFS= read -r line; do :; done
rm bridge.lock
: > graceful
"#;
        let (temp, host) = fake(script, Duration::from_secs(1));
        wait_for_fixture_file(&temp.path().join("ready"));
        let request = host.capabilities().unwrap();
        wait_for_fixture_file(&temp.path().join("admitted"));
        let started = Instant::now();
        host.shutdown().unwrap();
        assert!(started.elapsed() <= Duration::from_millis(750));
        assert_eq!(
            request.recv_timeout(Duration::from_millis(50)),
            Err(HostError::NotRunning("bridge shut down".into()))
        );
        assert!(temp.path().join("graceful").exists());
        assert!(!temp.path().join("bridge.lock").exists());
        let pid = fs::read_to_string(temp.path().join("child.pid")).unwrap();
        assert!(!Path::new(&format!("/proc/{pid}")).exists());
    }

    #[test]
    fn no_request_shutdown_during_delayed_startup_reaps_child_and_pipe_workers() {
        let script = r#"
printf '%s' $$ > child.pid
: > ready
# Model the Node CLI awaiting bridge.start() before it installs an stdin
# consumer. Shutdown must not spend the full host budget on this phase and
# then strand the three pipe workers.
sleep 2
while IFS= read -r line; do :; done
"#;
        let (temp, host) = fake(script, Duration::from_secs(1));
        wait_for_fixture_file(&temp.path().join("ready"));
        let pid = fs::read_to_string(temp.path().join("child.pid")).unwrap();
        let started = Instant::now();
        host.shutdown().unwrap();
        assert!(started.elapsed() <= Duration::from_millis(600));
        assert!(!Path::new(&format!("/proc/{pid}")).exists());
        assert!(host.inner.workers.lock().unwrap().is_empty());
    }

    #[test]
    fn shutdown_diagnostic_names_the_unfinished_worker_phase() {
        let (_temp, host) = fake(LOOP, Duration::from_secs(1));
        let generation = host.inner.generation.load(Ordering::Acquire);
        let (release, held) = mpsc::channel();
        let (started, ready) = mpsc::sync_channel(0);
        host.inner.workers.lock().unwrap().push((
            generation,
            "phase probe",
            thread::spawn(move || {
                started.send(()).unwrap();
                held.recv().unwrap();
            }),
        ));
        ready.recv().unwrap();
        let error = join_retired_workers_until(
            &host.inner,
            generation,
            Instant::now() + Duration::from_millis(5),
        )
        .unwrap_err();
        assert!(
            matches!(&error, HostError::Io(message) if message.contains("phase probe")),
            "{error:?}"
        );
        release.send(()).unwrap();
        host.shutdown().unwrap();
    }

    #[test]
    fn graceful_leader_exit_kills_owned_pipe_inheriting_descendant() {
        let script = r#"
printf '%s' $$ > child.pid
printf 'owned' > bridge.lock
(sleep 60) &
printf '%s' $! > descendant.pid
: > ready
while IFS= read -r line; do :; done
rm bridge.lock
: > graceful
exit 0
"#;
        let (temp, host) = fake(script, Duration::from_secs(1));
        wait_for_fixture_file(&temp.path().join("ready"));
        let descendant = fs::read_to_string(temp.path().join("descendant.pid")).unwrap();
        host.shutdown().unwrap();
        assert!(temp.path().join("graceful").exists());
        assert!(!temp.path().join("bridge.lock").exists());
        assert!(!Path::new(&format!("/proc/{descendant}")).exists());
        assert!(host.inner.workers.lock().unwrap().is_empty());
    }

    #[test]
    fn stubborn_shutdown_falls_back_to_owned_group_kill_within_deadline() {
        let script = r#"
printf '%s' $$ > child.pid
printf 'owned' > bridge.lock
: > ready
trap '' TERM
while :; do sleep 1; done
"#;
        let (temp, host) = fake(script, Duration::from_secs(1));
        wait_for_fixture_file(&temp.path().join("ready"));
        let pid = fs::read_to_string(temp.path().join("child.pid")).unwrap();
        let started = Instant::now();
        host.shutdown().unwrap();
        assert!(started.elapsed() <= Duration::from_millis(750));
        assert!(!Path::new(&format!("/proc/{pid}")).exists());
        assert!(
            temp.path().join("bridge.lock").exists(),
            "forced termination must not claim graceful lease cleanup"
        );
    }

    #[test]
    fn immediate_exit_cannot_be_overwritten_by_running_publication() {
        let (_temp, host) = fake("exit 0", Duration::from_secs(1));
        let deadline = Instant::now() + Duration::from_secs(1);
        while host.status().state == HostState::Running && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(host.status().state, HostState::Disconnected);
    }

    #[test]
    fn retired_eof_cannot_fault_restarted_generation_or_drain_its_request() {
        let (_temp, host) = fake(LOOP, Duration::from_secs(1));
        let retired = host.status().generation;
        host.restart().unwrap();
        let request = host.capabilities().unwrap();
        disconnect_generation(&host.inner, retired, "late old EOF");
        assert!(request.recv_timeout(Duration::from_secs(1)).is_ok());
        assert_eq!(host.status().state, HostState::Running);
    }

    #[test]
    fn restart_kills_pipe_inheriting_descendant_and_joins_retired_workers() {
        let first = format!(
            "if [ -e recover ]; then\n{LOOP}\nelse\n(sleep 60) &\necho $! > descendant.pid\nexit 0\nfi"
        );
        let (temp, host) = fake(&first, Duration::from_secs(1));
        let pid_path = temp.path().join("descendant.pid");
        let deadline = Instant::now() + Duration::from_secs(1);
        while !pid_path.exists() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(2));
        }
        let pid = fs::read_to_string(&pid_path).unwrap();
        let pid = pid.trim();
        fs::write(temp.path().join("recover"), b"").unwrap();
        host.restart().unwrap();
        assert!(!Path::new(&format!("/proc/{pid}")).exists());
        assert_eq!(
            host.inner.workers.lock().unwrap().len(),
            4,
            "only the restarted generation's workers remain owned"
        );
        assert!(
            host.capabilities()
                .unwrap()
                .recv_timeout(Duration::from_secs(1))
                .is_ok()
        );
        host.shutdown().unwrap();
        assert!(host.inner.workers.lock().unwrap().is_empty());
    }

    #[test]
    fn malformed_or_mismatched_proposal_is_rejected() {
        let script = r#"
IFS= read -r line
printf '%s\n' '{"schema_version":1,"request_id":"rust-1","ok":true,"result":{"proposal":{"kind":"filter","definition":{"schema_version":1,"expression":""},"explanation":"bad","originating_revision":{"data":"stale","definition":"definition-1"}}}}'
sleep 1
"#;
        let (_temp, host) = fake(script, Duration::from_secs(1));
        let result = host
            .propose(
                "session",
                ProposalKind::Filter,
                "x",
                OriginatingRevision {
                    data: "data-1".into(),
                    definition: "definition-1".into(),
                },
                ProposalContext {
                    inline_context: None,
                    inline_context_limit: 32 * 1024,
                    inspection_command: None,
                    manifest_path: "/tmp/m.json".into(),
                    dataset_paths: vec![],
                },
            )
            .unwrap()
            .recv_timeout(Duration::from_secs(1));
        assert!(matches!(result, Err(HostError::Protocol(_))));
    }
}

#[cfg(test)]
mod source_batch_tests {
    use super::*;
    fn file_source(id: &str, path: &str) -> serde_json::Value {
        serde_json::json!({
            "schema_version": 1, "id": id, "name": "logs",
            "kind": "file", "path": path, "follow": true,
            "identity_hints": {}, "retention": null,
        })
    }
    #[test]
    fn plural_source_batches_are_bounded_with_distinct_identities() {
        let first = file_source("11111111-1111-4111-8111-111111111111", "/tmp/a.log");
        let second = file_source("22222222-2222-4222-8222-222222222222", "/tmp/b.log");
        let batch = serde_json::json!({"schema_version": 1, "sources": [first.clone(), second]});
        assert!(validate_definition(ProposalKind::Sources, &batch).is_ok());
        let single = serde_json::json!({"schema_version": 1, "sources": [first.clone()]});
        assert!(validate_definition(ProposalKind::Sources, &single).is_ok());
        for definition in [
            serde_json::json!({"schema_version": 1, "sources": []}),
            serde_json::json!({
                "schema_version": 1,
                "sources": [first.clone(), file_source("11111111-1111-4111-8111-111111111111", "/tmp/c.log")],
            }),
            serde_json::json!({"schema_version": 1, "sources": [first], "extra": 1}),
            serde_json::json!({"schema_version": 1}),
        ] {
            assert!(
                validate_definition(ProposalKind::Sources, &definition).is_err(),
                "accepted invalid batch: {definition}"
            );
        }
        let overfull = serde_json::json!({
            "schema_version": 1,
            "sources": (0..=MAX_SOURCES_PER_PROPOSAL)
                .map(|index| file_source(&format!("11111111-1111-4111-8000-{index:012}"), "/tmp/a.log"))
                .collect::<Vec<_>>(),
        });
        assert!(validate_definition(ProposalKind::Sources, &overfull).is_err());
    }
    #[test]
    fn singular_legacy_source_answers_a_plural_request_as_one_batch() {
        let revision = OriginatingRevision {
            data: "discovery:2".into(),
            definition: "source-dialog:1".into(),
        };
        let definition = file_source("11111111-1111-4111-8111-111111111111", "/tmp/a.log");
        let wire = serde_json::json!({"proposal": {
            "kind": "source", "definition": definition,
            "explanation": "legacy", "originating_revision": revision,
        }});
        let proposal = validate_proposal(wire, ProposalKind::Sources, &revision).unwrap();
        assert_eq!(proposal.kind, ProposalKind::Sources);
        assert_eq!(proposal.definition["sources"].as_array().unwrap().len(), 1);
        let stale = serde_json::json!({"proposal": {
            "kind": "source",
            "definition": file_source("11111111-1111-4111-8111-111111111111", "/tmp/a.log"),
            "explanation": "legacy",
            "originating_revision": OriginatingRevision { data: "other".into(), definition: "source-dialog:1".into() },
        }});
        assert!(validate_proposal(stale, ProposalKind::Sources, &revision).is_err());
    }
}

#[cfg(test)]
mod inline_recipe_tests {
    use super::*;
    #[test]
    fn inline_recipe_stages_reject_duplicates_and_unrecognized_fields() {
        let mut definition = serde_json::json!({"schema_version":1,"id":"11111111-1111-4111-8111-111111111111","name":"Adapted","source_ids":["22222222-2222-4222-8222-222222222222"],"filter":null,"recipe_stage_revisions":[],"enrichments":[{"id":"existing","source":"/(?P<code>[0-9]+)/"}]});
        assert!(validate_view_definition(definition.as_object().unwrap()));
        definition["enrichments"] =
            serde_json::json!([{"id":"same","source":"x"},{"id":"same","source":"y"}]);
        assert!(!validate_view_definition(definition.as_object().unwrap()));
        definition["enrichments"] = serde_json::json!([{"id":"x","source":"x","command":"bad"}]);
        assert!(!validate_view_definition(definition.as_object().unwrap()));
        definition["enrichments"] = serde_json::json!([]);
        assert!(validate_view_definition(definition.as_object().unwrap()));
    }
}
