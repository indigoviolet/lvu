use lvu_core::{CommandDefinition, CommandProgram, RawRecord, RecordId, RestartPolicy};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::{
    collections::{HashMap, HashSet, VecDeque},
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

#[derive(Clone, Debug)]
pub struct Limits {
    pub max_events: usize,
    pub max_outstanding: usize,
    pub max_input_bytes: usize,
    pub max_line_bytes: usize,
    pub max_output_bytes: usize,
    pub max_stderr_bytes: usize,
    pub max_diagnostic_chars: usize,
    pub timeout: Duration,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            max_events: 1024,
            max_outstanding: 1024,
            max_input_bytes: 4 * 1024 * 1024,
            max_line_bytes: 1024 * 1024,
            max_output_bytes: 8 * 1024 * 1024,
            max_stderr_bytes: 32 * 1024,
            max_diagnostic_chars: 512,
            timeout: Duration::from_secs(10),
        }
    }
}
#[derive(Clone, Debug)]
pub struct EnrichmentEvent {
    pub record: RawRecord,
    pub raw: String,
    pub fields: Map<String, Value>,
}
#[derive(Clone, Debug, Eq, Hash, PartialEq, Deserialize, Serialize)]
pub struct EventId {
    pub source_id: String,
    pub sequence: u64,
}
impl From<RecordId> for EventId {
    fn from(id: RecordId) -> Self {
        Self {
            source_id: id.source_id.0.to_string(),
            sequence: id.sequence,
        }
    }
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OutcomeState {
    Ready,
    Error,
}
#[derive(Clone, Debug)]
pub struct EventOutcome {
    pub event: EnrichmentEvent,
    pub state: OutcomeState,
    pub derived: Map<String, Value>,
    pub diagnostics: Vec<Diagnostic>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Diagnostic {
    pub code: String,
    pub message: String,
}
#[derive(Clone, Debug)]
pub struct BatchOutcome {
    pub session: u64,
    pub revision: u64,
    pub events: Vec<EventOutcome>,
    pub diagnostics: Vec<Diagnostic>,
}
#[derive(Debug)]
pub struct AttemptLedger {
    capacity: usize,
    attempted: HashSet<EventId>,
}
impl AttemptLedger {
    pub fn new(capacity: usize) -> Result<Self, RunnerError> {
        if capacity == 0 {
            return Err(RunnerError::InvalidLimits(
                "attempt capacity must be nonzero".into(),
            ));
        }
        Ok(Self {
            capacity,
            attempted: HashSet::new(),
        })
    }
    pub fn contains(&self, id: &EventId) -> bool {
        self.attempted.contains(id)
    }
    pub fn len(&self) -> usize {
        self.attempted.len()
    }
    pub fn is_empty(&self) -> bool {
        self.attempted.is_empty()
    }
    fn remaining(&self) -> usize {
        self.capacity - self.attempted.len()
    }
    pub fn reserve(
        &mut self,
        ids: &HashSet<EventId>,
    ) -> Result<AttemptLedgerReservation, AttemptStoreError> {
        if ids.len() > self.remaining() {
            return Err(AttemptStoreError::new("attempt capacity exceeded"));
        }
        if ids.iter().any(|id| self.attempted.contains(id)) {
            return Err(AttemptStoreError::new(
                "attempt reservation contains an already attempted ID",
            ));
        }
        self.attempted.extend(ids.iter().cloned());
        Ok(AttemptLedgerReservation { ids: ids.clone() })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AttemptLedgerReservation {
    ids: HashSet<EventId>,
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
#[error("{message}")]
pub struct AttemptStoreError {
    pub message: String,
}

impl AttemptStoreError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

/// Durable attempt stores are scoped by their caller to one revision of the
/// command plus all preceding definitions which determine its inputs. They must
/// not be scoped to a live data/view revision: later arrivals remain in the same
/// attempt namespace and cannot reopen old IDs.
///
/// `reserve` must atomically reserve the complete set or return an error. The
/// runner treats every reservation error as ambiguous: it sends no payload and
/// does not assume that the store rolled back. Implementations must impose their
/// own bounded transaction/lock timeout; the runner cannot enforce a deadline
/// while blocked inside a synchronous store method.
pub trait AttemptStore {
    type Reservation;

    fn contains(&mut self, id: &EventId) -> Result<bool, AttemptStoreError>;
    fn remaining_capacity(&mut self) -> Result<usize, AttemptStoreError>;
    fn reserve(
        &mut self,
        ids: &std::collections::HashSet<EventId>,
    ) -> Result<Self::Reservation, AttemptStoreError>;

    /// Persist the final, fully validated batch outcome before the caller can
    /// observe success. A crash after reservation but before this hook leaves a
    /// visible attempted/result-unavailable record which must never be rerun.
    /// Implementations must update only IDs owned by `reservation`; in
    /// particular, already-attempted entries included in a mixed/repeated input
    /// batch must not overwrite an earlier durable Ready result.
    fn persist_outcome(
        &mut self,
        reservation: &Self::Reservation,
        outcome: &BatchOutcome,
    ) -> Result<(), AttemptStoreError>;
}

impl AttemptStore for AttemptLedger {
    type Reservation = AttemptLedgerReservation;

    fn contains(&mut self, id: &EventId) -> Result<bool, AttemptStoreError> {
        Ok(AttemptLedger::contains(self, id))
    }

    fn remaining_capacity(&mut self) -> Result<usize, AttemptStoreError> {
        Ok(self.remaining())
    }

    fn reserve(
        &mut self,
        ids: &std::collections::HashSet<EventId>,
    ) -> Result<Self::Reservation, AttemptStoreError> {
        AttemptLedger::reserve(self, ids)
    }

    fn persist_outcome(
        &mut self,
        reservation: &Self::Reservation,
        _outcome: &BatchOutcome,
    ) -> Result<(), AttemptStoreError> {
        debug_assert!(reservation.ids.iter().all(|id| self.attempted.contains(id)));
        Ok(())
    }
}
#[derive(Debug, Error)]
pub enum RunnerError {
    #[error("only structured executable command enrichment is supported")]
    UnsupportedProgram,
    #[error("invalid limits: {0}")]
    InvalidLimits(String),
    #[error("command spawn failed: {0}")]
    Spawn(#[from] std::io::Error),
    #[error("batch exceeds {0}")]
    BatchLimit(&'static str),
    #[error("command revision mismatch; create a new runner")]
    RevisionMismatch,
    #[error("command enrichment does not apply source restart policies")]
    UnsupportedRestartPolicy,
    #[error("attempt store failed: {0}")]
    AttemptStore(#[from] AttemptStoreError),
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Request<'a> {
    BatchBegin {
        session: u64,
        revision: u64,
        event_count: usize,
    },
    Event {
        session: u64,
        revision: u64,
        event_id: EventId,
        raw: &'a str,
        fields: &'a Map<String, Value>,
    },
    BatchEnd {
        session: u64,
        revision: u64,
    },
}
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Response {
    Event {
        session: u64,
        revision: u64,
        event_id: EventId,
        fields: Map<String, Value>,
    },
    BatchComplete {
        session: u64,
        revision: u64,
    },
}
enum ReadMessage {
    Line(Vec<u8>),
    Oversize,
    Eof,
    Error(String),
}
struct WriteRequest {
    bytes: Vec<u8>,
    ack: mpsc::SyncSender<Result<(), String>>,
}
struct BoundedBuffer {
    bytes: Vec<u8>,
    limit: usize,
}
impl Write for BoundedBuffer {
    fn write(&mut self, input: &[u8]) -> std::io::Result<usize> {
        if self.bytes.len().saturating_add(input.len()) > self.limit {
            return Err(std::io::Error::other("input byte limit exceeded"));
        }
        self.bytes.extend_from_slice(input);
        Ok(input.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
struct Running {
    child: Child,
    pgid: i32,
    writer: mpsc::SyncSender<WriteRequest>,
    responses: mpsc::Receiver<ReadMessage>,
    stderr: Arc<Mutex<VecDeque<u8>>>,
    threads: Vec<JoinHandle<()>>,
}
pub struct CommandEnricher {
    definition: CommandDefinition,
    revision: u64,
    limits: Limits,
    session: AtomicU64,
    running: Option<Running>,
    last_stderr: Vec<u8>,
}
impl CommandEnricher {
    pub fn new(
        definition: CommandDefinition,
        revision: u64,
        limits: Limits,
    ) -> Result<Self, RunnerError> {
        if !matches!(definition.program, CommandProgram::Exec { .. }) {
            return Err(RunnerError::UnsupportedProgram);
        }
        if definition.restart != RestartPolicy::Never {
            return Err(RunnerError::UnsupportedRestartPolicy);
        }
        if limits.max_events == 0
            || limits.max_outstanding == 0
            || limits.max_input_bytes == 0
            || limits.max_line_bytes == 0
            || limits.max_output_bytes == 0
        {
            return Err(RunnerError::InvalidLimits(
                "byte/event limits must be nonzero".into(),
            ));
        }
        Ok(Self {
            definition,
            revision,
            limits,
            session: AtomicU64::new(1),
            running: None,
            last_stderr: Vec::new(),
        })
    }
    pub fn run_batch(
        &mut self,
        revision: u64,
        events: Vec<EnrichmentEvent>,
        cancelled: &AtomicBool,
        attempts: &mut AttemptLedger,
    ) -> Result<BatchOutcome, RunnerError> {
        self.run_batch_with_store(revision, events, cancelled, attempts)
    }

    pub fn run_batch_with_store<S: AttemptStore>(
        &mut self,
        revision: u64,
        events: Vec<EnrichmentEvent>,
        cancelled: &AtomicBool,
        attempts: &mut S,
    ) -> Result<BatchOutcome, RunnerError> {
        if revision != self.revision {
            return Err(RunnerError::RevisionMismatch);
        }
        if events.len() > self.limits.max_events || events.len() > self.limits.max_outstanding {
            return Err(RunnerError::BatchLimit("event/outstanding limit"));
        }
        let deadline = Instant::now() + self.limits.timeout;
        let session = self.session.fetch_add(1, Ordering::Relaxed);
        let mut outcomes: Vec<EventOutcome> = events
            .into_iter()
            .map(|event| EventOutcome {
                event,
                state: OutcomeState::Error,
                derived: Map::new(),
                diagnostics: Vec::new(),
            })
            .collect();
        let mut indexes = HashMap::new();
        for (index, outcome) in outcomes.iter().enumerate() {
            let id = EventId::from(outcome.event.record.record_id);
            if indexes.insert(id, index).is_some() {
                return Err(RunnerError::BatchLimit("unique input IDs"));
            }
        }
        if cancelled.load(Ordering::Acquire) {
            for out in &mut outcomes {
                push_diag(
                    &self.limits,
                    &mut out.diagnostics,
                    "cancelled",
                    "command enrichment cancelled",
                )
            }
            return Ok(BatchOutcome {
                session,
                revision,
                events: outcomes,
                diagnostics: Vec::new(),
            });
        }
        let mut pending = HashSet::new();
        for (id, index) in &indexes {
            if attempts.contains(id)? {
                push_diag(
                    &self.limits,
                    &mut outcomes[*index].diagnostics,
                    "already_attempted",
                    "command result is already attempted and will not rerun",
                )
            } else {
                pending.insert(id.clone());
            }
        }
        if pending.len() > attempts.remaining_capacity()? {
            for id in &pending {
                push_diag(
                    &self.limits,
                    &mut outcomes[indexes[id]].diagnostics,
                    "attempt_capacity",
                    "attempt ledger is full; admission refused without eviction",
                )
            }
            pending.clear();
        }
        if pending.is_empty() {
            return Ok(BatchOutcome {
                session,
                revision,
                events: outcomes,
                diagnostics: Vec::new(),
            });
        }
        let mut payload = BoundedBuffer {
            bytes: Vec::with_capacity(self.limits.max_input_bytes.min(64 * 1024)),
            limit: self.limits.max_input_bytes,
        };
        let write_line =
            |buffer: &mut BoundedBuffer, value: &Request<'_>| -> Result<(), serde_json::Error> {
                serde_json::to_writer(&mut *buffer, value)?;
                buffer.write_all(b"\n").map_err(serde_json::Error::io)
            };
        let serialized = (|| {
            write_line(
                &mut payload,
                &Request::BatchBegin {
                    session,
                    revision,
                    event_count: pending.len(),
                },
            )?;
            // A set tracks admission, but must not randomize delivery order.
            for outcome in &outcomes {
                let id = EventId::from(outcome.event.record.record_id);
                if !pending.contains(&id) {
                    continue;
                }
                write_line(
                    &mut payload,
                    &Request::Event {
                        session,
                        revision,
                        event_id: id,
                        raw: &outcome.event.raw,
                        fields: &outcome.event.fields,
                    },
                )?;
            }
            write_line(&mut payload, &Request::BatchEnd { session, revision })?;
            Ok::<(), serde_json::Error>(())
        })();
        if serialized.is_err() {
            mark_pending(
                &self.limits,
                &mut outcomes,
                &indexes,
                &pending,
                "input_too_large",
                "serialized batch exceeds input byte limit",
            );
            return Ok(BatchOutcome {
                session,
                revision,
                events: outcomes,
                diagnostics: Vec::new(),
            });
        }
        if let Err(error) = self.ensure_running() {
            mark_all(
                &self.limits,
                &mut outcomes,
                "spawn_error",
                &error.to_string(),
            );
            return Ok(BatchOutcome {
                session,
                revision,
                events: outcomes,
                diagnostics: Vec::new(),
            });
        }
        if Instant::now() >= deadline {
            self.reset();
            mark_all(
                &self.limits,
                &mut outcomes,
                "timeout",
                "command startup exceeded batch deadline",
            );
            return Ok(BatchOutcome {
                session,
                revision,
                events: outcomes,
                diagnostics: Vec::new(),
            });
        }
        let reservation = match attempts.reserve(&pending) {
            Ok(reservation) => reservation,
            Err(error) => {
                // The reservation outcome may be ambiguous. Never deliver bytes,
                // and discard the idle child so no later request can inherit state.
                self.reset();
                return Err(error.into());
            }
        };
        // Durable reservation can block. Close the known cancellation/deadline
        // window before handing bytes to the writer. The reservation remains
        // authoritative even though this batch was never delivered.
        let stopped = if cancelled.load(Ordering::Acquire) {
            Some((
                "cancelled",
                "command enrichment cancelled after reservation",
            ))
        } else if Instant::now() >= deadline {
            Some((
                "timeout",
                "command enrichment timed out during attempt reservation",
            ))
        } else {
            None
        };
        if let Some((code, message)) = stopped {
            mark_pending(
                &self.limits,
                &mut outcomes,
                &indexes,
                &pending,
                code,
                message,
            );
            self.reset();
            let outcome = BatchOutcome {
                session,
                revision,
                events: outcomes,
                diagnostics: Vec::new(),
            };
            attempts.persist_outcome(&reservation, &outcome)?;
            return Ok(outcome);
        }
        self.running
            .as_ref()
            .expect("running")
            .stderr
            .lock()
            .expect("stderr mutex")
            .clear();
        let (ack_tx, ack_rx) = mpsc::sync_channel(1);
        if self
            .running
            .as_ref()
            .expect("running")
            .writer
            .send(WriteRequest {
                bytes: payload.bytes,
                ack: ack_tx,
            })
            .is_err()
        {
            self.reset();
            mark_all(
                &self.limits,
                &mut outcomes,
                "process_exit",
                "command stdin closed",
            );
            let outcome = BatchOutcome {
                session,
                revision,
                events: outcomes,
                diagnostics: Vec::new(),
            };
            attempts.persist_outcome(&reservation, &outcome)?;
            return Ok(outcome);
        }
        let mut total_output = 0;
        let mut batch_diagnostics = Vec::new();
        let mut wrote = false;
        let mut poisoned = false;
        let mut completed = false;
        while !completed {
            if cancelled.load(Ordering::Acquire) {
                mark_unfinished(
                    &self.limits,
                    &mut outcomes,
                    &indexes,
                    &pending,
                    "cancelled",
                    "command enrichment cancelled",
                );
                poisoned = true;
                break;
            }
            if Instant::now() >= deadline {
                if pending.is_empty() {
                    push_diag(
                        &self.limits,
                        &mut batch_diagnostics,
                        "missing_completion",
                        "command returned every event but no batch completion frame",
                    );
                }
                mark_unfinished(
                    &self.limits,
                    &mut outcomes,
                    &indexes,
                    &pending,
                    if pending.is_empty() {
                        "missing_completion"
                    } else {
                        "timeout"
                    },
                    "command batch did not complete before deadline",
                );
                poisoned = true;
                break;
            }
            if !wrote {
                match ack_rx.try_recv() {
                    Ok(Ok(())) => wrote = true,
                    Ok(Err(message)) => {
                        mark_pending(
                            &self.limits,
                            &mut outcomes,
                            &indexes,
                            &pending,
                            "input_error",
                            &message,
                        );
                        poisoned = true;
                        break;
                    }
                    Err(mpsc::TryRecvError::Disconnected) => {
                        mark_pending(
                            &self.limits,
                            &mut outcomes,
                            &indexes,
                            &pending,
                            "process_exit",
                            "command writer stopped",
                        );
                        poisoned = true;
                        break;
                    }
                    Err(mpsc::TryRecvError::Empty) => {}
                }
            }
            match self
                .running
                .as_ref()
                .expect("running")
                .responses
                .recv_timeout(Duration::from_millis(5))
            {
                Ok(ReadMessage::Line(line)) => {
                    total_output += line.len();
                    if total_output > self.limits.max_output_bytes {
                        push_diag(
                            &self.limits,
                            &mut batch_diagnostics,
                            "output_too_large",
                            "batch output byte limit exceeded",
                        );
                        mark_pending(
                            &self.limits,
                            &mut outcomes,
                            &indexes,
                            &pending,
                            "output_too_large",
                            "batch output byte limit exceeded",
                        );
                        poisoned = true;
                        break;
                    }
                    let response: Response = match serde_json::from_slice(&line) {
                        Ok(v) => v,
                        Err(e) => {
                            push_diag(
                                &self.limits,
                                &mut batch_diagnostics,
                                "malformed_json",
                                &e.to_string(),
                            );
                            mark_unfinished(
                                &self.limits,
                                &mut outcomes,
                                &indexes,
                                &pending,
                                "malformed_json",
                                "command emitted malformed JSON",
                            );
                            poisoned = true;
                            break;
                        }
                    };
                    let (response_session, response_revision, event_id, fields) = match response {
                        Response::Event {
                            session,
                            revision,
                            event_id,
                            fields,
                        } => (session, revision, event_id, fields),
                        Response::BatchComplete {
                            session: response_session,
                            revision: response_revision,
                        } => {
                            if response_session == session
                                && response_revision == revision
                                && pending.is_empty()
                            {
                                completed = true;
                                continue;
                            }
                            mark_unfinished(
                                &self.limits,
                                &mut outcomes,
                                &indexes,
                                &pending,
                                "protocol_error",
                                "stale or premature batch completion",
                            );
                            poisoned = true;
                            break;
                        }
                    };
                    if response_session != session || response_revision != revision {
                        mark_unfinished(
                            &self.limits,
                            &mut outcomes,
                            &indexes,
                            &pending,
                            "protocol_error",
                            "stale response session/revision",
                        );
                        poisoned = true;
                        break;
                    }
                    let Some(&index) = indexes.get(&event_id) else {
                        push_diag(
                            &self.limits,
                            &mut batch_diagnostics,
                            "unknown_id",
                            "command returned an unknown or late event ID",
                        );
                        mark_unfinished(
                            &self.limits,
                            &mut outcomes,
                            &indexes,
                            &pending,
                            "protocol_error",
                            "unknown response ID poisoned command session",
                        );
                        poisoned = true;
                        break;
                    };
                    if !pending.remove(&event_id) {
                        outcomes[index].state = OutcomeState::Error;
                        outcomes[index].derived.clear();
                        push_diag(
                            &self.limits,
                            &mut outcomes[index].diagnostics,
                            "duplicate_id",
                            "command returned multiple objects for one event",
                        );
                        mark_pending(
                            &self.limits,
                            &mut outcomes,
                            &indexes,
                            &pending,
                            "protocol_error",
                            "duplicate response poisoned command batch",
                        );
                        poisoned = true;
                        break;
                    }
                    if let Some(field) = fields
                        .keys()
                        .find(|name| *name == "raw" || name.starts_with("_lvu_"))
                    {
                        push_diag(
                            &self.limits,
                            &mut outcomes[index].diagnostics,
                            "protected_field",
                            &format!("command cannot write protected field {field:?}"),
                        );
                    } else if let Some(field) = fields
                        .keys()
                        .find(|name| outcomes[index].event.fields.contains_key(*name))
                    {
                        push_diag(
                            &self.limits,
                            &mut outcomes[index].diagnostics,
                            "existing_field",
                            &format!(
                                "command output must be additive and cannot replace input field {field:?}"
                            ),
                        );
                    } else {
                        outcomes[index].state = OutcomeState::Ready;
                        outcomes[index].derived = fields;
                    }
                }
                Ok(ReadMessage::Oversize) => {
                    mark_unfinished(
                        &self.limits,
                        &mut outcomes,
                        &indexes,
                        &pending,
                        "output_too_large",
                        "response line exceeds limit",
                    );
                    poisoned = true;
                    break;
                }
                Ok(ReadMessage::Eof) => {
                    mark_unfinished(
                        &self.limits,
                        &mut outcomes,
                        &indexes,
                        &pending,
                        "process_exit",
                        "command stdout closed before all replies",
                    );
                    poisoned = true;
                    break;
                }
                Ok(ReadMessage::Error(e)) => {
                    mark_unfinished(
                        &self.limits,
                        &mut outcomes,
                        &indexes,
                        &pending,
                        "output_error",
                        &e,
                    );
                    poisoned = true;
                    break;
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    mark_unfinished(
                        &self.limits,
                        &mut outcomes,
                        &indexes,
                        &pending,
                        "process_exit",
                        "command output worker stopped",
                    );
                    poisoned = true;
                    break;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
        }
        if poisoned && batch_diagnostics.is_empty() {
            push_diag(
                &self.limits,
                &mut batch_diagnostics,
                "batch_failed",
                "command batch did not reach a valid completion frame",
            );
        }
        if poisoned {
            self.reset()
        }
        let stderr = self.stderr_snapshot();
        if !stderr.is_empty() {
            let message = String::from_utf8_lossy(&stderr);
            for outcome in &mut outcomes {
                push_diag(
                    &self.limits,
                    &mut outcome.diagnostics,
                    "command_stderr",
                    &message,
                );
            }
        }
        let outcome = BatchOutcome {
            session,
            revision,
            events: outcomes,
            diagnostics: batch_diagnostics,
        };
        if let Err(error) = attempts.persist_outcome(&reservation, &outcome) {
            self.reset();
            return Err(error.into());
        }
        Ok(outcome)
    }
    pub fn stderr_snapshot(&self) -> Vec<u8> {
        self.running
            .as_ref()
            .map(|r| {
                r.stderr
                    .lock()
                    .expect("stderr mutex")
                    .iter()
                    .copied()
                    .collect()
            })
            .unwrap_or_else(|| self.last_stderr.clone())
    }
    fn ensure_running(&mut self) -> Result<(), RunnerError> {
        if self
            .running
            .as_mut()
            .is_some_and(|r| r.child.try_wait().ok().flatten().is_some())
        {
            self.reset()
        }
        if self.running.is_some() {
            return Ok(());
        }
        let CommandProgram::Exec { executable, args } = &self.definition.program else {
            return Err(RunnerError::UnsupportedProgram);
        };
        let mut command = Command::new(executable);
        command.args(args);
        if let Some(cwd) = &self.definition.cwd {
            command.current_dir(cwd);
        }
        command
            .envs(&self.definition.environment)
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
            .ok_or_else(|| std::io::Error::other("missing stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| std::io::Error::other("missing stdout"))?;
        let stderr_pipe = child
            .stderr
            .take()
            .ok_or_else(|| std::io::Error::other("missing stderr"))?;
        let (wtx, wrx) = mpsc::sync_channel(1);
        let writer = thread::spawn(move || writer_loop(stdin, wrx));
        let (rtx, rrx) = mpsc::sync_channel(1);
        let line_limit = self.limits.max_line_bytes;
        let reader = thread::spawn(move || read_stdout(stdout, line_limit, rtx));
        let stderr = Arc::new(Mutex::new(VecDeque::new()));
        let copy = Arc::clone(&stderr);
        let stderr_limit = self.limits.max_stderr_bytes;
        let err = thread::spawn(move || drain(stderr_pipe, copy, stderr_limit));
        self.running = Some(Running {
            child,
            pgid,
            writer: wtx,
            responses: rrx,
            stderr,
            threads: vec![writer, reader, err],
        });
        Ok(())
    }
    fn reset(&mut self) {
        if let Some(r) = self.running.take() {
            let Running {
                mut child,
                pgid,
                writer,
                responses,
                stderr,
                threads,
                ..
            } = r;
            unsafe {
                libc::kill(-pgid, libc::SIGKILL);
            }
            _ = child.wait();
            drop(writer);
            drop(responses);
            for task in threads {
                _ = task.join()
            }
            self.last_stderr = stderr
                .lock()
                .expect("stderr mutex")
                .iter()
                .copied()
                .collect();
        }
    }
}
impl Drop for CommandEnricher {
    fn drop(&mut self) {
        self.reset()
    }
}
fn push_diag(limits: &Limits, out: &mut Vec<Diagnostic>, code: &str, message: &str) {
    out.push(Diagnostic {
        code: code.into(),
        message: message.chars().take(limits.max_diagnostic_chars).collect(),
    })
}
fn mark_all(l: &Limits, out: &mut [EventOutcome], code: &str, msg: &str) {
    for item in out {
        push_diag(l, &mut item.diagnostics, code, msg)
    }
}
fn mark_pending(
    l: &Limits,
    out: &mut [EventOutcome],
    indexes: &HashMap<EventId, usize>,
    pending: &HashSet<EventId>,
    code: &str,
    msg: &str,
) {
    for id in pending {
        push_diag(l, &mut out[indexes[id]].diagnostics, code, msg)
    }
}
fn mark_unfinished(
    l: &Limits,
    out: &mut [EventOutcome],
    indexes: &HashMap<EventId, usize>,
    pending: &HashSet<EventId>,
    code: &str,
    msg: &str,
) {
    mark_pending(l, out, indexes, pending, code, msg)
}
fn writer_loop(mut stdin: impl Write, rx: mpsc::Receiver<WriteRequest>) {
    for req in rx {
        let result = stdin
            .write_all(&req.bytes)
            .and_then(|_| stdin.flush())
            .map_err(|e| e.to_string());
        _ = req.ack.send(result)
    }
}
fn read_stdout(stdout: impl Read, limit: usize, tx: mpsc::SyncSender<ReadMessage>) {
    let mut reader = BufReader::new(stdout);
    loop {
        let mut line = Vec::new();
        loop {
            let available = match reader.fill_buf() {
                Ok(v) => v,
                Err(e) => {
                    _ = tx.send(ReadMessage::Error(e.to_string()));
                    return;
                }
            };
            if available.is_empty() {
                _ = tx.send(if line.is_empty() {
                    ReadMessage::Eof
                } else {
                    ReadMessage::Line(line)
                });
                return;
            }
            let newline = available.iter().position(|b| *b == b'\n');
            let used = newline.map_or(available.len(), |p| p + 1);
            if line.len().saturating_add(used) > limit {
                _ = tx.send(ReadMessage::Oversize);
                return;
            }
            line.extend_from_slice(&available[..used]);
            reader.consume(used);
            if newline.is_some() {
                line.pop();
                if tx.send(ReadMessage::Line(line)).is_err() {
                    return;
                }
                break;
            }
        }
    }
}
fn drain(mut input: impl Read, target: Arc<Mutex<VecDeque<u8>>>, limit: usize) {
    let mut chunk = [0u8; 4096];
    while let Ok(n) = input.read(&mut chunk) {
        if n == 0 {
            break;
        }
        let mut out = target.lock().expect("stderr mutex");
        out.extend(&chunk[..n]);
        while out.len() > limit {
            out.pop_front();
        }
    }
}
