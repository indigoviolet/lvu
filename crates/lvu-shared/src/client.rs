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
    protocol::{PROTOCOL_VERSION, check_store_size},
    spawn::{CAPTURE_ROOT_ARG, SOCKET_ARG, SpawnSpec, WORKER_CHILD_FLAG},
};

/// Client bound for one store round trip. Exceeds the worker's own
/// `request_timeout` (30 s) so a worker-side timeout (outcome-unknown)
/// arrives as data before this fires; only a wedged connection trips it.
pub const STORE_ROUNDTRIP_TIMEOUT: Duration = Duration::from_secs(60);

/// Client bound for control round trips (start/stop/restart): same
/// reasoning as [`STORE_ROUNDTRIP_TIMEOUT`], one shared bound.
pub const CONTROL_ROUNDTRIP_TIMEOUT: Duration = Duration::from_secs(60);

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
/// A stale socket file (crashed worker) is unlinked only after proving the
/// owner dead, so a slow-starting live worker is never robbed of its bind.
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
pub struct WorkerClient {
    reader: BufReader<tokio::net::unix::OwnedReadHalf>,
    writer: tokio::net::unix::OwnedWriteHalf,
    decoder: FrameDecoder,
    window_id: String,
    next_request: u64,
    read_buf: Vec<u8>,
    _viewer: ViewerGuard,
}

impl WorkerClient {
    /// Attach-or-spawn then connect: the full cold-start flow in one call.
    /// On connect/hello failure the election is re-ensured once (a stale
    /// socket from a crashed worker resolves this way) before giving up.
    /// `window_pid` identifies this window's viewer slot and handshake;
    /// pass `std::process::id()` unless driving several logical windows
    /// from one process (as integration tests do).
    pub async fn attach(
        executable: &Path,
        capture_root: &Path,
        window_id: &str,
        window_pid: u32,
    ) -> Result<(Self, Vec<SourceSummary>), String> {
        let socket = ensure_worker(executable, capture_root).await?;
        match Self::connect(capture_root, &socket, window_id, window_pid).await {
            Ok(attached) => Ok(attached),
            Err(first) => {
                // Possibly a stale socket: re-ensure (unblocks election of
                // a replacement) and retry exactly once, then report both.
                let socket = ensure_worker(executable, capture_root).await?;
                Self::connect(capture_root, &socket, window_id, window_pid)
                    .await
                    .map_err(|second| format!("worker attach failed: {first}; retry: {second}"))
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
    ) -> Result<(Self, Vec<SourceSummary>), String> {
        let paths = WorkerPaths::new(capture_root);
        let viewer = take_viewer_lock(&paths, window_pid)
            .map_err(|error| format!("viewer lock: {error}"))?;
        let stream = tokio::time::timeout(
            crate::WORKER_HANDSHAKE_TIMEOUT,
            UnixStream::connect(socket_path),
        )
        .await
        .map_err(|_| {
            format!(
                "no worker answered at {} within {:?}",
                socket_path.display(),
                crate::WORKER_HANDSHAKE_TIMEOUT
            )
        })?
        .map_err(|error| format!("connect {}: {error}", socket_path.display()))?;
        let (reader, writer) = stream.into_split();
        let mut client = Self {
            reader: BufReader::new(reader),
            writer,
            decoder: FrameDecoder::new(),
            window_id: window_id.to_owned(),
            next_request: 0,
            read_buf: vec![0u8; crate::READ_CHUNK_BYTES],
            _viewer: viewer,
        };
        let request_id = client.take_request_id();
        let reply = client
            .roundtrip(
                WorkerRequest::Hello {
                    request_id,
                    window_pid,
                    window_id: window_id.to_owned(),
                    protocol: PROTOCOL_VERSION,
                },
                crate::WORKER_HANDSHAKE_TIMEOUT,
            )
            .await?;
        match reply.as_slice() {
            [
                WorkerEvent::Welcome {
                    protocol, sources, ..
                },
            ] if *protocol == PROTOCOL_VERSION => Ok((client, sources.clone())),
            [WorkerEvent::Welcome { protocol, .. }] => Err(format!(
                "worker speaks protocol {protocol}, this client speaks {PROTOCOL_VERSION}"
            )),
            [WorkerEvent::Refused { reason, .. }] => {
                Err(format!("worker refused attach: {reason}"))
            }
            other => Err(format!("unexpected attach reply: {other:?}")),
        }
    }

    fn take_request_id(&mut self) -> String {
        let id = format!("{}-{}", self.window_id, self.next_request);
        self.next_request += 1;
        id
    }

    /// Send one request value and collect its reply events, bounded. The
    /// frame cap is enforced on send; a reply that cannot be framed on the
    /// worker side fails the connection there, which surfaces here as EOF.
    /// Replies written back-to-back (a result plus its warning) may split
    /// across reads, so after the first events a short grace keeps reading
    /// for stragglers instead of dropping a warning silently.
    async fn roundtrip(
        &mut self,
        request: WorkerRequest,
        timeout: Duration,
    ) -> Result<Vec<WorkerEvent>, String> {
        let bytes = crate::encode_frame(
            &serde_json::to_value(&request)
                .map_err(|error| format!("encode worker request: {error}"))?,
        )
        .map_err(|error| format!("worker request exceeds the wire cap: {error}"))?;
        let deadline = Instant::now() + timeout;
        self.writer
            .write_all(&bytes)
            .await
            .map_err(|error| format!("write worker request: {error}"))?;
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
                Ok(Err(error)) => return Err(format!("read worker reply: {error}")),
                Ok(Ok(count)) => {
                    let decoded = self
                        .decoder
                        .push_bytes(&self.read_buf[..count])
                        .map_err(|error| format!("decode worker reply: {error}"))?;
                    for value in decoded {
                        events.push(
                            serde_json::from_value::<WorkerEvent>(value).map_err(|error| {
                                format!("unexpected worker reply shape: {error}")
                            })?,
                        );
                    }
                }
            }
        }
        if events.is_empty() {
            return Err(if closed {
                "worker closed the connection".into()
            } else {
                format!("worker request timed out after {timeout:?}")
            });
        }
        Ok(events)
    }

