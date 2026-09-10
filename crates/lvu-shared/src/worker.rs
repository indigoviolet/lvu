//! Worker service: socket-served acquisition plus mediated durable writes.
//!
//! This is the executable core of the shared worker, written against public
//! store/manager APIs only. It owns a `SourceManager`, a `WorkspaceStore`
//! (single writer — the store's own transactions serialize everything), the
//! session set, and the viewer set. Windows attach over the control socket;
//! every request is dispatched synchronously per connection, so there is no
//! cross-request batching to reason about:
//!
//! - View saves commit one transaction per request with the client's
//!   `expected_version` passed straight through to
//!   `save_sources_and_views`, whose `WHERE view_id AND version` guard is
//!   the cross-window conflict authority. A stale writer gets `Conflict`
//!   with the current version echoed — never last-writer-wins, never a
//!   silent overwrite. (The in-process worker instead coalesces queued
//!   saves by view; that batching exists to bound write amplification from
//!   one client's autosave bursts, and is unnecessary here because each
//!   mediated request already commits or fails explicitly.)
//! - Newest-sequence coalescing is likewise per-client by construction:
//!   each request carries its own sequence echo, and no ack is ever issued
//!   for a state that was not written.
//! - Recipe operations call the same store functions with the same rules
//!   (`expected_revision` hesitation, candidate limits, usage recording)
//!   as `lvu-app/src/memory.rs`; the line-level provenance is cited at each
//!   call site.
//!
//! Session persistence mirrors `lvu-app/src/session.rs` discipline
//! (additive manifest, tmp-file rename, `sync_data`, corrupt-reported)
//! without importing it: the worker cannot depend on the application crate.

use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use lvu_core::{SourceDefinition, SourceId};
use lvu_ingest::{RuntimeConfig, RuntimeError, SourceManager, StopReport};
use lvu_memory::WorkspaceStore;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{UnixListener, UnixStream},
    sync::Mutex,
};

use crate::{
    child::admission_key,
    frame::{FrameDecoder, FrameError, encode_frame},
    lifetime::ViewerSet,
    protocol::*,
};

/// Bounded per-request dispatch: an unresponsive store call fails the
/// request rather than stalling the connection.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// How often lifecycle changes re-broadcast presence while subscribers
/// exist. Counts are sampled at event time; there is no per-record feed.
pub const STATUS_INTERVAL: Duration = Duration::from_secs(2);

/// Lifecycle work detached from a timed-out socket request is capped at the
/// viewer cap. A connection dispatches one request at a time, and excess
/// work is refused instead of growing a hidden task/queue without bound.
const MAX_LIFECYCLE_SETTLEMENTS: usize = crate::MAX_VIEWERS;

/// Admission decision hook, implemented by the application with its real
/// acquisition comparator (duplicate identity/acquisition detection). The
/// worker never invents admission policy; it only enforces the verdict.
/// `admit_known` additionally sees the worker's live definitions so a
/// standalone child (which has no application process to ask) can refuse a
/// second acquisition of the same capture; the default keeps the old
/// behavior for hooks that carry their own live state.
pub trait AdmissionHook: Send + Sync {
    fn admit(&self, definition: &SourceDefinition) -> AdmissionVerdict;

    fn admit_known(
        &self,
        definition: &SourceDefinition,
        live: &[SourceDefinition],
    ) -> AdmissionVerdict {
        let _ = live;
        self.admit(definition)
    }
}

/// Mirrors the application's admission outcomes without depending on it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AdmissionVerdict {
    Admit,
    Present { live_id: SourceId },
    Refuse(String),
}

#[derive(Clone, Debug)]
pub struct WorkerConfig {
    pub capture_root: PathBuf,
    pub workspace_root: PathBuf,
    pub socket_path: PathBuf,
    pub viewer_grace: Duration,
    pub request_timeout: Duration,
}

impl WorkerConfig {
    pub fn new(capture_root: &Path, workspace_root: &Path, socket_path: &Path) -> Self {
        Self {
            capture_root: capture_root.to_path_buf(),
            workspace_root: workspace_root.to_path_buf(),
            socket_path: socket_path.to_path_buf(),
            viewer_grace: crate::WORKER_SHUTDOWN_GRACE,
            request_timeout: REQUEST_TIMEOUT,
        }
    }
}

/// Session manifest shape, byte-compatible with `session.json` written by
/// `lvu-app/src/session.rs`: additive `serde(default)` fields, same file
/// name, same tmp-rename-sync discipline. This is a read/write projection
/// of that file, not a second format: unknown fields are ignored on read
/// and preserved by never rewriting them (we always write this exact
/// shape, which the application reads as a subset).
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct SessionSet {
    #[serde(default)]
    pub schema_version: u32,
    #[serde(default)]
    pub sources: Vec<SourceDefinition>,
}

const MAX_SESSION_BYTES: u64 = 256 * 1024;

fn session_path(workspace_root: &Path) -> PathBuf {
    workspace_root.join("session.json")
}

/// Load the session acquisition set: missing/unreadable is empty, corrupt
/// is reported-and-empty (never blocks startup), oversize is refused.
pub fn load_session_set(workspace_root: &Path) -> Result<Vec<SourceDefinition>, String> {
    let path = session_path(workspace_root);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("read {}: {error}", path.display())),
    };
    if bytes.len() as u64 > MAX_SESSION_BYTES {
        return Err(format!(
            "session manifest exceeds {MAX_SESSION_BYTES} bytes"
        ));
    }
    serde_json::from_slice::<SessionSet>(&bytes)
        .map(|manifest| manifest.sources)
        .map_err(|error| format!("parse {}: {error}", path.display()))
}

/// Store the session acquisition set atomically: tmp file, newline,
/// `sync_data`, rename. Mirrors `session::store` exactly.
pub fn store_session_set(
    workspace_root: &Path,
    sources: &[SourceDefinition],
) -> Result<(), String> {
    std::fs::create_dir_all(workspace_root)
        .map_err(|error| format!("create {}: {error}", workspace_root.display()))?;
    let path = session_path(workspace_root);
    let temporary = path.with_extension("json.tmp");
    let manifest = SessionSet {
        schema_version: 1,
        sources: sources.to_vec(),
    };
    (|| -> std::io::Result<()> {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&temporary)?;
        serde_json::to_writer(&mut file, &manifest)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
        file.write_all(b"\n")?;
        file.sync_data()?;
        std::fs::rename(&temporary, &path)
    })()
    .map_err(|error| format!("write {}: {error}", path.display()))
}

/// One forwarded stdin stream: window-owned pipe bytes cross here into a
/// capture reader. Chunks must arrive in exact sequence order; any gap,
/// duplicate, or oversize chunk is a protocol violation that drops the
/// connection. Dropping the binding (close, EOF, crash) ends the capture
/// as incomplete with arrived bytes kept. A replacement worker never
/// inherits bindings: after a crash the pipe is gone, and stdin expressly
/// cannot resume from a durable cursor — a new attachment needs a fresh
/// source identity per the existing stdin invariant.
struct StdinBinding {
    writer: tokio::io::DuplexStream,
    expected_seq: u64,
}

/// Settlement of one canonical acquisition-key admission, shared between
/// the racing starter (leader) and joiners (waiters).
#[derive(Clone, Debug)]
enum StartSettlement {
    /// The winner's exact outcome; its definition is committed and its
    /// capture was successfully started. Joiners present its identity.
    Settled(StartedOutcome),
    /// The attempt failed; joiners surface this instead of starting over
    /// blindly. A later request may retry and lead anew.
    Failed(String),
    /// The detached leader task ended before settling (panic/runtime
    /// teardown); the reservation is void and joiners re-evaluate state.
    Abandoned,
}

/// One in-flight canonical-key admission, shared by reference. The outcome
/// travels over a versioned watch channel, so a settle that lands before a
/// joiner subscribes is still observed: no wakeup can be missed, and every
/// joiner wakes. Entries live in [`WorkerService::starting`] only while
/// unsettled-or-unobserved; settle and abandon paths both remove them.
struct SharedStart {
    outcome: tokio::sync::watch::Sender<Option<StartSettlement>>,
}

/// The role one caller takes for one canonical acquisition key.
enum KeyEntry {
    /// Sole leader: run admission plus the manager start, then settle.
    Lead(Arc<SharedStart>),
    /// Observer: return the leader's settled outcome, or re-evaluate when
    /// the detached leader task ended unsettled.
    Join(Arc<SharedStart>),
}

/// Normalize an incoming definition's file spelling at the worker
/// boundary: absolute paths canonicalize (symlinks, `.`/`..` resolved
/// independent of any process cwd), and anything uncanonicalizable keeps
/// its absolute spelling. `validate_start_boundary` has already rejected
/// relative paths before this infallible normalization runs.
fn canonicalize_definition(mut definition: SourceDefinition) -> SourceDefinition {
    if let lvu_core::Acquisition::File { path, .. } = &mut definition.acquisition
        && path.is_absolute()
    {
        // Single metadata syscall per explicit start; steady-state reads
        // never touch this.
        if let Ok(canonical) = std::fs::canonicalize(&path) {
            *path = canonical;
        }
    }
    definition
}

/// Validate process-relative meaning before any duplicate lookup. A worker
/// may have been elected by a different window and therefore must never use
/// its own current directory to reinterpret another window's request.
fn validate_start_boundary(definition: &SourceDefinition) -> Result<(), String> {
    if definition.schema_version != 1 {
        return Err(format!(
            "unsupported source schema_version {}",
            definition.schema_version
        ));
    }
    match &definition.acquisition {
        lvu_core::Acquisition::File { path, .. } => {
            if !path.is_absolute() {
                return Err("file path must be absolute before worker admission".into());
            }
        }
        lvu_core::Acquisition::Command { command } => {
            let cwd = command
                .cwd
                .as_ref()
                .ok_or_else(|| "command cwd must be explicit before worker admission".to_owned())?;
            if !cwd.is_absolute() {
                return Err("command cwd must be absolute before worker admission".into());
            }
            if let lvu_core::CommandProgram::Exec { executable, .. } = &command.program
                && !executable.is_absolute()
            {
                let mut components = executable.components();
                let bare_path_name =
                    matches!(components.next(), Some(std::path::Component::Normal(_)))
                        && components.next().is_none();
                if !bare_path_name {
                    return Err(
                        "relative command executable paths must be resolved before worker admission"
                            .into(),
                    );
                }
            }
        }
        lvu_core::Acquisition::Stdin | lvu_core::Acquisition::Http { .. } => {}
    }
    Ok(())
}

/// Removes a start reservation whose detached task never settled and marks
/// it abandoned so joiners re-evaluate instead of hanging. Ordinary request
/// timeout/cancellation cannot reach this path because it owns no leader
/// work. Removal is idempotent with the settle
/// path and with fellow joiners; entries never outlive their observers.
/// The registry is a plain `std` mutex because no holder ever keeps it
/// across an await (every critical section below is pointer-sized map
/// surgery), which is also what lets this cleanup run infallibly inside
/// `Drop` with no lock gaps for entries to leak through.
struct StartGuard {
    starting: Arc<std::sync::Mutex<HashMap<String, Arc<SharedStart>>>>,
    key: String,
    mine: Arc<SharedStart>,
    settled: bool,
}

impl StartGuard {
    fn disarm(&mut self) {
        self.settled = true;
    }
}

impl Drop for StartGuard {
    fn drop(&mut self) {
        if self.settled {
            return;
        }
        // Mark abandoned first so every joiner wakes and re-evaluates even
        // if this task is being torn down around them.
        self.mine.outcome.send_modify(|outcome| {
            if outcome.is_none() {
                *outcome = Some(StartSettlement::Abandoned);
            }
        });
        let mut starting = self.starting.lock().expect("start registry poisoned");
        if let Some(current) = starting.get(&self.key)
            && Arc::ptr_eq(current, &self.mine)
        {
            starting.remove(&self.key);
        }
    }
}

#[derive(Default)]
struct LifecycleState {
    closed: bool,
    in_flight: usize,
    next_id: u64,
    permits: HashMap<u64, LifecycleOperation>,
    settlements: HashMap<u64, tokio::task::AbortHandle>,
}

struct LifecycleOperation {
    source_id: SourceId,
    begun: bool,
}

/// One atomic authority for lifecycle admission and draining. The same
/// short-held mutex decides closed/open and reserves a bounded in-flight
/// slot, eliminating a flag-check/permit-acquire gap. RAII release covers
/// request cancellation and detached leader completion alike.
struct LifecycleAdmission {
    state: std::sync::Mutex<LifecycleState>,
    in_flight: tokio::sync::watch::Sender<usize>,
}

impl LifecycleAdmission {
    fn new() -> Arc<Self> {
        let (in_flight, _) = tokio::sync::watch::channel(0);
        Arc::new(Self {
            state: std::sync::Mutex::new(LifecycleState::default()),
            in_flight,
        })
    }

    fn try_enter(self: &Arc<Self>, source_id: SourceId) -> Result<LifecyclePermit, String> {
        let mut state = self.state.lock().expect("lifecycle admission poisoned");
        if state.closed {
            return Err("worker is shutting down".into());
        }
        if state.in_flight >= MAX_LIFECYCLE_SETTLEMENTS {
            return Err("worker lifecycle settlement capacity is full".into());
        }
        let id = state.next_id;
        state.next_id = state
            .next_id
            .checked_add(1)
            .ok_or_else(|| "worker lifecycle settlement id space exhausted".to_owned())?;
        state.in_flight += 1;
        state.permits.insert(
            id,
            LifecycleOperation {
                source_id,
                begun: false,
            },
        );
        self.in_flight.send_replace(state.in_flight);
        Ok(LifecyclePermit {
            admission: Arc::clone(self),
            id,
        })
    }

    fn close(&self) {
        self.state
            .lock()
            .expect("lifecycle admission poisoned")
            .closed = true;
    }

    async fn drained(&self) {
        let mut in_flight = self.in_flight.subscribe();
        while *in_flight.borrow_and_update() != 0 {
            if in_flight.changed().await.is_err() {
                return;
            }
        }
    }

    fn attach_settlement(&self, id: u64, settlement: tokio::task::AbortHandle) {
        let mut state = self.state.lock().expect("lifecycle admission poisoned");
        if state.permits.contains_key(&id) {
            state.settlements.insert(id, settlement);
        }
    }

    fn abort_settlements(&self) -> Vec<SourceId> {
        let state = self.state.lock().expect("lifecycle admission poisoned");
        let begun = state
            .settlements
            .keys()
            .filter_map(|id| state.permits.get(id))
            .filter(|operation| operation.begun)
            .map(|operation| operation.source_id)
            .collect();
        let settlements: Vec<_> = state.settlements.values().cloned().collect();
        drop(state);
        for settlement in settlements {
            settlement.abort();
        }
        begun
    }

    fn unsettled_sources(&self) -> Vec<SourceId> {
        self.state
            .lock()
            .expect("lifecycle admission poisoned")
            .permits
            .values()
            .map(|operation| operation.source_id)
            .collect()
    }
}

struct LifecyclePermit {
    admission: Arc<LifecycleAdmission>,
    id: u64,
}

impl LifecyclePermit {
    /// Commit this reservation to side effects. Closure and this check use
    /// the same mutex: either begin wins and shutdown drains this permit, or
    /// closure wins and the queued operation must settle without spawning.
    fn begin(&self) -> Result<(), String> {
        let mut state = self
            .admission
            .state
            .lock()
            .expect("lifecycle admission poisoned");
        if state.closed {
            Err("worker is shutting down".into())
        } else {
            state
                .permits
                .get_mut(&self.id)
                .expect("live lifecycle permit missing")
                .begun = true;
            Ok(())
        }
    }
}

impl Drop for LifecyclePermit {
    fn drop(&mut self) {
        let mut state = self
            .admission
            .state
            .lock()
            .expect("lifecycle admission poisoned");
        debug_assert!(state.in_flight > 0);
        state.in_flight -= 1;
        state.permits.remove(&self.id);
        state.settlements.remove(&self.id);
        self.admission.in_flight.send_replace(state.in_flight);
        drop(state);
    }
}

/// The executable worker: manager, store, viewers, session set, and stdin
/// bindings. All shared mutation sits behind short-held mutexes; blocking
/// store calls run in `spawn_blocking` so the async executors never stall.
pub struct WorkerService {
    config: WorkerConfig,
    manager: Arc<SourceManager>,
    store: Arc<std::sync::Mutex<WorkspaceStore>>,
    viewers: Mutex<ViewerSet>,
    admission: Arc<dyn AdmissionHook>,
    stdin_bindings: Mutex<HashMap<SourceId, StdinBinding>>,
    session: Mutex<Vec<SourceDefinition>>,
    definitions: Mutex<HashMap<SourceId, SourceDefinition>>,
    /// Canonical acquisition keys with an admission currently in flight
    /// (leader elected, outcome unsettled). Bounded by concurrent distinct
    /// in-flight starts: entries are removed on settle, on leader drop, and
    /// lazily by the first joiner to observe an abandonment. Plain `std`
    /// mutex: no holder spans an await (see `StartGuard`), so locking here
    /// can neither stall the executor nor strand an entry.
    starting: Arc<std::sync::Mutex<HashMap<String, Arc<SharedStart>>>>,
    /// Start, stop, restart and Present-to-restart transitions share this
    /// gate. The mutex state is constant-size; detached settlements are
    /// admitted and capped by `lifecycle_admission`.
    lifecycle: Mutex<()>,
    lifecycle_admission: Arc<LifecycleAdmission>,
    #[cfg(test)]
    start_side_effect_pause:
        std::sync::Mutex<Option<(Arc<tokio::sync::Barrier>, Arc<tokio::sync::Barrier>)>>,
    #[cfg(test)]
    lifecycle_admission_pause:
        std::sync::Mutex<Option<(Arc<tokio::sync::Barrier>, Arc<tokio::sync::Barrier>)>>,
    #[cfg(test)]
    stop_side_effect_pause:
        std::sync::Mutex<Option<(Arc<tokio::sync::Barrier>, Arc<tokio::sync::Barrier>)>>,
    #[cfg(test)]
    incomplete_stop_reports: std::sync::Mutex<std::collections::HashSet<SourceId>>,
    shutdown_flag: std::sync::atomic::AtomicBool,
    /// Worker lifetime nonce, minted once per `open` and published in
    /// `Welcome` and every progress answer. Windows key remote epoch on
    /// `(worker_session, generation)` so a replacement worker never reads
    /// as continuity.
    worker_session: String,
}

