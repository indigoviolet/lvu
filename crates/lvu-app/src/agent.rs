//! Bounded, UI-independent host for the local TypeScript Paseo bridge.

use std::{
    collections::HashMap,
    io::{Read, Write},
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
    Filter,
    Enrichment,
    View,
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
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct ProposalEnvelope {
    pub kind: ProposalKind,
    pub definition: Value,
    pub explanation: String,
    pub originating_revision: OriginatingRevision,
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
    dropped_events: u64,
}
struct Inner {
    config: AgentBridgeConfig,
    generation: AtomicU64,
    next_id: AtomicU64,
    closed: AtomicBool,
    writer: Mutex<Option<SyncSender<Vec<u8>>>>,
    child: Mutex<Option<(u64, Child)>>,
    workers: Mutex<Vec<(u64, thread::JoinHandle<()>)>>,
    lifecycle: Mutex<()>,
    pending: Mutex<HashMap<String, Pending>>,
    events_tx: SyncSender<BridgeEvent>,
    status: Mutex<StatusData>,
}

pub struct AgentBridgeHost {
    inner: Arc<Inner>,
    events_rx: Receiver<BridgeEvent>,
}

impl AgentBridgeHost {
    pub fn launch(config: AgentBridgeConfig) -> Result<Self, HostError> {
        if config.max_line_bytes == 0 || config.max_pending == 0 || config.event_capacity == 0 {
            return Err(HostError::Protocol("bridge limits must be positive".into()));
        }
        let (events_tx, events_rx) = mpsc::sync_channel(config.event_capacity);
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
            status: Mutex::new(StatusData {
                state: HostState::Starting,
                diagnostic: None,
                dropped_events: 0,
            }),
        });
        launch_generation(&inner)?;
        Ok(Self { inner, events_rx })
    }

    pub fn status(&self) -> HostStatus {
        let status = self.inner.status.lock().expect("status lock");
        HostStatus {
            state: status.state.clone(),
            generation: self.inner.generation.load(Ordering::Acquire),
            pending: self.inner.pending.lock().expect("pending lock").len(),
            dropped_events: status.dropped_events,
            diagnostic: status.diagnostic.clone(),
        }
    }

    pub fn poll_event(&self) -> Option<BridgeEvent> {
        self.events_rx.try_recv().ok()
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
        let mut request = json!({
            "method": "start_session", "provider": provider,
            "cwd": path_text(cwd)?,
        });
        insert_optional(&mut request, "mode_id", mode_id);
        insert_optional(&mut request, "thinking_option_id", thinking_option_id);
        insert_optional(&mut request, "title", title);
        self.submit(request, |value| required_string(&value, "session_id"))
    }

    pub fn resume_session(&self, session_id: &str) -> Result<Request<String>, HostError> {
        self.submit(
            json!({ "method": "resume_session", "session_id": session_id }),
            |value| required_string(&value, "session_id"),
        )
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
        self.submit(
            json!({
                "method": "request_proposal", "session_id": session_id,
                "kind": kind, "instruction": instruction,
                "originating_revision": revision,
                "context": { "manifest_path": manifest_path, "dataset_paths": dataset_paths },
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
        let cleanup = terminate_current(
            &self.inner,
            HostError::NotRunning("bridge shut down".into()),
        );
        self.inner.status.lock().expect("status lock").state = HostState::Stopped;
        cleanup
    }

    fn submit<T: Send + 'static>(
        &self,
        mut body: Value,
        parse_result: impl FnOnce(Value) -> Result<T, HostError> + Send + 'static,
    ) -> Result<Request<T>, HostError> {
        if self.status().state != HostState::Running {
            return Err(HostError::NotRunning("bridge is not running".into()));
        }
        let request_id = format!(
            "rust-{}",
            self.inner.next_id.fetch_add(1, Ordering::Relaxed)
        );
        let object = body
            .as_object_mut()
            .ok_or_else(|| HostError::Protocol("request must be an object".into()))?;
        object.insert("schema_version".into(), json!(PROTOCOL_VERSION));
        object.insert("request_id".into(), json!(request_id));
        let mut encoded = CappedBuffer::new(self.inner.config.max_line_bytes);
        serde_json::to_writer(&mut encoded, &body)
            .map_err(|_| HostError::Protocol("request exceeds JSONL byte limit".into()))?;
        let mut encoded = encoded.into_inner();
        encoded.push(b'\n');
        if encoded.len() > self.inner.config.max_line_bytes {
            return Err(HostError::Protocol(
                "request exceeds JSONL byte limit".into(),
            ));
        }
        let (tx, rx) = mpsc::sync_channel(1);
        let callback: Parser = Box::new(move |value| {
            let _ = tx.send(value.and_then(parse_result));
        });
        let generation = self.inner.generation.load(Ordering::Acquire);
        {
            let mut pending = self.inner.pending.lock().expect("pending lock");
            if pending.len() >= self.inner.config.max_pending {
                return Err(HostError::Capacity);
            }
            pending.insert(
                request_id.clone(),
                Pending {
                    generation,
                    deadline: Instant::now() + self.inner.config.request_timeout,
                    parse: callback,
                },
            );
        }
        let writer = self.inner.writer.lock().expect("writer lock").clone();
        match writer.map(|writer| writer.try_send(encoded)) {
            Some(Ok(())) => {}
            Some(Err(TrySendError::Full(_))) => {
                fail_one(&self.inner, &request_id, HostError::Capacity);
            }
            Some(Err(TrySendError::Disconnected(_))) | None => {
                fail_one(
                    &self.inner,
                    &request_id,
                    HostError::NotRunning("bridge stdin disconnected".into()),
                );
            }
        }
        Ok(Request { request_id, rx })
    }
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
            let message = error.to_string();
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
    }
    let mut workers = inner.workers.lock().expect("workers lock");
    let weak = Arc::downgrade(inner);
    workers.push((
        generation,
        thread::spawn(move || writer_loop(stdin, writer_rx, weak, generation)),
    ));
    let weak = Arc::downgrade(inner);
    let max_line = inner.config.max_line_bytes;
    workers.push((
        generation,
        thread::spawn(move || stdout_loop(stdout, weak, generation, max_line)),
    ));
    let weak = Arc::downgrade(inner);
    workers.push((
        generation,
        thread::spawn(move || stderr_loop(stderr, weak, generation)),
    ));
    let weak = Arc::downgrade(inner);
    workers.push((
        generation,
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
                    status.diagnostic = Some(tail(&text, 4096));
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
    let event = BridgeEvent {
        session_id: session_id.to_owned(),
        kind: kind.to_owned(),
        payload: value,
    };
    if let Err(TrySendError::Full(_)) = inner.events_tx.try_send(event) {
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

fn fault_generation(inner: &Arc<Inner>, generation: u64, message: &str) {
    if inner.generation.load(Ordering::Acquire) != generation {
        return;
    }
    let lifecycle = inner.lifecycle.lock().expect("lifecycle lock");
    if inner.generation.load(Ordering::Acquire) != generation {
        return;
    }
    {
        let mut status = inner.status.lock().expect("status lock");
        status.state = HostState::Faulted;
        status.diagnostic = Some(tail(message, 4096));
    }
    *inner.writer.lock().expect("writer lock") = None;
    let pending = take_pending_generation(inner, generation);
    let child = take_child(inner, generation);
    drop(lifecycle);
    for pending in pending {
        deliver_error(pending, HostError::NotRunning(message.to_owned()));
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
    {
        let mut status = inner.status.lock().expect("status lock");
        status.state = HostState::Disconnected;
        status.diagnostic = Some(tail(message, 4096));
    }
    *inner.writer.lock().expect("writer lock") = None;
    let pending = take_pending_generation(inner, generation);
    let child = take_child(inner, generation);
    drop(lifecycle);
    for pending in pending {
        deliver_error(pending, HostError::NotRunning(message.to_owned()));
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
                    inner
                        .workers
                        .lock()
                        .expect("workers lock")
                        .push((generation, reaper));
                    break;
                }
            }
        }
    }
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
    let deadline = Instant::now() + inner.config.shutdown_timeout;
    while owned.iter().any(|(_, worker)| !worker.is_finished()) && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(2));
    }
    let mut incomplete = Vec::new();
    for worker in owned {
        if worker.1.is_finished() {
            let _ = worker.1.join();
        } else {
            incomplete.push(worker);
        }
    }
    if incomplete.is_empty() {
        Ok(())
    } else {
        let count = incomplete.len();
        inner
            .workers
            .lock()
            .expect("workers lock")
            .extend(incomplete);
        let message = format!("{count} bridge worker thread(s) exceeded shutdown deadline");
        record_diagnostic(inner, message.clone());
        Err(HostError::Io(message))
    }
}

fn validate_proposal(
    value: Value,
    kind: ProposalKind,
    revision: &OriginatingRevision,
) -> Result<ProposalEnvelope, HostError> {
    let proposal: ProposalEnvelope = serde_json::from_value(
        value
            .get("proposal")
            .cloned()
            .ok_or_else(|| HostError::Protocol("missing proposal".into()))?,
    )
    .map_err(|error| HostError::Protocol(error.to_string()))?;
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