    /// Explicit user-approved acquisition. The definition travels as its
    /// canonical DTO value; the worker parses and admits it.
    pub async fn request_start(
        &mut self,
        definition: &SourceDefinition,
    ) -> Result<StartOutcome, String> {
        let request_id = self.take_request_id();
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
        match events.first() {
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
        let events = self
            .roundtrip(
                WorkerRequest::RequestStop {
                    request_id,
                    source_id: source_id.0.to_string(),
                },
                CONTROL_ROUNDTRIP_TIMEOUT,
            )
            .await?;
        match events.first() {
            Some(WorkerEvent::Stopped { .. }) => Ok(()),
            Some(WorkerEvent::Refused { reason, .. }) => Err(reason.clone()),
            other => Err(format!("unexpected stop reply: {other:?}")),
        }
    }

    /// Explicit restart of a stopped capture. The worker restarts the
    /// remembered definition; this never invents one.
    pub async fn request_restart(&mut self, source_id: SourceId) -> Result<StartOutcome, String> {
        let request_id = self.take_request_id();
        let events = self
            .roundtrip(
                WorkerRequest::RequestRestart {
                    request_id,
                    source_id: source_id.0.to_string(),
                },
                CONTROL_ROUNDTRIP_TIMEOUT,
            )
            .await?;
        match events.first() {
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

    /// One mediated store call: size-checked before sending (the wire cap
    /// would refuse it anyway, but the local error names the method), then
    /// a single `Store` reply under the store bound. The payload is the
    /// canonical DTO the worker maps 1:1 onto `memory::Command` — sequence,
    /// definition, view, and expected version travel verbatim so CAS
    /// semantics are the worker's, not a second evaluator's.
    pub async fn store(&mut self, method: StoreMethod) -> Result<StoreEvent, String> {
        let window_id = self.window_id.clone();
        let mut method = method;
        set_store_window(&mut method, &window_id);
        if let Err(error) = check_store_size(&method) {
            return Err(error.to_string());
        }
        let bytes = crate::encode_frame(
            &serde_json::to_value(&method)
                .map_err(|error| format!("encode store method: {error}"))?,
        )
        .map_err(|error| format!("store method exceeds the wire cap: {error}"))?;
        let deadline = Instant::now() + STORE_ROUNDTRIP_TIMEOUT;
        self.writer
            .write_all(&bytes)
            .await
            .map_err(|error| format!("write store method: {error}"))?;
        loop {
            if Instant::now() >= deadline {
                return Err(format!(
                    "store request timed out after {STORE_ROUNDTRIP_TIMEOUT:?}"
                ));
            }
            let remaining = deadline - Instant::now();
            let count = tokio::time::timeout(remaining, self.reader.read(&mut self.read_buf))
                .await
                .map_err(|_| format!("store request timed out after {STORE_ROUNDTRIP_TIMEOUT:?}"))?
                .map_err(|error| format!("read store reply: {error}"))?;
            if count == 0 {
                return Err("worker closed the connection mid-request".into());
            }
            let decoded = self
                .decoder
                .push_bytes(&self.read_buf[..count])
                .map_err(|error| format!("decode store reply: {error}"))?;
            let mut decoded = decoded.into_iter();
            let Some(value) = decoded.next() else {
                // Partial frame: keep reading.
                continue;
            };
            if decoded.next().is_some() {
                return Err("worker sent an unsolicited batch mid-request".into());
            }
            let event: WorkerEvent = serde_json::from_value(value)
                .map_err(|error| format!("unexpected store reply shape: {error}"))?;
            // Strictly sequential requests: any `Store` reply on this
            // connection answers the outstanding one. Anything else
            // (status, notices) cannot arrive unsubscribed; refusal to
            // guess treats it as a violation.
            match event {
                WorkerEvent::Store(store) => return Ok(store),
                WorkerEvent::ShutdownNotice { reason } => {
                    return Err(format!("worker is stopping: {reason}"));
                }
                other => return Err(format!("unexpected event mid-request: {other:?}")),
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

    /// Bounded shutdown drain: flush, say goodbye, wait for the close
    /// (bounded), then drop the viewer lock by consuming `self`. The worker
    /// removes the viewer on EOF either way, so this converges even if the
    /// goodbye frame is lost — the wait is courtesy, not consensus.
    pub async fn shutdown(mut self) -> Result<(), String> {
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
}