impl WorkerService {
    /// Open manager, store, and session set. Does not resume captures and
    /// does not bind the socket: call [`WorkerService::resume_session`]
    /// then serve explicitly so tests can drive each phase.
    pub fn open(
        config: WorkerConfig,
        admission: Arc<dyn AdmissionHook>,
    ) -> Result<(Arc<Self>, Option<String>), String> {
        let viewer_grace = config.viewer_grace;
        let manager = SourceManager::new(config.capture_root.clone(), RuntimeConfig::default())
            .map_err(|error| format!("open source manager: {error}"))?;
        let store = WorkspaceStore::open(config.workspace_root.clone())
            .map_err(|error| format!("open workspace store: {error}"))?;
        // A corrupt or oversize manifest degrades to an empty session with
        // a reported diagnostic, mirroring session load discipline: it must
        // never block capture startup, and nothing is silently discarded
        // because the diagnostic travels with the service.
        let (session, session_warning) = match load_session_set(&config.workspace_root) {
            Ok(session) => (session, None),
            Err(error) => (Vec::new(), Some(error)),
        };
        Ok((
            Arc::new(Self {
                config,
                manager: Arc::new(manager),
                store: Arc::new(std::sync::Mutex::new(store)),
                viewers: Mutex::new(ViewerSet::new(std::time::Instant::now(), viewer_grace)),
                admission,
                stdin_bindings: Mutex::new(HashMap::new()),
                session: Mutex::new(session),
                definitions: Mutex::new(HashMap::new()),
                starting: Arc::new(std::sync::Mutex::new(HashMap::new())),
                lifecycle: Mutex::new(()),
                lifecycle_admission: LifecycleAdmission::new(),
                #[cfg(test)]
                start_side_effect_pause: std::sync::Mutex::new(None),
                #[cfg(test)]
                lifecycle_admission_pause: std::sync::Mutex::new(None),
                #[cfg(test)]
                stop_side_effect_pause: std::sync::Mutex::new(None),
                #[cfg(test)]
                incomplete_stop_reports: std::sync::Mutex::new(std::collections::HashSet::new()),
                shutdown_flag: std::sync::atomic::AtomicBool::new(false),
                worker_session: uuid::Uuid::new_v4().to_string(),
            }),
            session_warning,
        ))
    }

    /// This worker's lifetime nonce (see the field docs).
    pub fn worker_session(&self) -> &str {
        &self.worker_session
    }

    #[cfg(test)]
    fn set_start_side_effect_pause(
        &self,
        pause: Option<(Arc<tokio::sync::Barrier>, Arc<tokio::sync::Barrier>)>,
    ) {
        *self
            .start_side_effect_pause
            .lock()
            .expect("test start pause poisoned") = pause;
    }

    #[cfg(test)]
    async fn pause_after_manager_start(&self) {
        let pause = self
            .start_side_effect_pause
            .lock()
            .expect("test start pause poisoned")
            .clone();
        if let Some((reached, release)) = pause {
            reached.wait().await;
            release.wait().await;
        }
    }

    #[cfg(test)]
    fn set_lifecycle_admission_pause(
        &self,
        pause: Option<(Arc<tokio::sync::Barrier>, Arc<tokio::sync::Barrier>)>,
    ) {
        *self
            .lifecycle_admission_pause
            .lock()
            .expect("test lifecycle admission pause poisoned") = pause;
    }

    #[cfg(test)]
    async fn pause_before_lifecycle_admission(&self) {
        let pause = self
            .lifecycle_admission_pause
            .lock()
            .expect("test lifecycle admission pause poisoned")
            .clone();
        if let Some((reached, release)) = pause {
            reached.wait().await;
            release.wait().await;
        }
    }

    #[cfg(test)]
    fn set_stop_side_effect_pause(
        &self,
        pause: Option<(Arc<tokio::sync::Barrier>, Arc<tokio::sync::Barrier>)>,
    ) {
        *self
            .stop_side_effect_pause
            .lock()
            .expect("test stop pause poisoned") = pause;
    }

    #[cfg(test)]
    async fn pause_after_manager_stop(&self) {
        let pause = self
            .stop_side_effect_pause
            .lock()
            .expect("test stop pause poisoned")
            .clone();
        if let Some((reached, release)) = pause {
            reached.wait().await;
            release.wait().await;
        }
    }

    #[cfg(test)]
    fn force_incomplete_stop_report(&self, id: SourceId) {
        self.incomplete_stop_reports
            .lock()
            .expect("test incomplete stop reports poisoned")
            .insert(id);
    }

    #[cfg(test)]
    fn reported_stop_complete(&self, id: SourceId, complete: bool) -> bool {
        complete
            && !self
                .incomplete_stop_reports
                .lock()
                .expect("test incomplete stop reports poisoned")
                .remove(&id)
    }

    #[cfg(not(test))]
    fn reported_stop_complete(&self, _id: SourceId, complete: bool) -> bool {
        complete
    }

    /// Resume the persisted session set with exactly startup-resume rules:
    /// files re-acquire from durable cursors; anything else (commands,
    /// HTTP, stdin without a live pipe) is recorded with its reason and
    /// left stopped. In particular a remembered command is never launched
    /// here: it waits for an explicit `RequestStart`, mirroring restore's
    /// `RestoreWouldLaunchCommand` refusal.
    pub async fn resume_session(self: &Arc<Self>) -> Vec<(SourceId, Result<(), String>)> {
        let session = self.session.lock().await.clone();
        let mut outcomes = Vec::with_capacity(session.len());
        for definition in session {
            let id = definition.id;
            if let Err(reason) = validate_start_boundary(&definition) {
                outcomes.push((id, Err(format!("resume: {reason}"))));
                continue;
            }
            let definition = canonicalize_definition(definition);
            let outcome = match &definition.acquisition {
                lvu_core::Acquisition::File { .. } => {
                    let outcome = self
                        .manager
                        .restore(definition.clone())
                        .await
                        .map_err(|error| format!("resume: {error}"));
                    if outcome.is_ok() {
                        self.definitions.lock().await.insert(id, definition);
                    }
                    outcome.map(|_| ())
                }
                other => Err(format!(
                    "not acquiring on resume (needs explicit start): {}",
                    acquisition_kind(other)
                )),
            };
            outcomes.push((id, outcome));
        }
        outcomes
    }

    /// Publish all worker-owned state for a manager-started source. Every
    /// async lock is acquired before the first mutation; from that point to
    /// the durable write there is no cancellation point. Thus shutdown may
    /// abort a pre-publication settlement safely, or drain a wholly published
    /// one, but can never leave only half of bindings/definitions/session.
    async fn publish_started(
        &self,
        definition: &SourceDefinition,
        stdin_binding: Option<StdinBinding>,
    ) -> Option<String> {
        let mut stdin_bindings = self.stdin_bindings.lock().await;
        let mut definitions = self.definitions.lock().await;
        let mut session = self.session.lock().await;
        if let Some(binding) = stdin_binding {
            stdin_bindings.insert(definition.id, binding);
        }
        definitions.insert(definition.id, definition.clone());
        if !session.iter().any(|value| value.id == definition.id) {
            session.push(definition.clone());
        }
        let snapshot = session.clone();
        store_session_set(&self.config.workspace_root, &snapshot).err()
    }

    async fn note_session_stopped(&self, id: SourceId) -> Result<(), String> {
        let mut session = self.session.lock().await;
        session.retain(|definition| definition.id != id);
        let snapshot = session.clone();
        drop(session);
        store_session_set(&self.config.workspace_root, &snapshot)
    }

    /// Capture journal path for a source, mirroring the ingest layout
    /// (`<capture-root>/<uuid>/capture.journal`, see `manager.rs`). Used
    /// for `Started` replies and sidebar presence so windows never hardcode
    /// the layout; readability is proven by tail tests, not by this string.
    pub fn journal_path_for(&self, id: SourceId) -> PathBuf {
        self.config
            .capture_root
            .join(id.0.to_string())
            .join("capture.journal")
    }

    /// True when shutdown was requested or no viewers remain past grace.
    /// The serve loop polls this alongside socket activity. Synchronous
    /// request path so signal handlers and tests flip it without awaiting.
    pub async fn should_stop(&self) -> bool {
        if self
            .shutdown_flag
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return true;
        }
        self.viewers
            .lock()
            .await
            .should_shutdown(std::time::Instant::now())
    }

    pub fn request_shutdown(&self) {
        self.shutdown_flag
            .store(true, std::sync::atomic::Ordering::Release);
        self.lifecycle_admission.close();
    }
}

fn acquisition_kind(acquisition: &lvu_core::Acquisition) -> &'static str {
    match acquisition {
        lvu_core::Acquisition::File { .. } => "file",
        lvu_core::Acquisition::Command { .. } => "command",
        lvu_core::Acquisition::Http { .. } => "http",
        lvu_core::Acquisition::Stdin => "stdin",
    }
}

impl WorkerService {
    /// Current presence snapshot for sidebars: every known definition with
    /// live health when the manager holds it, `stopped` otherwise. Counts
    /// are sampled at event time (see `STATUS_INTERVAL`); the journal files
    /// remain the authority for rows.
    pub async fn presence_snapshot(&self) -> Vec<SourceSummary> {
        let definitions = self.definitions.lock().await;
        let mut summaries: Vec<SourceSummary> = definitions
            .values()
            .map(|definition| {
                let id = definition.id;
                let (health, journal) = match self.manager.source(id) {
                    Some(handle) => {
                        let progress = handle.progress();
                        let health = match progress.last_error.clone() {
                            Some(error) => {
                                format!("{:?}: {}", progress.state, error)
                            }
                            None => format!("{:?}", progress.state),
                        };
                        (health, self.journal_path_for(id).display().to_string())
                    }
                    None => (
                        "stopped".to_owned(),
                        self.journal_path_for(id).display().to_string(),
                    ),
                };
                SourceSummary {
                    id: id.0.to_string(),
                    name: definition.name.clone(),
                    kind: acquisition_kind(&definition.acquisition).into(),
                    health,
                    journal_path: journal,
                }
            })
            .collect();
        summaries.sort_by(|first, second| first.name.cmp(&second.name));
        summaries
    }

    /// Sample one live source's canonical progress for the poll RPC. The
    /// value travels verbatim — never projected, never zero-filled — so
    /// adapter scans and app diagnostics read the same truth the local
    /// watch would show. Stopped handles report their terminal snapshot
    /// (that is how windows read final counts and `last_error` after a
    /// capture ends); only truly unknown ids are refused. Notably this
    /// offers NO publication fencing: it is a point sample, not a
    /// `lock_progress` equivalent, and must never be presented as one
    /// (remote unions stay refused until the cross-process fence exists).
    fn poll_source_progress(&self, source_id: &str) -> Result<lvu_ingest::SourceProgress, String> {
        let id = uuid::Uuid::parse_str(source_id)
            .map(SourceId)
            .map_err(|_| "invalid source id".to_owned())?;
        let handle = self
            .manager
            .source(id)
            .ok_or_else(|| "source is not live on this worker".to_owned())?;
        Ok(handle.progress())
    }

    /// Admit one explicit acquisition request: the hook decides, the manager
    /// executes, the session records. Stdin definitions bind a forwarded
    /// stream instead of attaching a pipe: the caller gets `StdinOpen` and
    /// must stream chunks (see `note_stdin_chunk`).
    ///
    /// Concurrent starts of one canonical acquisition serialize on a
    /// per-key reservation: exactly one leader runs admission plus the
    /// manager start, and every joiner receives the winner's identity. The
    /// reservation never spans an await while held — the map is only
    /// touched for pointer-sized insert/remove — so no connection can stall
    /// another, and the per-request dispatch timeout still bounds every
    /// waiter. Stdin attachments bypass the reservation entirely: each one
    /// is an independent pipeline by invariant.
    pub async fn request_start(
        self: &Arc<Self>,
        definition: SourceDefinition,
    ) -> Result<StartedOutcome, String> {
        #[cfg(test)]
        self.pause_before_lifecycle_admission().await;
        // This must precede canonicalization, key construction and admission:
        // an invalid candidate may resemble a live definition but must never
        // receive a success-shaped Present reply.
        validate_start_boundary(&definition)?;
        let definition = canonicalize_definition(definition);
        let Some(key) = admission_key(&definition)? else {
            // Stdin attachments are independent pipelines by invariant:
            // no shared identity exists to deduplicate. Its settlement still
            // outlives the request so a dispatch timeout cannot orphan a
            // manager-owned reader before definitions/session registration.
            let permit = self.lifecycle_admission.try_enter(definition.id)?;
            return self.spawn_keyless_start(definition, permit).await;
        };
        loop {
            // Admission and shutdown closure are one atomic decision. A
            // leader transfers this permit to its detached settlement; a
            // joiner retains it while observing the shared outcome.
            let permit = self.lifecycle_admission.try_enter(definition.id)?;
            match self.enter_start_key(&key) {
                KeyEntry::Lead(shared) => {
                    let service = Arc::clone(self);
                    let leader_shared = Arc::clone(&shared);
                    let leader_key = key.clone();
                    let leader_definition = definition.clone();
                    let settlement_id = permit.id;
                    let settlement = tokio::spawn(async move {
                        service
                            .settle_start_leader(
                                leader_key,
                                leader_shared,
                                leader_definition,
                                permit,
                            )
                            .await;
                    });
                    self.lifecycle_admission
                        .attach_settlement(settlement_id, settlement.abort_handle());
                    if let Some(outcome) = self.wait_start_settlement(&key, shared, true).await? {
                        return Ok(outcome);
                    }
                }
                KeyEntry::Join(shared) => {
                    let _permit = permit;
                    if let Some(outcome) = self.wait_start_settlement(&key, shared, false).await? {
                        return Ok(outcome);
                    }
                }
            }
        }
    }

    /// Settle one explicit acquisition request against current worker state:
    /// the hook decides, the manager executes, the session records. Shared
    /// by leaders (under reservation) and keyless requests (stdin).
    async fn settle_start_path(
        &self,
        definition: SourceDefinition,
    ) -> Result<StartedOutcome, String> {
        // Snapshot of live definitions for hooks that dedup against worker
        // state (the standalone child); hooks carrying their own live state
        // keep using `admit` through the default.
        let live: Vec<SourceDefinition> = self.definitions.lock().await.values().cloned().collect();
        match self.admission.admit_known(&definition, &live) {
            AdmissionVerdict::Refuse(reason) => Err(reason),
            AdmissionVerdict::Present { live_id } => self.present_or_restart(live_id).await,
            AdmissionVerdict::Admit => self.start_admitted(definition).await,
        }
    }

    async fn spawn_keyless_start(
        self: &Arc<Self>,
        definition: SourceDefinition,
        permit: LifecyclePermit,
    ) -> Result<StartedOutcome, String> {
        let (send, receive) = tokio::sync::oneshot::channel();
        let service = Arc::clone(self);
        let settlement_id = permit.id;
        let settlement = tokio::spawn(async move {
            let _permit = permit;
            let _lifecycle = service.lifecycle.lock().await;
            if let Err(error) = _permit.begin() {
                let _ = send.send(Err(error));
                return;
            }
            // Keyless means stdin. Deliberately bypass `admit_known`: every
            // fresh pipe is independent, while SourceManager still refuses
            // an already-used id.
            let outcome = service.start_admitted(definition).await;
            let _ = send.send(outcome);
        });
        self.lifecycle_admission
            .attach_settlement(settlement_id, settlement.abort_handle());
        receive
            .await
            .map_err(|_| "worker lifecycle settlement task closed".to_owned())?
    }

    async fn settle_start_leader(
        self: &Arc<Self>,
        key: String,
        shared: Arc<SharedStart>,
        definition: SourceDefinition,
        permit: LifecyclePermit,
    ) {
        // This is deliberately declared before StartGuard: reverse drop
        // order makes reservation abandonment/removal happen before the
        // lifecycle count can publish fully drained during task abortion.
        let _permit = permit;
        let mut guard = StartGuard {
            starting: Arc::clone(&self.starting),
            key: key.clone(),
            mine: Arc::clone(&shared),
            settled: false,
        };
        let _lifecycle = self.lifecycle.lock().await;
        let outcome = match _permit.begin() {
            Ok(()) => self.settle_start_path(definition).await,
            Err(error) => Err(error),
        };
        self.publish_settlement(&key, &shared, &outcome, &mut guard);
    }

