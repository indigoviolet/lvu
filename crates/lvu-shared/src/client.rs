//! Window-side shared-worker client: attach-or-spawn, typed requests, and
//! bounded save-drain shutdown.
//!
//! One `attach` call takes a window from cold start to a welcomed worker
//! connection: ensure a worker is elected (spawning the child when the
//! election is empty), connect, handshake, and hold the viewer lock for the
//! attachment lifetime. Requests are strictly one-at-a-time per connection —
//! the client awaits each reply before sending the next — so every
//! `Store` reply is unambiguously the answer to the outstanding request and
//! no demultiplexing state exists to desync.
//!
//! Bounds (mirroring the worker side): sends pass through the frame cap;
//! every wait carries an explicit deadline; shutdown drains with `Flush`
//! then `Goodbye` and never blocks past its bound. Errors are strings, per
//! crate convention. Stdin forwarding (chunk/credit flow) is not driven
//! here yet: `request_start` surfaces `StdinBound` explicitly so a caller
//! can never mistake it for a file capture.

use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use lvu_core::{SourceDefinition, SourceId};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
};

#[cfg(unix)]
use crate::spawn::pre_exec_detach;
use crate::{
    FrameDecoder, SourceSummary, StoreEvent, StoreMethod, WorkerEvent, WorkerRequest,
    election::{ViewerGuard, WorkerPaths, owner_is_live, take_viewer_lock, try_take_owner},
    protocol::{PROTOCOL_VERSION, UnionCommitStatus, check_store_size},
    spawn::{CAPTURE_ROOT_ARG, SOCKET_ARG, SpawnSpec, WORKER_CHILD_FLAG},
    union_commit::{CommitDigest, CommitReceipt, CommitRequest},
};

/// Client bound for one store round trip. Exceeds the worker's own
/// `request_timeout` (30 s) so a worker-side timeout (outcome-unknown)
/// arrives as data before this fires; only a wedged connection trips it.
pub const STORE_ROUNDTRIP_TIMEOUT: Duration = Duration::from_secs(60);

/// Client bound for control round trips (start/stop/restart): same
/// reasoning as [`STORE_ROUNDTRIP_TIMEOUT`], one shared bound.
pub const CONTROL_ROUNDTRIP_TIMEOUT: Duration = Duration::from_secs(60);

/// Total bound for [`WorkerClient::attach`]: one full handshake plus a
/// replacement election and child startup when the first attempt meets a
/// stale socket or a still-starting worker.
pub const ATTACH_TIMEOUT: Duration = Duration::from_secs(10);

/// Cap on remembered ambiguous request ids (see `stale_request_ids`).
/// Attempts are deadline-bounded seconds apart, so reaching this means a
/// genuinely ancient reply; retiring then is correct, not a leak.
const MAX_STALE_REQUEST_IDS: usize = 64;

/// How a connect attempt failed: transport trouble (retryable within
/// [`ATTACH_TIMEOUT`]) or an explicit worker refusal / reply-shape skew
/// (deterministic, never retried).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConnectError {
    Transport(String),
    Refused(String),
}

impl std::fmt::Display for ConnectError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConnectError::Transport(reason) | ConnectError::Refused(reason) => {
                write!(formatter, "{reason}")
            }
        }
    }
}

/// Default window identity: `window-<pid>`, matching the convention the
/// worker tests and presence reporting use. Unique per window process.
pub fn default_window_id() -> String {
    format!("window-{}", std::process::id())
}

/// Outcome of an explicit acquisition request. Both a fresh start and a
/// re-presentation of a live capture arrive as `Started` (the worker uses
/// one shape for both); either way the capture is live at the journal path
/// and needs no distinction here.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StartOutcome {
    Started {
        source_id: SourceId,
        journal_path: PathBuf,
        warning: Option<String>,
    },
    /// The worker bound a stdin stream to this connection. Chunk driving
    /// is not implemented in this client yet; the caller must refuse or
    /// handle the acquisition explicitly rather than proceed as if capture
    /// were a file.
    StdinBound {
        source_id: SourceId,
        chunk_bytes: u32,
    },
}