    /// Wait for a detached leader. `Ok(None)` asks the caller to re-enter
    /// admission after a panicked/abandoned leader; ordinary request
    /// cancellation cannot produce abandonment because the leader task owns
    /// the settlement independently.
    async fn wait_start_settlement(
        &self,
        key: &str,
        shared: Arc<SharedStart>,
        leader: bool,
    ) -> Result<Option<StartedOutcome>, String> {
        let mut settled = shared.outcome.subscribe();
        loop {
            if let Some(outcome) = (*settled.borrow()).clone() {
                match outcome {
                    StartSettlement::Settled(outcome) => {
                        // The elected caller receives the exact completed
                        // operation even when a finite source reaches EOF
                        // before this task is scheduled again. Liveness
                        // revalidation is only for joiners deciding whether
                        // an older success can still be presented.
                        if leader {
                            return Ok(Some(outcome));
                        }
                        let id = match &outcome {
                            StartedOutcome::Started { source_id, .. }
                            | StartedOutcome::StdinBound { source_id, .. } => *source_id,
                            StartedOutcome::Present { live_id } => *live_id,
                        };
                        if self.is_live(id) {
                            return Ok(Some(StartedOutcome::Present { live_id: id }));
                        }
                    }
                    StartSettlement::Failed(error) => return Err(error),
                    StartSettlement::Abandoned => {}
                }
                let mut starting = self.starting.lock().expect("start registry poisoned");
                if let Some(current) = starting.get(key)
                    && Arc::ptr_eq(current, &shared)
                {
                    starting.remove(key);
                }
                return Ok(None);
            }
            if settled.changed().await.is_err() {
                return Ok(None);
            }
        }
    }

    /// A handle counts as live unless its manager progress is terminally
    /// finished. Synchronous point sample over short-held locks only.
    fn is_live(&self, id: SourceId) -> bool {
        self.manager
            .source(id)
            .is_some_and(|handle| !handle.progress().state.is_terminal())
    }

    /// Resolve a structural match against actual capture liveness: only a
    /// live (or settling) acquisition may present. An exact stopped match
    /// restarts the original id — preserving its identity, journal path and
    /// durable cursor with no recapture — and stdin refuses honestly since
    /// a stopped pipe cannot resume. Never emits `Started` for an unchanged
    /// terminal handle.
    async fn present_or_restart(&self, live_id: SourceId) -> Result<StartedOutcome, String> {
        if self.is_live(live_id) {
            return Ok(StartedOutcome::Present { live_id });
        }
        let definition = self
            .definitions
            .lock()
            .await
            .get(&live_id)
            .cloned()
            .ok_or_else(|| format!("unknown source {}", live_id.0))?;
        if matches!(definition.acquisition, lvu_core::Acquisition::Stdin) {
            return Err(format!(
                "stdin source {} is stopped; attach a fresh pipeline",
                live_id.0
            ));
        }
        self.restart_original(definition).await
    }

    /// Stop-if-present then start the same definition under its original id.
    /// Callers hold the canonical-key leadership (or run keyless), so no
    /// second capture of the acquisition can interleave; the manager
    /// resumes the durable cursor under the original id and journal path.
    async fn restart_original(
        &self,
        definition: SourceDefinition,
    ) -> Result<StartedOutcome, String> {
        let id = definition.id;
        if let Some(handle) = self.manager.source(id)
            && !handle.progress().state.is_terminal()
        {
            let report = handle
                .stop()
                .await
                .map_err(|error| format!("stop: {error}"))?;
            if !report.complete {
                return Err("capture stop incomplete; restart was not attempted".into());
            }
        }
        self.start_admitted(definition).await
    }

    /// The role this caller takes for one canonical key: sole leader, or a
    /// joiner observing the leader's settled outcome. Synchronous: the
    /// registry is never held across an await.
    fn enter_start_key(&self, key: &str) -> KeyEntry {
        let mut starting = self.starting.lock().expect("start registry poisoned");
        match starting.get(key) {
            Some(shared) => KeyEntry::Join(Arc::clone(shared)),
            None => {
                let (outcome, _) = tokio::sync::watch::channel(None::<StartSettlement>);
                let shared = Arc::new(SharedStart { outcome });
                starting.insert(key.to_owned(), Arc::clone(&shared));
                KeyEntry::Lead(shared)
            }
        }
    }

    /// Publish a leader outcome to joiners and release the reservation.
    /// Removal is unconditional here: no second leader can exist while this
    /// entry is present, and abandon-observers only remove entries whose
    /// outcome is still unsettled.
    fn publish_settlement(
        &self,
        key: &str,
        shared: &Arc<SharedStart>,
        outcome: &Result<StartedOutcome, String>,
        guard: &mut StartGuard,
    ) {
        let settlement = match outcome {
            Ok(outcome) => StartSettlement::Settled(outcome.clone()),
            Err(error) => StartSettlement::Failed(error.clone()),
        };
        shared.outcome.send_modify(|slot| {
            *slot = Some(settlement);
        });
        guard.disarm();
        self.starting
            .lock()
            .expect("start registry poisoned")
            .remove(key);
    }

    async fn start_admitted(&self, definition: SourceDefinition) -> Result<StartedOutcome, String> {
        let id = definition.id;
        if matches!(definition.acquisition, lvu_core::Acquisition::Stdin) {
            let (writer, reader) = tokio::io::duplex(crate::STDIN_BUFFER_BYTES);
            let handle = self
                .manager
                .start_with_reader(definition.clone(), reader)
                .await
                .map_err(|error| format!("start stdin: {error}"))?;
            drop(handle);
            #[cfg(test)]
            self.pause_after_manager_start().await;
            let warning = self
                .publish_started(
                    &definition,
                    Some(StdinBinding {
                        writer,
                        expected_seq: 0,
                    }),
                )
                .await;
            return Ok(StartedOutcome::StdinBound {
                source_id: id,
                warning,
            });
        }
        let handle = self
            .manager
            .start(definition.clone())
            .await
            .map_err(|error| format!("start {}: {error}", definition.name))?;
        drop(handle);
        #[cfg(test)]
        self.pause_after_manager_start().await;
        let warning = self.publish_started(&definition, None).await;
        Ok(StartedOutcome::Started {
            source_id: id,
            warning,
        })
    }

    /// Note one validated stdin chunk into its bound stream, enforcing exact
    /// sequence order. Returns the ack sequence for the credit reply.
    /// Unknown streams report `UnknownStream` (stale after a replacement:
    /// re-open); gaps, duplicates, and oversize payloads report `Desync`
    /// and the caller drops the connection.
    pub async fn note_stdin_chunk(
        &self,
        source_id: SourceId,
        seq: u64,
        raw: &[u8],
    ) -> Result<u64, StdinNoteError> {
        use tokio::io::AsyncWriteExt;
        if raw.len() > crate::MAX_STDIN_CHUNK_BYTES {
            return Err(StdinNoteError::Desync(format!(
                "stdin chunk is {} bytes; limit is {}",
                raw.len(),
                crate::MAX_STDIN_CHUNK_BYTES
            )));
        }
        let mut bindings = self.stdin_bindings.lock().await;
        let Some(binding) = bindings.get_mut(&source_id) else {
            return Err(StdinNoteError::UnknownStream);
        };
        if seq != binding.expected_seq {
            return Err(StdinNoteError::Desync(format!(
                "stdin chunk out of order: got {seq}, expected {}",
                binding.expected_seq
            )));
        }
        binding
            .writer
            .write_all(raw)
            .await
            .map_err(|error| StdinNoteError::Desync(format!("stdin stream closed: {error}")))?;
        binding.expected_seq += 1;
        Ok(seq)
    }

    /// End one forwarded stream: dropping the writer delivers EOF to the
    /// capture, which closes as incomplete with arrived bytes kept.
    pub async fn close_stdin(&self, source_id: SourceId) {
        self.stdin_bindings.lock().await.remove(&source_id);
    }

    /// Explicit stop: halts capture, drops any stdin binding, and removes
    /// the definition from the session set.
    pub async fn request_stop(self: &Arc<Self>, id: SourceId) -> Result<(), String> {
        #[cfg(test)]
        self.pause_before_lifecycle_admission().await;
        let permit = self.lifecycle_admission.try_enter(id)?;
        let (send, receive) = tokio::sync::oneshot::channel();
        let service = Arc::clone(self);
        let settlement_id = permit.id;
        let settlement = tokio::spawn(async move {
            let _permit = permit;
            let _lifecycle = service.lifecycle.lock().await;
            let outcome = match _permit.begin() {
                Ok(()) => service.stop_inner(id).await,
                Err(error) => Err(error),
            };
            let _ = send.send(outcome);
        });
        self.lifecycle_admission
            .attach_settlement(settlement_id, settlement.abort_handle());
        receive
            .await
            .map_err(|_| "worker lifecycle settlement task closed".to_owned())?
    }

    async fn stop_inner(&self, id: SourceId) -> Result<(), String> {
        self.stdin_bindings.lock().await.remove(&id);
        if let Some(handle) = self.manager.source(id)
            && !handle.progress().state.is_terminal()
        {
            let report = handle
                .stop()
                .await
                .map_err(|error| format!("stop: {error}"))?;
            #[cfg(test)]
            self.pause_after_manager_stop().await;
            return self
                .finish_stop(id, self.reported_stop_complete(id, report.complete))
                .await;
        }
        self.finish_stop(id, true).await
    }

    async fn finish_stop(&self, id: SourceId, complete: bool) -> Result<(), String> {
        if !complete {
            return Err("capture stop incomplete; source remains in the session".into());
        }
        self.note_session_stopped(id).await.map_err(|error| {
            format!("capture stopped but session update failed: {error}; retry stop")
        })?;
        Ok(())
    }

    /// Explicit restart: stdin refuses per the existing rule (a fresh
    /// pipeline needs a new attachment); anything else stops completely
    /// first and only then starts, so a partial stop never silently becomes
    /// a second capture. The shared lifecycle gate orders this against every
    /// start, Present-to-restart and stop transition.
    pub async fn request_restart(self: &Arc<Self>, id: SourceId) -> Result<StartedOutcome, String> {
        #[cfg(test)]
        self.pause_before_lifecycle_admission().await;
        let permit = self.lifecycle_admission.try_enter(id)?;
        let (send, receive) = tokio::sync::oneshot::channel();
        let service = Arc::clone(self);
        let settlement_id = permit.id;
        let settlement = tokio::spawn(async move {
            let _permit = permit;
            let _lifecycle = service.lifecycle.lock().await;
            let outcome = match _permit.begin() {
                Ok(()) => service.restart_inner(id).await,
                Err(error) => Err(error),
            };
            let _ = send.send(outcome);
        });
        self.lifecycle_admission
            .attach_settlement(settlement_id, settlement.abort_handle());
        receive
            .await
            .map_err(|_| "worker lifecycle settlement task closed".to_owned())?
    }

    async fn restart_inner(&self, id: SourceId) -> Result<StartedOutcome, String> {
        let definition = self
            .definitions
            .lock()
            .await
            .get(&id)
            .cloned()
            .ok_or_else(|| "unknown source".to_owned())?;
        if matches!(definition.acquisition, lvu_core::Acquisition::Stdin) {
            return Err("stdin cannot restart; attach a fresh pipeline".into());
        }
        self.restart_original(definition).await
    }

    /// Look up the currently committed version of a view for conflict
    /// reports. Diagnostic only: the store row remains the authority, and
    /// this extra read happens solely on the failure path.
    async fn current_view_version(
        &self,
        source_id: SourceId,
        view_id: lvu_core::ViewId,
    ) -> Option<u64> {
        let views = self
            .with_store(move |store| {
                // Within the store page limit (100): a bounded diagnostic
                // read on the failure path only.
                store
                    .working_views_for_source(source_id, 64)
                    .map_err(|error| error.to_string())
            })
            .await
            .ok()?;
        views
            .into_iter()
            .find(|view| view.id == view_id)
            .map(|view| view.version)
    }

    /// Mediated view save: one synchronous store transaction with the
    /// client's `expected_version` passed straight through. There is no
    /// cross-request batching here by design (see the module docs): every
    /// request commits or fails explicitly, so every ack is truthful and no
    /// superseded state can be acked as saved. A version mismatch fails with
    /// the committed version echoed; the window keeps its draft and retries
    /// only after an explicit user rebase.
    pub async fn mediated_save(
        &self,
        request_id: String,
        sequence: u64,
        definition: SourceDefinition,
        view_id: lvu_core::ViewId,
        state: lvu_memory::WorkingView,
        expected_version: Option<u64>,
    ) -> StoreEvent {
        if state.id != view_id || state.source_id != definition.id {
            return StoreEvent::SaveFailed {
                request_id,
                source_id: definition.id,
                view_id,
                sequence,
                reason: "view/source identity mismatch".into(),
                current_version: None,
            };
        }
        let metadata = source_metadata(definition.clone());
        let outcome = self
            .with_store(move |store| {
                store
                    .save_sources_and_views(&[(metadata, state, expected_version)])
                    .into_iter()
                    .next()
                    .expect("single entry yields a single outcome")
            })
            .await;
        match outcome {
            Ok(version) => StoreEvent::Saved {
                request_id,
                source_id: definition.id,
                view_id,
                sequence,
                version,
            },
            Err(error) => {
                let current_version = self.current_view_version(definition.id, view_id).await;
                StoreEvent::SaveFailed {
                    request_id,
                    source_id: definition.id,
                    view_id,
                    sequence,
                    reason: error,
                    current_version,
                }
            }
        }
    }

    /// Mediated view load: upsert source, ensure the canonical view, and
    /// return existing views untouched with their persisted versions (the
    /// values later saves must echo back). Mirrors the `Command::Load`
    /// worker arm (`memory.rs`), including the canonical-view addition rule.
    pub async fn mediated_load(
        &self,
        request_id: String,
        definition: SourceDefinition,
        view_id: lvu_core::ViewId,
    ) -> StoreEvent {
        let source_id = definition.id;
        let outcome = self
            .with_store(move |store| {
                let metadata = source_metadata(definition.clone());
                store
                    .upsert_source(&metadata)
                    .map_err(|error| error.to_string())?;
                let canonical = store
                    .ensure_canonical_view(
                        definition.id,
                        canonical_view_id(definition.id),
                        "All events",
                    )
                    .map_err(|error| error.to_string())?;
                let mut views = store
                    .working_views_for_source(definition.id, 33)
                    .map_err(|error| error.to_string())?;
                if !views.iter().any(|value| value.id == canonical.id) {
                    views.push(canonical);
                }
                Ok::<_, String>(views)
            })
            .await;
        match outcome {
            Ok(views) => StoreEvent::Loaded {
                request_id,
                source_id,
                view_id,
                views,
            },
            Err(error) => StoreEvent::LoadFailed {
                request_id,
                source_id,
                view_id,
                reason: error,
            },
        }
    }

    /// Mediated derived-view creation: the store's uniqueness on `view_id`
    /// is the exactly-once authority (a retry with the same id fails
    /// instead of forking a duplicate); only success installs the view.
    /// Fork identity therefore needs no extra tracking: a fork mints a
    /// fresh view UUID, and a repeated create with an occupied UUID is a
    /// conflict, never a second view.
    pub async fn mediated_create_derived_view(
        &self,
        request_id: String,
        sequence: u64,
        definition: SourceDefinition,
        view_id: lvu_core::ViewId,
        state: lvu_memory::WorkingView,
    ) -> StoreEvent {
        if state.id != view_id || state.source_id != definition.id {
            return StoreEvent::SaveFailed {
                request_id,
                source_id: definition.id,
                view_id,
                sequence,
                reason: "view/source identity mismatch".into(),
                current_version: None,
            };
        }
        let metadata = source_metadata(definition.clone());
        let outcome = self
            .with_store(move |store| {
                store
                    .upsert_source(&metadata)
                    .map_err(|error| error.to_string())?;
                store
                    .create_view(&state)
                    .map_err(|error| error.to_string())?;
                let views = store
                    .working_views_for_source(definition.id, 64)
                    .map_err(|error| error.to_string())?;
                views
                    .into_iter()
                    .find(|view| view.id == view_id)
                    .map(|view| view.version)
                    .ok_or_else(|| "created view is missing after commit".to_owned())
            })
            .await;
        match outcome {
            Ok(version) => StoreEvent::Saved {
                request_id,
                source_id: definition.id,
                view_id,
                sequence,
                version,
            },
            Err(error) => StoreEvent::SaveFailed {
                request_id,
                source_id: definition.id,
                view_id,
                sequence,
                reason: error,
                current_version: None,
            },
        }
    }

    /// Mediated recipe save: `expected_revision` hesitation preserved
    /// verbatim from `Command::SaveRecipe` (`memory.rs`): a revisioned save
    /// updates exactly that revision, an unrevisioned save with suggestion
    /// context upserts its source first, otherwise a brand-new recipe row.
    pub async fn mediated_save_recipe(
        &self,
        request_id: String,
        meta: RequestMeta,
        recipe: lvu_memory::RecipeFile,
        expected_revision: Option<uuid::Uuid>,
        context: Option<SuggestionContextShape>,
    ) -> StoreEvent {
        let outcome = self
            .with_store(move |store| {
                if let Some(revision) = expected_revision {
                    return store
                        .update_recipe_revision(
                            recipe.recipe_id,
                            revision,
                            &recipe.view,
                            recipe.saved_at_unix_nanos,
                        )
                        .map_err(|error| error.to_string());
                }
                if let Some(context) = context {
                    let mut metadata = source_metadata(recipe.source.clone());
                    metadata.project = context.project;
                    metadata.command = context.command;
                    metadata.fields = context.fields;
                    store
                        .upsert_source(&metadata)
                        .map_err(|error| error.to_string())?;
                }
                store
                    .save_new_recipe(&recipe)
                    .map_err(|error| error.to_string())
            })
            .await;
        match outcome {
            Ok(saved) => StoreEvent::RecipeSaved {
                request_id,
                meta,
                saved,
            },
            Err(error) => StoreEvent::RecipeFailed {
                request_id,
                meta,
                reason: format!("save recipe: {error}"),
            },
        }
    }

    /// Mediated recipe import/export/history/list: direct store calls with
    /// the same limits the worker applies in-process (list 128, history
    /// 100, candidates 16). Paths are absolute; the worker reads/writes
    /// them directly.
    pub async fn mediated_import_recipe(
        &self,
        request_id: String,
        meta: RequestMeta,
        path: PathBuf,
    ) -> StoreEvent {
        match self
            .with_store(move |store| {
                store
                    .import_new_recipe(&path)
                    .map_err(|error| error.to_string())
            })
            .await
        {
            Ok(saved) => StoreEvent::RecipeSaved {
                request_id,
                meta,
                saved,
            },
            Err(error) => StoreEvent::RecipeFailed {
                request_id,
                meta,
                reason: format!("import recipe: {error}"),
            },
        }
    }

    pub async fn mediated_export_recipe(
        &self,
        request_id: String,
        meta: RequestMeta,
        recipe_id: lvu_core::RecipeId,
        revision: uuid::Uuid,
        path: PathBuf,
    ) -> StoreEvent {
        match self
            .with_store(move |store| {
                store
                    .export_recipe_revision(recipe_id, revision, &path)
                    .map_err(|error| error.to_string())
            })
            .await
        {
            Ok(saved) => StoreEvent::RecipeExported {
                request_id,
                meta,
                saved,
            },
            Err(error) => StoreEvent::RecipeFailed {
                request_id,
                meta,
                reason: format!("export recipe: {error}"),
            },
        }
    }

    pub async fn mediated_recipe_history(
        &self,
        request_id: String,
        meta: RequestMeta,
        recipe_id: lvu_core::RecipeId,
    ) -> StoreEvent {
        match self
            .with_store(move |store| {
                store
                    .recipe_revision_documents(recipe_id, 100)
                    .map_err(|error| error.to_string())
            })
            .await
        {
            Ok(revisions) => StoreEvent::RecipeHistory {
                request_id,
                meta,
                revisions,
            },
            Err(error) => StoreEvent::RecipeFailed {
                request_id,
                meta,
                reason: format!("recipe history: {error}"),
            },
        }
    }

    pub async fn mediated_list_recipes(
        &self,
        request_id: String,
        meta: RequestMeta,
        context: Option<SuggestionContextShape>,
    ) -> StoreEvent {
        let outcome = self
            .with_store(move |store| {
                let values = store.list_recipes(128).map_err(|error| error.to_string())?;
                let candidates = context
                    .map(|context| {
                        store
                            .candidates(
                                context.source,
                                context.project.as_deref(),
                                context.command.as_deref(),
                                &context.fields,
                                16,
                            )
                            .map_err(|error| error.to_string())
                    })
                    .transpose()?
                    .unwrap_or_default();
                Ok::<_, String>((values, candidates))
            })
            .await;
        match outcome {
            Ok((recipes, candidates)) => StoreEvent::Recipes {
                request_id,
                meta,
                recipes,
                candidates,
            },
            Err(error) => StoreEvent::RecipeFailed {
                request_id,
                meta,
                reason: format!("list recipes: {error}"),
            },
        }
    }

    /// Mediated suggestion outcome: parses and validates identifiers exactly
    /// like the worker (`memory.rs` record path), records the outcome, and
    /// records usage on acceptance. Pure store call, no subprocess.
    pub async fn mediated_record_suggestion(
        &self,
        request_id: String,
        outcome: SuggestionOutcomeShape,
    ) -> StoreEvent {
        let outcome = self
            .with_store(move |store| {
                use std::time::{SystemTime, UNIX_EPOCH};
                let parse = |value: &str| {
                    uuid::Uuid::parse_str(value)
                        .map_err(|error| lvu_memory::MemoryError::InvalidData(error.to_string()))
                };
                let source = lvu_core::SourceId(parse(&outcome.source_id)?);
                let recipe = lvu_core::RecipeId(parse(&outcome.recipe_id)?);
                let revision = parse(&outcome.revision)?;
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs()
                    .min(i64::MAX as u64) as i64;
                let kind = if outcome.accepted {
                    lvu_memory::SuggestionOutcome::Accepted
                } else {
                    lvu_memory::SuggestionOutcome::Rejected
                };
                store.record_suggestion(source, recipe, revision, kind, now)?;
                if outcome.accepted {
                    store.record_usage(source, recipe, now)?;
                }
                Ok::<_, lvu_memory::MemoryError>(())
            })
            .await;
        match outcome {
            Ok(()) => StoreEvent::SuggestionRecorded { request_id },
            Err(error) => StoreEvent::SuggestionFailed {
                request_id,
                reason: error,
            },
        }
    }

    pub async fn mediated_recent(&self, request_id: String) -> StoreEvent {
        match self
            .with_store(|store| {
                store
                    .recent_sources(None, 32)
                    .map_err(|error| error.to_string())
            })
            .await
        {
            Ok(sources) => StoreEvent::Recent {
                request_id,
                sources,
            },
            Err(error) => StoreEvent::RecentFailed {
                request_id,
                reason: format!("recent sources: {error}"),
            },
        }
    }

    /// Route one parsed store method: window check, size-check at the real
    /// boundary (the frame cap bounds the wire, this bounds the decoded
    /// value before any store work), then the matching mediated call under
    /// a bounded timeout. Every arm maps 1:1 to a `memory::Command` variant
    /// with identical success/failure shapes. A timed-out dispatch reports
    /// outcome-unknown rather than success or failure: the spawned store
    /// work is not cancelled (it may still commit late), so neither claim
    /// would be honest.
    pub async fn dispatch_store(&self, attached: &str, method: StoreMethod) -> StoreEvent {
        if method.window_id() != attached {
            return store_failure(
                method,
                format!("window identity mismatch: attached as '{attached}'"),
            );
        }
        // Fixed-size methods (two short strings at most) skip the payload
        // check: the frame cap already bounded their wire bytes, and there
        // is no DTO to measure. Everything carrying a definition, view,
        // recipe, or outcome is measured below before any store work.
        match &method {
            StoreMethod::Recent { .. } | StoreMethod::Flush { .. } => {}
            _ => {
                if let Err(error) = check_store_size(&method) {
                    return store_failure(method, error.to_string());
                }
            }
        }
        let timeout = self.config.request_timeout;
        let unknown = store_timeout(&method, timeout);
        match tokio::time::timeout(timeout, self.dispatch_store_inner(method)).await {
            Ok(event) => event,
            Err(_) => unknown,
        }
    }

    async fn dispatch_store_inner(&self, method: StoreMethod) -> StoreEvent {
        match method {
            StoreMethod::Load {
                request_id,
                window_id: _,
                definition,
                view_id,
            } => self.mediated_load(request_id, definition, view_id).await,
            StoreMethod::Save {
                request_id,
                window_id: _,
                sequence,
                definition,
                view_id,
                state,
                expected_version,
            } => {
                self.mediated_save(
                    request_id,
                    sequence,
                    definition,
                    view_id,
                    state,
                    expected_version,
                )
                .await
            }
            StoreMethod::CreateDerivedView {
                request_id,
                window_id: _,
                sequence,
                definition,
                view_id,
                state,
            } => {
                self.mediated_create_derived_view(request_id, sequence, definition, view_id, state)
                    .await
            }
            StoreMethod::Recent {
                request_id,
                window_id: _,
            } => self.mediated_recent(request_id).await,
            StoreMethod::ListRecipes {
                request_id,
                window_id: _,
                meta,
                context,
            } => self.mediated_list_recipes(request_id, meta, context).await,
            StoreMethod::RecipeHistory {
                request_id,
                window_id: _,
                meta,
                recipe_id,
            } => {
                self.mediated_recipe_history(request_id, meta, recipe_id)
                    .await
            }
            StoreMethod::SaveRecipe {
                request_id,
                window_id: _,
                meta,
                recipe,
                expected_revision,
                context,
            } => {
                self.mediated_save_recipe(request_id, meta, recipe, expected_revision, context)
                    .await
            }
            StoreMethod::ImportRecipe {
                request_id,
                window_id: _,
                meta,
                path,
            } => self.mediated_import_recipe(request_id, meta, path).await,
            StoreMethod::ExportRecipe {
                request_id,
                window_id: _,
                meta,
                recipe_id,
                revision,
                path,
            } => {
                self.mediated_export_recipe(request_id, meta, recipe_id, revision, path)
                    .await
            }
            StoreMethod::RecordSuggestion {
                request_id,
                window_id: _,
                outcome,
            } => self.mediated_record_suggestion(request_id, outcome).await,
            StoreMethod::Flush { request_id, .. } => {
                // No queue exists behind this boundary: every request above
                // committed or failed explicitly before its reply, so flush
                // acknowledges immediately. (The in-process worker batches
                // across clients and truly drains; that batching is what the
                // per-request transactions here replace.)
                StoreEvent::Flushed { request_id }
            }
        }
    }
}

/// A refused store command, mapped explicitly per method so the caller
/// learns which request died and why. Takes the method by value because the
/// failure shapes need its correlation ids; covers both oversize refusals
/// and pre-dispatch rejections like window-identity mismatches.
fn store_failure(method: StoreMethod, reason: String) -> StoreEvent {
    match method {
        StoreMethod::Load {
            request_id,
            definition,
            view_id,
            ..
        } => StoreEvent::LoadFailed {
            request_id,
            source_id: definition.id,
            view_id,
            reason,
        },
        StoreMethod::Save {
            request_id,
            definition,
            view_id,
            sequence,
            ..
        }
        | StoreMethod::CreateDerivedView {
            request_id,
            definition,
            view_id,
            sequence,
            ..
        } => StoreEvent::SaveFailed {
            request_id,
            source_id: definition.id,
            view_id,
            sequence,
            reason,
            current_version: None,
        },
        StoreMethod::Recent { request_id, .. } => StoreEvent::RecentFailed { request_id, reason },
        StoreMethod::ListRecipes {
            request_id, meta, ..
        }
        | StoreMethod::RecipeHistory {
            request_id, meta, ..
        }
        | StoreMethod::SaveRecipe {
            request_id, meta, ..
        }
        | StoreMethod::ImportRecipe {
            request_id, meta, ..
        }
        | StoreMethod::ExportRecipe {
            request_id, meta, ..
        } => StoreEvent::RecipeFailed {
            request_id,
            meta,
            reason,
        },
        StoreMethod::RecordSuggestion { request_id, .. } => {
            StoreEvent::SuggestionFailed { request_id, reason }
        }
        // Only pre-store rejections (window mismatch) reach here: oversize
        // checks skip fixed-size methods, and flush commits nothing.
        StoreMethod::Flush { request_id, .. } => StoreEvent::FlushFailed { request_id, reason },
    }
}

/// The outcome-unknown reply for a timed-out dispatch. Built by reference
/// before the method moves into the timeout future: the spawned store work
/// is not cancelled and may still commit late, so neither success nor
/// failure would be honest. The reason names the bound and the request so a
/// window can re-derive correlation (reload and compare versions).
fn store_timeout(method: &StoreMethod, timeout: std::time::Duration) -> StoreEvent {
    let reason =
        format!("store request timed out after {timeout:?}; outcome unknown, reload to reconcile");
    match method {
        StoreMethod::Load {
            request_id,
            definition,
            view_id,
            ..
        } => StoreEvent::LoadFailed {
            request_id: request_id.clone(),
            source_id: definition.id,
            view_id: *view_id,
            reason,
        },
        StoreMethod::Save {
            request_id,
            definition,
            view_id,
            sequence,
            ..
        }
        | StoreMethod::CreateDerivedView {
            request_id,
            definition,
            view_id,
            sequence,
            ..
        } => StoreEvent::SaveFailed {
            request_id: request_id.clone(),
            source_id: definition.id,
            view_id: *view_id,
            sequence: *sequence,
            reason,
            current_version: None,
        },
        StoreMethod::Recent { request_id, .. } => StoreEvent::RecentFailed {
            request_id: request_id.clone(),
            reason,
        },
        StoreMethod::ListRecipes {
            request_id, meta, ..
        }
        | StoreMethod::RecipeHistory {
            request_id, meta, ..
        }
        | StoreMethod::SaveRecipe {
            request_id, meta, ..
        }
        | StoreMethod::ImportRecipe {
            request_id, meta, ..
        }
        | StoreMethod::ExportRecipe {
            request_id, meta, ..
        } => StoreEvent::RecipeFailed {
            request_id: request_id.clone(),
            meta: *meta,
            reason,
        },
        StoreMethod::RecordSuggestion { request_id, .. } => StoreEvent::SuggestionFailed {
            request_id: request_id.clone(),
            reason,
        },
        StoreMethod::Flush { request_id, .. } => StoreEvent::FlushFailed {
            request_id: request_id.clone(),
            reason,
        },
    }
}

/// One decoded request: either events to flush, or events plus dropping
/// the connection afterwards.
enum DispatchOutcome {
    Reply(Vec<WorkerEvent>),
    Close(Vec<WorkerEvent>),
}

/// What an admitted start produced, with the identities the reply needs.
/// `Present` reuses the live capture under its own identity; `StdinBound`
/// waits for forwarded chunks.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StartedOutcome {
    Started {
        source_id: SourceId,
        warning: Option<String>,
    },
    Present {
        live_id: SourceId,
    },
    StdinBound {
        source_id: SourceId,
        warning: Option<String>,
    },
}

/// Source metadata exactly as the application builds it for the store
/// (`lvu-app/src/memory.rs::source_metadata`): display command rendering,
/// no project/fields yet, current timestamp, not missing. Ported rather
/// than imported because the worker cannot depend on the application crate;
/// any drift here fails visibly as mismatched catalogue rows, and the two
/// must be reunified if either changes.
fn source_metadata(definition: SourceDefinition) -> lvu_memory::SourceMetadata {
    use std::time::{SystemTime, UNIX_EPOCH};
    let command = match &definition.acquisition {
        lvu_core::Acquisition::File { path, .. } => Some(path.to_string_lossy().into_owned()),
        lvu_core::Acquisition::Command { command } => Some(format!("{command:?}")),
        lvu_core::Acquisition::Http { url, .. } => Some(url.clone()),
        lvu_core::Acquisition::Stdin => Some("standard input".into()),
    };
    lvu_memory::SourceMetadata {
        definition,
        project: None,
        command,
        fields: BTreeMap::new(),
        last_seen: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            .min(i64::MAX as u64) as i64,
        missing: false,
    }
}

/// Canonical-view namespace, byte-identical to `memory::SOURCE_NAMESPACE`
/// (`lvu-app/src/memory.rs`): deterministic per-source canonical view
/// identity. Copied rather than imported for the same cycle reason as
/// above; it is a frozen namespace constant, and any change must update
/// both sites together (flagged for union review).
const CANONICAL_VIEW_NAMESPACE: uuid::Uuid = uuid::Uuid::from_bytes([
    0x6c, 0x76, 0x75, 0x00, 0x73, 0x6f, 0x75, 0x72, 0x63, 0x65, 0x00, 0x6e, 0x73, 0x00, 0x00, 0x01,
]);

fn canonical_view_id(source_id: SourceId) -> lvu_core::ViewId {
    lvu_core::ViewId(uuid::Uuid::new_v5(
        &CANONICAL_VIEW_NAMESPACE,
        source_id.0.as_bytes(),
    ))
}

impl WorkerService {
    /// Run one store closure on the blocking pool against the serialized
    /// store. Every mediated write funnels through here: one synchronous
    /// transaction per call, so concurrent windows serialize in SQLite and
    /// the row-version guard stays the sole conflict authority.
    async fn with_store<T, E, F>(&self, operation: F) -> Result<T, String>
    where
        T: Send + 'static,
        E: std::fmt::Display + Send + 'static,
        F: FnOnce(&mut WorkspaceStore) -> Result<T, E> + Send + 'static,
    {
        let store = Arc::clone(&self.store);
        tokio::task::spawn_blocking(move || {
            let mut store = store.lock().expect("workspace store poisoned");
            operation(&mut store)
        })
        .await
        .map_err(|error| format!("store task failed: {error}"))?
        .map_err(|error| error.to_string())
    }
}