/// Ensure exactly one worker is elected for `capture_root`, spawning the
/// child when the election is empty, and return the control socket path.
/// The returned path may still be a stale socket file (crashed owner) or
/// pre-bind: the child unlinks the stale file after winning the election,
/// and callers confirm liveness with a handshake, never the path alone.
/// Bounded by [`crate::WORKER_HANDSHAKE_TIMEOUT`]; races between two
/// spawners resolve through the election (the loser attaches).
pub async fn ensure_worker(executable: &Path, capture_root: &Path) -> Result<PathBuf, String> {
    let paths = WorkerPaths::new(capture_root);
    paths
        .ensure_directories()
        .map_err(|error| format!("worker directories: {error}"))?;
    let socket_path = paths.socket_path();
    let deadline = Instant::now() + crate::WORKER_HANDSHAKE_TIMEOUT;
    loop {
        match try_take_owner(&paths) {
            Ok(Some(_guard)) => {
                // Won: drop immediately (the child takes the election
                // itself; holding it here would force an INCUMBENT exit)
                // and spawn.
                spawn_child(executable, capture_root, &paths)?;
            }
            Ok(None) => {}
            Err(error) => return Err(format!("worker election I/O: {error}")),
        }
        if socket_path.exists() {
            return Ok(socket_path);
        }
        if Instant::now() >= deadline {
            let live = owner_is_live(&paths)
                .map_err(|error| format!("worker election I/O while giving up: {error}"))?;
            return Err(if live {
                format!(
                    "worker holds the election but {} never appeared within {:?}",
                    socket_path.display(),
                    crate::WORKER_HANDSHAKE_TIMEOUT
                )
            } else {
                format!(
                    "no worker elected for {} within {:?}",
                    capture_root.display(),
                    crate::WORKER_HANDSHAKE_TIMEOUT
                )
            });
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// Spawn the worker child detached with null stdio: closing any window can
/// neither block on nor signal the worker through stdio. Stderr appends to
/// the bounded worker log so startup failures leave a trace. The argv tail
/// is exactly [`SpawnSpec::argv`] minus the executable.
#[cfg(unix)]
fn spawn_child(executable: &Path, capture_root: &Path, paths: &WorkerPaths) -> Result<(), String> {
    use std::os::unix::process::CommandExt;
    let spec = SpawnSpec::new(executable, capture_root, &paths.socket_path());
    let mut argv = spec.argv();
    argv.remove(0);
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(paths.worker_log())
        .map_err(|error| format!("worker log: {error}"))?;
    let mut command = std::process::Command::new(executable);
    command
        .args(&argv)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(log);
    // SAFETY: `pre_exec_detach` only calls async-signal-safe `setsid`, and
    // `Command::pre_exec` runs it post-fork pre-exec with no other threads.
    // The closure body sits textually inside this block, which is what
    // covers the unsafe call.
    unsafe {
        command.pre_exec(|| pre_exec_detach());
    }
    command
        .spawn()
        .map_err(|error| format!("spawn worker child: {error}"))?;
    Ok(())
}

#[cfg(not(unix))]
fn spawn_child(executable: &Path, capture_root: &Path, paths: &WorkerPaths) -> Result<(), String> {
    let _ = (executable, capture_root, paths);
    Err("shared capture spawning needs a Unix detach port".into())
}

/// A window's connection to its worker: strictly sequential requests,
/// viewer lock held for the whole attachment (dropping detaches).
///
/// Exchange discipline (a shifted ack wedges every later request, so this
/// is fail-closed): at most one exchange is ever in flight (`in_flight`
/// is set before the first await and cleared only by a fully matched
/// reply); every reply's correlation id is validated against the
/// outstanding request; any mismatch, timeout, cancellation residue, or
/// unexpected frame retires the connection (`retired`) instead of leaving
/// a possibly-poisoned stream reusable. A retired client reports a clear
/// error on every later operation; recovery is a fresh attach, which the
/// election makes cheap. Refusals that carry the matching id are clean
/// answers, not faults: they neither retire nor disturb reuse.
///
/// Exception: the union commit/status calls use a recoverable exchange
/// (see `store_recoverable`) whose timeouts record the outstanding id in
/// `stale_request_ids` and leave the connection usable, so a lost reply's
/// ambiguity is resolved by status recovery on the same connection
/// instead of a fresh attach. A late reply carrying a remembered stale id
/// is drained, never mistaken for a later answer; an id that was never
/// issued still retires, preserving the no-shifted-ack invariant.
pub struct WorkerClient {
    reader: BufReader<tokio::net::unix::OwnedReadHalf>,
    writer: tokio::net::unix::OwnedWriteHalf,
    decoder: FrameDecoder,
    window_id: String,
    worker_pid: u32,
    worker_session: String,
    next_request: u64,
    read_buf: Vec<u8>,
    _viewer: ViewerGuard,
    retired: bool,
    in_flight: bool,
    /// Request ids whose attempts timed out under a recoverable exchange
    /// and whose late replies must be drained, not matched. Request ids
    /// are unique per connection (`next_request` never repeats), so a
    /// remembered id can only ever name its own superseded attempt.
    stale_request_ids: Vec<String>,
}

impl WorkerClient {
    /// Attach-or-spawn then connect: the full cold-start flow in one call.
    /// The first attempt is one ensure plus one full-handshake connect.
    /// Transport failures after that (stale socket from a dead owner, or a
    /// replacement still binding) re-ensure and retry with short attempts
    /// until [`ATTACH_TIMEOUT`]: each round re-runs the election, so a dead
    /// owner yields a freshly spawned worker whose first act is unlinking
    /// the stale file. Explicit refusals and reply-shape skew return
    /// immediately — retrying those would only burn the deadline.
    /// `window_pid` identifies this window's viewer slot and handshake;
    /// pass `std::process::id()` unless driving several logical windows
    /// from one process (as integration tests do).
    pub async fn attach(
        executable: &Path,
        capture_root: &Path,
        window_id: &str,
        window_pid: u32,
    ) -> Result<(Self, Vec<SourceSummary>), String> {
        let start = Instant::now();
        let deadline = start + ATTACH_TIMEOUT;
        let socket = ensure_worker(executable, capture_root).await?;
        match Self::connect(capture_root, &socket, window_id, window_pid).await {
            Ok(attached) => Ok(attached),
            Err(ConnectError::Refused(reason)) => Err(reason),
            Err(ConnectError::Transport(first)) => {
                let mut last = first;
                while Instant::now() < deadline {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    let socket = match ensure_worker(executable, capture_root).await {
                        Ok(socket) => socket,
                        Err(error) => {
                            last = error;
                            continue;
                        }
                    };
                    match Self::connect_with_timeout(
                        capture_root,
                        &socket,
                        window_id,
                        window_pid,
                        Duration::from_secs(1),
                    )
                    .await
                    {
                        Ok(attached) => return Ok(attached),
                        Err(ConnectError::Refused(reason)) => return Err(reason),
                        Err(ConnectError::Transport(error)) => last = error,
                    }
                }
                Err(format!(
                    "worker attach failed within {ATTACH_TIMEOUT:?}: {last}"
                ))
            }
        }
    }

    /// Connect to a known-live socket and handshake. The viewer lock is
    /// taken first so a crash between hello and lock can never strand an
    /// anonymous attachment.
    pub async fn connect(
        capture_root: &Path,
        socket_path: &Path,
        window_id: &str,
        window_pid: u32,
    ) -> Result<(Self, Vec<SourceSummary>), ConnectError> {
        Self::connect_with_timeout(
            capture_root,
            socket_path,
            window_id,
            window_pid,
            crate::WORKER_HANDSHAKE_TIMEOUT,
        )
        .await
    }

    async fn connect_with_timeout(
        capture_root: &Path,
        socket_path: &Path,
        window_id: &str,
        window_pid: u32,
        timeout: Duration,
    ) -> Result<(Self, Vec<SourceSummary>), ConnectError> {
        use ConnectError::{Refused, Transport};
        if window_id.len() > crate::MAX_WINDOW_ID_BYTES {
            return Err(Refused(format!(
                "window id is {} bytes; limit is {}",
                window_id.len(),
                crate::MAX_WINDOW_ID_BYTES
            )));
        }
        let paths = WorkerPaths::new(capture_root);
        let viewer = take_viewer_lock(&paths, window_pid)
            .map_err(|error| Transport(format!("viewer lock: {error}")))?;
        let stream = tokio::time::timeout(timeout, UnixStream::connect(socket_path))
            .await
            .map_err(|_| {
                Transport(format!(
                    "no worker answered at {} within {timeout:?}",
                    socket_path.display()
                ))
            })?
            .map_err(|error| Transport(format!("connect {}: {error}", socket_path.display())))?;
        let (reader, writer) = stream.into_split();
        let mut client = Self {
            reader: BufReader::new(reader),
            writer,
            decoder: FrameDecoder::new(),
            window_id: window_id.to_owned(),
            worker_pid: 0,
            worker_session: String::new(),
            next_request: 0,
            read_buf: vec![0u8; crate::READ_CHUNK_BYTES],
            _viewer: viewer,
            retired: false,
            in_flight: false,
            stale_request_ids: Vec::new(),
        };
        let request_id = client.take_request_id();
        let expected = request_id.clone();
        let reply = client
            .roundtrip(
                WorkerRequest::Hello {
                    request_id,
                    window_pid,
                    window_id: window_id.to_owned(),
                    protocol: PROTOCOL_VERSION,
                },
                timeout,
            )
            .await
            .map_err(Transport)?;
        match primary(&reply, &expected) {
            Some(WorkerEvent::Welcome {
                worker_pid,
                worker_session,
                protocol,
                sources,
                ..
            }) if *protocol == PROTOCOL_VERSION => {
                client.worker_pid = *worker_pid;
                client.worker_session = worker_session.clone();
                Ok((client, sources.clone()))
            }
            Some(WorkerEvent::Welcome { protocol, .. }) => Err(Refused(format!(
                "worker speaks protocol {protocol}, this client speaks {PROTOCOL_VERSION}"
            ))),
            Some(WorkerEvent::Refused { reason, .. }) => {
                Err(Refused(format!("worker refused attach: {reason}")))
            }
            _ => Err(Refused(format!("unexpected attach reply: {reply:?}"))),
        }
    }

    /// The worker process id from the handshake welcome. Useful for
    /// diagnostics (and for proving a replacement worker took over).
    pub fn worker_pid(&self) -> u32 {
        self.worker_pid
    }

    /// The worker lifetime nonce from the handshake welcome. Progress
    /// answers are checked against it (see `poll_progress`).
    pub fn worker_session(&self) -> &str {
        &self.worker_session
    }

    fn take_request_id(&mut self) -> String {
        let id = format!("{}-{}", self.window_id, self.next_request);
        self.next_request += 1;
        id
    }

    /// Open an exchange: refuse retired clients outright, and refuse a new
    /// exchange while a previous one never completed (its late reply may
    /// still be in the stream — typically a cancelled or timed-out await
    /// that ran past us). The stuck case retires first: the stream cannot
    /// be proven clean, so it is not reused.
    async fn begin_exchange(&mut self) -> Result<(), String> {
        if self.retired {
            return Err(
                "worker connection retired after a transport fault; re-attach for a fresh one"
                    .into(),
            );
        }
        if self.in_flight {
            return Err(self
                .retire(
                    "previous exchange never completed (cancelled or lost without reply); \
                     its late answer may still be in the stream, so the connection is retired",
                )
                .await);
        }
        self.in_flight = true;
        Ok(())
    }

    /// Retire the connection: mark unusable, drop the in-flight claim,
    /// and best-effort FIN the socket so the worker releases the viewer
    /// promptly instead of at client drop. Returns the message it was
    /// given, so call sites read `return Err(self.retire(reason).await)`.
    async fn retire(&mut self, reason: impl Into<String>) -> String {
        let reason = reason.into();
        self.retired = true;
        self.in_flight = false;
        let _ = self.writer.shutdown().await;
        reason
    }

    /// Send one request value and collect its reply events, bounded. The
    /// frame cap is enforced on send; a reply that cannot be framed on the
    /// worker side fails the connection there, which surfaces here as EOF.
    /// Replies written back-to-back (a result plus its warning) may split
    /// across reads, so after the first events a short grace keeps reading
    /// for stragglers instead of dropping a warning silently.
    ///
    /// The returned batch always contains the matching primary reply (see
    /// `check_batch`): anything else — timeout, EOF, a foreign id, an
    /// unprompted kind — retires the connection instead of handing back a
    /// possibly-shifted answer.
    async fn roundtrip(
        &mut self,
        request: WorkerRequest,
        timeout: Duration,
    ) -> Result<Vec<WorkerEvent>, String> {
        let expected = request_request_id(&request).to_owned();
        let bytes = crate::encode_frame(
            &serde_json::to_value(&request)
                .map_err(|error| format!("encode worker request: {error}"))?,
        )
        .map_err(|error| format!("worker request exceeds the wire cap: {error}"))?;
        // Encoding happens before the exchange opens: nothing was sent, so
        // a failure here leaves the stream clean and reusable.
        self.begin_exchange().await?;
        let deadline = Instant::now() + timeout;
        if let Err(error) = self.writer.write_all(&bytes).await {
            return Err(self.retire(format!("write worker request: {error}")).await);
        }
        let mut events = Vec::new();
        let mut closed = false;
        loop {
            if Instant::now() >= deadline {
                break;
            }
            // Once events have arrived, only wait a grace for stragglers.
            let idle = if events.is_empty() {
                deadline - Instant::now()
            } else {
                (deadline - Instant::now()).min(Duration::from_millis(100))
            };
            match tokio::time::timeout(idle, self.reader.read(&mut self.read_buf)).await {
                // Grace elapsed with no stragglers: reply complete.
                Err(_) => break,
                Ok(Ok(0)) => {
                    closed = true;
                    break;
                }
                Ok(Err(error)) => {
                    return Err(self.retire(format!("read worker reply: {error}")).await);
                }
                Ok(Ok(count)) => {
                    let decoded = match self.decoder.push_bytes(&self.read_buf[..count]) {
                        Err(error) => {
                            return Err(self.retire(format!("decode worker reply: {error}")).await);
                        }
                        Ok(values) => values,
                    };
                    for value in decoded {
                        match serde_json::from_value::<WorkerEvent>(value) {
                            Err(error) => {
                                return Err(self
                                    .retire(format!("unexpected worker reply shape: {error}"))
                                    .await);
                            }
                            Ok(event) => events.push(event),
                        }
                    }
                }
            }
        }
        if events.is_empty() {
            return Err(self
                .retire(if closed {
                    "worker closed the connection".to_owned()
                } else {
                    format!("worker request timed out after {timeout:?}; connection retired")
                })
                .await);
        }
        if let Err(fault) = check_batch(&events, &expected) {
            return Err(self.retire(fault).await);
        }
        self.in_flight = false;
        Ok(events)
    }

    /// Explicit user-approved acquisition. The definition travels as its
    /// canonical DTO value; the worker parses and admits it.
    pub async fn request_start(
        &mut self,
        definition: &SourceDefinition,
    ) -> Result<StartOutcome, String> {
        let request_id = self.take_request_id();
        let expected = request_id.clone();
        let definition = serde_json::to_value(definition)
            .map_err(|error| format!("encode source definition: {error}"))?;
        let events = self
            .roundtrip(
                WorkerRequest::RequestStart {
                    request_id,
                    definition,
                },
                CONTROL_ROUNDTRIP_TIMEOUT,
            )
            .await?;
        let mut warning = None;
        for event in &events {
            if let WorkerEvent::ShutdownNotice { reason } = event {
                warning = Some(reason.clone());
            }
        }
        match primary(&events, &expected) {
            Some(WorkerEvent::Started {
                source_id,
                journal_path,
                ..
            }) => {
                let source_id = parse_source_id(source_id)?;
                let journal_path = PathBuf::from(journal_path);
                // A `Present` answer carries no warning event; it is
                // indistinguishable here from a fresh start, and needs no
                // distinction: either way the capture is live at the path.
                Ok(StartOutcome::Started {
                    source_id,
                    journal_path,
                    warning,
                })
            }
            Some(WorkerEvent::StdinOpen {
                source_id,
                chunk_bytes,
                ..
            }) => Ok(StartOutcome::StdinBound {
                source_id: parse_source_id(source_id)?,
                chunk_bytes: *chunk_bytes,
            }),
            Some(WorkerEvent::Refused { reason, .. }) => Err(reason.clone()),
            other => Err(format!("unexpected start reply: {other:?}")),
        }
    }

    /// Explicit stop of a running capture.
    pub async fn request_stop(&mut self, source_id: SourceId) -> Result<(), String> {
        let request_id = self.take_request_id();
        let expected = request_id.clone();
        let events = self
            .roundtrip(
                WorkerRequest::RequestStop {
                    request_id,
                    source_id: source_id.0.to_string(),
                },
                CONTROL_ROUNDTRIP_TIMEOUT,
            )
            .await?;
        match primary(&events, &expected) {
            Some(WorkerEvent::Stopped { .. }) => Ok(()),
            Some(WorkerEvent::Refused { reason, .. }) => Err(reason.clone()),
            other => Err(format!("unexpected stop reply: {other:?}")),
        }
    }

    /// Explicit restart of a stopped capture. The worker restarts the
    /// remembered definition; this never invents one.
    pub async fn request_restart(&mut self, source_id: SourceId) -> Result<StartOutcome, String> {
        let request_id = self.take_request_id();
        let expected = request_id.clone();
        let events = self
            .roundtrip(
                WorkerRequest::RequestRestart {
                    request_id,
                    source_id: source_id.0.to_string(),
                },
                CONTROL_ROUNDTRIP_TIMEOUT,
            )
            .await?;
        match primary(&events, &expected) {
            Some(WorkerEvent::Started {
                source_id,
                journal_path,
                ..
            }) => Ok(StartOutcome::Started {
                source_id: parse_source_id(source_id)?,
                journal_path: PathBuf::from(journal_path),
                warning: None,
            }),
            Some(WorkerEvent::Refused { reason, .. }) => Err(reason.clone()),
            other => Err(format!("unexpected restart reply: {other:?}")),
        }
    }

    /// Submit one union commit attempt: one request, one receipt, under the
    /// caller's per-attempt bound (the transport partitions the window's
    /// absolute deadline across attempts; see the recovery loop that calls
    /// this). The request travels verbatim — same nonce and digest on every
    /// replay, never a fresh identity. The exchange is recoverable: a
    /// transport timeout records the attempt as stale and reports outcome
    /// unknown WITHOUT retiring, which is exactly the ambiguity status
    /// recovery resolves on this same connection.
    pub async fn request_union_commit(
        &mut self,
        window_id: &str,
        request: &CommitRequest,
        timeout: Duration,
    ) -> Result<CommitReceipt, String> {
        let request_id = self.take_request_id();
        let method = StoreMethod::UnionCommit {
            request_id,
            window_id: window_id.to_owned(),
            request: request.clone(),
        };
        match self.store_recoverable(method, timeout).await? {
            StoreEvent::UnionCommitted { receipt, .. } => Ok(receipt),
            StoreEvent::UnionStatus { status, .. } => Err(format!(
                "union commit answered status instead of receipt: {status:?}"
            )),
            other => Err(format!("unexpected union commit reply: {other:?}")),
        }
    }

    /// Read-only status for one exact attempt, under the caller's
    /// per-attempt bound. Never mutates worker state; `Unknown` means the
    /// attempt was never admitted (replay the identical request). The
    /// exchange is recoverable like the commit call, so a lost status
    /// reply degrades to another poll rather than a retired connection.
    pub async fn request_union_status(
        &mut self,
        window_id: &str,
        union_view_id: &str,
        candidate_generation: u64,
        nonce: &str,
        digest: &CommitDigest,
        timeout: Duration,
    ) -> Result<UnionCommitStatus, String> {
        let request_id = self.take_request_id();
        let method = StoreMethod::UnionStatus {
            request_id,
            window_id: window_id.to_owned(),
            union_view_id: union_view_id.to_owned(),
            candidate_generation,
            nonce: nonce.to_owned(),
            digest: *digest,
        };
        match self.store_recoverable(method, timeout).await? {
            StoreEvent::UnionStatus { status, .. } => Ok(status),
            StoreEvent::UnionCommitted { receipt, .. } => Err(format!(
                "union status answered receipt instead of status: {receipt:?}"
            )),
            other => Err(format!("unexpected union status reply: {other:?}")),
        }
    }

    /// Poll one source's canonical progress: one request, one reply, same
    /// strict sequential discipline as every other call (progress is never
    /// pushed, so no unsolicited frame can arrive mid-request). The answer
    /// is validated before it is returned: the source identity must match
    /// the requested source, and the worker session must match the handshake
    /// session — a mismatch means a confused peer or a replaced worker, and
    /// reads as a violation rather than an update. Unknown or not-live
    /// sources come back as worker refusals. Feeders poll per their own
    /// cadence (bounded staleness is the poller's policy); every answer is
    /// sampled live, so staleness never exceeds the poll interval.
    pub async fn poll_progress(
        &mut self,
        source_id: SourceId,
    ) -> Result<lvu_ingest::SourceProgress, String> {
        let request_id = self.take_request_id();
        let expected = request_id.clone();
        let events = self
            .roundtrip(
                WorkerRequest::RequestProgress {
                    request_id,
                    source_id: source_id.0.to_string(),
                },
                crate::WORKER_HANDSHAKE_TIMEOUT,
            )
            .await?;
        match primary(&events, &expected) {
            Some(WorkerEvent::SourceProgress {
                worker_session,
                progress,
                ..
            }) => {
                if progress.source_id != source_id {
                    return Err(self
                        .retire(format!(
                            "worker answered progress for another source: {}; connection retired",
                            progress.source_id.0
                        ))
                        .await);
                }
                if worker_session != &self.worker_session {
                    return Err(self
                        .retire(
                            "worker session changed mid-connection; re-attach instead of \
                             caching: connection retired",
                        )
                        .await);
                }
                Ok(progress.clone())
            }
            Some(WorkerEvent::Refused { reason, .. }) => Err(reason.clone()),
            other => Err(format!("unexpected progress reply: {other:?}")),
        }
    }

    /// One mediated store call: size-checked before sending (the wire cap
    /// would refuse it anyway, but the local error names the method), then
    /// a single `Store` reply under the store bound. The payload is the
    /// canonical DTO the worker maps 1:1 onto `memory::Command` — sequence,
    /// definition, view, and expected version travel verbatim so CAS
    /// semantics are the worker's, not a second evaluator's.
    pub async fn store(&mut self, method: StoreMethod) -> Result<StoreEvent, String> {
        self.store_with_timeout(method, STORE_ROUNDTRIP_TIMEOUT)
            .await
    }

    /// Same exchange with a caller-chosen bound, for deadline-partitioned
    /// callers (union commit/status recovery splits the window's absolute
    /// deadline across attempts). Timeout behavior is identical to
    /// [`Self::store`], reporting outcome-unknown and retiring on timeout.
    pub async fn store_with_timeout(
        &mut self,
        method: StoreMethod,
        timeout: Duration,
    ) -> Result<StoreEvent, String> {
        let window_id = self.window_id.clone();
        let mut method = method;
        set_store_window(&mut method, &window_id);
        if let Err(error) = check_store_size(&method) {
            return Err(error.to_string());
        }
        let expected = method.request_id().to_owned();
        let bytes = crate::encode_frame(
            &serde_json::to_value(&method)
                .map_err(|error| format!("encode store method: {error}"))?,
        )
        .map_err(|error| format!("store method exceeds the wire cap: {error}"))?;
        // Encoding and size checks happen before the exchange opens:
        // nothing was sent, so a failure here leaves the stream clean.
        self.begin_exchange().await?;
        let deadline = Instant::now() + timeout;
        if let Err(error) = self.writer.write_all(&bytes).await {
            return Err(self.retire(format!("write store method: {error}")).await);
        }
        loop {
            if Instant::now() >= deadline {
                // Outcome unknown, never failure: the worker may still
                // commit after our bound. The late answer must not meet a
                // later request, so the connection retires with the report.
                return Err(self
                    .retire(format!(
                        "store request timed out after {timeout:?}; \
                         outcome unknown — the worker may still commit, reload to reconcile; \
                         connection retired"
                    ))
                    .await);
            }
            let remaining = deadline - Instant::now();
            let count =
                match tokio::time::timeout(remaining, self.reader.read(&mut self.read_buf)).await {
                    Err(_) => {
                        return Err(self
                            .retire(format!(
                                "store request timed out after {timeout:?}; \
                             outcome unknown — the worker may still commit, reload to reconcile; \
                             connection retired"
                            ))
                            .await);
                    }
                    Ok(Err(error)) => {
                        return Err(self.retire(format!("read store reply: {error}")).await);
                    }
                    Ok(Ok(count)) => count,
                };
            if count == 0 {
                return Err(self
                    .retire("worker closed the connection mid-request")
                    .await);
            }
            let decoded = match self.decoder.push_bytes(&self.read_buf[..count]) {
                Err(error) => {
                    return Err(self.retire(format!("decode store reply: {error}")).await);
                }
                Ok(values) => values,
            };
            let mut decoded = decoded.into_iter();
            let Some(value) = decoded.next() else {
                // Partial frame: keep reading.
                continue;
            };
            if decoded.next().is_some() {
                return Err(self
                    .retire("worker sent an unsolicited batch mid-request")
                    .await);
            }
            let event: WorkerEvent = match serde_json::from_value(value) {
                Err(error) => {
                    return Err(self
                        .retire(format!("unexpected store reply shape: {error}"))
                        .await);
                }
                Ok(event) => event,
            };
            // Exactly one frame answers one request here: anything else —
            // a foreign id, a second kind, or a catastrophic Fatal that
            // carries no correlation at all — retires instead of risking a
            // shifted ack (a shifted Saved sequence would wedge every
            // later acknowledgement).
            match event {
                WorkerEvent::Store(StoreEvent::Fatal { reason }) => {
                    return Err(self
                        .retire(format!("worker reported fatal mid-request: {reason}"))
                        .await);
                }
                WorkerEvent::Store(store) => match store_event_request_id(&store) {
                    Some(id) if id == expected => {
                        self.in_flight = false;
                        return Ok(store);
                    }
                    Some(id) => {
                        return Err(self
                            .retire(format!(
                                "store reply id mismatch: expected {expected}, got {id}; \
                                 connection retired"
                            ))
                            .await);
                    }
                    None => {
                        return Err(self
                            .retire("store reply without correlation; connection retired")
                            .await);
                    }
                },
                other => {
                    return Err(self
                        .retire(format!("unexpected event mid-request: {other:?}"))
                        .await);
                }
            }
        }
    }

    /// Remember one ambiguous request id, keeping the set bounded: the
    /// oldest entry is dropped past the cap. Dropping only risks retiring
    /// on a genuinely ancient late reply (dozens of attempts old), which
    /// is the safe direction — never a shifted ack.
    fn remember_stale(&mut self, request_id: &str) {
        if self.stale_request_ids.len() >= MAX_STALE_REQUEST_IDS {
            self.stale_request_ids.remove(0);
        }
        self.stale_request_ids.push(request_id.to_owned());
    }

    /// Recoverable store exchange for union commit/status recovery: same
    /// wire shape and per-attempt bound as [`Self::store_with_timeout`],
    /// but a timeout records the outstanding id as stale and returns an
    /// outcome-unknown error WITHOUT retiring, so the caller can resolve
    /// the ambiguity with status polls on this same connection. A later
    /// reply carrying a remembered stale id is drained (its outcome is
    /// re-derived, never trusted); a reply with an id that was never
    /// issued still retires, exactly as in the strict exchange. Fatal,
    /// EOF, decode, shape, and batch faults retire: only timeouts and
    /// remembered-stale replies are survivable, because only those carry
    /// no evidence of stream confusion.
    async fn store_recoverable(
        &mut self,
        method: StoreMethod,
        timeout: Duration,
    ) -> Result<StoreEvent, String> {
        let window_id = self.window_id.clone();
        let mut method = method;
        set_store_window(&mut method, &window_id);
        if let Err(error) = check_store_size(&method) {
            return Err(error.to_string());
        }
        let expected = method.request_id().to_owned();
        let bytes = crate::encode_frame(
            &serde_json::to_value(&method)
                .map_err(|error| format!("encode store method: {error}"))?,
        )
        .map_err(|error| format!("store method exceeds the wire cap: {error}"))?;
        // Encoding and size checks happen before the exchange opens:
        // nothing was sent, so a failure here leaves the stream clean.
        self.begin_exchange().await?;
        let deadline = Instant::now() + timeout;
        if let Err(error) = self.writer.write_all(&bytes).await {
            return Err(self.retire(format!("write store method: {error}")).await);
        }
        // Outcome-unknown without retiring: the worker may still settle
        // this attempt late. Its id joins the stale set so the late reply
        // is drained by this or a later exchange, never matched; the
        // in-flight claim is released so status recovery can proceed on
        // this same connection.
        macro_rules! ambiguous {
            () => {{
                self.remember_stale(&expected);
                self.in_flight = false;
                return Err(format!(
                    "store request timed out after {timeout:?}; outcome unknown — \
                     the worker may still settle, resolve with a status poll on \
                     this same connection (attempt id remembered as stale)"
                ));
            }};
        }
        loop {
            if Instant::now() >= deadline {
                ambiguous!();
            }
            let remaining = deadline - Instant::now();
            let count =
                match tokio::time::timeout(remaining, self.reader.read(&mut self.read_buf)).await {
                    Err(_) => {
                        ambiguous!();
                    }
                    Ok(Err(error)) => {
                        return Err(self.retire(format!("read store reply: {error}")).await);
                    }
                    Ok(Ok(count)) => count,
                };
            if count == 0 {
                return Err(self
                    .retire("worker closed the connection mid-request")
                    .await);
            }
            let decoded = match self.decoder.push_bytes(&self.read_buf[..count]) {
                Err(error) => {
                    return Err(self.retire(format!("decode store reply: {error}")).await);
                }
                Ok(values) => values,
            };
            if decoded.is_empty() {
                // Partial frame: keep reading.
                continue;
            }
            // A superseded attempt's late answer may share a read with this
            // attempt's reply (the worker writes back-to-back; the socket
            // coalesces). Drain remembered-stale frames wherever they land
            // in the batch; what remains must be exactly one reply, as in
            // the strict exchange — anything else is genuine confusion.
            let mut rest = Vec::with_capacity(decoded.len());
            for value in decoded {
                let event: WorkerEvent = match serde_json::from_value(value) {
                    Err(error) => {
                        return Err(self
                            .retire(format!("unexpected store reply shape: {error}"))
                            .await);
                    }
                    Ok(event) => event,
                };
                match &event {
                    WorkerEvent::Store(store) => match store_event_request_id(store) {
                        Some(id)
                            if id != expected
                                && self.stale_request_ids.iter().any(|stale| stale == id) =>
                        {
                            // A superseded attempt's late answer: its
                            // outcome is being re-derived by the recovery
                            // loop, so this copy is stale information,
                            // never a match. Drain it.
                            continue;
                        }
                        _ => rest.push(event),
                    },
                    _ => rest.push(event),
                }
            }
            if rest.is_empty() {
                // Only stale frames so far: keep waiting within the bound.
                continue;
            }
            if rest.len() > 1 {
                return Err(self
                    .retire("worker sent an unsolicited batch mid-request")
                    .await);
            }
            let event = rest.into_iter().next().expect("single rest event");
            // Exactly one frame answers one request here, as in the strict
            // exchange — except a reply naming a remembered stale attempt
            // is drained (above) and the wait continues within this bound.
            match event {
                WorkerEvent::Store(StoreEvent::Fatal { reason }) => {
                    return Err(self
                        .retire(format!("worker reported fatal mid-request: {reason}"))
                        .await);
                }
                WorkerEvent::Store(store) => match store_event_request_id(&store) {
                    Some(id) if id == expected => {
                        self.in_flight = false;
                        return Ok(store);
                    }
                    // Stale ids never reach here (drained from the batch
                    // above): any other id is foreign, exactly as strict.
                    Some(id) => {
                        return Err(self
                            .retire(format!(
                                "store reply id mismatch: expected {expected}, got {id}; \
                                 connection retired"
                            ))
                            .await);
                    }
                    None => {
                        return Err(self
                            .retire("store reply without correlation; connection retired")
                            .await);
                    }
                },
                other => {
                    return Err(self
                        .retire(format!("unexpected event mid-request: {other:?}"))
                        .await);
                }
            }
        }
    }

    /// Save-drain step one: `Flush` and await its acknowledgement. Every
    /// request already committed or failed explicitly, so this is the
    /// durability handshake before detach, not a queue drain.
    pub async fn flush(&mut self) -> Result<(), String> {
        let request_id = self.take_request_id();
        let window_id = self.window_id.clone();
        match self
            .store(StoreMethod::Flush {
                request_id,
                window_id,
            })
            .await?
        {
            StoreEvent::Flushed { .. } => Ok(()),
            StoreEvent::FlushFailed { reason, .. } => Err(reason),
            other => Err(format!("unexpected flush reply: {other:?}")),
        }
    }

    /// Bounded shutdown drain: flush first, and a flush failure returns
    /// BEFORE goodbye is sent — a failed drain does not detach, so the
    /// caller keeps the client and can surface or retry instead of
    /// abandoning state the worker may still hold unwritten. On flush
    /// success, say goodbye and wait for the close (bounded), then drop
    /// the viewer lock by consuming `self`. Detach needs no goodbye: the
    /// worker removes the viewer on EOF either way, so dropping the client
    /// (or crashing) detaches — the wait here is courtesy, not consensus.
    pub async fn shutdown(mut self) -> Result<(), String> {
        self.detach().await
    }

    /// Bounded shutdown drain without consuming the client: flush, say
    /// goodbye, wait for the close (bounded). For shared ownership, where
    /// feeders and store calls hold the same client behind a mutex and no
    /// single owner can drop it: after `detach` the connection is politely
    /// closed server-side, and dropping the last clone finishes locally.
    /// A flush failure returns before goodbye, exactly like `shutdown`.
    pub async fn detach(&mut self) -> Result<(), String> {
        self.flush().await?;
        let request_id = self.take_request_id();
        let bytes = crate::encode_frame(
            &serde_json::to_value(WorkerRequest::Goodbye { request_id })
                .map_err(|error| format!("encode goodbye: {error}"))?,
        )
        .map_err(|error| format!("goodbye exceeds the wire cap: {error}"))?;
        self.writer
            .write_all(&bytes)
            .await
            .map_err(|error| format!("write goodbye: {error}"))?;
        let deadline = Instant::now() + crate::WORKER_HANDSHAKE_TIMEOUT;
        loop {
            if Instant::now() >= deadline {
                break;
            }
            let remaining = deadline - Instant::now();
            match tokio::time::timeout(remaining, self.reader.read(&mut self.read_buf)).await {
                Ok(Ok(0)) => break,
                Ok(Ok(_)) => continue,
                Ok(Err(_)) | Err(_) => break,
            }
        }
        Ok(())
    }
}

/// Correlation id of an outbound request. Every request variant carries
/// one; replies echo it, and the client refuses answers that do not match
/// the outstanding exchange.
fn request_request_id(request: &WorkerRequest) -> &str {
    match request {
        WorkerRequest::Hello { request_id, .. }
        | WorkerRequest::Goodbye { request_id, .. }
        | WorkerRequest::RequestStart { request_id, .. }
        | WorkerRequest::RequestStop { request_id, .. }
        | WorkerRequest::RequestRestart { request_id, .. }
        | WorkerRequest::StatusSubscribe { request_id, .. }
        | WorkerRequest::StdinChunk { request_id, .. }
        | WorkerRequest::RequestProgress { request_id, .. }
        | WorkerRequest::StdinClose { request_id, .. } => request_id,
    }
}

/// Correlation id of a store reply, if it carries one. Only `Fatal` (a
/// worker-level catastrophe, never one request's answer) has none.
/// Deliberately exhaustive over new variants: anything added later must be
/// classified here as correlating or not, never silently inherit.
fn store_event_request_id(event: &StoreEvent) -> Option<&str> {
    match event {
        StoreEvent::Loaded { request_id, .. }
        | StoreEvent::LoadFailed { request_id, .. }
        | StoreEvent::Saved { request_id, .. }
        | StoreEvent::SaveFailed { request_id, .. }
        | StoreEvent::DerivedViewCreated { request_id, .. }
        | StoreEvent::Recent { request_id, .. }
        | StoreEvent::RecentFailed { request_id, .. }
        | StoreEvent::Recipes { request_id, .. }
        | StoreEvent::RecipeHistory { request_id, .. }
        | StoreEvent::RecipeSaved { request_id, .. }
        | StoreEvent::RecipeExported { request_id, .. }
        | StoreEvent::RecipeFailed { request_id, .. }
        | StoreEvent::SuggestionRecorded { request_id, .. }
        | StoreEvent::SuggestionFailed { request_id, .. }
        | StoreEvent::Flushed { request_id, .. }
        | StoreEvent::FlushFailed { request_id, .. }
        | StoreEvent::UnionCommitted { request_id, .. }
        | StoreEvent::UnionStatus { request_id, .. } => Some(request_id),
        StoreEvent::Fatal { .. } => None,
    }
}

/// Correlation id of a worker event, if it carries one. Ancillary events
/// (`ShutdownNotice`) ride reply batches without ids; `Store` delegates to
/// the inner reply. Exhaustive for the same reason as above.
fn event_request_id(event: &WorkerEvent) -> Option<&str> {
    match event {
        WorkerEvent::Welcome { request_id, .. }
        | WorkerEvent::Started { request_id, .. }
        | WorkerEvent::Refused { request_id, .. }
        | WorkerEvent::Stopped { request_id, .. }
        | WorkerEvent::StdinOpen { request_id, .. }
        | WorkerEvent::SourceProgress { request_id, .. } => Some(request_id),
        WorkerEvent::Store(inner) => store_event_request_id(inner),
        WorkerEvent::ShutdownNotice { .. }
        | WorkerEvent::SourceStatus { .. }
        | WorkerEvent::StdinCredit { .. } => None,
    }
}

/// Verify one collected reply batch against the outstanding request id.
/// Returns `Ok` only when exactly the matching primary is present:
/// ancillary `ShutdownNotice` events ride along unchecked, but a foreign
/// id, a second primary, or an event kind that can never answer a control
/// request (`Store`, `SourceStatus`, `StdinCredit`) is a transport fault.
/// Callers retire on `Err` — the stream cannot be proven clean.
fn check_batch(events: &[WorkerEvent], expected: &str) -> Result<(), String> {
    let mut matched = false;
    for event in events {
        match event {
            WorkerEvent::ShutdownNotice { .. } => {}
            WorkerEvent::Store(_)
            | WorkerEvent::SourceStatus { .. }
            | WorkerEvent::StdinCredit { .. } => {
                return Err(format!(
                    "worker sent an unprompted {event:?} mid-request; connection retired"
                ));
            }
            other => match event_request_id(other) {
                Some(id) if id == expected => {
                    if matched {
                        return Err(format!(
                            "worker sent two primaries for {expected}; connection retired"
                        ));
                    }
                    matched = true;
                }
                Some(id) => {
                    return Err(format!(
                        "reply id mismatch: expected {expected}, got {id}; connection retired"
                    ));
                }
                None => {
                    return Err(format!(
                        "worker sent an uncorrelated {event:?} mid-request; connection retired"
                    ));
                }
            },
        }
    }
    if matched {
        Ok(())
    } else {
        Err(format!(
            "worker reply carried no answer to {expected}; connection retired"
        ))
    }
}

/// The primary answer within a validated batch: the first event carrying
/// the expected id. `check_batch` already guaranteed one exists; callers
/// match its kind (a valid primary of the wrong kind for this operation
/// is a logic surprise, not transport poison, so it stays reusable).
fn primary<'a>(events: &'a [WorkerEvent], expected: &str) -> Option<&'a WorkerEvent> {
    events
        .iter()
        .find(|event| event_request_id(event) == Some(expected))
}

/// Stamp the attached window identity onto an outbound store method. The
/// worker refuses mismatches before any store work, so the client must
/// never forward a method carrying another window's id.
fn set_store_window(method: &mut StoreMethod, window_id: &str) {
    match method {
        StoreMethod::Load {
            window_id: slot, ..
        }
        | StoreMethod::Save {
            window_id: slot, ..
        }
        | StoreMethod::CreateDerivedView {
            window_id: slot, ..
        }
        | StoreMethod::Recent {
            window_id: slot, ..
        }
        | StoreMethod::ListRecipes {
            window_id: slot, ..
        }
        | StoreMethod::RecipeHistory {
            window_id: slot, ..
        }
        | StoreMethod::SaveRecipe {
            window_id: slot, ..
        }
        | StoreMethod::ImportRecipe {
            window_id: slot, ..
        }
        | StoreMethod::ExportRecipe {
            window_id: slot, ..
        }
        | StoreMethod::RecordSuggestion {
            window_id: slot, ..
        }
        | StoreMethod::Flush {
            window_id: slot, ..
        }
        | StoreMethod::UnionCommit {
            window_id: slot, ..
        }
        | StoreMethod::UnionStatus {
            window_id: slot, ..
        } => *slot = window_id.to_owned(),
    }
}

fn parse_source_id(raw: &str) -> Result<SourceId, String> {
    uuid::Uuid::parse_str(raw)
        .map(SourceId)
        .map_err(|error| format!("worker returned an invalid source id: {error}"))
}

/// Build the exact argv tail `ensure_worker` passes to the child, sharing
/// the contract with [`SpawnSpec::argv`] (which includes the executable).
/// Exported so wiring and tests assert one shape, never two.
pub fn child_argv_tail(capture_root: &Path, socket_path: &Path) -> Vec<OsString> {
    vec![
        OsString::from(WORKER_CHILD_FLAG),
        OsString::from(CAPTURE_ROOT_ARG),
        capture_root.as_os_str().to_os_string(),
        OsString::from(SOCKET_ARG),
        socket_path.as_os_str().to_os_string(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn child_argv_tail_matches_spawn_spec_shape() {
        let spec = SpawnSpec::new(
            Path::new("/usr/bin/lvu"),
            Path::new("/data/cap"),
            Path::new("/data/cap/shared-worker/control.sock"),
        );
        assert_eq!(
            spec.argv()[1..],
            child_argv_tail(
                Path::new("/data/cap"),
                Path::new("/data/cap/shared-worker/control.sock")
            )
        );
    }

    #[test]
    fn store_window_stamp_covers_every_method() {
        let meta = crate::RequestMeta {
            request_id: 1,
            dialog_id: 2,
            dialog_revision: 3,
        };
        let definition = SourceDefinition {
            schema_version: 1,
            id: SourceId(uuid::Uuid::from_u128(1)),
            name: "x".into(),
            acquisition: lvu_core::Acquisition::Stdin,
            identity_hints: Default::default(),
            retention: None,
        };
        let mut methods = vec![
            StoreMethod::Recent {
                request_id: "r".into(),
                window_id: "foreign".into(),
            },
            StoreMethod::Flush {
                request_id: "r".into(),
                window_id: "foreign".into(),
            },
            StoreMethod::Load {
                request_id: "r".into(),
                window_id: "foreign".into(),
                definition: definition.clone(),
                view_id: lvu_core::ViewId(uuid::Uuid::from_u128(2)),
            },
            StoreMethod::RecordSuggestion {
                request_id: "r".into(),
                window_id: "foreign".into(),
                outcome: crate::SuggestionOutcomeShape {
                    source_id: "s".into(),
                    recipe_id: "r".into(),
                    revision: "v".into(),
                    accepted: true,
                },
            },
            StoreMethod::ListRecipes {
                request_id: "r".into(),
                window_id: "foreign".into(),
                meta,
                context: None,
            },
        ];
        for method in &mut methods {
            set_store_window(method, "mine");
            assert_eq!(method.window_id(), "mine");
        }
    }

    /// A scripted peer speaking just enough worker to drive handshake and
    /// progress polls. Each script entry builds one reply from the incoming
    /// request's id (echoing is what a correct peer does); entries that
    /// need a wrong id for violation coverage ignore it explicitly. Lets
    /// the validation rules fail deterministically without a worker.
    async fn scripted_peer(
        listener: tokio::net::UnixListener,
        replies: Vec<Box<dyn FnOnce(String) -> WorkerEvent + Send>>,
    ) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let (stream, _) = listener.accept().await.expect("peer accepts");
        let (reader, mut writer) = stream.into_split();
        let mut reader = tokio::io::BufReader::new(reader);
        let mut decoder = FrameDecoder::new();
        let mut buffer = vec![0u8; crate::READ_CHUNK_BYTES];
        let mut replies = replies.into_iter();
        loop {
            let count = reader.read(&mut buffer).await.expect("peer reads");
            if count == 0 {
                return;
            }
            let values = decoder.push_bytes(&buffer[..count]).expect("peer decodes");
            for value in values {
                let id = value
                    .get("request_id")
                    .and_then(|id| id.as_str())
                    .unwrap_or("")
                    .to_owned();
                let Some(make) = replies.next() else {
                    return;
                };
                let reply = make(id);
                let wire =
                    crate::encode_frame(&serde_json::to_value(&reply).expect("peer encodes"))
                        .expect("peer frames");
                writer.write_all(&wire).await.expect("peer writes");
            }
        }
    }

    fn peer_progress(source_id: SourceId, generation: u64) -> lvu_ingest::SourceProgress {
        lvu_ingest::SourceProgress {
            source_id,
            generation,
            state: lvu_ingest::RuntimeState::Running,
            records: 9,
            high_watermark: None,
            journal_bytes: 0,
            synced_records: 0,
            syncs: 0,
            handovers: 0,
            writer_cpu_nanos: 0,
            reader_cpu_nanos: 0,
            boundaries: 0,
            exit_code: None,
            discarded_bytes: 0,
            discarded_bytes_known: true,
            last_error: None,
        }
    }

    fn welcome_reply(id: String) -> WorkerEvent {
        WorkerEvent::Welcome {
            request_id: id,
            worker_pid: 1,
            protocol: crate::protocol::PROTOCOL_VERSION,
            worker_session: "session-test".into(),
            sources: Vec::new(),
        }
    }

    fn progress_reply(
        session: &str,
        progress: lvu_ingest::SourceProgress,
    ) -> Box<dyn FnOnce(String) -> WorkerEvent + Send> {
        let session = session.to_owned();
        Box::new(move |id| WorkerEvent::SourceProgress {
            request_id: id,
            worker_session: session,
            progress,
        })
    }

    async fn connected_peer(
        root: &std::path::Path,
        socket: &std::path::Path,
        window: &str,
        pid: u32,
        script: Vec<Box<dyn FnOnce(String) -> WorkerEvent + Send>>,
    ) -> WorkerClient {
        let listener = tokio::net::UnixListener::bind(socket).unwrap();
        tokio::spawn(scripted_peer(listener, script));
        let (client, _) = WorkerClient::connect(root, socket, window, pid)
            .await
            .expect("connect");
        assert_eq!(client.worker_session(), "session-test");
        client
    }

    #[tokio::test]
    async fn poll_progress_validates_identity_and_session() {
        let root = tempfile::tempdir().unwrap();
        crate::election::WorkerPaths::new(root.path())
            .ensure_directories()
            .expect("viewer directories");
        let socket = root.path().join("control.sock");
        let wanted = SourceId(uuid::Uuid::from_u128(701));
        let other = SourceId(uuid::Uuid::from_u128(702));
        // Correct content first (proves the happy path through validation),
        // then a foreign source: refused as a violation, not cached.
        let script: Vec<Box<dyn FnOnce(String) -> WorkerEvent + Send>> = vec![
            Box::new(welcome_reply),
            progress_reply("session-test", peer_progress(wanted, 3)),
            progress_reply("session-test", peer_progress(other, 3)),
        ];
        let mut client = connected_peer(root.path(), &socket, "window-t", 5001, script).await;
        let progress = client.poll_progress(wanted).await.expect("valid poll");
        assert_eq!(progress.source_id, wanted);
        assert_eq!(progress.generation, 3);
        let error = client
            .poll_progress(wanted)
            .await
            .expect_err("foreign source must be refused");
        assert!(
            error.contains("another source"),
            "identity violation must name itself: {error}"
        );
        // The violation retired the connection: no reuse, even though the
        // stream still holds a live peer.
        let error = client
            .poll_progress(wanted)
            .await
            .expect_err("retired connection must refuse reuse");
        assert!(
            error.contains("retired"),
            "retirement must name itself: {error}"
        );
    }

    #[tokio::test]
    async fn poll_progress_wrong_session_retires() {
        let root = tempfile::tempdir().unwrap();
        crate::election::WorkerPaths::new(root.path())
            .ensure_directories()
            .expect("viewer directories");
        let socket = root.path().join("control.sock");
        let wanted = SourceId(uuid::Uuid::from_u128(703));
        let script: Vec<Box<dyn FnOnce(String) -> WorkerEvent + Send>> = vec![
            Box::new(welcome_reply),
            progress_reply("session-other", peer_progress(wanted, 3)),
        ];
        let mut client = connected_peer(root.path(), &socket, "window-t", 5002, script).await;
        let error = client
            .poll_progress(wanted)
            .await
            .expect_err("foreign session must be refused");
        assert!(
            error.contains("session changed"),
            "epoch violation must name itself: {error}"
        );
        let error = client
            .poll_progress(wanted)
            .await
            .expect_err("retired connection must refuse reuse");
        assert!(error.contains("retired"), "{error}");
    }

    #[tokio::test]
    async fn store_wrong_id_retires_without_shifted_ack() {
        let root = tempfile::tempdir().unwrap();
        crate::election::WorkerPaths::new(root.path())
            .ensure_directories()
            .expect("viewer directories");
        let socket = root.path().join("control.sock");
        // A lying answer to the store: right shape, wrong correlation.
        let script: Vec<Box<dyn FnOnce(String) -> WorkerEvent + Send>> = vec![
            Box::new(welcome_reply),
            Box::new(|_| {
                WorkerEvent::Store(StoreEvent::RecentFailed {
                    request_id: "WRONG".into(),
                    reason: "boom".into(),
                })
            }),
        ];
        let mut client = connected_peer(root.path(), &socket, "window-t", 5003, script).await;
        let error = client
            .store(StoreMethod::Recent {
                request_id: "unused".into(),
                window_id: "window-t".into(),
            })
            .await
            .expect_err("foreign id must be refused");
        assert!(
            error.contains("mismatch"),
            "id violation must name itself: {error}"
        );
        // The late-or-wrong answer must never become someone else's ack:
        // the next operation fails retired without reading the stream.
        let error = client
            .store(StoreMethod::Recent {
                request_id: "unused".into(),
                window_id: "window-t".into(),
            })
            .await
            .expect_err("retired connection must refuse reuse");
        assert!(error.contains("retired"), "{error}");
    }

    #[tokio::test]
    async fn roundtrip_timeout_retires_before_late_reply() {
        let root = tempfile::tempdir().unwrap();
        let socket = root.path().join("control.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        // Peer answers hello at once, then sits on the next request past
        // the test's short bound before delivering the (correct!) late
        // answer. Correctness of the late bytes must not resurrect the
        // already-timed-out exchange.
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let (stream, _) = listener.accept().await.expect("peer accepts");
            let (reader, mut writer) = stream.into_split();
            let mut reader = tokio::io::BufReader::new(reader);
            let mut decoder = FrameDecoder::new();
            let mut buffer = vec![0u8; crate::READ_CHUNK_BYTES];
            let mut requests = 0u32;
            loop {
                let count = reader.read(&mut buffer).await.expect("peer reads");
                if count == 0 {
                    return;
                }
                let values = decoder.push_bytes(&buffer[..count]).expect("peer decodes");
                for value in values {
                    requests += 1;
                    let id = value
                        .get("request_id")
                        .and_then(|id| id.as_str())
                        .unwrap_or("")
                        .to_owned();
                    let reply = if requests == 1 {
                        WorkerEvent::Welcome {
                            request_id: id,
                            worker_pid: 1,
                            protocol: crate::protocol::PROTOCOL_VERSION,
                            worker_session: "session-test".into(),
                            sources: Vec::new(),
                        }
                    } else {
                        tokio::time::sleep(Duration::from_millis(300)).await;
                        WorkerEvent::Refused {
                            request_id: id,
                            reason: "late".into(),
                        }
                    };
                    let wire =
                        crate::encode_frame(&serde_json::to_value(&reply).expect("peer encodes"))
                            .expect("peer frames");
                    writer.write_all(&wire).await.expect("peer writes");
                }
            }
        });
        crate::election::WorkerPaths::new(root.path())
            .ensure_directories()
            .expect("viewer directories");
        let (mut client, _) = WorkerClient::connect(root.path(), &socket, "window-t", 5004)
            .await
            .expect("connect");
        // Private roundtrip with a short bound: the peer's 300ms nap
        // outlasts it, so this times out and retires.
        let error = client
            .roundtrip(
                WorkerRequest::StatusSubscribe {
                    request_id: "slow".into(),
                },
                Duration::from_millis(100),
            )
            .await
            .expect_err("slow peer must time out");
        assert!(error.contains("timed out"), "{error}");
        // Let the late (correct!) answer land in the socket buffer, then
        // prove the next exchange refuses retired instead of consuming it.
        tokio::time::sleep(Duration::from_millis(500)).await;
        let error = client
            .roundtrip(
                WorkerRequest::StatusSubscribe {
                    request_id: "next".into(),
                },
                Duration::from_secs(5),
            )
            .await
            .expect_err("retired connection must refuse reuse");
        assert!(error.contains("retired"), "{error}");
    }

    #[tokio::test]
    async fn cancelled_exchange_retires_before_next_operation() {
        let root = tempfile::tempdir().unwrap();
        crate::election::WorkerPaths::new(root.path())
            .ensure_directories()
            .expect("viewer directories");
        let socket = root.path().join("control.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        // Peer answers hello, then never answers polls: the poll task
        // blocks until aborted.
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let (stream, _) = listener.accept().await.expect("peer accepts");
            let (reader, mut writer) = stream.into_split();
            let mut reader = tokio::io::BufReader::new(reader);
            let mut decoder = FrameDecoder::new();
            let mut buffer = vec![0u8; crate::READ_CHUNK_BYTES];
            let mut hellos = 0u32;
            loop {
                let count = reader.read(&mut buffer).await.expect("peer reads");
                if count == 0 {
                    return;
                }
                let values = decoder.push_bytes(&buffer[..count]).expect("peer decodes");
                for value in values {
                    let id = value
                        .get("request_id")
                        .and_then(|id| id.as_str())
                        .unwrap_or("")
                        .to_owned();
                    hellos += 1;
                    if hellos == 1 {
                        let welcome = WorkerEvent::Welcome {
                            request_id: id,
                            worker_pid: 1,
                            protocol: crate::protocol::PROTOCOL_VERSION,
                            worker_session: "session-test".into(),
                            sources: Vec::new(),
                        };
                        let wire = crate::encode_frame(
                            &serde_json::to_value(&welcome).expect("peer encodes"),
                        )
                        .expect("peer frames");
                        writer.write_all(&wire).await.expect("peer writes");
                    }
                    // Polls are never answered: the exchange stays open.
                }
            }
        });
        let (client, _) = WorkerClient::connect(root.path(), &socket, "window-t", 5005)
            .await
            .expect("connect");
        // Shared the way the feeder shares it: aborting the task drops a
        // held async-mutex guard cleanly, while the in-flight flag stays
        // set — exactly the residue the next operation must refuse.
        let shared = std::sync::Arc::new(tokio::sync::Mutex::new(client));
        let wanted = SourceId(uuid::Uuid::from_u128(704));
        let stalled = {
            let shared = std::sync::Arc::clone(&shared);
            tokio::spawn(async move { shared.lock().await.poll_progress(wanted).await })
        };
        tokio::time::sleep(Duration::from_millis(50)).await;
        stalled.abort();
        // Deterministic abort delivery: the join reports cancellation, so
        // the exchange below provably follows it.
        assert!(stalled.await.unwrap_err().is_cancelled());
        // The aborted exchange never completed: the next operation retires
        // instead of reusing a stream that may still deliver its answer.
        let error = shared
            .lock()
            .await
            .poll_progress(wanted)
            .await
            .expect_err("cancelled exchange must retire the client");
        assert!(
            error.contains("retired") || error.contains("never completed"),
            "{error}"
        );
    }

    fn union_test_request(window: &str) -> CommitRequest {
        CommitRequest {
            window_id: window.into(),
            union_view_id: "union-view".into(),
            candidate_generation: 7,
            nonce: "nonce-7".into(),
            digest: [0xA5; crate::union_commit::COMMIT_DIGEST_BYTES],
            frozen: vec![crate::union_commit::UnionSourceFence {
                source_id: "source-a".into(),
                generation: 3,
                high_watermark: Some(9),
            }],
        }
    }

    #[tokio::test]
    async fn recoverable_union_timeout_survives_and_drains_late_reply() {
        let root = tempfile::tempdir().unwrap();
        crate::election::WorkerPaths::new(root.path())
            .ensure_directories()
            .expect("viewer directories");
        let socket = root.path().join("control.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        // Async sleeps only: a blocking sleep here would freeze this
        // single-threaded test runtime's timers and the client's attempt
        // bound would never fire. The commit reply arrives 300ms after its
        // request — past the 100ms attempt bound — then a status poll is
        // answered at once. The late commit receipt must be drained as
        // stale, never matched to the status poll, and the connection must
        // stay usable.
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let (stream, _) = listener.accept().await.expect("peer accepts");
            let (reader, mut writer) = stream.into_split();
            let mut reader = tokio::io::BufReader::new(reader);
            let mut decoder = FrameDecoder::new();
            let mut buffer = vec![0u8; crate::READ_CHUNK_BYTES];
            let mut requests = 0u32;
            loop {
                let count = reader.read(&mut buffer).await.expect("peer reads");
                if count == 0 {
                    return;
                }
                let values = decoder.push_bytes(&buffer[..count]).expect("peer decodes");
                for value in values {
                    requests += 1;
                    let id = value
                        .get("request_id")
                        .and_then(|id| id.as_str())
                        .unwrap_or("")
                        .to_owned();
                    let reply = if requests == 1 {
                        WorkerEvent::Welcome {
                            request_id: id,
                            worker_pid: 1,
                            protocol: crate::protocol::PROTOCOL_VERSION,
                            worker_session: "session-test".into(),
                            sources: Vec::new(),
                        }
                    } else if requests == 2 {
                        tokio::time::sleep(Duration::from_millis(300)).await;
                        WorkerEvent::Store(StoreEvent::UnionCommitted {
                            request_id: id,
                            receipt: CommitReceipt::answer(
                                "session-test",
                                &union_test_request("window-t"),
                                crate::union_commit::CommitOutcome::Committed { current: vec![] },
                            ),
                        })
                    } else {
                        WorkerEvent::Store(StoreEvent::UnionStatus {
                            request_id: id,
                            status: UnionCommitStatus::Unknown,
                        })
                    };
                    let wire =
                        crate::encode_frame(&serde_json::to_value(&reply).expect("peer encodes"))
                            .expect("peer frames");
                    writer.write_all(&wire).await.expect("peer writes");
                }
            }
        });
        let (mut client, _) = WorkerClient::connect(root.path(), &socket, "window-t", 5006)
            .await
            .expect("connect");
        let error = client
            .request_union_commit(
                "window-t",
                &union_test_request("window-t"),
                Duration::from_millis(100),
            )
            .await
            .expect_err("slow commit must time out");
        assert!(
            error.contains("status poll"),
            "recoverable timeout must direct recovery, not retire: {error}"
        );
        assert!(
            !client.retired,
            "recoverable timeout must leave the connection usable"
        );
        // The peer's late commit receipt lands mid-poll: it names the
        // superseded attempt, so the status exchange drains it and matches
        // its own reply instead of retiring on a shifted ack.
        let status = client
            .request_union_status(
                "window-t",
                "union-view",
                7,
                "nonce-7",
                &[0xA5; crate::union_commit::COMMIT_DIGEST_BYTES],
                Duration::from_secs(5),
            )
            .await
            .expect("status poll must survive the late commit reply");
        assert!(
            matches!(status, UnionCommitStatus::Unknown),
            "stale drain must yield the poll's own answer: {status:?}"
        );
        assert!(
            !client.retired,
            "draining a stale reply must not retire the connection"
        );
    }

    #[tokio::test]
    async fn strict_store_timeout_still_retires() {
        let root = tempfile::tempdir().unwrap();
        crate::election::WorkerPaths::new(root.path())
            .ensure_directories()
            .expect("viewer directories");
        let socket = root.path().join("control.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        // Same slow peer, strict exchange: the timeout must retire, and a
        // later operation must refuse reuse without reading the stream.
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let (stream, _) = listener.accept().await.expect("peer accepts");
            let (reader, mut writer) = stream.into_split();
            let mut reader = tokio::io::BufReader::new(reader);
            let mut decoder = FrameDecoder::new();
            let mut buffer = vec![0u8; crate::READ_CHUNK_BYTES];
            let mut requests = 0u32;
            loop {
                let count = reader.read(&mut buffer).await.expect("peer reads");
                if count == 0 {
                    return;
                }
                let values = decoder.push_bytes(&buffer[..count]).expect("peer decodes");
                for value in values {
                    requests += 1;
                    let id = value
                        .get("request_id")
                        .and_then(|id| id.as_str())
                        .unwrap_or("")
                        .to_owned();
                    let reply = if requests == 1 {
                        WorkerEvent::Welcome {
                            request_id: id,
                            worker_pid: 1,
                            protocol: crate::protocol::PROTOCOL_VERSION,
                            worker_session: "session-test".into(),
                            sources: Vec::new(),
                        }
                    } else {
                        tokio::time::sleep(Duration::from_millis(300)).await;
                        WorkerEvent::Store(StoreEvent::RecentFailed {
                            request_id: id,
                            reason: "late".into(),
                        })
                    };
                    let wire =
                        crate::encode_frame(&serde_json::to_value(&reply).expect("peer encodes"))
                            .expect("peer frames");
                    writer.write_all(&wire).await.expect("peer writes");
                }
            }
        });
        let (mut client, _) = WorkerClient::connect(root.path(), &socket, "window-t", 5007)
            .await
            .expect("connect");
        let error = client
            .store_with_timeout(
                StoreMethod::Recent {
                    request_id: "unused".into(),
                    window_id: "window-t".into(),
                },
                Duration::from_millis(100),
            )
            .await
            .expect_err("slow store must time out");
        assert!(error.contains("retired"), "{error}");
        let error = client
            .store(StoreMethod::Recent {
                request_id: "unused".into(),
                window_id: "window-t".into(),
            })
            .await
            .expect_err("retired connection must refuse reuse");
        assert!(error.contains("retired"), "{error}");
    }
}