impl WorkerService {
    /// Serve one already-bound socket until shutdown. Accept loop with a
    /// one-second shutdown poll: explicit `request_shutdown` and last-detach
    /// drain both surface through `should_stop` within one poll. Returns
    /// for the caller to stop captures and exit.
    pub async fn serve(self: &Arc<Self>, listener: UnixListener) {
        use tokio::time::timeout;
        loop {
            if self.should_stop().await {
                break;
            }
            match timeout(Duration::from_secs(1), listener.accept()).await {
                Ok(Ok((stream, _))) => {
                    let service = Arc::clone(self);
                    tokio::spawn(async move {
                        service.serve_connection(stream).await;
                    });
                }
                Ok(Err(_)) => {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                Err(_) => continue,
            }
        }
    }

    /// Clean shutdown: stop every capture, flush nothing pending (each
    /// request already committed or failed explicitly), and return the
    /// manager's stop reports for diagnostics. The socket file is unlinked
    /// by the wiring that bound it.
    pub async fn shutdown(&self) -> Vec<(SourceId, Result<String, String>)> {
        self.shutdown_flag
            .store(true, std::sync::atomic::Ordering::Release);
        self.lifecycle_admission.close();
        // A request deadline retires only its waiter; lifecycle work keeps a
        // admission until definitions/session registration settles. Closing
        // the same primitive that grants admission and waiting for its
        // in-flight count therefore drains every detached operation without relying
        // on a definitions snapshot that may not contain it yet. The wait is
        // bounded; on expiry SourceManager::shutdown applies its own bounded
        // start cancellation/stop compensation to every manager-owned source.
        let mut failed_settlements = Vec::new();
        if tokio::time::timeout(
            self.config.request_timeout,
            self.lifecycle_admission.drained(),
        )
        .await
        .is_err()
        {
            // A settlement that outlives the public deadline must not resume
            // later and publish definitions/session state after teardown.
            // Abort only the bounded detached lifecycle tasks owned by this
            // service, then give their RAII permits one final bounded drain.
            failed_settlements = self.lifecycle_admission.abort_settlements();
            if tokio::time::timeout(
                self.config.request_timeout,
                self.lifecycle_admission.drained(),
            )
            .await
            .is_err()
            {
                failed_settlements.extend(self.lifecycle_admission.unsettled_sources());
            }
        }
        self.stdin_bindings.lock().await.clear();
        let reports = self.manager.shutdown().await;
        let mut reports: Vec<_> = reports
            .into_iter()
            .map(|(id, outcome)| (id, shutdown_stop_outcome(outcome)))
            .collect();
        for id in failed_settlements {
            let failure =
                Err("lifecycle settlement did not settle cleanly during shutdown".to_owned());
            match reports.iter_mut().find(|(reported, _)| *reported == id) {
                Some((_, outcome)) => *outcome = failure,
                None => reports.push((id, failure)),
            }
        }
        reports
    }

    #[cfg(test)]
    async fn snapshot_definitions(&self) -> Vec<(SourceId, SourceDefinition)> {
        self.definitions
            .lock()
            .await
            .iter()
            .map(|(id, definition)| (*id, definition.clone()))
            .collect()
    }

    /// Serve one window connection: handshake, then one request at a time
    /// until EOF, protocol violation, or shutdown. Reads use bounded
    /// chunks (`READ_CHUNK_BYTES`); every inbound request passes size and
    /// stdin validation before dispatch; every dispatch runs under
    /// `request_timeout` so one slow store call cannot wedge the window.
    async fn serve_connection(self: Arc<Self>, stream: UnixStream) {
        let (reader, mut writer) = stream.into_split();
        let mut reader = tokio::io::BufReader::new(reader);
        let mut decoder = FrameDecoder::new();
        let mut window: Option<(u32, String)> = None;
        let mut subscribed = false;
        let mut buffer = vec![0u8; crate::READ_CHUNK_BYTES];
        loop {
            let count = match reader.read(&mut buffer).await {
                Ok(0) => break,
                Ok(count) => count,
                Err(_) => break,
            };
            let values = match decoder.push_bytes(&buffer[..count]) {
                Ok(values) => values,
                Err(_) => break,
            };
            let mut replies = Vec::with_capacity(values.len());
            let mut close_after_flush = false;
            for value in values {
                match self
                    .dispatch_request(&mut window, &mut subscribed, value)
                    .await
                {
                    DispatchOutcome::Reply(events) => replies.extend(events),
                    DispatchOutcome::Close(events) => {
                        replies.extend(events);
                        close_after_flush = true;
                        break;
                    }
                }
            }
            let mut failed = false;
            for event in &replies {
                // `encode_frame` re-enforces the wire cap per event: a reply
                // that somehow exceeds it fails this connection loudly
                // instead of splitting framing mid-message.
                let bytes = match serde_json::to_value(event)
                    .map_err(|error| FrameError::Malformed(error.to_string()))
                    .and_then(|value| encode_frame(&value))
                {
                    Ok(bytes) => bytes,
                    Err(_) => {
                        failed = true;
                        break;
                    }
                };
                if writer.write_all(&bytes).await.is_err() {
                    failed = true;
                    break;
                }
            }
            if failed || close_after_flush {
                break;
            }
        }
        if let Some((pid, _)) = window.take() {
            self.viewers
                .lock()
                .await
                .goodbye(std::time::Instant::now(), pid);
        }
    }

    /// Dispatch one decoded request. Unparseable values and pre-handshake
    /// non-hellos drop the connection without touching worker state. Store
    /// methods parse after control requests; the two tables have disjoint
    /// method names so sequential parsing is unambiguous.
    async fn dispatch_request(
        self: &Arc<Self>,
        window: &mut Option<(u32, String)>,
        subscribed: &mut bool,
        value: serde_json::Value,
    ) -> DispatchOutcome {
        use DispatchOutcome::{Close, Reply};
        if let Some((_, attached)) = window
            && let Ok(method) = serde_json::from_value::<StoreMethod>(value.clone())
        {
            let event = self.dispatch_store(attached, method).await;
            return Reply(vec![WorkerEvent::Store(event)]);
        }
        let request: WorkerRequest = match serde_json::from_value(value) {
            Ok(request) => request,
            Err(_) => return Close(Vec::new()),
        };
        if let Err(error) = validate_inbound(&request) {
            return Close(vec![WorkerEvent::Refused {
                request_id: request_id_of(&request),
                reason: format!("protocol violation: {error}"),
            }]);
        }
        if window.is_none() {
            return match request {
                WorkerRequest::Hello {
                    request_id,
                    window_pid,
                    window_id,
                    protocol,
                } => {
                    self.dispatch_hello(request_id, window_pid, window_id, protocol, window)
                        .await
                }
                _ => Close(Vec::new()),
            };
        }
        let timeout = self.config.request_timeout;
        match request {
            WorkerRequest::Hello { .. } => Close(vec![WorkerEvent::Refused {
                request_id: request_id_of(&request),
                reason: "already attached; open a new connection instead".into(),
            }]),
            WorkerRequest::Goodbye { .. } => Close(Vec::new()),
            WorkerRequest::RequestStart {
                request_id,
                definition,
            } => {
                let definition: SourceDefinition = match serde_json::from_value(definition) {
                    Ok(definition) => definition,
                    Err(error) => {
                        return Reply(vec![WorkerEvent::Refused {
                            request_id,
                            reason: format!("invalid source definition: {error}"),
                        }]);
                    }
                };
                let service = Arc::clone(self);
                match tokio::time::timeout(timeout, service.request_start(definition)).await {
                    Err(_) => Reply(vec![WorkerEvent::Refused {
                        request_id,
                        reason: "start timed out".into(),
                    }]),
                    Ok(Ok(StartedOutcome::Started { source_id, warning })) => {
                        let mut events = vec![WorkerEvent::Started {
                            request_id,
                            source_id: source_id.0.to_string(),
                            journal_path: self.journal_path_for(source_id).display().to_string(),
                        }];
                        if let Some(warning) = warning {
                            events.push(WorkerEvent::ShutdownNotice { reason: warning });
                        }
                        self.maybe_push_status(subscribed, &mut events).await;
                        Reply(events)
                    }
                    Ok(Ok(StartedOutcome::Present { live_id })) => {
                        Reply(vec![WorkerEvent::Started {
                            request_id,
                            source_id: live_id.0.to_string(),
                            journal_path: self.journal_path_for(live_id).display().to_string(),
                        }])
                    }
                    Ok(Ok(StartedOutcome::StdinBound { source_id, warning })) => {
                        let mut events = vec![WorkerEvent::StdinOpen {
                            request_id,
                            source_id: source_id.0.to_string(),
                            chunk_bytes: crate::MAX_STDIN_CHUNK_BYTES as u32,
                        }];
                        if let Some(warning) = warning {
                            events.push(WorkerEvent::ShutdownNotice { reason: warning });
                        }
                        Reply(events)
                    }
                    Ok(Err(reason)) => Reply(vec![WorkerEvent::Refused { request_id, reason }]),
                }
            }
            WorkerRequest::RequestStop {
                request_id,
                source_id,
            } => {
                let id = match uuid::Uuid::parse_str(&source_id) {
                    Ok(id) => SourceId(id),
                    Err(_) => {
                        return Reply(vec![WorkerEvent::Refused {
                            request_id,
                            reason: "invalid source id".into(),
                        }]);
                    }
                };
                let service = Arc::clone(self);
                match tokio::time::timeout(timeout, service.request_stop(id)).await {
                    Err(_) => Reply(vec![WorkerEvent::Refused {
                        request_id,
                        reason: "stop timed out".into(),
                    }]),
                    Ok(Ok(())) => {
                        let mut events = vec![WorkerEvent::Stopped {
                            request_id,
                            source_id: id.0.to_string(),
                        }];
                        self.maybe_push_status(subscribed, &mut events).await;
                        Reply(events)
                    }
                    Ok(Err(reason)) => Reply(vec![WorkerEvent::Refused { request_id, reason }]),
                }
            }
            WorkerRequest::RequestRestart {
                request_id,
                source_id,
            } => {
                let id = match uuid::Uuid::parse_str(&source_id) {
                    Ok(id) => SourceId(id),
                    Err(_) => {
                        return Reply(vec![WorkerEvent::Refused {
                            request_id,
                            reason: "invalid source id".into(),
                        }]);
                    }
                };
                let service = Arc::clone(self);
                match tokio::time::timeout(timeout, service.request_restart(id)).await {
                    Err(_) => Reply(vec![WorkerEvent::Refused {
                        request_id,
                        reason: "restart timed out".into(),
                    }]),
                    Ok(Ok(StartedOutcome::Started { source_id, warning })) => {
                        let mut events = vec![WorkerEvent::Started {
                            request_id,
                            source_id: source_id.0.to_string(),
                            journal_path: self.journal_path_for(source_id).display().to_string(),
                        }];
                        if let Some(warning) = warning {
                            events.push(WorkerEvent::ShutdownNotice { reason: warning });
                        }
                        self.maybe_push_status(subscribed, &mut events).await;
                        Reply(events)
                    }
                    Ok(Ok(StartedOutcome::Present { live_id })) => {
                        Reply(vec![WorkerEvent::Started {
                            request_id,
                            source_id: live_id.0.to_string(),
                            journal_path: self.journal_path_for(live_id).display().to_string(),
                        }])
                    }
                    Ok(Ok(StartedOutcome::StdinBound { .. })) => {
                        Reply(vec![WorkerEvent::Refused {
                            request_id,
                            reason: "restart produced a stdin binding; attach explicitly".into(),
                        }])
                    }
                    Ok(Err(reason)) => Reply(vec![WorkerEvent::Refused { request_id, reason }]),
                }
            }
            WorkerRequest::StatusSubscribe { .. } => {
                *subscribed = true;
                Reply(vec![WorkerEvent::SourceStatus {
                    sources: self.presence_snapshot().await,
                }])
            }
            WorkerRequest::RequestProgress {
                request_id,
                source_id,
            } => {
                // Pure in-memory read (manager lookup + watch borrow), so no
                // timeout wrapper: there is nothing here that can wedge.
                match self.poll_source_progress(&source_id) {
                    Ok(progress) => Reply(vec![WorkerEvent::SourceProgress {
                        request_id,
                        worker_session: self.worker_session.clone(),
                        progress,
                    }]),
                    Err(reason) => Reply(vec![WorkerEvent::Refused { request_id, reason }]),
                }
            }
            WorkerRequest::StdinChunk {
                request_id,
                source_id,
                seq,
                base64,
            } => {
                let id = match uuid::Uuid::parse_str(&source_id) {
                    Ok(id) => SourceId(id),
                    Err(_) => {
                        return Reply(vec![WorkerEvent::Refused {
                            request_id,
                            reason: "invalid source id".into(),
                        }]);
                    }
                };
                let raw = match crate::protocol::decoded_base64_len(&base64) {
                    Ok(len) if len <= crate::MAX_STDIN_CHUNK_BYTES => {
                        match base64_decode_bounded(&base64, len) {
                            Some(raw) => raw,
                            None => {
                                return Close(vec![WorkerEvent::Refused {
                                    request_id,
                                    reason: "stdin chunk failed to decode".into(),
                                }]);
                            }
                        }
                    }
                    _ => {
                        return Close(vec![WorkerEvent::Refused {
                            request_id,
                            reason: "stdin chunk violates size bounds".into(),
                        }]);
                    }
                };
                match self.note_stdin_chunk(id, seq, &raw).await {
                    Ok(ack) => Reply(vec![WorkerEvent::StdinCredit {
                        source_id: id.0.to_string(),
                        ack_seq: ack,
                        window: crate::STDIN_CREDIT_WINDOW,
                    }]),
                    Err(StdinNoteError::UnknownStream) => Reply(vec![WorkerEvent::Refused {
                        request_id,
                        reason: "stdin stream is not bound (stale or replaced worker)".into(),
                    }]),
                    Err(StdinNoteError::Desync(reason)) => {
                        Close(vec![WorkerEvent::Refused { request_id, reason }])
                    }
                }
            }
            WorkerRequest::StdinClose {
                request_id,
                source_id,
            } => {
                let id = match uuid::Uuid::parse_str(&source_id) {
                    Ok(id) => SourceId(id),
                    Err(_) => {
                        return Reply(vec![WorkerEvent::Refused {
                            request_id,
                            reason: "invalid source id".into(),
                        }]);
                    }
                };
                self.close_stdin(id).await;
                Reply(vec![WorkerEvent::Stopped {
                    request_id,
                    source_id: id.0.to_string(),
                }])
            }
        }
    }

    /// Handshake: version check, viewer-cap admission, presence snapshot.
    /// Anything else closes the connection without touching worker state.
    async fn dispatch_hello(
        &self,
        request_id: String,
        window_pid: u32,
        window_id: String,
        protocol: u32,
        window: &mut Option<(u32, String)>,
    ) -> DispatchOutcome {
        use crate::ViewerAdmission;
        if protocol != PROTOCOL_VERSION {
            return DispatchOutcome::Close(vec![WorkerEvent::Refused {
                request_id,
                reason: format!(
                    "protocol version {protocol} is not supported here (worker speaks {PROTOCOL_VERSION})"
                ),
            }]);
        }
        match self.viewers.lock().await.hello(window_pid) {
            ViewerAdmission::Admitted => {
                *window = Some((window_pid, window_id));
                DispatchOutcome::Reply(vec![WorkerEvent::Welcome {
                    request_id,
                    worker_pid: std::process::id(),
                    protocol: PROTOCOL_VERSION,
                    worker_session: self.worker_session.clone(),
                    sources: self.presence_snapshot().await,
                }])
            }
            ViewerAdmission::RefusedFull => DispatchOutcome::Close(vec![WorkerEvent::Refused {
                request_id,
                reason: format!("worker is full ({} windows)", crate::MAX_VIEWERS),
            }]),
        }
    }

    /// Append a presence snapshot for status subscribers after acquisition
    /// outcomes. Counts are sampled at event time; journals stay authoritative.
    async fn maybe_push_status(&self, subscribed: &bool, events: &mut Vec<WorkerEvent>) {
        if *subscribed {
            events.push(WorkerEvent::SourceStatus {
                sources: self.presence_snapshot().await,
            });
        }
    }
}

fn shutdown_stop_outcome(outcome: Result<StopReport, RuntimeError>) -> Result<String, String> {
    match outcome {
        Ok(report) if report.complete => Ok("stopped".to_owned()),
        Ok(_) => Err("stop incomplete".to_owned()),
        Err(error) => Err(format!("stop: {error}")),
    }
}

/// Correlation id of any request, for refusal paths that no longer own it.
fn request_id_of(request: &WorkerRequest) -> String {
    match request {
        WorkerRequest::Hello { request_id, .. }
        | WorkerRequest::Goodbye { request_id }
        | WorkerRequest::RequestStart { request_id, .. }
        | WorkerRequest::RequestStop { request_id, .. }
        | WorkerRequest::RequestRestart { request_id, .. }
        | WorkerRequest::StatusSubscribe { request_id }
        | WorkerRequest::StdinChunk { request_id, .. }
        | WorkerRequest::RequestProgress { request_id, .. }
        | WorkerRequest::StdinClose { request_id, .. } => request_id.clone(),
    }
}

/// Why a stdin chunk was not accepted. Unknown streams (stale after a
/// replacement) get an actionable refusal; desync drops the connection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StdinNoteError {
    UnknownStream,
    Desync(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FrameDecoder, encode_frame};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::UnixStream,
    };

    struct AdmitAll;

    impl AdmissionHook for AdmitAll {
        fn admit(&self, _definition: &SourceDefinition) -> AdmissionVerdict {
            AdmissionVerdict::Admit
        }
    }

    struct ScriptedAdmission {
        verdict: AdmissionVerdict,
    }

    impl AdmissionHook for ScriptedAdmission {
        fn admit(&self, _definition: &SourceDefinition) -> AdmissionVerdict {
            self.verdict.clone()
        }
    }

    struct CountingAdmission(std::sync::atomic::AtomicUsize);

    impl AdmissionHook for CountingAdmission {
        fn admit(&self, _definition: &SourceDefinition) -> AdmissionVerdict {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            AdmissionVerdict::Admit
        }
    }

    fn test_config(root: &Path) -> WorkerConfig {
        WorkerConfig {
            capture_root: root.join("captures"),
            workspace_root: root.join("workspace"),
            socket_path: root.join("shared-worker/control.sock"),
            viewer_grace: Duration::from_millis(100),
            request_timeout: Duration::from_secs(10),
        }
    }

    fn file_definition(id: u128, path: &Path) -> SourceDefinition {
        SourceDefinition {
            schema_version: 1,
            id: SourceId(uuid::Uuid::from_u128(id)),
            name: format!("log-{id}"),
            acquisition: lvu_core::Acquisition::File {
                path: path.to_path_buf(),
                follow: true,
            },
            identity_hints: Default::default(),
            retention: None,
        }
    }

    fn test_view(source: SourceId, view: u128, version: u64) -> lvu_memory::WorkingView {
        lvu_memory::WorkingView {
            id: lvu_core::ViewId(uuid::Uuid::from_u128(view)),
            source_id: source,
            name: "All events".into(),
            role: lvu_memory::ViewRole::Derived,
            applied_revision_id: None,
            applied_search: String::new(),
            search_draft: None,
            applied_advanced_filter: None,
            advanced_filter_draft: None,
            navigation: lvu_memory::NavigationState {
                selected: None,
                anchor: None,
                follow: true,
            },
            presentation: lvu_memory::PresentationState::default(),
            version,
        }
    }

    /// Drive one connection: send values, collect reply events with a hard
    /// deadline. Replies arrive in request order on one connection.
    async fn rpc(
        client: &mut UnixStream,
        decoder: &mut FrameDecoder,
        value: serde_json::Value,
    ) -> Vec<WorkerEvent> {
        let mut bytes = encode_frame(&value).unwrap();
        let _ = bytes.pop();
        client.write_all(&bytes).await.unwrap();
        client.write_all(b"\n").await.unwrap();
        read_events(client, decoder, 1, Duration::from_secs(5)).await
    }

    async fn read_events(
        client: &mut UnixStream,
        decoder: &mut FrameDecoder,
        want: usize,
        deadline: Duration,
    ) -> Vec<WorkerEvent> {
        let start = std::time::Instant::now();
        let mut events = Vec::new();
        let mut buffer = vec![0u8; crate::READ_CHUNK_BYTES];
        while events.len() < want {
            if std::time::Instant::now() - start > deadline {
                panic!("timed out waiting for {} events", want);
            }
            let count = tokio::time::timeout(Duration::from_secs(2), client.read(&mut buffer))
                .await
                .expect("readable socket")
                .expect("socket readable");
            if count == 0 {
                panic!("socket closed while waiting for events");
            }
            for value in decoder.push_bytes(&buffer[..count]).unwrap() {
                events.push(serde_json::from_value(value).unwrap());
            }
        }
        events
    }

    fn hello_value(request_id: &str, pid: u32) -> serde_json::Value {
        serde_json::to_value(WorkerRequest::Hello {
            request_id: request_id.into(),
            window_pid: pid,
            window_id: format!("window-{pid}"),
            protocol: PROTOCOL_VERSION,
        })
        .unwrap()
    }

    /// Attach a client through a real socket pair driven by the service's
    /// own connection handler: same framing, same handshake, same dispatch
    /// as the Unix-listener path, without the bind.
    async fn attach(service: &Arc<WorkerService>, pid: u32) -> (UnixStream, FrameDecoder) {
        let (client, server) = UnixStream::pair().unwrap();
        let service = Arc::clone(service);
        tokio::spawn(async move {
            service.serve_connection(server).await;
        });
        let mut client = client;
        let mut decoder = FrameDecoder::new();
        let events = rpc(&mut client, &mut decoder, hello_value("hello", pid)).await;
        assert!(
            matches!(events.as_slice(), [WorkerEvent::Welcome { .. }]),
            "attach must be welcomed: {events:?}"
        );
        (client, decoder)
    }

    #[tokio::test]
    async fn two_windows_share_one_capture_over_sockets() {
        let root = tempfile::tempdir().unwrap();
        let log = root.path().join("app.log");
        std::fs::write(&log, "one\n").unwrap();
        let config = test_config(root.path());
        let (service, warning) = WorkerService::open(config, Arc::new(AdmitAll)).unwrap();
        assert!(warning.is_none());
        let (mut first, mut first_decoder) = attach(&service, 101).await;
        let definition = file_definition(11, &log);
        let start = serde_json::to_value(WorkerRequest::RequestStart {
            request_id: "start-1".into(),
            definition: serde_json::to_value(&definition).unwrap(),
        })
        .unwrap();
        let events = rpc(&mut first, &mut first_decoder, start).await;
        let (source_id, journal_path) = match events.as_slice() {
            [
                WorkerEvent::Started {
                    source_id,
                    journal_path,
                    ..
                },
            ] => (source_id.clone(), journal_path.clone()),
            other => panic!("expected Started, got {other:?}"),
        };
        assert_eq!(source_id, definition.id.0.to_string());
        std::fs::write(&log, "one\ntwo\n").unwrap();
        // A second window attaches and reads the same journal through its
        // own tail: one capture, two readers, no second acquisition.
        let (mut second, mut second_decoder) = attach(&service, 102).await;
        let tail = crate::FileJournalTail::new(definition.id, Path::new(&journal_path));
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let page = loop {
            if let Ok(page) = tail.read_page(0, 128, 1024 * 1024)
                && page.records.len() >= 2
            {
                break page;
            }
            if std::time::Instant::now() >= deadline {
                panic!("shared rows never arrived");
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        };
        assert_eq!(page.records.len(), 2);
        let welcome = rpc(
            &mut second,
            &mut second_decoder,
            serde_json::to_value(WorkerRequest::StatusSubscribe {
                request_id: "status".into(),
            })
            .unwrap(),
        )
        .await;
        assert!(
            welcome.iter().any(|event| matches!(
                event,
                WorkerEvent::SourceStatus { sources } if sources.iter().any(|source| source.id == source_id)
            )),
            "second window sees the shared source: {welcome:?}"
        );
        service.request_shutdown();
        service.shutdown().await;
    }

    #[tokio::test]
    async fn stale_cross_window_save_loses_with_current_version() {
        let root = tempfile::tempdir().unwrap();
        let config = test_config(root.path());
        let (service, _) = WorkerService::open(config, Arc::new(AdmitAll)).unwrap();
        let source = SourceId(uuid::Uuid::from_u128(21));
        let view = lvu_core::ViewId(uuid::Uuid::from_u128(22));
        let definition = file_definition(21, &root.path().join("v.log"));
        // Window A creates the view (no base version): committed as 0.
        let saved = service
            .mediated_save(
                "a-1".into(),
                1,
                definition.clone(),
                view,
                test_view(source, 22, 0),
                None,
            )
            .await;
        let version = match saved {
            StoreEvent::Saved { version, .. } => version,
            other => panic!("expected Saved, got {other:?}"),
        };
        // Window B, holding the same base version the row still has, saves
        // first and wins: versions serialize in the store transaction.
        let saved_b = service
            .mediated_save(
                "b-1".into(),
                1,
                definition.clone(),
                view,
                test_view(source, 22, 0),
                Some(version),
            )
            .await;
        let version_b = match saved_b {
            StoreEvent::Saved { version, .. } => version,
            other => panic!("expected Saved, got {other:?}"),
        };
        assert_ne!(version, version_b);
        // Window A retries with its now-stale base: Conflict carries the
        // committed version, and A's draft is never written over B's.
        match service
            .mediated_save(
                "a-2".into(),
                2,
                definition.clone(),
                view,
                test_view(source, 22, 0),
                Some(version),
            )
            .await
        {
            StoreEvent::SaveFailed {
                reason,
                current_version,
                ..
            } => {
                assert_eq!(current_version, Some(version_b), "{reason}");
            }
            other => panic!("expected conflict, got {other:?}"),
        }
        // A replays explicitly against the fresh version and wins.
        match service
            .mediated_save(
                "a-3".into(),
                3,
                definition,
                view,
                test_view(source, 22, 0),
                Some(version_b),
            )
            .await
        {
            StoreEvent::Saved { version, .. } => assert_ne!(version, version_b),
            other => panic!("expected Saved after rebase, got {other:?}"),
        }
        service.request_shutdown();
        service.shutdown().await;
    }

    #[tokio::test]
    async fn replacement_resumes_files_not_commands_or_stdin() {
        let root = tempfile::tempdir().unwrap();
        let log = root.path().join("app.log");
        std::fs::write(&log, "one\n").unwrap();
        let file = file_definition(31, &log);
        let mut command = file_definition(32, &root.path().join("cmd.log"));
        command.name = "sleeper".into();
        command.acquisition = lvu_core::Acquisition::Command {
            command: lvu_core::CommandDefinition {
                program: lvu_core::CommandProgram::Exec {
                    executable: "/bin/sleep".into(),
                    args: vec!["60".into()],
                },
                cwd: Some(root.path().to_path_buf()),
                environment: Default::default(),
                restart: lvu_core::RestartPolicy::Never,
            },
        };
        let mut stdin = file_definition(33, &root.path().join("unused.log"));
        stdin.name = "pipe".into();
        stdin.acquisition = lvu_core::Acquisition::Stdin;
        // Plant the remembered session first: the service loads it at
        // open, exactly like startup restore.
        let config = test_config(root.path());
        store_session_set(
            &root.path().join("workspace"),
            &[file.clone(), command.clone(), stdin.clone()],
        )
        .unwrap();
        let (first, _) = WorkerService::open(config.clone(), Arc::new(AdmitAll)).unwrap();
        let outcomes = first.resume_session().await;
        assert!(
            outcomes
                .iter()
                .any(|(id, outcome)| *id == file.id && outcome.is_ok()),
            "file must resume: {outcomes:?}"
        );
        assert!(
            outcomes.iter().any(|(id, outcome)| *id == command.id
                && outcome.as_ref().unwrap_err().contains("explicit start")),
            "command must wait for explicit start: {outcomes:?}"
        );
        assert!(
            outcomes.iter().any(|(id, outcome)| *id == stdin.id
                && outcome.as_ref().unwrap_err().contains("explicit start")),
            "stdin without a live pipe must wait: {outcomes:?}"
        );
        std::fs::write(&log, "one\ntwo\n").unwrap();
        // Drain both rows under the first service so the handoff point is
        // deterministic: whatever follows must continue, never repeat.
        let tail = crate::FileJournalTail::new(file.id, &first.journal_path_for(file.id));
        wait_for_records(&tail, 2).await;
        // Clean stop releases locks and cursors durably; the in-process
        // registry clears on drop. Kernel release on real kill is proven by
        // the election contention tests with a separate holder process.
        first.shutdown().await;
        first.manager.shutdown().await;
        drop(first);
        let (second, _) = WorkerService::open(config, Arc::new(AdmitAll)).unwrap();
        let outcomes = second.resume_session().await;
        assert!(
            outcomes
                .iter()
                .any(|(id, outcome)| *id == file.id && outcome.is_ok()),
            "replacement resumes the file: {outcomes:?}"
        );
        assert!(
            second.manager.source(command.id).is_none(),
            "replacement must not relaunch the remembered command"
        );
        assert!(
            second.manager.source(stdin.id).is_none(),
            "replacement must not invent a stdin attachment"
        );
        // Sequences continue across the replacement with no repeats. The
        // jump below is pre-existing ingest semantics, not a replacement
        // defect: file acquisition reserves sequence blocks ahead and
        // persists the reservation watermark, so any restart (in-process or
        // replacement alike) continues from the watermark. The product
        // invariant is no-repeat, not contiguity.
        std::fs::write(&log, "one\ntwo\nthree\n").unwrap();
        let tail = crate::FileJournalTail::new(file.id, &second.journal_path_for(file.id));
        let page = wait_for_records(&tail, 3).await;
        let sequences: Vec<u64> = page
            .records
            .iter()
            .map(|record| record.record_id.sequence)
            .collect();
        assert_eq!(&sequences[..2], &[0, 1]);
        assert!(sequences.windows(2).all(|pair| pair[0] < pair[1]));
        let mut ordered = sequences.clone();
        ordered.sort_unstable();
        ordered.dedup();
        assert_eq!(ordered.len(), sequences.len(), "no repeated sequences");
        second.request_shutdown();
        second.shutdown().await;
    }

    async fn wait_for_records(
        tail: &crate::FileJournalTail,
        count: usize,
    ) -> lvu_core::journal::JournalPage {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            if let Ok(page) = tail.read_page(0, 256, 4 * 1024 * 1024)
                && page.records.len() >= count
            {
                return page;
            }
            if std::time::Instant::now() >= deadline {
                panic!("waited 10s for {count} records");
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    #[tokio::test]
    async fn stdin_forwarding_is_bounded_and_detaches_cleanly() {
        let root = tempfile::tempdir().unwrap();
        let config = test_config(root.path());
        let (service, _) = WorkerService::open(config, Arc::new(AdmitAll)).unwrap();
        let mut definition = file_definition(41, &root.path().join("unused.log"));
        definition.name = "pipe".into();
        definition.acquisition = lvu_core::Acquisition::Stdin;
        let outcome = service.request_start(definition.clone()).await.unwrap();
        let source_id = match outcome {
            StartedOutcome::StdinBound { source_id, .. } => source_id,
            other => panic!("expected stdin binding, got {other:?}"),
        };
        assert_eq!(source_id, definition.id);
        for seq in 0..3u64 {
            let ack = service
                .note_stdin_chunk(source_id, seq, format!("line-{seq}\n").as_bytes())
                .await
                .unwrap();
            assert_eq!(ack, seq);
        }
        assert!(matches!(
            service.note_stdin_chunk(source_id, 9, b"x").await,
            Err(crate::worker::StdinNoteError::Desync(_))
        ));
        let oversize = vec![b'x'; crate::MAX_STDIN_CHUNK_BYTES + 1];
        assert!(matches!(
            service.note_stdin_chunk(source_id, 3, &oversize).await,
            Err(crate::worker::StdinNoteError::Desync(_))
        ));
        service.close_stdin(source_id).await;
        assert!(matches!(
            service.note_stdin_chunk(source_id, 3, b"x").await,
            Err(crate::worker::StdinNoteError::UnknownStream)
        ));
        // Arrived bytes survive as an incomplete capture.
        let tail = crate::FileJournalTail::new(source_id, &service.journal_path_for(source_id));
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let page = loop {
            if let Ok(page) = tail.read_page(0, 128, 1024 * 1024)
                && page.records.len() >= 3
            {
                break page;
            }
            if std::time::Instant::now() >= deadline {
                panic!("forwarded stdin rows never arrived");
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        };
        assert_eq!(page.records.len(), 3);
        service.request_shutdown();
        service.shutdown().await;
    }

    #[tokio::test]
    async fn last_detach_stops_worker_and_handshake_guards_hold() {
        let root = tempfile::tempdir().unwrap();
        let config = test_config(root.path());
        let (service, _) = WorkerService::open(config, Arc::new(AdmitAll)).unwrap();
        assert!(!service.should_stop().await);
        // Wrong protocol version is refused without touching state.
        let (client, server) = UnixStream::pair().unwrap();
        let service_clone = Arc::clone(&service);
        let task = tokio::spawn(async move {
            service_clone.serve_connection(server).await;
        });
        let mut client = client;
        let mut decoder = FrameDecoder::new();
        let bad = serde_json::to_value(WorkerRequest::Hello {
            request_id: "bad".into(),
            window_pid: 555,
            window_id: "w".into(),
            protocol: PROTOCOL_VERSION + 99,
        })
        .unwrap();
        let events = rpc(&mut client, &mut decoder, bad).await;
        assert!(matches!(events.as_slice(), [WorkerEvent::Refused { .. }]));
        task.abort();
        // One hello starts the drain clock on goodbye; expiry stops.
        let (client, decoder) = attach(&service, 201).await;
        drop(client);
        assert!(!service.should_stop().await);
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(service.should_stop().await);
        service.request_shutdown();
        service.shutdown().await;
        let _ = decoder;
    }

    #[tokio::test]
    async fn explicit_shutdown_stops_despite_viewers() {
        let root = tempfile::tempdir().unwrap();
        let config = test_config(root.path());
        let (service, _) = WorkerService::open(config, Arc::new(AdmitAll)).unwrap();
        let (_client, _decoder) = attach(&service, 301).await;
        assert!(!service.should_stop().await);
        service.request_shutdown();
        assert!(service.should_stop().await);
        service.shutdown().await;
    }

    /// Store dispatch over real socket frames: two windows saving one view
    /// through the wire protocol observe saved/conflict/version echoes end
    /// to end, and an oversize command is refused at the boundary.
    #[tokio::test]
    async fn store_dispatch_routes_typed_methods_over_frames() {
        use crate::protocol::{StoreEvent, StoreMethod};

        let root = tempfile::tempdir().unwrap();
        let config = test_config(root.path());
        let (service, _) = WorkerService::open(config, Arc::new(AdmitAll)).unwrap();
        let source = SourceId(uuid::Uuid::from_u128(61));
        let view = lvu_core::ViewId(uuid::Uuid::from_u128(62));
        let definition = file_definition(61, &root.path().join("v.log"));
        let state = test_view(source, 62, 0);

        async fn roundtrip(
            client: &mut UnixStream,
            decoder: &mut FrameDecoder,
            method: StoreMethod,
        ) -> StoreEvent {
            let request_id = match &method {
                StoreMethod::Save { request_id, .. }
                | StoreMethod::Load { request_id, .. }
                | StoreMethod::CreateDerivedView { request_id, .. }
                | StoreMethod::Recent { request_id, .. }
                | StoreMethod::ListRecipes { request_id, .. }
                | StoreMethod::RecipeHistory { request_id, .. }
                | StoreMethod::SaveRecipe { request_id, .. }
                | StoreMethod::ImportRecipe { request_id, .. }
                | StoreMethod::ExportRecipe { request_id, .. }
                | StoreMethod::RecordSuggestion { request_id, .. }
                | StoreMethod::Flush { request_id, .. } => request_id.clone(),
            };
            let wire = encode_frame(&serde_json::to_value(&method).unwrap()).unwrap();
            client.write_all(&wire).await.unwrap();
            let events = read_events(client, decoder, 1, Duration::from_secs(5)).await;
            let WorkerEvent::Store(event) = events.into_iter().next().expect("one reply") else {
                panic!("expected a store reply");
            };
            let echoed = match &event {
                StoreEvent::Saved { request_id, .. }
                | StoreEvent::SaveFailed { request_id, .. }
                | StoreEvent::LoadFailed { request_id, .. } => request_id.clone(),
                _ => String::new(),
            };
            assert_eq!(echoed, request_id);
            event
        }

        let (mut first, mut first_decoder) = attach(&service, 401).await;
        let (mut second, mut second_decoder) = attach(&service, 402).await;
        // Window A creates the view over the wire: version 0.
        let created = roundtrip(
            &mut first,
            &mut first_decoder,
            StoreMethod::Save {
                request_id: "a-create".into(),
                window_id: "window-401".into(),
                sequence: 1,
                definition: definition.clone(),
                view_id: view,
                state: state.clone(),
                expected_version: None,
            },
        )
        .await;
        assert!(
            matches!(created, StoreEvent::Saved { version: 0, .. }),
            "{created:?}"
        );
        // Window B saves against version 0 and wins version 1.
        let won = roundtrip(
            &mut second,
            &mut second_decoder,
            StoreMethod::Save {
                request_id: "b-save".into(),
                window_id: "window-402".into(),
                sequence: 1,
                definition: definition.clone(),
                view_id: view,
                state: state.clone(),
                expected_version: Some(0),
            },
        )
        .await;
        assert!(
            matches!(won, StoreEvent::Saved { version: 1, .. }),
            "{won:?}"
        );
        // Window A's in-flight edit is now stale: conflict with the
        // committed version echoed, draft unwritten.
        let lost = roundtrip(
            &mut first,
            &mut first_decoder,
            StoreMethod::Save {
                request_id: "a-stale".into(),
                window_id: "window-401".into(),
                sequence: 2,
                definition: definition.clone(),
                view_id: view,
                state,
                expected_version: Some(0),
            },
        )
        .await;
        match lost {
            StoreEvent::SaveFailed {
                reason,
                current_version: Some(1),
                ..
            } => assert!(
                reason.contains("conflict") || reason.contains("Conflict"),
                "{reason}"
            ),
            other => panic!("expected versioned conflict, got {other:?}"),
        }
        service.request_shutdown();
        service.shutdown().await;
    }

    #[tokio::test]
    async fn timed_out_store_dispatch_reports_unknown_not_failure() {
        use crate::protocol::StoreMethod;

        let root = tempfile::tempdir().unwrap();
        let mut config = test_config(root.path());
        // A nanosecond bound no real store call can meet: the save awaits
        // on store I/O, which yields, and the deadline is already past at
        // the next poll — so this must take the timeout branch every run.
        config.request_timeout = Duration::from_nanos(1);
        let (service, _) = WorkerService::open(config, Arc::new(AdmitAll)).unwrap();
        let source = SourceId(uuid::Uuid::from_u128(81));
        let view = lvu_core::ViewId(uuid::Uuid::from_u128(82));
        let event = service
            .dispatch_store(
                "w",
                StoreMethod::Save {
                    request_id: "slow".into(),
                    window_id: "w".into(),
                    sequence: 1,
                    definition: file_definition(81, &root.path().join("v.log")),
                    view_id: view,
                    state: test_view(source, 82, 0),
                    expected_version: None,
                },
            )
            .await;
        match event {
            StoreEvent::SaveFailed {
                request_id, reason, ..
            } => {
                assert_eq!(request_id, "slow");
                assert!(
                    reason.contains("unknown"),
                    "timeout must not read as failure: {reason}"
                );
            }
            other => panic!("expected outcome-unknown timeout, got {other:?}"),
        }
        service.request_shutdown();
        service.shutdown().await;
    }

    #[tokio::test]
    async fn store_dispatch_refuses_foreign_window_identity() {
        use crate::protocol::StoreMethod;

        let root = tempfile::tempdir().unwrap();
        let config = test_config(root.path());
        let (service, _) = WorkerService::open(config, Arc::new(AdmitAll)).unwrap();
        let source = SourceId(uuid::Uuid::from_u128(83));
        let view = lvu_core::ViewId(uuid::Uuid::from_u128(84));
        let definition = file_definition(83, &root.path().join("v.log"));
        let refused = service
            .dispatch_store(
                "window-1",
                StoreMethod::Save {
                    request_id: "foreign".into(),
                    window_id: "window-2".into(),
                    sequence: 1,
                    definition,
                    view_id: view,
                    state: test_view(source, 84, 0),
                    expected_version: None,
                },
            )
            .await;
        match refused {
            StoreEvent::SaveFailed { reason, .. } => {
                assert!(reason.contains("mismatch"), "{reason}")
            }
            other => panic!("expected window-identity refusal, got {other:?}"),
        }
        // The refusal precedes any store work: the view was never written.
        match service
            .dispatch_store(
                "window-1",
                StoreMethod::Recent {
                    request_id: "after".into(),
                    window_id: "window-1".into(),
                },
            )
            .await
        {
            StoreEvent::Recent { sources, .. } => assert!(sources.is_empty()),
            other => panic!("expected empty recent list, got {other:?}"),
        }
        service.request_shutdown();
        service.shutdown().await;
    }

    #[tokio::test]
    async fn oversize_load_refusal_echoes_actual_view_id() {
        use crate::protocol::StoreMethod;

        let root = tempfile::tempdir().unwrap();
        let config = test_config(root.path());
        let (service, _) = WorkerService::open(config, Arc::new(AdmitAll)).unwrap();
        // A Load carries no draft, so bloat the definition path instead:
        // the serialized method still trips the transport bound, and the
        // refusal happens before any store work touches the path.
        let view = lvu_core::ViewId(uuid::Uuid::from_u128(86));
        let oversized = StoreMethod::Load {
            request_id: "big-load".into(),
            window_id: "w".into(),
            definition: file_definition(85, &root.path().join("x".repeat(300 * 1024))),
            view_id: view,
        };
        match service.dispatch_store("w", oversized).await {
            StoreEvent::LoadFailed {
                request_id,
                view_id: echoed,
                reason,
                ..
            } => {
                assert_eq!(request_id, "big-load");
                assert_eq!(echoed, view);
                assert!(
                    reason.contains("limit") || reason.contains("bytes"),
                    "{reason}"
                );
            }
            other => panic!("expected oversize load refusal, got {other:?}"),
        }
        service.request_shutdown();
        service.shutdown().await;
    }

    #[tokio::test]
    async fn save_without_base_conflicts_against_existing_row() {
        use crate::protocol::StoreMethod;

        let root = tempfile::tempdir().unwrap();
        let config = test_config(root.path());
        let (service, _) = WorkerService::open(config, Arc::new(AdmitAll)).unwrap();
        let source = SourceId(uuid::Uuid::from_u128(91));
        let view = lvu_core::ViewId(uuid::Uuid::from_u128(92));
        let definition = file_definition(91, &root.path().join("v.log"));
        let save = |sequence| StoreMethod::Save {
            request_id: format!("s-{sequence}"),
            window_id: "w".into(),
            sequence,
            definition: definition.clone(),
            view_id: view,
            state: test_view(source, 92, 0),
            expected_version: None,
        };
        // First write inserts at version 0.
        match service.dispatch_store("w", save(1)).await {
            StoreEvent::Saved { version: 0, .. } => {}
            other => panic!("expected initial insert, got {other:?}"),
        }
        // The row exists now: the same baseless save conflicts, permanently.
        // Every None retry fails identically with the committed version
        // echoed — never silently overwriting — which is why a first save
        // after load/create must carry a seeded base.
        for sequence in 2..=3 {
            match service.dispatch_store("w", save(sequence)).await {
                StoreEvent::SaveFailed {
                    current_version: Some(0),
                    ..
                } => {}
                other => panic!("expected permanent conflict, got {other:?}"),
            }
        }
        service.request_shutdown();
        service.shutdown().await;
    }

    #[tokio::test]
    async fn load_echoes_versions_and_create_seeds_zero() {
        use crate::protocol::StoreMethod;

        let root = tempfile::tempdir().unwrap();
        let config = test_config(root.path());
        let (service, _) = WorkerService::open(config, Arc::new(AdmitAll)).unwrap();
        let source = SourceId(uuid::Uuid::from_u128(93));
        let derived = lvu_core::ViewId(uuid::Uuid::from_u128(94));
        let definition = file_definition(93, &root.path().join("v.log"));
        // Create answers Saved with the version read back post-commit.
        match service
            .dispatch_store(
                "w",
                StoreMethod::CreateDerivedView {
                    request_id: "c".into(),
                    window_id: "w".into(),
                    sequence: 1,
                    definition: definition.clone(),
                    view_id: derived,
                    state: test_view(source, 94, 0),
                },
            )
            .await
        {
            StoreEvent::Saved { version: 0, .. } => {}
            other => panic!("expected created version 0, got {other:?}"),
        }
        // A save carrying the seeded base 0 commits version 1.
        match service
            .dispatch_store(
                "w",
                StoreMethod::Save {
                    request_id: "s".into(),
                    window_id: "w".into(),
                    sequence: 2,
                    definition: definition.clone(),
                    view_id: derived,
                    state: test_view(source, 94, 0),
                    expected_version: Some(0),
                },
            )
            .await
        {
            StoreEvent::Saved { version: 1, .. } => {}
            other => panic!("expected version 1, got {other:?}"),
        }
        // Load echoes the persisted versions, which is what reseeds bases.
        match service
            .dispatch_store(
                "w",
                StoreMethod::Load {
                    request_id: "l".into(),
                    window_id: "w".into(),
                    definition: definition.clone(),
                    view_id: derived,
                },
            )
            .await
        {
            StoreEvent::Loaded { views, .. } => {
                let found = views
                    .iter()
                    .find(|view| view.id == derived)
                    .expect("created view is listed");
                assert_eq!(found.version, 1);
            }
            other => panic!("expected loaded views, got {other:?}"),
        }
        service.request_shutdown();
        service.shutdown().await;
    }

    #[tokio::test]
    async fn worker_sessions_are_unique_per_open() {
        let first_root = tempfile::tempdir().unwrap();
        let second_root = tempfile::tempdir().unwrap();
        let (first, _) =
            WorkerService::open(test_config(first_root.path()), Arc::new(AdmitAll)).unwrap();
        let (second, _) =
            WorkerService::open(test_config(second_root.path()), Arc::new(AdmitAll)).unwrap();
        assert!(!first.worker_session().is_empty());
        assert_ne!(first.worker_session(), second.worker_session());
        // A UUIDv4, so windows can key remote epoch on it without parsing
        // surprises.
        let parsed =
            uuid::Uuid::parse_str(first.worker_session()).expect("session is a UUID string");
        assert_eq!(parsed.get_version(), Some(uuid::Version::Random));
    }

    #[tokio::test]
    async fn progress_poll_serves_canonical_snapshot_and_refuses_gone() {
        let root = tempfile::tempdir().unwrap();
        let log = root.path().join("v.log");
        std::fs::write(&log, "one\n").unwrap();
        let config = test_config(root.path());
        let (service, _) = WorkerService::open(config, Arc::new(AdmitAll)).unwrap();
        let session = service.worker_session().to_owned();
        let definition = file_definition(95, &log);
        service
            .request_start(definition.clone())
            .await
            .expect("start");
        let (mut client, mut decoder) = attach(&service, 503).await;
        // A live source answers with the canonical progress verbatim,
        // bound to this worker's session.
        let poll = serde_json::to_value(crate::protocol::WorkerRequest::RequestProgress {
            request_id: "p1".into(),
            source_id: definition.id.0.to_string(),
        })
        .unwrap();
        match rpc(&mut client, &mut decoder, poll).await.as_slice() {
            [
                WorkerEvent::SourceProgress {
                    worker_session,
                    progress,
                    ..
                },
            ] => {
                assert_eq!(worker_session, &session);
                assert_eq!(progress.source_id, definition.id);
                // The generation value itself is manager-assigned; what the
                // transport guarantees is verbatim stability across polls.
                let again = serde_json::to_value(crate::protocol::WorkerRequest::RequestProgress {
                    request_id: "p1b".into(),
                    source_id: definition.id.0.to_string(),
                })
                .unwrap();
                match rpc(&mut client, &mut decoder, again).await.as_slice() {
                    [
                        WorkerEvent::SourceProgress {
                            worker_session: session_again,
                            progress: progress_again,
                            ..
                        },
                    ] => {
                        assert_eq!(session_again, &session);
                        assert_eq!(progress_again.generation, progress.generation);
                    }
                    other => panic!("expected progress snapshot, got {other:?}"),
                }
            }
            other => panic!("expected progress snapshot, got {other:?}"),
        }
        // Unknown ids are refused, never zero-filled.
        let unknown = serde_json::to_value(crate::protocol::WorkerRequest::RequestProgress {
            request_id: "p2".into(),
            source_id: uuid::Uuid::nil().to_string(),
        })
        .unwrap();
        match rpc(&mut client, &mut decoder, unknown).await.as_slice() {
            [WorkerEvent::Refused { .. }] => {}
            other => panic!("expected refusal, got {other:?}"),
        }
        // A stopped source reports its terminal snapshot (final counts
        // and last_error stay readable after the capture ends); only a
        // never-known id is refused.
        service.request_stop(definition.id).await.expect("stop");
        let ended = serde_json::to_value(crate::protocol::WorkerRequest::RequestProgress {
            request_id: "p3".into(),
            source_id: definition.id.0.to_string(),
        })
        .unwrap();
        match rpc(&mut client, &mut decoder, ended).await.as_slice() {
            [WorkerEvent::SourceProgress { progress, .. }] => {
                assert_eq!(progress.source_id, definition.id);
                assert!(progress.state.is_terminal());
            }
            other => panic!("expected terminal snapshot, got {other:?}"),
        }
        service.request_shutdown();
        service.shutdown().await;
    }

    #[tokio::test]
    async fn oversize_store_method_refused_at_dispatch_boundary() {
        use crate::protocol::StoreMethod;

        let root = tempfile::tempdir().unwrap();
        let config = test_config(root.path());
        let (service, _) = WorkerService::open(config, Arc::new(AdmitAll)).unwrap();
        let source = SourceId(uuid::Uuid::from_u128(71));
        let view = lvu_core::ViewId(uuid::Uuid::from_u128(72));
        // A fat-but-valid view state; the encoded method must trip the
        // transport bound before any store work.
        let mut state = test_view(source, 72, 0);
        state.search_draft = Some("x".repeat(300 * 1024));
        let oversized = StoreMethod::Save {
            request_id: "big".into(),
            window_id: "w".into(),
            sequence: 1,
            definition: file_definition(71, &root.path().join("v.log")),
            view_id: view,
            state,
            expected_version: None,
        };
        match service.dispatch_store("w", oversized).await {
            StoreEvent::SaveFailed { reason, .. } => {
                assert!(
                    reason.contains("limit") || reason.contains("bytes"),
                    "{reason}"
                )
            }
            other => panic!("expected oversize refusal, got {other:?}"),
        }
        service.request_shutdown();
        service.shutdown().await;
    }

    #[tokio::test]
    async fn admission_hook_verdicts_surface_unchanged() {
        let root = tempfile::tempdir().unwrap();
        let config = test_config(root.path());
        let refusing = Arc::new(ScriptedAdmission {
            verdict: AdmissionVerdict::Refuse("nope".into()),
        });
        let (service, _) = WorkerService::open(config.clone(), refusing).unwrap();
        let definition = file_definition(51, &root.path().join("x.log"));
        assert!(matches!(
            service.request_start(definition).await,
            Err(reason) if reason == "nope"
        ));
        // A hook verdict naming an id the worker never started is not live:
        // surfacing it as `Present` would re-present a dead capture, so the
        // worker refuses with that id instead of trusting the verdict.
        let live = SourceId(uuid::Uuid::from_u128(52));
        let presenting = Arc::new(ScriptedAdmission {
            verdict: AdmissionVerdict::Present { live_id: live },
        });
        let (service, _) = WorkerService::open(config, presenting).unwrap();
        let definition = file_definition(53, &root.path().join("y.log"));
        assert!(matches!(
            service.request_start(definition).await,
            Err(reason) if reason.contains("unknown source")
        ));
        service.request_shutdown();
        service.shutdown().await;
    }

    #[tokio::test]
    async fn invalid_schema_and_relative_process_context_refuse_before_admission() {
        let root = tempfile::tempdir().unwrap();
        let admission = Arc::new(CountingAdmission(std::sync::atomic::AtomicUsize::new(0)));
        let (service, _) =
            WorkerService::open(test_config(root.path()), admission.clone()).unwrap();

        let mut unsupported = file_definition(54, &root.path().join("schema.log"));
        unsupported.schema_version = 2;
        assert!(matches!(
            service.request_start(unsupported).await,
            Err(reason) if reason.contains("unsupported source schema_version 2")
        ));

        let relative = file_definition(55, Path::new("window.log"));
        assert!(matches!(
            service.request_start(relative).await,
            Err(reason) if reason.contains("file path must be absolute")
        ));

        let command = |id, executable: &str, cwd: Option<PathBuf>| SourceDefinition {
            schema_version: 1,
            id: SourceId(uuid::Uuid::from_u128(id)),
            name: format!("command-{id}"),
            acquisition: lvu_core::Acquisition::Command {
                command: lvu_core::CommandDefinition {
                    program: lvu_core::CommandProgram::Exec {
                        executable: PathBuf::from(executable),
                        args: Vec::new(),
                    },
                    cwd,
                    environment: Default::default(),
                    restart: lvu_core::RestartPolicy::Never,
                },
            },
            identity_hints: Default::default(),
            retention: None,
        };
        assert!(matches!(
            service.request_start(command(56, "./tool", Some(root.path().to_path_buf()))).await,
            Err(reason) if reason.contains("relative command executable paths")
        ));
        assert!(
            validate_start_boundary(&command(58, "tool", Some(root.path().to_path_buf()))).is_ok(),
            "bare executable names retain PATH lookup semantics"
        );
        assert!(
            validate_start_boundary(&command(59, "bin/tool", Some(root.path().to_path_buf())))
                .is_err(),
            "path-like executable names must arrive resolved"
        );
        assert!(matches!(
            service.request_start(command(57, "tool", None)).await,
            Err(reason) if reason.contains("command cwd must be explicit")
        ));
        assert_eq!(
            admission.0.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "boundary failures never reach duplicate admission"
        );
        service.request_shutdown();
        service.shutdown().await;
    }

    #[tokio::test]
    async fn timed_out_start_finishes_registration_before_reuse() {
        let root = tempfile::tempdir().unwrap();
        let log = root.path().join("timeout.log");
        std::fs::write(&log, "one\n").unwrap();
        let (service, _) = WorkerService::open(
            test_config(root.path()),
            Arc::new(crate::child::ChildAdmission),
        )
        .unwrap();
        let reached = Arc::new(tokio::sync::Barrier::new(2));
        let release = Arc::new(tokio::sync::Barrier::new(2));
        service.set_start_side_effect_pause(Some((Arc::clone(&reached), Arc::clone(&release))));

        let definition = file_definition(58, &log);
        let request = {
            let service = Arc::clone(&service);
            let definition = definition.clone();
            tokio::spawn(async move {
                tokio::time::timeout(Duration::from_millis(20), service.request_start(definition))
                    .await
            })
        };
        reached.wait().await;
        assert!(
            request.await.unwrap().is_err(),
            "request deadline must elapse"
        );
        service.set_start_side_effect_pause(None);
        release.wait().await;

        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            if service.snapshot_definitions().await.len() == 1 {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "settlement did not register"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let mut duplicate = definition.clone();
        duplicate.id = SourceId(uuid::Uuid::from_u128(59));
        assert!(matches!(
            service.request_start(duplicate).await,
            Ok(StartedOutcome::Present { live_id }) if live_id == definition.id
        ));
        assert_eq!(
            load_session_set(&root.path().join("workspace")).unwrap(),
            vec![definition]
        );
        service.request_shutdown();
        service.shutdown().await;
    }

    #[tokio::test]
    async fn shutdown_drains_a_timed_out_start_before_stopping_it() {
        let root = tempfile::tempdir().unwrap();
        let log = root.path().join("shutdown-timeout.log");
        std::fs::write(&log, "one\n").unwrap();
        let (service, _) =
            WorkerService::open(test_config(root.path()), Arc::new(AdmitAll)).unwrap();
        let reached = Arc::new(tokio::sync::Barrier::new(2));
        let release = Arc::new(tokio::sync::Barrier::new(2));
        service.set_start_side_effect_pause(Some((Arc::clone(&reached), Arc::clone(&release))));
        let definition = file_definition(65, &log);
        let request = {
            let service = Arc::clone(&service);
            let definition = definition.clone();
            tokio::spawn(async move {
                tokio::time::timeout(Duration::from_millis(20), service.request_start(definition))
                    .await
            })
        };
        reached.wait().await;
        assert!(request.await.unwrap().is_err());

        let mut shutdown = {
            let service = Arc::clone(&service);
            tokio::spawn(async move { service.shutdown().await })
        };
        assert!(
            tokio::time::timeout(Duration::from_millis(30), &mut shutdown)
                .await
                .is_err(),
            "shutdown must wait for detached definition/session settlement"
        );
        service.set_start_side_effect_pause(None);
        release.wait().await;
        let reports = shutdown.await.unwrap();
        assert!(reports.iter().any(|(id, outcome)| {
            *id == definition.id && outcome.as_ref().is_ok_and(|report| report == "stopped")
        }));
        assert_eq!(service.snapshot_definitions().await.len(), 1);
        assert_eq!(
            load_session_set(&root.path().join("workspace")).unwrap(),
            vec![definition.clone()]
        );
        assert!(
            service
                .manager
                .source(definition.id)
                .is_none_or(|handle| handle.progress().state.is_terminal())
        );
    }

    #[tokio::test]
    async fn shutdown_closes_admission_before_a_paused_request_can_reserve() {
        let root = tempfile::tempdir().unwrap();
        let log = root.path().join("pre-admission.log");
        std::fs::write(&log, "one\n").unwrap();
        let (service, _) =
            WorkerService::open(test_config(root.path()), Arc::new(AdmitAll)).unwrap();
        let reached = Arc::new(tokio::sync::Barrier::new(2));
        let release = Arc::new(tokio::sync::Barrier::new(2));
        service.set_lifecycle_admission_pause(Some((Arc::clone(&reached), Arc::clone(&release))));
        let definition = file_definition(67, &log);
        let request = {
            let service = Arc::clone(&service);
            let definition = definition.clone();
            tokio::spawn(async move { service.request_start(definition).await })
        };
        reached.wait().await;

        assert!(service.shutdown().await.is_empty());
        service.set_lifecycle_admission_pause(None);
        release.wait().await;
        assert!(matches!(
            request.await.unwrap(),
            Err(reason) if reason.contains("shutting down")
        ));
        assert!(service.snapshot_definitions().await.is_empty());
        assert!(service.manager.source(definition.id).is_none());
        assert!(
            load_session_set(&root.path().join("workspace"))
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn shutdown_aborts_an_overdue_settlement_without_late_registration() {
        let root = tempfile::tempdir().unwrap();
        let log = root.path().join("overdue-settlement.log");
        std::fs::write(&log, "one\n").unwrap();
        let mut config = test_config(root.path());
        config.request_timeout = Duration::from_millis(20);
        let (service, _) = WorkerService::open(config, Arc::new(AdmitAll)).unwrap();
        let reached = Arc::new(tokio::sync::Barrier::new(2));
        let release = Arc::new(tokio::sync::Barrier::new(2));
        service.set_start_side_effect_pause(Some((Arc::clone(&reached), release)));
        let definition = file_definition(68, &log);
        let request = {
            let service = Arc::clone(&service);
            let definition = definition.clone();
            tokio::spawn(async move { service.request_start(definition).await })
        };
        reached.wait().await;

        let reports = tokio::time::timeout(Duration::from_secs(2), service.shutdown())
            .await
            .expect("shutdown remains bounded after aborting overdue settlement");
        assert!(reports.iter().any(|(id, _)| *id == definition.id));
        service.set_start_side_effect_pause(None);
        assert!(matches!(
            request.await.unwrap(),
            Err(reason) if reason.contains("shutting down") || reason.contains("closed")
        ));
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(service.snapshot_definitions().await.is_empty());
        assert!(
            load_session_set(&root.path().join("workspace"))
                .unwrap()
                .is_empty(),
            "aborted settlement cannot publish a session after shutdown"
        );
        assert!(
            service
                .manager
                .source(definition.id)
                .is_none_or(|handle| handle.progress().state.is_terminal())
        );
    }

    #[tokio::test]
    async fn shutdown_reports_a_stop_aborted_before_session_removal() {
        let root = tempfile::tempdir().unwrap();
        let log = root.path().join("aborted-stop.log");
        std::fs::write(&log, "one\n").unwrap();
        let mut config = test_config(root.path());
        config.request_timeout = Duration::from_millis(20);
        let (service, _) = WorkerService::open(config, Arc::new(AdmitAll)).unwrap();
        let definition = file_definition(70, &log);
        service.request_start(definition.clone()).await.unwrap();
        let reached = Arc::new(tokio::sync::Barrier::new(2));
        let release = Arc::new(tokio::sync::Barrier::new(2));
        service.set_stop_side_effect_pause(Some((Arc::clone(&reached), release)));
        let stop = {
            let service = Arc::clone(&service);
            tokio::spawn(async move { service.request_stop(definition.id).await })
        };
        reached.wait().await;

        let reports = service.shutdown().await;
        service.set_stop_side_effect_pause(None);
        assert!(reports.iter().any(|(id, outcome)| {
            *id == definition.id
                && matches!(outcome, Err(reason) if reason.contains("did not settle cleanly"))
        }));
        assert!(stop.await.unwrap().is_err());
        assert_eq!(
            load_session_set(&root.path().join("workspace")).unwrap(),
            vec![definition.clone()],
            "an aborted stop must preserve durable restart intent"
        );
        assert!(
            service
                .manager
                .source(definition.id)
                .is_none_or(|handle| handle.progress().state.is_terminal())
        );
    }

    #[tokio::test]
    async fn incomplete_and_undurable_stops_retain_session_until_retry() {
        let root = tempfile::tempdir().unwrap();
        let log = root.path().join("stop-persist.log");
        std::fs::write(&log, "one\n").unwrap();
        let (service, _) =
            WorkerService::open(test_config(root.path()), Arc::new(AdmitAll)).unwrap();
        let definition = file_definition(66, &log);
        service.request_start(definition.clone()).await.unwrap();

        service.force_incomplete_stop_report(definition.id);
        assert!(matches!(
            service.request_stop(definition.id).await,
            Err(reason) if reason.contains("stop incomplete") && reason.contains("remains in the session")
        ));
        assert_eq!(
            load_session_set(&root.path().join("workspace")).unwrap(),
            vec![definition.clone()],
            "an incomplete manager report must retain durable restart intent"
        );

        let temporary = root.path().join("workspace/session.json.tmp");
        std::fs::create_dir(&temporary).unwrap();
        assert!(matches!(
            service.request_stop(definition.id).await,
            Err(reason) if reason.contains("session update failed") && reason.contains("retry stop")
        ));
        assert_eq!(
            load_session_set(&root.path().join("workspace")).unwrap(),
            vec![definition.clone()],
            "failed removal never masquerades as durable"
        );

        std::fs::remove_dir(&temporary).unwrap();
        service.request_stop(definition.id).await.unwrap();
        assert!(
            load_session_set(&root.path().join("workspace"))
                .unwrap()
                .is_empty()
        );
        service.request_shutdown();
        service.shutdown().await;
    }

    #[test]
    fn shutdown_maps_an_incomplete_manager_report_to_error() {
        assert_eq!(
            shutdown_stop_outcome(Ok(StopReport {
                complete: false,
                discarded_bytes: 17,
                discarded_bytes_known: false,
            })),
            Err("stop incomplete".to_owned())
        );
    }

    #[tokio::test]
    async fn shutdown_reports_a_settlement_that_survives_abort_and_second_drain() {
        let root = tempfile::tempdir().unwrap();
        let mut config = test_config(root.path());
        config.request_timeout = Duration::from_millis(5);
        let (service, _) = WorkerService::open(config, Arc::new(AdmitAll)).unwrap();
        let id = SourceId(uuid::Uuid::from_u128(69));
        let permit = service.lifecycle_admission.try_enter(id).unwrap();

        let reports = service.shutdown().await;
        assert!(reports.iter().any(|(reported, outcome)| {
            *reported == id
                && matches!(outcome, Err(reason) if reason.contains("did not settle cleanly"))
        }));
        drop(permit);
    }

    #[tokio::test]
    async fn stop_waits_for_restart_settlement_and_wins_last() {
        let root = tempfile::tempdir().unwrap();
        let log = root.path().join("lifecycle.log");
        std::fs::write(&log, "one\n").unwrap();
        let (service, _) =
            WorkerService::open(test_config(root.path()), Arc::new(AdmitAll)).unwrap();
        let definition = file_definition(60, &log);
        service.request_start(definition.clone()).await.unwrap();

        let reached = Arc::new(tokio::sync::Barrier::new(2));
        let release = Arc::new(tokio::sync::Barrier::new(2));
        service.set_start_side_effect_pause(Some((Arc::clone(&reached), Arc::clone(&release))));
        let restart = {
            let service = Arc::clone(&service);
            tokio::spawn(async move { service.request_restart(definition.id).await })
        };
        reached.wait().await;
        let mut stop = {
            let service = Arc::clone(&service);
            tokio::spawn(async move { service.request_stop(definition.id).await })
        };
        assert!(
            tokio::time::timeout(Duration::from_millis(30), &mut stop)
                .await
                .is_err(),
            "stop must not overlap the paused restart"
        );
        service.set_start_side_effect_pause(None);
        release.wait().await;
        restart.await.unwrap().unwrap();
        stop.await.unwrap().unwrap();
        assert!(
            service
                .manager
                .source(definition.id)
                .is_none_or(|handle| handle.progress().state.is_terminal()),
            "serialized later stop leaves no restarted capture live"
        );
        service.request_shutdown();
        service.shutdown().await;
    }

    #[tokio::test]
    async fn stop_waits_for_present_to_restart_settlement_and_wins_last() {
        let root = tempfile::tempdir().unwrap();
        let log = root.path().join("present-lifecycle.log");
        std::fs::write(&log, "one\n").unwrap();
        let (service, _) = WorkerService::open(
            test_config(root.path()),
            Arc::new(crate::child::ChildAdmission),
        )
        .unwrap();
        let definition = file_definition(63, &log);
        service.request_start(definition.clone()).await.unwrap();
        service.request_stop(definition.id).await.unwrap();

        let reached = Arc::new(tokio::sync::Barrier::new(2));
        let release = Arc::new(tokio::sync::Barrier::new(2));
        service.set_start_side_effect_pause(Some((Arc::clone(&reached), Arc::clone(&release))));
        let present_restart = {
            let service = Arc::clone(&service);
            let mut duplicate = definition.clone();
            duplicate.id = SourceId(uuid::Uuid::from_u128(64));
            tokio::spawn(async move { service.request_start(duplicate).await })
        };
        reached.wait().await;
        let mut stop = {
            let service = Arc::clone(&service);
            tokio::spawn(async move { service.request_stop(definition.id).await })
        };
        assert!(
            tokio::time::timeout(Duration::from_millis(30), &mut stop)
                .await
                .is_err(),
            "stop must not overlap the paused Present-to-restart transition"
        );
        service.set_start_side_effect_pause(None);
        release.wait().await;
        assert!(matches!(
            present_restart.await.unwrap().unwrap(),
            StartedOutcome::Started { source_id, .. } if source_id == definition.id
        ));
        stop.await.unwrap().unwrap();
        assert!(
            service
                .manager
                .source(definition.id)
                .is_none_or(|handle| handle.progress().state.is_terminal())
        );
        service.request_shutdown();
        service.shutdown().await;
    }

    async fn wait_terminal(service: &Arc<WorkerService>, id: SourceId) {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            let terminal = service
                .manager
                .source(id)
                .is_none_or(|handle| handle.progress().state.is_terminal());
            if terminal {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "capture never reached a terminal state"
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    #[tokio::test]
    async fn present_resolves_liveness_restart_and_stdin_refusal() {
        let root = tempfile::tempdir().unwrap();
        let log = root.path().join("app.log");
        std::fs::write(&log, "one\n").unwrap();
        let config = test_config(root.path());
        let (service, _) = WorkerService::open(config, Arc::new(AdmitAll)).unwrap();
        let id = SourceId(uuid::Uuid::from_u128(61));
        let mut definition = file_definition(61, &log);
        definition.id = id;
        // Live structural match presents under the original id.
        assert!(matches!(
            service.request_start(definition).await,
            Ok(StartedOutcome::Started { source_id, .. }) if source_id == id
        ));
        assert!(matches!(
            service.present_or_restart(id).await,
            Ok(StartedOutcome::Present { live_id }) if live_id == id
        ));
        // After an explicit stop the same match restarts the original id
        // instead of presenting the terminal handle.
        service.request_stop(id).await.expect("stop works");
        wait_terminal(&service, id).await;
        assert!(matches!(
            service.present_or_restart(id).await,
            Ok(StartedOutcome::Started { source_id, .. }) if source_id == id
        ));
        // A stopped stdin definition refuses honestly: the pipe is gone and
        // cannot resume, so no id is ever re-presented for it.
        let stdin_id = SourceId(uuid::Uuid::from_u128(62));
        let stdin = lvu_core::SourceDefinition {
            schema_version: 1,
            id: stdin_id,
            name: "stdin".into(),
            acquisition: lvu_core::Acquisition::Stdin,
            identity_hints: Default::default(),
            retention: None,
        };
        service
            .definitions
            .lock()
            .await
            .insert(stdin_id, stdin.clone());
        assert!(matches!(
            service.present_or_restart(stdin_id).await,
            Err(reason) if reason.contains("fresh pipeline")
        ));
        service.request_shutdown();
        service.shutdown().await;
    }

    #[tokio::test]
    async fn concurrent_fresh_id_starts_share_the_first_capture() {
        let root = tempfile::tempdir().unwrap();
        let log = root.path().join("app.log");
        std::fs::write(&log, "one\n").unwrap();
        let config = test_config(root.path());
        let (service, _) = WorkerService::open(config, Arc::new(AdmitAll)).unwrap();
        // Nine parties at the barrier so all eight starters are already
        // inside `request_start` before any of them can settle: post-fix
        // every interleaving elects one leader and joins the rest.
        let barrier = Arc::new(tokio::sync::Barrier::new(9));
        let mut tasks = Vec::new();
        for _ in 0..8 {
            let service = Arc::clone(&service);
            let barrier = Arc::clone(&barrier);
            let mut definition = file_definition(0, &log);
            definition.id = SourceId(uuid::Uuid::new_v4());
            definition.name = format!("window-{}", definition.id.0);
            tasks.push(tokio::spawn(async move {
                barrier.wait().await;
                service.request_start(definition).await
            }));
        }
        barrier.wait().await;
        let mut ids = Vec::new();
        for task in tasks {
            let outcome = task.await.expect("starter joins").expect("start works");
            match outcome {
                StartedOutcome::Started { source_id, .. }
                | StartedOutcome::Present { live_id: source_id } => ids.push(source_id),
                StartedOutcome::StdinBound { .. } => panic!("file start bound stdin"),
            }
        }
        let first = ids[0];
        assert!(
            ids.iter().all(|id| *id == first),
            "every concurrent start shares the first capture: {ids:?}"
        );
        // Exactly one definition committed: no second capture exists.
        assert_eq!(service.snapshot_definitions().await.len(), 1);
        service.request_shutdown();
        service.shutdown().await;
    }
}
