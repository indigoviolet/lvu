// The static Linux release target is musl, whose allocator costs roughly twice
// the wall clock on Polars' allocation pattern: a 50000-record literal scan
// measured 205ms against musl's malloc and 97ms against glibc. Supplying an
// allocator here keeps the portable build as fast as the glibc one it replaces.
// Nothing else in the binary changes, and no other target is affected.
#[cfg(target_env = "musl")]
#[global_allocator]
static GLOBAL_ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

use std::{
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    env,
    ffi::OsString,
    io::{IsTerminal, Read, Write},
    path::{Path, PathBuf},
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc as std_mpsc,
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

use lvu::theme::ThemeId;
use lvu::{
    App, AskAiKind, AskAiRequest, AskAiStage, DiscoveryItem, DiscoveryUiRequest, InvestigationItem,
    InvestigationRequest, InvestigationStage, PathCompletionRequest, RowProvider, SettingsContext,
    SettingsRequest, SettingsValues, SourceAiPreview, SourceAiRequest, SourceAiStage, SourceItem,
    SourceKind, SourceLaunchRequest, ViewItem, ViewportRequest, terminal::run_with_tick_mut,
};
use lvu_core::{
    Acquisition, CommandDefinition, CommandProgram, RestartPolicy, SourceDefinition, SourceId,
};
use lvu_discovery::{
    CancellationToken, DiscoveryCandidate, DiscoveryLimits, DiscoveryRequest, DiscoveryResult,
    DockerConfig, ProcConfig, ProjectConfig, ProviderStatus,
};
use lvu_ingest::{RuntimeConfig, SourceHandle, SourceManager};
use lvu_live::{LiveConfig, LiveRowProvider};
use lvu_query::CompilerHostConfig;
use lvu_view::{
    AssistancePreparationJob, AssistancePreparationLimits, AssistancePreparationResult,
    AssistancePreparationState, NativeViewAdapter, RowReadiness, ScanState, SnapshotJob,
    SnapshotLimits, SnapshotState, ViewConfig,
};
use tokio::io::AsyncRead;
use tokio::sync::mpsc;
use uuid::Uuid;

pub mod agent;
mod command_controller;
mod command_execution;
mod command_rows;
mod command_snapshot;
mod memory;
mod resources;
pub mod settings;
mod storage;
mod time_recognition;
use agent::{
    AgentBridgeConfig, AgentBridgeHost, BridgeEvent, HostState, OriginatingRevision,
    ProposalContext, ProposalEnvelope, ProposalKind, Request as AgentRequest, SessionPurpose,
};
use memory::{Event as MemoryEvent, MemoryWorker, SaveRequest, SuggestionContext};
use storage::StorageJob;

const SOURCE_NAMESPACE: Uuid = Uuid::from_bytes([
    0x8a, 0x57, 0xd8, 0xc1, 0x2e, 0x99, 0x44, 0x64, 0xb7, 0x03, 0x0e, 0xba, 0xd7, 0xf0, 0x03, 0x11,
]);
const MAX_TICK_UPDATES: usize = 64;
const MAX_PENDING_STARTS: usize = 8;
const MAX_SOURCES: usize = 16;
const MAX_VIEWS: usize = 128;
const MAX_VIEWS_PER_SOURCE: usize = 16;
const MAX_DISCOVERY_CANDIDATES: usize = 128;
const MAX_PATH_CANDIDATES: usize = 64;
const MAX_PATH_ENTRIES: usize = 1024;
const MAX_AI_DATASETS: usize = 64;
const MAX_SESSION_RECORD_JOBS: usize = 4;
const MAX_INVESTIGATIONS: usize = 64;
const MAX_INVESTIGATION_SCAN_DIRS: usize = 256;
const MAX_INVESTIGATION_RECORD_BYTES: u64 = 32 * 1024;
const MAX_DEFERRED_OWNED_LIFECYCLE_EVENTS: usize = 32;

#[derive(Clone)]
struct AiStart {
    generation: u64,
    view_id: String,
    definition_revision: u64,
    kind: AskAiKind,
    instruction: String,
    provider: String,
    mode: String,
    thinking: String,
}

enum AiWork {
    Sampling {
        start: AiStart,
        job: AssistancePreparationJob,
        cancelled: bool,
    },
    Starting {
        start: AiStart,
        output_dir: PathBuf,
        manifest_path: PathBuf,
        datasets: Vec<PathBuf>,
        inline_context: Option<serde_json::Value>,
        inspection_command: Option<Vec<String>>,
        revision: OriginatingRevision,
        request: AgentRequest<String>,
        cancelled: bool,
    },
    Proposing {
        start: AiStart,
        output_dir: PathBuf,
        session_id: String,
        request: AgentRequest<ProposalEnvelope>,
    },
    Cancelling {
        generation: u64,
        session_id: String,
        request: AgentRequest<serde_json::Value>,
        failure: Option<(AiStart, String)>,
    },
}

struct PreparedAiContext {
    manifest_path: PathBuf,
    datasets: Vec<PathBuf>,
    inline_context: Option<serde_json::Value>,
    inspection_command: Option<Vec<String>>,
    revision: OriginatingRevision,
}

struct SessionRecordJob {
    result: std_mpsc::Receiver<Result<(), String>>,
    worker: Option<JoinHandle<()>>,
}

#[derive(Clone)]
struct InvestigationStart {
    generation: u64,
    view_id: String,
    definition_revision: u64,
    question: String,
    provider: String,
    mode: String,
    thinking: String,
}

enum InvestigationWork {
    Snapshot {
        start: InvestigationStart,
        job: SnapshotJob,
    },
    Preparing {
        start: InvestigationStart,
        output_dir: PathBuf,
        cancelled: bool,
        result: std_mpsc::Receiver<Result<PreparedAiContext, String>>,
        worker: JoinHandle<()>,
    },
    Starting {
        start: InvestigationStart,
        output_dir: PathBuf,
        context: PreparedAiContext,
        request: AgentRequest<String>,
        cancelled: bool,
    },
    Resuming {
        generation: u64,
        item: InvestigationItem,
        request: AgentRequest<String>,
        cancelled: bool,
    },
    Sending {
        generation: u64,
        item: InvestigationItem,
        request: AgentRequest<serde_json::Value>,
        event_floor: u64,
        turn_started: bool,
    },
    Watching {
        generation: u64,
        item: InvestigationItem,
        event_floor: u64,
        turn_started: bool,
    },
    Cancelling {
        generation: u64,
        item: InvestigationItem,
        request: AgentRequest<serde_json::Value>,
    },
    Unresolved {
        generation: u64,
        item: InvestigationItem,
        diagnostic: String,
    },
}

struct InvestigationLoadJob {
    result: std_mpsc::Receiver<Result<InvestigationLoadResult, String>>,
    worker: Option<JoinHandle<()>>,
}

struct InvestigationLoadResult {
    items: Vec<InvestigationItem>,
    diagnostic: Option<String>,
}

struct SettingsSaveJob {
    generation: u64,
    result: std_mpsc::Receiver<Result<SettingsContext, String>>,
    worker: Option<JoinHandle<()>>,
}

impl SettingsSaveJob {
    fn settle(&mut self, timeout: Duration) -> Result<(), String> {
        let deadline = Instant::now() + timeout;
        while self
            .worker
            .as_ref()
            .is_some_and(|worker| !worker.is_finished())
            && Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(1));
        }
        if self
            .worker
            .as_ref()
            .is_some_and(|worker| !worker.is_finished())
        {
            return Err("settings save did not settle before shutdown deadline".into());
        }
        if let Some(worker) = self.worker.take() {
            worker
                .join()
                .map_err(|_| "settings worker panicked".to_owned())?;
        }
        self.result
            .try_recv()
            .map_err(|_| "settings worker stopped without a result".to_owned())?
            .map(|_| ())
    }
}

#[derive(Clone, Debug)]
struct Options {
    capture_dir: Option<PathBuf>,
    sources: Vec<SourceArgument>,
}

#[derive(Clone, Debug)]
enum SourceArgument {
    File(PathBuf),
    Command(String),
    Stdin,
}

struct StartedSource {
    definition: SourceDefinition,
    view_id: String,
    handle: SourceHandle,
    origin: Option<StartOrigin>,
}

#[derive(Clone)]
enum StartOrigin {
    Manual(SourceLaunchRequest),
    Discovery { generation: u64 },
    Ai { generation: u64 },
}

#[derive(Clone)]
struct SourceAiStart {
    generation: u64,
    instruction: String,
    provider: String,
    mode: String,
    thinking: String,
}

#[derive(Debug)]
struct SourceAiContext {
    directory: PathBuf,
    manifest: PathBuf,
    revision: OriginatingRevision,
}

enum SourceAiWork {
    Preparing {
        start: SourceAiStart,
        cancel: CancellationToken,
        cancelled: bool,
        result: std_mpsc::Receiver<Result<SourceAiContext, String>>,
        worker: JoinHandle<()>,
    },
    Starting {
        start: SourceAiStart,
        context: SourceAiContext,
        request: AgentRequest<String>,
        cancelled: bool,
    },
    Proposing {
        start: SourceAiStart,
        context: SourceAiContext,
        session_id: String,
        request: AgentRequest<ProposalEnvelope>,
    },
    Cancelling {
        generation: u64,
        request: AgentRequest<serde_json::Value>,
    },
    Unresolved {
        generation: u64,
        diagnostic: String,
    },
}

#[derive(Clone)]
enum CandidateSelection {
    Live(DiscoveryCandidate),
    Recent(SourceDefinition),
}

struct StartFailure {
    source_id: SourceId,
    origin: StartOrigin,
    message: String,
}

struct PendingMemorySave {
    request: Box<SaveRequest>,
    dirty_since: std::time::Instant,
}

type StartResult = Result<StartedSource, StartFailure>;

struct ScanResult {
    generation: u64,
    result: DiscoveryResult,
}

struct PathCompletionResult {
    generation: u64,
    draft: String,
    replacement: Option<String>,
    candidates: Vec<String>,
    error: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SessionConfig {
    provider: String,
    mode: String,
    thinking: String,
}

impl SessionConfig {
    fn from_ai(start: &AiStart) -> Self {
        Self {
            provider: start.provider.clone(),
            mode: start.mode.clone(),
            thinking: start.thinking.clone(),
        }
    }

    fn from_source(start: &SourceAiStart) -> Self {
        Self {
            provider: start.provider.clone(),
            mode: start.mode.clone(),
            thinking: start.thinking.clone(),
        }
    }
}

fn apply_owned_session_event(
    event: &BridgeEvent,
    owned_ai_session: &mut Option<String>,
    owned_ai_session_config: &mut Option<SessionConfig>,
    retire_ai_session: &mut bool,
    ai_session_busy: &mut bool,
    source_ai_session: &mut Option<(String, u64)>,
    source_ai_session_config: &mut Option<SessionConfig>,
) -> Option<String> {
    let owns_ask = owned_ai_session.as_deref() == Some(event.session_id.as_str());
    let owns_source = source_ai_session
        .as_ref()
        .is_some_and(|(session_id, _)| session_id == &event.session_id);
    if !owns_ask && !owns_source {
        return None;
    }
    let activity_path = event
        .payload
        .get("activity_path")
        .and_then(serde_json::Value::as_str);
    match event.kind.as_str() {
        "session_archived" => {
            if owns_ask {
                *owned_ai_session = None;
                *owned_ai_session_config = None;
                *retire_ai_session = false;
                *ai_session_busy = false;
            }
            if owns_source {
                *source_ai_session = None;
                *source_ai_session_config = None;
            }
            Some(activity_path.map_or_else(
                || "Agent session archived after its activity was saved".into(),
                |path| format!("Agent activity saved: {path}"),
            ))
        }
        "archive_failed" => {
            if owns_ask {
                *ai_session_busy = true;
            }
            let error = event
                .payload
                .get("error")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("archive remains pending");
            Some(match activity_path {
                Some(path) => {
                    format!("Agent activity saved at {path}, but session archival failed: {error}")
                }
                None => format!("Agent session archival failed: {error}"),
            })
        }
        _ => None,
    }
}

// The explicit references keep this transition testable without introducing a
// second owner for Composition's Ask/source session bookkeeping.
#[allow(clippy::too_many_arguments)]
fn apply_or_defer_owned_session_event(
    event: &BridgeEvent,
    deferred: &mut VecDeque<BridgeEvent>,
    deferred_overflowed: &mut bool,
    registration_pending: bool,
    owned_ai_session: &mut Option<String>,
    owned_ai_session_config: &mut Option<SessionConfig>,
    retire_ai_session: &mut bool,
    ai_session_busy: &mut bool,
    source_ai_session: &mut Option<(String, u64)>,
    source_ai_session_config: &mut Option<SessionConfig>,
) -> Option<String> {
    let notice = apply_owned_session_event(
        event,
        owned_ai_session,
        owned_ai_session_config,
        retire_ai_session,
        ai_session_busy,
        source_ai_session,
        source_ai_session_config,
    );
    if notice.is_none()
        && matches!(event.kind.as_str(), "session_archived" | "archive_failed")
        && !registration_pending
    {
        let path = event
            .payload
            .get("activity_path")
            .and_then(serde_json::Value::as_str)?;
        return Some(if event.kind == "session_archived" {
            format!("Recovered agent activity saved: {path}")
        } else {
            let error = event
                .payload
                .get("error")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("archive remains pending");
            format!("Recovered agent activity at {path}; archival remains pending: {error}")
        });
    }
    if notice.is_none() && matches!(event.kind.as_str(), "session_archived" | "archive_failed") {
        if deferred.len() == MAX_DEFERRED_OWNED_LIFECYCLE_EVENTS {
            *deferred_overflowed = true;
            return Some(
                "Agent lifecycle event buffer filled before ownership was registered; restart lvu to reconcile managed sessions safely"
                    .into(),
            );
        }
        deferred.push_back(event.clone());
    }
    notice
}

fn owned_session_start_admission(deferred_overflowed: bool) -> Result<(), &'static str> {
    if deferred_overflowed {
        Err(
            "agent lifecycle reconciliation overflowed; restart lvu before starting more managed assistance",
        )
    } else {
        Ok(())
    }
}

#[allow(clippy::too_many_arguments)]
fn apply_deferred_owned_session_events(
    session_id: &str,
    deferred: &mut VecDeque<BridgeEvent>,
    owned_ai_session: &mut Option<String>,
    owned_ai_session_config: &mut Option<SessionConfig>,
    retire_ai_session: &mut bool,
    ai_session_busy: &mut bool,
    source_ai_session: &mut Option<(String, u64)>,
    source_ai_session_config: &mut Option<SessionConfig>,
) -> Option<String> {
    let mut notice = None;
    while let Some(index) = deferred
        .iter()
        .position(|event| event.session_id == session_id)
    {
        let event = deferred
            .remove(index)
            .expect("deferred lifecycle event index was present");
        if let Some(current) = apply_owned_session_event(
            &event,
            owned_ai_session,
            owned_ai_session_config,
            retire_ai_session,
            ai_session_busy,
            source_ai_session,
            source_ai_session_config,
        ) {
            notice = Some(current);
        }
    }
    notice
}

struct SourceControlJob {
    restart: bool,
    result: tokio::sync::oneshot::Receiver<Result<Option<SourceHandle>, String>>,
    worker: tokio::task::JoinHandle<()>,
}

async fn control_source(
    manager: Arc<SourceManager>,
    definition: SourceDefinition,
    restart: bool,
) -> Result<Option<SourceHandle>, String> {
    if restart && matches!(definition.acquisition, Acquisition::Stdin) {
        return Err("stdin cannot restart; provide a fresh pipeline".into());
    }
    let handle = manager
        .source(definition.id)
        .ok_or("source is unavailable")?;
    if !handle.progress().state.is_terminal() {
        match handle.stop().await {
            Ok(report) if report.complete => {}
            Ok(_) => return Err("capture stop incomplete; restart was not attempted".into()),
            Err(lvu_ingest::RuntimeError::NotActive) if handle.progress().state.is_terminal() => {}
            Err(error) => return Err(format!("stop capture: {error}")),
        }
    }
    if restart {
        manager
            .start(definition)
            .await
            .map(Some)
            .map_err(|error| format!("restart capture: {error}"))
    } else {
        let progress = handle.progress();
        if matches!(
            progress.state,
            lvu_ingest::RuntimeState::Error
                | lvu_ingest::RuntimeState::Incomplete
                | lvu_ingest::RuntimeState::StorageBlocked
        ) {
            return Err(format!(
                "capture ended as {:?}: {}",
                progress.state,
                progress
                    .last_error
                    .as_deref()
                    .unwrap_or("inspect source diagnostics")
            ));
        }
        Ok(None)
    }
}

struct Composition {
    manager: Arc<SourceManager>,
    raw: Arc<LiveRowProvider>,
    runtime: tokio::runtime::Handle,
    starts_tx: mpsc::Sender<StartResult>,
    starts_rx: mpsc::Receiver<StartResult>,
    sources: HashMap<SourceId, String>,
    definitions: HashMap<SourceId, SourceDefinition>,
    pending_starts: HashSet<SourceId>,
    source_controls: HashMap<SourceId, SourceControlJob>,
    cwd: PathBuf,
    scans_tx: mpsc::Sender<ScanResult>,
    scans_rx: mpsc::Receiver<ScanResult>,
    active_scan: Option<(u64, CancellationToken)>,
    pending_scan: Option<u64>,
    discovery_candidates: HashMap<String, CandidateSelection>,
    recent_sources: Vec<lvu_memory::SourceMetadata>,
    memory: MemoryWorker,
    memory_ready: HashSet<SourceId>,
    memory_restoring: HashSet<lvu_core::ViewId>,
    memory_deferred: HashMap<lvu_core::ViewId, lvu_memory::WorkingView>,
    memory_load_fences: HashMap<lvu_core::ViewId, u64>,
    memory_last: HashMap<lvu_core::ViewId, lvu::PersistentViewState>,
    /// Set once the memory worker has announced it will never serve this
    /// session, so a shutdown flush is not reported as a fresh failure.
    memory_unavailable: bool,
    memory_pending: HashMap<lvu_core::ViewId, PendingMemorySave>,
    memory_inflight: HashMap<u64, (lvu_core::ViewId, lvu::PersistentViewState)>,
    memory_failed: HashMap<lvu_core::ViewId, lvu::PersistentViewState>,
    memory_ack_sequence: HashMap<lvu_core::ViewId, u64>,
    memory_sequence: u64,
    completions_tx: mpsc::Sender<PathCompletionResult>,
    completions_rx: mpsc::Receiver<PathCompletionResult>,
    active_completion: Option<(u64, Arc<AtomicBool>)>,
    pending_completion: Option<PathCompletionRequest>,
    home: Option<PathBuf>,
    snapshot_root: PathBuf,
    agent: Option<AgentBridgeHost>,
    agent_error: Option<String>,
    active_ai: Option<AiWork>,
    owned_ai_session: Option<String>,
    owned_ai_session_config: Option<SessionConfig>,
    retire_ai_session: bool,
    ai_session_busy: bool,
    source_ai_work: Option<SourceAiWork>,
    source_ai_session: Option<(String, u64)>,
    source_ai_session_config: Option<SessionConfig>,
    deferred_owned_lifecycle_events: VecDeque<BridgeEvent>,
    deferred_owned_lifecycle_overflowed: bool,
    source_ai_proposals: HashMap<u64, SourceDefinition>,
    session_records: Vec<SessionRecordJob>,
    investigation_work: Option<InvestigationWork>,
    investigation_session: Option<InvestigationItem>,
    investigation_load: Option<InvestigationLoadJob>,
    storage_root: PathBuf,
    storage_job: Option<StorageJob>,
    pending_storage: Option<lvu::StorageRequest>,
    query_index_limit: u64,
    storage_review: Vec<lvu_live::DerivedArtifactIdentity>,
    settings_file: PathBuf,
    settings_paths: settings::AppPaths,
    applied_settings: settings::ValidatedSettings,
    settings_job: Option<SettingsSaveJob>,
    capture_root: PathBuf,
    command_controller: command_controller::CommandController,
}

impl Composition {
    fn tick(&mut self, app: &mut App, adapter: &mut NativeViewAdapter) -> bool {
        let mut changed = adapter.drain_updates(MAX_TICK_UPDATES) > 0;
        changed |= self.handle_source_controls(app, adapter);
        changed |= self.handle_settings(app);
        changed |= self.handle_storage(app, adapter);
        changed |= Self::handle_time_recognition(app, adapter);
        changed |= app.refresh_rolling_capture_times(unix_now_nanos(), Instant::now());
        changed |= self.poll_memory(app, adapter);
        changed |= self.handle_recipe_requests(app, adapter);
        changed |= self.handle_source_ai(app);
        changed |= self.handle_ai(app, adapter);
        changed |= self.handle_investigation(app, adapter);
        if let Some((generation, cancel)) = &self.active_completion
            && app.active_path_completion_generation() != Some(*generation)
        {
            cancel.store(true, Ordering::Release);
        }
        for request in app.take_path_completion_requests() {
            changed = true;
            if self.active_completion.is_some() {
                self.pending_completion = Some(request);
            } else {
                self.start_path_completion(request);
            }
        }
        while let Ok(result) = self.completions_rx.try_recv() {
            changed = true;
            self.active_completion = None;
            app.apply_path_completion_result(
                result.generation,
                &result.draft,
                result.replacement,
                result.candidates,
                result.error,
            );
            if let Some(request) = self.pending_completion.take() {
                self.start_path_completion(request);
            }
        }
        for request in app.take_discovery_requests() {
            changed = true;
            match request {
                DiscoveryUiRequest::Scan { generation } => self.spawn_scan(generation),
                DiscoveryUiRequest::Cancel { generation } => {
                    if let Some((active, cancel)) = &self.active_scan
                        && *active == generation
                    {
                        cancel.cancel();
                    }
                    if self.pending_scan == Some(generation) {
                        self.pending_scan = None;
                    }
                }
                DiscoveryUiRequest::Select { generation, key } => {
                    let Some(candidate) = self.discovery_candidates.get(&key).cloned() else {
                        app.discovery_selection_failed(
                            generation,
                            "candidate is stale; rescan discovery".into(),
                        );
                        continue;
                    };
                    let definition = match candidate {
                        CandidateSelection::Live(value) => value.source,
                        CandidateSelection::Recent(value) => value,
                    };
                    self.admit_definition(app, definition, StartOrigin::Discovery { generation });
                }
            }
        }
        while let Ok(scan) = self.scans_rx.try_recv() {
            changed = true;
            if self
                .active_scan
                .as_ref()
                .is_some_and(|(generation, _)| *generation == scan.generation)
            {
                self.active_scan = None;
                let mut items: Vec<_> = self
                    .recent_sources
                    .iter()
                    .map(recent_discovery_item)
                    .collect();
                items.extend(scan.result.candidates.iter().map(discovery_item));
                if app.apply_discovery_result(
                    scan.generation,
                    items,
                    discovery_status(&scan.result),
                ) {
                    self.discovery_candidates = scan
                        .result
                        .candidates
                        .iter()
                        .map(|candidate| {
                            (
                                candidate.fingerprint.clone(),
                                CandidateSelection::Live(candidate.clone()),
                            )
                        })
                        .collect();
                    for source in &self.recent_sources {
                        self.discovery_candidates.insert(
                            recent_key(source.definition.id),
                            CandidateSelection::Recent(source.definition.clone()),
                        );
                    }
                }
                if let Some(generation) = self.pending_scan.take() {
                    self.start_scan_task(generation);
                }
            }
        }
        for request in app.take_source_requests() {
            changed = true;
            let argument = match request.kind {
                SourceKind::File => match expand_tilde_path(&request.text, self.home.as_deref()) {
                    Ok(path) => SourceArgument::File(path),
                    Err(message) => {
                        app.source_request_failed(request, message);
                        continue;
                    }
                },
                SourceKind::Command => SourceArgument::Command(request.text.clone()),
            };
            let definition = match definition(argument, &self.cwd) {
                Ok(definition) => definition,
                Err(message) => {
                    app.source_request_failed(request, message);
                    continue;
                }
            };
            self.admit_definition(app, definition, StartOrigin::Manual(request));
        }
        while let Ok(result) = self.starts_rx.try_recv() {
            changed = true;
            match result {
                Ok(started) => {
                    let source_id = started.definition.id;
                    let definition = started.definition.clone();
                    self.pending_starts.remove(&source_id);
                    let origin = started.origin.clone().expect("dynamic start origin");
                    match register_started(adapter, app, &mut self.sources, started) {
                        Ok(view_id) => {
                            self.definitions.insert(source_id, definition.clone());
                            if let Err(error) = self.request_restore(app, definition) {
                                app.source_notice = Some(format!(
                                    "memory error: {error}; raw browsing remains available"
                                ));
                            }
                            start_succeeded(app, &origin, &view_id)
                        }
                        Err(message) => {
                            if let Some(handle) = self.manager.source(source_id) {
                                self.runtime.spawn(async move {
                                    let _ = handle.stop().await;
                                });
                            }
                            start_failed(app, origin, message);
                        }
                    }
                }
                Err(failure) => {
                    self.pending_starts.remove(&failure.source_id);
                    start_failed(app, failure.origin, failure.message);
                }
            }
        }
        changed |= self.handle_view_forks(app, adapter);
        changed |= self.handle_view_requests(app, adapter);
        changed |= self.handle_correlation(app, adapter);
        changed |= self.handle_command_enrichment(app, adapter);
        changed |= self.queue_memory_saves(app, false);
        for view in app.views().to_vec() {
            if let Some(status) = adapter.status(&view.id) {
                let mut health = match status.state {
                    ScanState::Raw => "raw view".to_owned(),
                    ScanState::Pending => {
                        format!("query pending: scanned {}", status.scanned_records)
                    }
                    ScanState::Ready => format!(
                        "query ready: matched {} / scanned {}",
                        status.matched_records, status.scanned_records
                    ),
                    ScanState::Limited => format!(
                        "query limit: matched {} / scanned {}",
                        status.matched_records, status.scanned_records
                    ),
                    ScanState::Error => "query error".to_owned(),
                    ScanState::Shutdown => "query shut down".to_owned(),
                };
                if let Some(diagnostic) = status.diagnostic {
                    health.push_str(": ");
                    health.push_str(&diagnostic);
                }
                // A satisfied query over rows that never arrive renders an empty
                // pane under a confident "query ready". The view adapter already
                // distinguishes those cases; say which one it is instead of
                // leaving the pane unexplained.
                if adapter.index_budget_unverified(&view.id) {
                    health.push_str(" · index cache total unverified");
                }
                if let Some(explanation) = row_delivery_explanation(adapter, &view.id) {
                    health.push_str(" · ");
                    health.push_str(&explanation);
                    if let Some(counters) = row_request_counters(adapter) {
                        health.push_str(" · ");
                        health.push_str(&counters);
                    }
                }
                if let Ok(id) = Uuid::parse_str(&view.source_id)
                    && let Some(handle) = self.manager.source(SourceId(id))
                {
                    let progress = handle.progress();
                    app.update_source_health(
                        &view.source_id,
                        format!("{:?}: {} records", progress.state, progress.records),
                    );
                }
                app.update_view_runtime_status(&view.id, health);
            }
        }
        changed
    }

    fn handle_settings(&mut self, app: &mut App) -> bool {
        let mut changed = false;
        for request in app.layers.settings.outbox.take() {
            changed = true;
            if self.settings_job.is_some() {
                app.complete_settings_save(request.generation, Err("settings save busy".into()));
                continue;
            }
            let path = self.settings_file.clone();
            let paths = self.settings_paths.clone();
            let capture_root = self.capture_root.clone();
            let applied = self.applied_settings.clone();
            let generation = request.generation;
            let (tx, rx) = std_mpsc::sync_channel(1);
            let worker = std::thread::spawn(move || {
                let result = save_settings_request(request, &path, &paths, &capture_root, &applied);
                let _ = tx.send(result);
            });
            self.settings_job = Some(SettingsSaveJob {
                generation,
                result: rx,
                worker: Some(worker),
            });
        }
        let result = self
            .settings_job
            .as_ref()
            .and_then(|job| job.result.try_recv().ok());
        if let Some(result) = result {
            let mut job = self.settings_job.take().expect("polled settings job");
            if let Some(worker) = job.worker.take() {
                let _ = worker.join();
            }
            app.complete_settings_save(job.generation, result);
            changed = true;
        } else if self
            .settings_job
            .as_ref()
            .is_some_and(|job| job.worker.as_ref().is_some_and(JoinHandle::is_finished))
        {
            let mut job = self.settings_job.take().expect("finished settings job");
            if let Some(worker) = job.worker.take() {
                let _ = worker.join();
            }
            app.complete_settings_save(
                job.generation,
                Err("settings worker stopped without a result".into()),
            );
            changed = true;
        }
        changed
    }

    /// Answers the Time dialog's request for timestamp-field candidates. Bounded
    /// and cheap enough to run inline: it reads one sampled page.
    fn handle_time_recognition(app: &mut App, adapter: &NativeViewAdapter) -> bool {
        let requests = app.layers.time.outbox.take();
        let mut changed = false;
        let year = Self::utc_year(unix_now_nanos());
        for request in requests {
            let recognition = time_recognition::recognize(&request, &adapter.rows(), year);
            changed |= app
                .layers
                .time
                .complete_recognition(request.generation, recognition);
        }
        changed
    }

    /// Calendar year of an instant, for readings that carry no year of their own.
    fn utc_year(unix_nanos: i64) -> i32 {
        let days = unix_nanos.div_euclid(86_400_000_000_000);
        // 1970-01-01 plus whole days, by the proleptic Gregorian calendar.
        let mut year = 1970i32;
        let mut remaining = days;
        loop {
            let length = if Self::is_leap(year) { 366 } else { 365 };
            if remaining < length {
                return year;
            }
            remaining -= length;
            year += 1;
        }
    }

    fn is_leap(year: i32) -> bool {
        year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
    }

    fn handle_storage(&mut self, app: &mut App, adapter: &NativeViewAdapter) -> bool {
        let mut changed = false;
        let requests = app.layers.storage.outbox.take();
        for (position, request) in requests.iter().copied().enumerate() {
            changed = true;
            if !matches!(request.kind, lvu::StorageRequestKind::Cancel)
                && requests[position + 1..].iter().any(|later| {
                    later.generation == request.generation
                        && matches!(later.kind, lvu::StorageRequestKind::Cancel)
                })
            {
                continue;
            }
            if let Some(job) = &self.storage_job {
                job.cancel();
            }
            match request.kind {
                lvu::StorageRequestKind::Cancel => self.pending_storage = None,
                _ if self.storage_job.is_some() => self.pending_storage = Some(request),
                _ => self.start_storage(request, adapter),
            }
        }
        if let Some(job) = &self.storage_job {
            if let Some(result) = job.poll() {
                changed = true;
                if app.layers.storage.complete(
                    result.generation,
                    result.snapshot,
                    result.status,
                    true,
                ) {
                    self.storage_review = result.reviewed;
                }
            }
            if job.finished() {
                let mut job = self.storage_job.take().expect("storage job");
                job.join();
                if let Some(request) = self.pending_storage.take() {
                    self.start_storage(request, adapter);
                }
            }
        }
        changed
    }

    fn start_storage(&mut self, request: lvu::StorageRequest, adapter: &NativeViewAdapter) {
        let clear = matches!(request.kind, lvu::StorageRequestKind::ClearUnusedDerived);
        self.storage_job = Some(StorageJob::start(
            request.generation,
            self.storage_root.clone(),
            Arc::clone(&self.raw),
            clear,
            adapter.membership_bytes_used(),
            self.query_index_limit,
            if clear {
                self.storage_review.clone()
            } else {
                Vec::new()
            },
        ));
    }

    fn handle_recipe_requests(&mut self, app: &mut App, adapter: &NativeViewAdapter) -> bool {
        let requests = app.take_recipe_requests();
        let changed = !requests.is_empty();
        for request in requests {
            match request {
                lvu::RecipeRequest::List { meta } => {
                    let context = suggestion_context(app, adapter, &self.definitions, &self.cwd);
                    if let Err(error) = self.memory.list_recipes(meta, context) {
                        app.recipe_failed(meta, error);
                    }
                }
                lvu::RecipeRequest::History { meta, recipe_id } => {
                    let result = Uuid::parse_str(&recipe_id)
                        .map_err(|e| e.to_string())
                        .and_then(|id| self.memory.recipe_history(meta, lvu_core::RecipeId(id)));
                    if let Err(error) = result {
                        app.recipe_failed(meta, error);
                    }
                }
                lvu::RecipeRequest::Save {
                    update,
                    meta,
                    name,
                    view_id,
                    config: state,
                } => {
                    let duplicate = app
                        .layers
                        .recipes
                        .state()
                        .items
                        .iter()
                        .any(|item| item.name == name);
                    if duplicate && update.is_none() {
                        app.recipe_failed(meta, "a recipe with that name already exists; select it and use Alt-U to update".into());
                        continue;
                    }
                    let Some(view) = app.views().iter().find(|view| view.id == view_id) else {
                        app.recipe_failed(meta, "selected view is unavailable".into());
                        continue;
                    };
                    let Ok(source_id) = Uuid::parse_str(&view.source_id).map(SourceId) else {
                        app.recipe_failed(meta, "selected source identity is invalid".into());
                        continue;
                    };
                    let Some(source) = self.definitions.get(&source_id).cloned() else {
                        app.recipe_failed(meta, "source definition unavailable".into());
                        continue;
                    };
                    let update = match update
                        .map(|(id, revision)| {
                            Uuid::parse_str(&id).and_then(|id| {
                                Uuid::parse_str(&revision)
                                    .map(|revision| (lvu_core::RecipeId(id), revision))
                            })
                        })
                        .transpose()
                    {
                        Ok(value) => value,
                        Err(error) => {
                            app.recipe_failed(meta, format!("invalid recipe identity: {error}"));
                            continue;
                        }
                    };
                    let stages = recipe_extraction_stages(&state);
                    let color_rules = state
                        .color_field
                        .clone()
                        .map(|field| lvu_memory::ColorRule {
                            expression: field,
                            style: "stable-value".into(),
                        })
                        .into_iter()
                        .collect();
                    let recipe = lvu_memory::RecipeFile {
                        schema_version: lvu_memory::RECIPE_SCHEMA_VERSION,
                        recipe_id: update.map_or_else(lvu_core::RecipeId::new, |value| value.0),
                        revision_id: Uuid::new_v4(),
                        name: name.clone(),
                        description: String::new(),
                        saved_at_unix_nanos: Some(
                            std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .ok()
                                .and_then(|since| i64::try_from(since.as_nanos()).ok())
                                .unwrap_or_default(),
                        ),
                        source: source.clone(),
                        view: lvu_memory::NamedViewDefinition {
                            schema_version: 1,
                            id: lvu_core::ViewId(Uuid::new_v4()),
                            name,
                            source_ids: vec![source.id],
                            stages,
                            search: state.search,
                            advanced_filter: (!state.advanced.is_empty()).then_some(
                                lvu_memory::ExpressionDefinition {
                                    expression: state.advanced,
                                },
                            ),
                            pinned_columns: state.pinned_columns,
                            color_rules,
                            time_policy: state.capture_time_policy.map_or_else(
                                || {
                                    state.capture_time.map_or(
                                        lvu_memory::TimePolicy::All,
                                        |window| lvu_memory::TimePolicy::Absolute {
                                            start_unix_nanos: window.start_unix_nanos,
                                            end_unix_nanos: window.end_unix_nanos,
                                        },
                                    )
                                },
                                |policy| match policy {
                                    lvu::CaptureTimePolicy::Absolute(window) => {
                                        lvu_memory::TimePolicy::Absolute {
                                            start_unix_nanos: window.start_unix_nanos,
                                            end_unix_nanos: window.end_unix_nanos,
                                        }
                                    }
                                    lvu::CaptureTimePolicy::Recent { seconds } => {
                                        lvu_memory::TimePolicy::Recent { seconds }
                                    }
                                },
                            ),
                            time_basis: match state.time_basis {
                                lvu::TimeBasis::Capture => lvu_memory::TimeBasis::Capture,
                                lvu::TimeBasis::Event => lvu_memory::TimeBasis::Event,
                                lvu::TimeBasis::Extracted => lvu_memory::TimeBasis::Extracted,
                                // A recipe carries no field token, so it cannot
                                // describe a chosen field; see `recipe_time_basis`.
                                lvu::TimeBasis::Selected => lvu_memory::TimeBasis::Capture,
                            },
                            grouping: (!state.grouping.is_empty()).then_some(state.grouping),
                        },
                    };
                    let context = suggestion_context_for_view(
                        app,
                        adapter,
                        &self.definitions,
                        &self.cwd,
                        &view_id,
                    );
                    if let Err(error) =
                        self.memory
                            .save_recipe(meta, recipe, update.map(|value| value.1), context)
                    {
                        app.recipe_failed(meta, error);
                    }
                }
                lvu::RecipeRequest::Import { meta, path } => {
                    if let Err(error) = self.memory.import_recipe(meta, PathBuf::from(path)) {
                        app.recipe_failed(meta, error);
                    }
                }
                lvu::RecipeRequest::Export {
                    meta,
                    path,
                    recipe_id,
                    revision,
                } => {
                    let result = Uuid::parse_str(&recipe_id)
                        .and_then(|id| {
                            Uuid::parse_str(&revision)
                                .map(|revision| (lvu_core::RecipeId(id), revision))
                        })
                        .map_err(|error| format!("invalid recipe identity: {error}"))
                        .and_then(|(recipe, revision)| {
                            self.memory
                                .export_recipe(meta, recipe, revision, PathBuf::from(path))
                        });
                    if let Err(error) = result {
                        app.recipe_failed(meta, error);
                    }
                }
                lvu::RecipeRequest::Outcome(outcome) => {
                    if let Err(error) = self.memory.record_suggestion(outcome) {
                        memory_notice(app, error);
                    }
                }
            }
        }
        changed
    }

    fn handle_source_ai(&mut self, app: &mut App) -> bool {
        let mut changed = false;
        for request in app.take_source_ai_requests() {
            changed = true;
            match request {
                SourceAiRequest::Start {
                    generation,
                    instruction,
                    provider,
                    mode,
                    thinking,
                } => {
                    self.source_ai_proposals.clear();
                    if self.source_ai_work.is_some() {
                        app.finish_source_ai(
                            generation,
                            Err("another source agent request is settling".into()),
                        );
                        continue;
                    }
                    if let Some(error) = &self.agent_error {
                        app.finish_source_ai(generation, Err(error.clone()));
                        continue;
                    }
                    let start = SourceAiStart {
                        generation,
                        instruction,
                        provider,
                        mode,
                        thinking,
                    };
                    let cancel = CancellationToken::default();
                    let context_cwd = self.cwd.clone();
                    let directory = self
                        .snapshot_root
                        .join("assistance")
                        .join(format!("source-ai-{}", Uuid::new_v4()));
                    let (result, worker) =
                        spawn_source_ai_context_worker(context_cwd, directory, cancel.clone());
                    self.source_ai_work = Some(SourceAiWork::Preparing {
                        start,
                        cancel,
                        cancelled: false,
                        result,
                        worker,
                    });
                }
                SourceAiRequest::Apply { generation } => {
                    let Some(definition) = self.source_ai_proposals.remove(&generation) else {
                        app.finish_source_ai(
                            generation,
                            Err("proposal is stale; request it again".into()),
                        );
                        continue;
                    };
                    self.admit_definition(app, definition, StartOrigin::Ai { generation });
                }
                SourceAiRequest::Cancel { generation } => {
                    self.source_ai_proposals.remove(&generation);
                    self.cancel_source_ai(generation);
                }
            }
        }
        let Some(work) = self.source_ai_work.take() else {
            return changed;
        };
        match work {
            SourceAiWork::Preparing {
                start,
                cancel,
                cancelled,
                result,
                worker,
            } => match result.try_recv() {
                Ok(Ok(context)) => {
                    let _ = worker.join();
                    if cancelled {
                        cleanup_unstarted_source_ai_context(&context);
                    } else {
                        self.begin_source_ai_session(app, start, context);
                    }
                }
                Ok(Err(error)) => {
                    let _ = worker.join();
                    if !cancelled {
                        app.finish_source_ai(start.generation, Err(error));
                    }
                }
                Err(std_mpsc::TryRecvError::Empty) => {
                    self.source_ai_work = Some(SourceAiWork::Preparing {
                        start,
                        cancel,
                        cancelled,
                        result,
                        worker,
                    });
                }
                Err(std_mpsc::TryRecvError::Disconnected) => {
                    let _ = worker.join();
                    app.finish_source_ai(
                        start.generation,
                        Err("source context worker disconnected".into()),
                    );
                }
            },
            SourceAiWork::Starting {
                start,
                context,
                request,
                cancelled,
            } => match request.try_result() {
                None => {
                    self.source_ai_work = Some(SourceAiWork::Starting {
                        start,
                        context,
                        request,
                        cancelled,
                    })
                }
                Some(Err(error)) => {
                    app.finish_source_ai(
                        start.generation,
                        Err(format!("start session: {}", host_error_message(error))),
                    );
                }
                Some(Ok(session_id)) => {
                    self.source_ai_session = Some((session_id.clone(), start.generation));
                    self.source_ai_session_config = Some(SessionConfig::from_source(&start));
                    if self.deferred_owned_lifecycle_overflowed {
                        app.finish_source_ai(
                            start.generation,
                            Err("agent lifecycle reconciliation overflowed; restart lvu before starting more managed assistance".into()),
                        );
                        return true;
                    }
                    if let Some(notice) = apply_deferred_owned_session_events(
                        &session_id,
                        &mut self.deferred_owned_lifecycle_events,
                        &mut self.owned_ai_session,
                        &mut self.owned_ai_session_config,
                        &mut self.retire_ai_session,
                        &mut self.ai_session_busy,
                        &mut self.source_ai_session,
                        &mut self.source_ai_session_config,
                    ) {
                        app.source_notice = Some(notice);
                    }
                    if self.source_ai_session.is_none() {
                        app.finish_source_ai(
                            start.generation,
                            Err(
                                "source-assistance session archived before proposal submission"
                                    .into(),
                            ),
                        );
                        return true;
                    }
                    if let Err(error) = admit_session_record(
                        &mut self.session_records,
                        &context.directory,
                        &session_id,
                    ) {
                        app.source_notice = Some(format!("source agent session record: {error}"));
                    }
                    if cancelled {
                        self.begin_source_ai_cancel(start.generation, session_id, app);
                    } else {
                        self.begin_source_ai_proposal(app, start, context, session_id);
                    }
                }
            },
            SourceAiWork::Proposing {
                start,
                context,
                session_id,
                request,
            } => match request.try_result() {
                None => {
                    self.source_ai_work = Some(SourceAiWork::Proposing {
                        start,
                        context,
                        session_id,
                        request,
                    })
                }
                Some(Err(error)) => {
                    app.finish_source_ai(
                        start.generation,
                        Err(format!("source proposal: {}", host_error_message(error))),
                    );
                    self.begin_source_ai_cancel(start.generation, session_id, app);
                }
                Some(Ok(proposal)) => match parse_source_proposal(&proposal, &self.cwd) {
                    Ok((definition, preview)) => {
                        if app.finish_source_ai(start.generation, Ok(preview)) {
                            self.source_ai_proposals
                                .insert(start.generation, definition);
                        }
                    }
                    Err(error) => {
                        app.finish_source_ai(start.generation, Err(error));
                    }
                },
            },
            SourceAiWork::Cancelling {
                generation,
                request,
            } => match request.try_result() {
                None => {
                    self.source_ai_work = Some(SourceAiWork::Cancelling {
                        generation,
                        request,
                    })
                }
                Some(Ok(value)) => {
                    if let Err(error) = validate_remote_cancellation(&value) {
                        app.finish_source_ai(generation, Err(error.clone()));
                        self.source_ai_work = Some(SourceAiWork::Unresolved {
                            generation,
                            diagnostic: error,
                        });
                    } else {
                        self.source_ai_session = None;
                        self.source_ai_session_config = None;
                    }
                }
                Some(Err(error)) => {
                    let diagnostic =
                        format!("source agent cancellation: {}", host_error_message(error));
                    app.finish_source_ai(generation, Err(diagnostic.clone()));
                    self.source_ai_work = Some(SourceAiWork::Unresolved {
                        generation,
                        diagnostic,
                    });
                }
            },
            work @ SourceAiWork::Unresolved { .. } => self.source_ai_work = Some(work),
        }
        true
    }

    fn begin_source_ai_session(
        &mut self,
        app: &mut App,
        start: SourceAiStart,
        context: SourceAiContext,
    ) {
        if let Err(error) = owned_session_start_admission(self.deferred_owned_lifecycle_overflowed)
        {
            app.finish_source_ai(start.generation, Err(error.into()));
            return;
        }
        if self.source_ai_session.is_some() {
            app.finish_source_ai(
                start.generation,
                Err(
                    "the previous source-assistance session is still archiving; retry shortly"
                        .into(),
                ),
            );
            return;
        }
        let Some(host) = &self.agent else {
            app.finish_source_ai(
                start.generation,
                Err("local agent service unavailable".into()),
            );
            return;
        };
        match host.start_session_with_purpose(
            &start.provider,
            &self.cwd,
            Some(&start.mode),
            Some(&start.thinking),
            Some("lvu source definition assistance"),
            Some(SessionPurpose::SourceAssistance),
        ) {
            Ok(request) => {
                app.update_source_ai_progress(
                    start.generation,
                    SourceAiStage::Starting,
                    "starting separate source-definition session".into(),
                    None,
                );
                self.source_ai_work = Some(SourceAiWork::Starting {
                    start,
                    context,
                    request,
                    cancelled: false,
                });
            }
            Err(error) => {
                app.finish_source_ai(
                    start.generation,
                    Err(format!("start session: {}", host_error_message(error))),
                );
            }
        }
    }

    fn begin_source_ai_proposal(
        &mut self,
        app: &mut App,
        start: SourceAiStart,
        context: SourceAiContext,
        session_id: String,
    ) {
        let Some(host) = &self.agent else {
            app.finish_source_ai(
                start.generation,
                Err("local agent service unavailable".into()),
            );
            return;
        };
        match host.propose(
            &session_id,
            ProposalKind::Source,
            &start.instruction,
            context.revision.clone(),
            ProposalContext {
                inline_context: None,
                inspection_command: None,
                manifest_path: context.manifest.clone(),
                dataset_paths: Vec::new(),
            },
        ) {
            Ok(request) => {
                app.update_source_ai_progress(
                    start.generation,
                    SourceAiStage::Proposing,
                    "agent is reviewing local discovery evidence".into(),
                    Some(session_id.clone()),
                );
                self.source_ai_work = Some(SourceAiWork::Proposing {
                    start,
                    context,
                    session_id,
                    request,
                });
            }
            Err(error) => {
                app.finish_source_ai(
                    start.generation,
                    Err(format!("source proposal: {}", host_error_message(error))),
                );
                self.begin_source_ai_cancel(start.generation, session_id, app);
            }
        }
    }

    fn cancel_source_ai(&mut self, generation: u64) {
        let Some(work) = self.source_ai_work.take() else {
            if let Some((session_id, owner_generation)) = self.source_ai_session.clone()
                && owner_generation == generation
                && let Some(host) = &self.agent
            {
                match host.cancel(&session_id) {
                    Ok(request) => {
                        self.source_ai_work = Some(SourceAiWork::Cancelling {
                            generation,
                            request,
                        });
                    }
                    Err(error) => {
                        self.source_ai_work = Some(SourceAiWork::Unresolved {
                            generation,
                            diagnostic: host_error_message(error),
                        });
                    }
                }
            }
            return;
        };
        let active = source_ai_generation(&work);
        if active != generation {
            self.source_ai_work = Some(work);
            return;
        }
        match work {
            SourceAiWork::Preparing {
                start,
                cancel,
                result,
                worker,
                ..
            } => {
                cancel.cancel();
                self.source_ai_work = Some(SourceAiWork::Preparing {
                    start,
                    cancel,
                    cancelled: true,
                    result,
                    worker,
                });
            }
            SourceAiWork::Starting {
                start,
                context,
                request,
                ..
            } => {
                self.source_ai_work = Some(SourceAiWork::Starting {
                    start,
                    context,
                    request,
                    cancelled: true,
                });
            }
            SourceAiWork::Proposing { session_id, .. } => {
                if let Some(host) = &self.agent {
                    match host.cancel(&session_id) {
                        Ok(request) => {
                            self.source_ai_work = Some(SourceAiWork::Cancelling {
                                generation,
                                request,
                            })
                        }
                        Err(error) => {
                            self.source_ai_work = Some(SourceAiWork::Unresolved {
                                generation,
                                diagnostic: host_error_message(error),
                            })
                        }
                    }
                }
            }
            work @ (SourceAiWork::Cancelling { .. } | SourceAiWork::Unresolved { .. }) => {
                self.source_ai_work = Some(work)
            }
        }
    }

    fn begin_source_ai_cancel(&mut self, generation: u64, session_id: String, app: &mut App) {
        let Some(host) = &self.agent else {
            self.source_ai_work = Some(SourceAiWork::Unresolved {
                generation,
                diagnostic: "bridge unavailable during source agent cleanup".into(),
            });
            return;
        };
        match host.cancel(&session_id) {
            Ok(request) => {
                self.source_ai_work = Some(SourceAiWork::Cancelling {
                    generation,
                    request,
                })
            }
            Err(error) => {
                let message = format!("source agent cleanup: {}", host_error_message(error));
                app.finish_source_ai(generation, Err(message.clone()));
                self.source_ai_work = Some(SourceAiWork::Unresolved {
                    generation,
                    diagnostic: message,
                });
            }
        }
    }

    fn handle_ai(&mut self, app: &mut App, adapter: &NativeViewAdapter) -> bool {
        let mut changed = self.poll_session_records(app);
        for request in app.take_ask_ai_requests() {
            changed = true;
            match request {
                AskAiRequest::Start {
                    generation,
                    view_id,
                    definition_revision,
                    kind,
                    instruction,
                    provider,
                    mode,
                    thinking,
                } => {
                    if self.active_ai.is_some() || self.ai_session_busy {
                        app.finish_ask_ai(
                            generation,
                            &view_id,
                            definition_revision,
                            Err("another agent request is still settling".into()),
                        );
                        continue;
                    }
                    if let Some(error) = &self.agent_error {
                        app.finish_ask_ai(
                            generation,
                            &view_id,
                            definition_revision,
                            Err(error.clone()),
                        );
                        continue;
                    }
                    let start = AiStart {
                        generation,
                        view_id: view_id.clone(),
                        definition_revision,
                        kind,
                        instruction,
                        provider,
                        mode,
                        thinking,
                    };
                    match adapter.start_assistance_preparation(
                        &view_id,
                        self.snapshot_root.join("assistance"),
                        AssistancePreparationLimits::default(),
                    ) {
                        Ok(job) => {
                            app.update_ask_ai_progress(
                                generation,
                                AskAiStage::Snapshot,
                                "preparing typed samples from the applied view".into(),
                                None,
                                Some(job.output_dir().display().to_string()),
                            );
                            self.active_ai = Some(AiWork::Sampling {
                                start,
                                job,
                                cancelled: false,
                            });
                        }
                        Err(error) => {
                            app.finish_ask_ai(
                                generation,
                                &view_id,
                                definition_revision,
                                Err(format!("assistance preparation: {error}")),
                            );
                        }
                    }
                }
                AskAiRequest::Cancel { generation } => self.cancel_ai(generation),
            }
        }
        self.poll_agent_events(app, &mut changed);
        let Some(work) = self.active_ai.take() else {
            return changed;
        };
        match work {
            AiWork::Sampling {
                start,
                mut job,
                cancelled,
            } => {
                if cancelled {
                    if job.try_wait().is_none() {
                        self.active_ai = Some(AiWork::Sampling {
                            start,
                            job,
                            cancelled,
                        });
                    }
                    return true;
                }
                let status = job.poll();
                match status.state {
                    AssistancePreparationState::Pending | AssistancePreparationState::Running => {
                        app.update_ask_ai_progress(
                            start.generation,
                            AskAiStage::Snapshot,
                            format!(
                                "preparing typed samples: {} records scanned",
                                status.scanned_records
                            ),
                            None,
                            None,
                        );
                        self.active_ai = Some(AiWork::Sampling {
                            start,
                            job,
                            cancelled,
                        });
                    }
                    AssistancePreparationState::Complete => {
                        if app.view_definition_revision(&start.view_id)
                            != Some(start.definition_revision)
                        {
                            finish_ai_error(app, &start, "view changed while preparing assistance; submit the current definition".into());
                        } else {
                            match status
                                .result
                                .ok_or_else(|| {
                                    "assistance preparation completed without context".to_owned()
                                })
                                .and_then(|result| prepared_sample_context(&start, result))
                            {
                                Ok((output_dir, context)) => {
                                    self.begin_agent_request(app, start, output_dir, context)
                                }
                                Err(error) => finish_ai_error(app, &start, error),
                            }
                        }
                    }
                    AssistancePreparationState::Limited
                    | AssistancePreparationState::Failed
                    | AssistancePreparationState::Cancelled => finish_ai_error(
                        app,
                        &start,
                        status.diagnostic.unwrap_or_else(|| {
                            format!("assistance preparation {:?}", status.state)
                        }),
                    ),
                }
                changed = true;
            }
            AiWork::Starting {
                start,
                output_dir,
                manifest_path,
                datasets,
                inline_context,
                inspection_command,
                revision,
                request,
                cancelled,
            } => match request.try_result() {
                None => {
                    self.active_ai = Some(AiWork::Starting {
                        start,
                        output_dir,
                        manifest_path,
                        datasets,
                        inline_context,
                        inspection_command,
                        revision,
                        request,
                        cancelled,
                    });
                }
                Some(Err(error)) => {
                    finish_ai_host_error(app, &start, error);
                    changed = true;
                }
                Some(Ok(session_id)) => {
                    self.owned_ai_session = Some(session_id.clone());
                    self.owned_ai_session_config = Some(SessionConfig::from_ai(&start));
                    self.ai_session_busy = true;
                    if self.deferred_owned_lifecycle_overflowed {
                        finish_ai_error(
                            app,
                            &start,
                            "agent lifecycle reconciliation overflowed; restart lvu before starting more managed assistance".into(),
                        );
                        changed = true;
                        return changed;
                    }
                    if let Some(notice) = apply_deferred_owned_session_events(
                        &session_id,
                        &mut self.deferred_owned_lifecycle_events,
                        &mut self.owned_ai_session,
                        &mut self.owned_ai_session_config,
                        &mut self.retire_ai_session,
                        &mut self.ai_session_busy,
                        &mut self.source_ai_session,
                        &mut self.source_ai_session_config,
                    ) {
                        app.source_notice = Some(notice);
                    }
                    if self.owned_ai_session.is_none() {
                        finish_ai_error(
                            app,
                            &start,
                            "definition-assistance session archived before proposal submission"
                                .into(),
                        );
                        changed = true;
                        return changed;
                    }
                    if let Err(error) =
                        admit_session_record(&mut self.session_records, &output_dir, &session_id)
                    {
                        app.source_notice = Some(format!("agent session record error: {error}"));
                    }
                    if cancelled {
                        let _ = self.begin_cancel(session_id, None, start.generation);
                    } else {
                        self.begin_proposal(
                            app,
                            start,
                            output_dir,
                            PreparedAiContext {
                                manifest_path,
                                datasets,
                                inline_context,
                                inspection_command,
                                revision,
                            },
                            session_id,
                        );
                    }
                    changed = true;
                }
            },
            AiWork::Proposing {
                start,
                output_dir,
                session_id,
                request,
            } => match request.try_result() {
                None => {
                    self.active_ai = Some(AiWork::Proposing {
                        start,
                        output_dir,
                        session_id,
                        request,
                    });
                }
                Some(Err(error)) => {
                    let message = host_error_message(error);
                    if let Err(cleanup) = self.begin_cancel(
                        session_id,
                        Some((start.clone(), format!("local agent service: {message}"))),
                        start.generation,
                    ) {
                        finish_ai_error(
                            app,
                            &start,
                            format!("local agent service: {message}; {cleanup}"),
                        );
                    }
                    changed = true;
                }
                Some(Ok(proposal)) => {
                    let expression = if start.kind == AskAiKind::Recipe {
                        let expected = app.view_source_ids(&start.view_id);
                        if expected.is_empty() {
                            Err("adaptation view is no longer available".into())
                        } else {
                            validate_recipe_proposal_source(&proposal, &expected)
                                .and_then(|()| proposal_expression(start.kind, &proposal))
                        }
                    } else {
                        proposal_expression(start.kind, &proposal)
                    };
                    if start.kind == AskAiKind::Recipe {
                        let result = expression.and_then(|value| {
                            proposal_recipe_enrichments(&proposal)
                                .map(|chain| (value, proposal.explanation.clone(), chain))
                        });
                        app.finish_recipe_ai(
                            start.generation,
                            &start.view_id,
                            start.definition_revision,
                            result,
                        );
                    } else {
                        app.finish_ask_ai(
                            start.generation,
                            &start.view_id,
                            start.definition_revision,
                            expression.map(|value| (value, proposal.explanation)),
                        );
                    }
                    changed = true;
                }
            },
            AiWork::Cancelling {
                generation,
                session_id,
                request,
                failure,
            } => match request.try_result() {
                None => {
                    self.active_ai = Some(AiWork::Cancelling {
                        generation,
                        session_id,
                        request,
                        failure,
                    });
                }
                Some(Ok(result)) => {
                    match validate_remote_cancellation(&result) {
                        Ok(()) => {
                            self.ai_session_busy = false;
                            if self.retire_ai_session
                                && self.owned_ai_session.as_deref() == Some(&session_id)
                            {
                                self.owned_ai_session = None;
                                self.owned_ai_session_config = None;
                            }
                            self.retire_ai_session = false;
                            if let Some((start, message)) = failure {
                                finish_ai_error(app, &start, message);
                            }
                        }
                        Err(error) => {
                            self.agent_error = Some(error.clone());
                            if let Some((start, message)) = failure {
                                finish_ai_error(app, &start, format!("{message}; {error}"));
                            } else {
                                app.source_notice = Some(format!("agent session cleanup: {error}"));
                            }
                        }
                    }
                    changed = true;
                }
                Some(Err(error)) => {
                    let cleanup =
                        format!("session cancellation failed: {}", host_error_message(error));
                    self.agent_error = Some(cleanup.clone());
                    if let Some((start, message)) = failure {
                        finish_ai_error(app, &start, format!("{message}; session cleanup failed"));
                    } else {
                        app.source_notice = Some(format!("agent session cleanup: {cleanup}"));
                    }
                    changed = true;
                }
            },
        }
        changed
    }

    fn handle_investigation(&mut self, app: &mut App, adapter: &NativeViewAdapter) -> bool {
        let mut changed = self.poll_investigation_load(app);
        if let Some((generation, item, event_floor)) =
            investigation_observation(self.investigation_work.as_ref())
        {
            let failure = match &self.agent {
                Some(host) => {
                    let status = host.status();
                    investigation_health_failure(status.state, status.dropped_events, event_floor)
                }
                None => Some("agent bridge unavailable while waiting for the turn".into()),
            };
            if let Some(failure) = failure {
                finish_investigation_error(app, generation, &failure);
                self.start_investigation_cancel(generation, item);
                changed = true;
            }
        }
        for request in app.take_investigation_requests() {
            changed = true;
            match request {
                InvestigationRequest::Start {
                    generation,
                    view_id,
                    definition_revision,
                    question,
                    provider,
                    mode,
                    thinking,
                } => {
                    if self.investigation_work.is_some() {
                        finish_investigation_error(
                            app,
                            generation,
                            "another investigation turn is active",
                        );
                        continue;
                    }
                    if let Some(error) = &self.agent_error {
                        finish_investigation_error(app, generation, error);
                        continue;
                    }
                    let start = InvestigationStart {
                        generation,
                        view_id: view_id.clone(),
                        definition_revision,
                        question,
                        provider,
                        mode,
                        thinking,
                    };
                    let limits = SnapshotLimits {
                        maximum_rows: 50_000,
                        maximum_input_bytes: 512 * 1024 * 1024,
                        maximum_disk_bytes: 512 * 1024 * 1024,
                        maximum_parts: 512,
                        ..SnapshotLimits::default()
                    };
                    match adapter.start_snapshot(&view_id, &self.snapshot_root, limits) {
                        Ok(job) => {
                            app.update_investigation_progress(
                                generation,
                                InvestigationStage::Snapshot,
                                "exporting fixed applied view".into(),
                                None,
                                Some(job.output_dir().display().to_string()),
                                None,
                            );
                            self.investigation_work =
                                Some(InvestigationWork::Snapshot { start, job });
                        }
                        Err(error) => finish_investigation_error(
                            app,
                            generation,
                            &format!("snapshot: {error}"),
                        ),
                    }
                }
                InvestigationRequest::Resume { generation, item } => {
                    if self.investigation_work.is_some() {
                        finish_investigation_error(
                            app,
                            generation,
                            "another investigation turn is active",
                        );
                        continue;
                    }
                    let Some(host) = &self.agent else {
                        finish_investigation_error(
                            app,
                            generation,
                            "local agent service unavailable",
                        );
                        continue;
                    };
                    match host.resume_session_with_purpose(
                        &item.session_id,
                        Some(SessionPurpose::Investigation),
                    ) {
                        Ok(request) => {
                            app.update_investigation_progress(
                                generation,
                                InvestigationStage::Resuming,
                                "resuming local agent session".into(),
                                Some(item.session_id.clone()),
                                Some(item.snapshot_dir.clone()),
                                Some(item.manifest_path.clone()),
                            );
                            self.investigation_work = Some(InvestigationWork::Resuming {
                                generation,
                                item,
                                request,
                                cancelled: false,
                            });
                        }
                        Err(error) => finish_investigation_error(
                            app,
                            generation,
                            &format!("resume: {}", host_error_message(error)),
                        ),
                    }
                }
                InvestigationRequest::Send {
                    generation,
                    session_id,
                    prompt,
                } => {
                    if self.investigation_work.is_some() {
                        finish_investigation_error(
                            app,
                            generation,
                            "another investigation turn or unresolved cleanup is active",
                        );
                        continue;
                    }
                    let Some(item) = self
                        .investigation_session
                        .clone()
                        .filter(|item| item.session_id == session_id)
                    else {
                        finish_investigation_error(
                            app,
                            generation,
                            "selected session is unavailable",
                        );
                        continue;
                    };
                    self.start_investigation_prompt(app, generation, item, prompt);
                }
                InvestigationRequest::Cancel { generation } => {
                    self.cancel_investigation(generation);
                }
            }
        }
        let Some(work) = self.investigation_work.take() else {
            return changed;
        };
        match work {
            InvestigationWork::Snapshot { start, job } => match job.poll().state {
                SnapshotState::Pending | SnapshotState::Running => {
                    self.investigation_work = Some(InvestigationWork::Snapshot { start, job });
                }
                SnapshotState::Complete => {
                    let status = job.poll();
                    let output_dir = job.output_dir().to_path_buf();
                    let manifest = status
                        .manifest_path
                        .unwrap_or_else(|| output_dir.join("manifest.json"));
                    let (result, worker) = prepare_ai_context(
                        output_dir.clone(),
                        manifest,
                        start.view_id.clone(),
                        start.definition_revision,
                    );
                    app.update_investigation_progress(
                        start.generation,
                        InvestigationStage::Snapshot,
                        "preparing absolute snapshot context".into(),
                        None,
                        None,
                        None,
                    );
                    self.investigation_work = Some(InvestigationWork::Preparing {
                        start,
                        output_dir,
                        cancelled: false,
                        result,
                        worker,
                    });
                }
                state => finish_investigation_error(
                    app,
                    start.generation,
                    &format!("snapshot {state:?}"),
                ),
            },
            InvestigationWork::Preparing {
                start,
                output_dir,
                cancelled,
                result,
                worker,
            } => match result.try_recv() {
                Err(std_mpsc::TryRecvError::Empty) => {
                    self.investigation_work = Some(InvestigationWork::Preparing {
                        start,
                        output_dir,
                        cancelled,
                        result,
                        worker,
                    });
                }
                Err(std_mpsc::TryRecvError::Disconnected) => {
                    let _ = worker.join();
                    if !cancelled {
                        finish_investigation_error(
                            app,
                            start.generation,
                            "snapshot worker disconnected",
                        );
                    }
                }
                Ok(Err(error)) => {
                    let _ = worker.join();
                    if !cancelled {
                        finish_investigation_error(app, start.generation, &error);
                    }
                }
                Ok(Ok(context)) => {
                    let _ = worker.join();
                    if cancelled {
                        return true;
                    }
                    let Some(host) = &self.agent else {
                        finish_investigation_error(
                            app,
                            start.generation,
                            "local agent service unavailable",
                        );
                        return true;
                    };
                    match host.start_session_with_purpose(
                        &start.provider,
                        &output_dir,
                        Some(&start.mode),
                        Some(&start.thinking),
                        Some("lvu investigation"),
                        Some(SessionPurpose::Investigation),
                    ) {
                        Ok(request) => {
                            app.update_investigation_progress(
                                start.generation,
                                InvestigationStage::StartingSession,
                                "starting separate local investigation session".into(),
                                None,
                                None,
                                Some(context.manifest_path.display().to_string()),
                            );
                            self.investigation_work = Some(InvestigationWork::Starting {
                                start,
                                output_dir,
                                context,
                                request,
                                cancelled: false,
                            });
                        }
                        Err(error) => finish_investigation_error(
                            app,
                            start.generation,
                            &format!("start session: {}", host_error_message(error)),
                        ),
                    }
                }
            },
            InvestigationWork::Starting {
                start,
                output_dir,
                context,
                request,
                cancelled,
            } => match request.try_result() {
                None => {
                    self.investigation_work = Some(InvestigationWork::Starting {
                        start,
                        output_dir,
                        context,
                        request,
                        cancelled,
                    });
                }
                Some(Err(error)) => finish_investigation_error(
                    app,
                    start.generation,
                    &format!("start session: {}", host_error_message(error)),
                ),
                Some(Ok(session_id)) => {
                    let item = InvestigationItem {
                        id: Uuid::new_v4().to_string(),
                        view_id: start.view_id,
                        session_id: session_id.clone(),
                        snapshot_dir: output_dir.display().to_string(),
                        manifest_path: context.manifest_path.display().to_string(),
                        question: start.question.clone(),
                    };
                    self.investigation_session = Some(item.clone());
                    if let Err(error) =
                        admit_investigation_record(&mut self.session_records, &output_dir, &item)
                    {
                        app.source_notice = Some(format!("investigation record error: {error}"));
                    }
                    if cancelled {
                        self.start_investigation_cancel(start.generation, item);
                    } else {
                        app.investigation_ready(start.generation, item.clone());
                        let prompt = investigation_prompt(&start.question, &context);
                        self.start_investigation_prompt(app, start.generation, item, prompt);
                    }
                }
            },
            InvestigationWork::Resuming {
                generation,
                item,
                request,
                cancelled,
            } => match request.try_result() {
                None => {
                    self.investigation_work = Some(InvestigationWork::Resuming {
                        generation,
                        item,
                        request,
                        cancelled,
                    });
                }
                Some(Err(error)) => {
                    finish_investigation_error(
                        app,
                        generation,
                        &format!(
                            "resume: {}; verifying remote session cleanup",
                            host_error_message(error)
                        ),
                    );
                    self.start_investigation_cancel(generation, item);
                }
                Some(Ok(session_id)) if session_id == item.session_id => {
                    self.investigation_session = Some(item.clone());
                    if cancelled {
                        self.start_investigation_cancel(generation, item);
                    } else {
                        app.investigation_ready(generation, item);
                        app.update_investigation_progress(
                            generation,
                            InvestigationStage::Conversation,
                            "session resumed; enter a follow-up (no prompt sent automatically)"
                                .into(),
                            Some(session_id),
                            None,
                            None,
                        );
                    }
                }
                Some(Ok(_)) => {
                    finish_investigation_error(
                        app,
                        generation,
                        "resume returned a different session; cancelling selected session",
                    );
                    self.start_investigation_cancel(generation, item);
                }
            },
            InvestigationWork::Sending {
                generation,
                item,
                request,
                event_floor,
                turn_started,
            } => match request.try_result() {
                None => {
                    self.investigation_work = Some(InvestigationWork::Sending {
                        generation,
                        item,
                        request,
                        event_floor,
                        turn_started,
                    });
                }
                Some(Err(error)) => {
                    finish_investigation_error(
                        app,
                        generation,
                        &format!(
                            "send prompt: {}; cancelling owned session",
                            host_error_message(error)
                        ),
                    );
                    self.start_investigation_cancel(generation, item);
                }
                Some(Ok(_)) => {
                    self.investigation_work = Some(InvestigationWork::Watching {
                        generation,
                        item,
                        event_floor,
                        turn_started,
                    });
                }
            },
            work @ InvestigationWork::Watching { .. } => {
                self.investigation_work = Some(work);
            }
            InvestigationWork::Cancelling {
                generation,
                item,
                request,
            } => match request.try_result() {
                None => {
                    self.investigation_work = Some(InvestigationWork::Cancelling {
                        generation,
                        item,
                        request,
                    });
                }
                Some(Ok(value)) => {
                    if let Err(error) = validate_remote_cancellation(&value) {
                        app.source_notice = Some(format!("investigation cleanup: {error}"));
                        self.investigation_session = Some(item.clone());
                        self.investigation_work = Some(InvestigationWork::Unresolved {
                            generation,
                            item,
                            diagnostic: error,
                        });
                    }
                }
                Some(Err(error)) => {
                    let diagnostic =
                        format!("investigation cleanup: {}", host_error_message(error));
                    app.source_notice = Some(diagnostic.clone());
                    self.investigation_session = Some(item.clone());
                    self.investigation_work = Some(InvestigationWork::Unresolved {
                        generation,
                        item,
                        diagnostic,
                    });
                }
            },
            work @ InvestigationWork::Unresolved { .. } => {
                self.investigation_work = Some(work);
            }
        }
        true
    }

    fn start_investigation_prompt(
        &mut self,
        app: &mut App,
        generation: u64,
        item: InvestigationItem,
        prompt: String,
    ) {
        let Some(host) = &self.agent else {
            finish_investigation_error(app, generation, "local agent service unavailable");
            self.investigation_session = Some(item.clone());
            self.investigation_work = Some(InvestigationWork::Unresolved {
                generation,
                item,
                diagnostic: "local agent service unavailable; remote session ownership unresolved"
                    .into(),
            });
            return;
        };
        let event_floor = host.status().dropped_events;
        let submitted = host.send_prompt(&item.session_id, &prompt);
        match submitted {
            Ok(request) => {
                app.update_investigation_progress(
                    generation,
                    InvestigationStage::Sending,
                    "sending context to local agent".into(),
                    Some(item.session_id.clone()),
                    Some(item.snapshot_dir.clone()),
                    Some(item.manifest_path.clone()),
                );
                self.investigation_work = Some(InvestigationWork::Sending {
                    generation,
                    item,
                    request,
                    event_floor,
                    turn_started: false,
                });
            }
            Err(error) => {
                finish_investigation_error(
                    app,
                    generation,
                    &format!(
                        "send prompt: {}; cancelling owned session",
                        host_error_message(error)
                    ),
                );
                self.start_investigation_cancel(generation, item);
            }
        }
    }

    fn cancel_investigation(&mut self, generation: u64) {
        let Some(work) = self.investigation_work.take() else {
            return;
        };
        if investigation_generation(&work) != generation {
            self.investigation_work = Some(work);
            return;
        }
        match work {
            InvestigationWork::Snapshot { job, .. } => job.cancel(),
            InvestigationWork::Preparing {
                start,
                output_dir,
                result,
                worker,
                ..
            } => {
                self.investigation_work = Some(InvestigationWork::Preparing {
                    start,
                    output_dir,
                    cancelled: true,
                    result,
                    worker,
                });
            }
            InvestigationWork::Starting {
                start,
                output_dir,
                context,
                request,
                ..
            } => {
                self.investigation_work = Some(InvestigationWork::Starting {
                    start,
                    output_dir,
                    context,
                    request,
                    cancelled: true,
                });
            }
            InvestigationWork::Resuming {
                item,
                request,
                generation,
                ..
            } => {
                self.investigation_work = Some(InvestigationWork::Resuming {
                    generation,
                    item,
                    request,
                    cancelled: true,
                });
            }
            InvestigationWork::Sending { item, .. } | InvestigationWork::Watching { item, .. } => {
                self.start_investigation_cancel(generation, item);
            }
            work @ InvestigationWork::Cancelling { .. }
            | work @ InvestigationWork::Unresolved { .. } => {
                self.investigation_work = Some(work);
            }
        }
    }

    fn start_investigation_cancel(&mut self, generation: u64, item: InvestigationItem) {
        let Some(host) = &self.agent else {
            let diagnostic =
                "investigation cancellation unavailable; remote session ownership unresolved"
                    .to_owned();
            self.agent_error = Some(diagnostic.clone());
            self.investigation_session = Some(item.clone());
            self.investigation_work = Some(InvestigationWork::Unresolved {
                generation,
                item,
                diagnostic,
            });
            return;
        };
        match host.cancel(&item.session_id) {
            Ok(request) => {
                self.investigation_work = Some(InvestigationWork::Cancelling {
                    generation,
                    item,
                    request,
                });
            }
            Err(error) => {
                let diagnostic = format!(
                    "investigation cancellation failed: {}",
                    host_error_message(error)
                );
                self.agent_error = Some(diagnostic.clone());
                self.investigation_work = Some(InvestigationWork::Unresolved {
                    generation,
                    item,
                    diagnostic,
                });
            }
        }
    }

    fn poll_investigation_load(&mut self, app: &mut App) -> bool {
        let Some(job) = &mut self.investigation_load else {
            return false;
        };
        match job.result.try_recv() {
            Err(std_mpsc::TryRecvError::Empty) => false,
            result => {
                let mut job = self.investigation_load.take().expect("load exists");
                if let Some(worker) = job.worker.take() {
                    let _ = worker.join();
                }
                match result {
                    Ok(Ok(loaded)) => {
                        app.set_investigations(loaded.items);
                        if let Some(diagnostic) = loaded.diagnostic {
                            app.source_notice = Some(diagnostic);
                        }
                    }
                    Ok(Err(error)) => {
                        app.source_notice = Some(format!("investigation load error: {error}"));
                    }
                    Err(_) => {
                        app.source_notice = Some("investigation load worker disconnected".into());
                    }
                }
                true
            }
        }
    }

    fn cancel_ai(&mut self, generation: u64) {
        if self
            .active_ai
            .as_ref()
            .is_some_and(|work| ai_generation(work) == generation)
        {
            let work = self.active_ai.take().expect("active agent checked above");
            match work {
                AiWork::Sampling { start, job, .. } => {
                    job.cancel();
                    self.active_ai = Some(AiWork::Sampling {
                        start,
                        job,
                        cancelled: true,
                    });
                }
                AiWork::Starting {
                    start,
                    output_dir,
                    manifest_path,
                    datasets,
                    inline_context,
                    inspection_command,
                    revision,
                    request,
                    ..
                } => {
                    self.active_ai = Some(AiWork::Starting {
                        start,
                        output_dir,
                        manifest_path,
                        datasets,
                        inline_context,
                        inspection_command,
                        revision,
                        request,
                        cancelled: true,
                    });
                }
                AiWork::Proposing { session_id, .. } => {
                    let _ = self.begin_cancel(session_id, None, generation);
                }
                work @ AiWork::Cancelling { .. } => self.active_ai = Some(work),
            }
        } else if self.active_ai.is_none()
            && !self.ai_session_busy
            && let Some(session_id) = self.owned_ai_session.clone()
        {
            let _ = self.begin_cancel(session_id, None, generation);
        }
    }

    fn poll_agent_events(&mut self, app: &mut App, changed: &mut bool) {
        for _ in 0..16 {
            let Some(event) = self.agent.as_ref().and_then(AgentBridgeHost::poll_event) else {
                break;
            };
            *changed = true;
            if investigation_session_id(self.investigation_work.as_ref())
                == Some(event.session_id.as_str())
            {
                let generation = investigation_generation(
                    self.investigation_work
                        .as_ref()
                        .expect("investigation session matched"),
                );
                let active_turn = matches!(
                    self.investigation_work,
                    Some(InvestigationWork::Sending { .. } | InvestigationWork::Watching { .. })
                );
                let turn_started = investigation_turn_started(self.investigation_work.as_ref());
                if event.kind == "turn_completed" && active_turn && turn_started {
                    let message = event
                        .payload
                        .pointer("/payload/last_message")
                        .or_else(|| event.payload.get("last_message"))
                        .and_then(serde_json::Value::as_str)
                        .filter(|message| !message.trim().is_empty())
                        .unwrap_or("agent completed without a text response")
                        .trim()
                        .to_owned();
                    app.push_investigation_event(
                        &event.session_id,
                        format!("Agent: {message}"),
                        Ok(()),
                    );
                    self.investigation_work = None;
                } else if event.kind == "turn_failed" && active_turn && turn_started {
                    let error = event
                        .payload
                        .pointer("/payload/error")
                        .or_else(|| event.payload.get("error"))
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("agent turn failed")
                        .to_owned();
                    app.push_investigation_event(&event.session_id, String::new(), Err(error));
                    if event
                        .payload
                        .pointer("/payload/remote_agent_may_still_be_running")
                        .and_then(serde_json::Value::as_bool)
                        == Some(true)
                    {
                        if let Some(item) = self.investigation_session.clone() {
                            self.start_investigation_cancel(generation, item);
                        }
                    } else {
                        self.investigation_work = None;
                    }
                } else if event.kind.contains("permission") && active_turn {
                    app.update_investigation_progress(
                        generation,
                        InvestigationStage::Sending,
                        "agent is waiting for a local permission decision".into(),
                        Some(event.session_id),
                        None,
                        None,
                    );
                } else if event.kind == "turn_started" && active_turn {
                    mark_investigation_turn_started(self.investigation_work.as_mut());
                    app.update_investigation_progress(
                        generation,
                        InvestigationStage::Sending,
                        "local agent is exploring the fixed snapshot".into(),
                        Some(event.session_id),
                        None,
                        None,
                    );
                } else if let Some(message) = event
                    .payload
                    .pointer("/payload/message")
                    .or_else(|| event.payload.pointer("/payload/delta"))
                    .and_then(serde_json::Value::as_str)
                {
                    app.append_investigation_output(&event.session_id, format!("Agent: {message}"));
                }
                continue;
            }
            let registration_pending =
                matches!(self.active_ai.as_ref(), Some(AiWork::Starting { .. }))
                    || matches!(
                        self.source_ai_work.as_ref(),
                        Some(SourceAiWork::Starting { .. })
                    );
            if let Some(notice) = apply_or_defer_owned_session_event(
                &event,
                &mut self.deferred_owned_lifecycle_events,
                &mut self.deferred_owned_lifecycle_overflowed,
                registration_pending,
                &mut self.owned_ai_session,
                &mut self.owned_ai_session_config,
                &mut self.retire_ai_session,
                &mut self.ai_session_busy,
                &mut self.source_ai_session,
                &mut self.source_ai_session_config,
            ) {
                app.source_notice = Some(notice);
                continue;
            }
            if event.kind.contains("permission")
                && let Some(work) = &self.active_ai
            {
                app.update_ask_ai_progress(
                    ai_generation(work),
                    AskAiStage::Proposing,
                    "agent is waiting for a local permission decision".into(),
                    Some(event.session_id),
                    None,
                );
            }
        }
    }

    fn begin_agent_request(
        &mut self,
        app: &mut App,
        start: AiStart,
        output_dir: PathBuf,
        context: PreparedAiContext,
    ) {
        if let Err(error) = owned_session_start_admission(self.deferred_owned_lifecycle_overflowed)
        {
            finish_ai_error(app, &start, error.into());
            return;
        }
        if self.owned_ai_session.is_some() {
            finish_ai_error(
                app,
                &start,
                "the previous definition-assistance session is still archiving; retry shortly"
                    .into(),
            );
            return;
        }
        let Some(host) = &self.agent else {
            finish_ai_error(app, &start, "local agent service unavailable".into());
            return;
        };
        match host.start_session_with_purpose(
            &start.provider,
            &output_dir,
            Some(&start.mode),
            Some(&start.thinking),
            Some("lvu Ask agent"),
            Some(SessionPurpose::Ask),
        ) {
            Ok(request) => {
                app.update_ask_ai_progress(
                    start.generation,
                    AskAiStage::StartingSession,
                    "starting local agent session".into(),
                    None,
                    None,
                );
                self.active_ai = Some(AiWork::Starting {
                    start,
                    output_dir,
                    manifest_path: context.manifest_path,
                    datasets: context.datasets,
                    inline_context: context.inline_context,
                    inspection_command: context.inspection_command,
                    revision: context.revision,
                    request,
                    cancelled: false,
                });
            }
            Err(error) => finish_ai_host_error(app, &start, error),
        }
    }

    fn begin_proposal(
        &mut self,
        app: &mut App,
        start: AiStart,
        output_dir: PathBuf,
        context: PreparedAiContext,
        session_id: String,
    ) {
        let kind = match start.kind {
            AskAiKind::Filter => ProposalKind::Filter,
            AskAiKind::Enrichment => ProposalKind::Enrichment,
            AskAiKind::Recipe => ProposalKind::View,
        };
        let Some(host) = &self.agent else {
            self.ai_session_busy = false;
            finish_ai_error(app, &start, "local agent service unavailable".into());
            return;
        };
        match host.propose(
            &session_id,
            kind,
            &start.instruction,
            context.revision,
            ProposalContext {
                inline_context: context.inline_context,
                inspection_command: context.inspection_command,
                manifest_path: context.manifest_path,
                dataset_paths: context.datasets,
            },
        ) {
            Ok(request) => {
                app.update_ask_ai_progress(
                    start.generation,
                    AskAiStage::Proposing,
                    "agent is inspecting the fixed snapshot".into(),
                    Some(session_id.clone()),
                    None,
                );
                self.active_ai = Some(AiWork::Proposing {
                    start,
                    output_dir,
                    session_id,
                    request,
                });
            }
            Err(error) => {
                let message = host_error_message(error);
                if let Err(cleanup) = self.begin_cancel(
                    session_id,
                    Some((start.clone(), message.clone())),
                    start.generation,
                ) {
                    finish_ai_error(app, &start, format!("{message}; {cleanup}"));
                }
            }
        }
    }

    fn begin_cancel(
        &mut self,
        session_id: String,
        failure: Option<(AiStart, String)>,
        generation: u64,
    ) -> Result<(), String> {
        self.ai_session_busy = true;
        let Some(host) = &self.agent else {
            self.agent_error = Some("bridge unavailable during session cleanup".into());
            return Err("session cleanup unavailable".into());
        };
        match host.cancel(&session_id) {
            Ok(request) => {
                self.active_ai = Some(AiWork::Cancelling {
                    generation,
                    session_id,
                    request,
                    failure,
                });
                Ok(())
            }
            Err(error) => {
                let cleanup = host_error_message(error);
                self.agent_error = Some(format!("session cancellation failed: {cleanup}"));
                // No public protocol exists to destroy one session. Keep it
                // marked busy so shutdown must settle the bridge process.
                self.active_ai = None;
                Err(format!("session cleanup failed: {cleanup}"))
            }
        }
    }

    fn poll_session_records(&mut self, app: &mut App) -> bool {
        let mut changed = false;
        let mut index = 0;
        while index < self.session_records.len() {
            match self.session_records[index].result.try_recv() {
                Ok(result) => {
                    let mut job = self.session_records.swap_remove(index);
                    if let Some(worker) = job.worker.take() {
                        let _ = worker.join();
                    }
                    if let Err(error) = result {
                        app.source_notice = Some(format!("agent session record error: {error}"));
                    }
                    changed = true;
                }
                Err(std_mpsc::TryRecvError::Disconnected) => {
                    let mut job = self.session_records.swap_remove(index);
                    if let Some(worker) = job.worker.take() {
                        let _ = worker.join();
                    }
                    app.source_notice = Some("agent session record worker disconnected".into());
                    changed = true;
                }
                Err(std_mpsc::TryRecvError::Empty) => index += 1,
            }
        }
        changed
    }

    fn shutdown_ai(&mut self, timeout: Duration) -> Result<(), String> {
        let deadline = std::time::Instant::now() + timeout;
        let mut failures = settle_ai_work(
            self.active_ai.take(),
            self.owned_ai_session.clone(),
            self.agent.as_ref(),
            deadline,
        );
        for mut job in self.session_records.drain(..) {
            match job.result.recv_timeout(remaining(deadline)) {
                Ok(Ok(())) => {
                    if let Some(worker) = job.worker.take() {
                        let _ = worker.join();
                    }
                }
                Ok(result) => {
                    if let Some(worker) = job.worker.take() {
                        let _ = worker.join();
                    }
                    if let Err(error) = result {
                        failures.push(error);
                    }
                }
                Err(_) => failures.push("agent session record did not finish".into()),
            }
        }
        if let Some(host) = &self.agent
            && let Err(error) = host.shutdown()
        {
            failures.push(format!(
                "agent bridge shutdown: {}",
                host_error_message(error)
            ));
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures.join("; "))
        }
    }

    fn shutdown_source_ai(&mut self, timeout: Duration) -> Result<(), String> {
        let deadline = std::time::Instant::now() + timeout;
        let mut failures = Vec::new();
        let mut session = None;
        let mut cancelled = false;
        if let Some(work) = self.source_ai_work.take() {
            match work {
                SourceAiWork::Preparing {
                    cancel,
                    result,
                    worker,
                    ..
                } => {
                    cancel.cancel();
                    match result.recv_timeout(remaining(deadline)) {
                        Ok(_) | Err(std_mpsc::RecvTimeoutError::Disconnected) => {
                            if worker.join().is_err() {
                                failures.push("source agent context worker panicked".into());
                            }
                        }
                        Err(std_mpsc::RecvTimeoutError::Timeout) => {
                            failures.push(
                                "source agent context worker did not stop before deadline".into(),
                            );
                        }
                    }
                }
                SourceAiWork::Starting { request, .. } => {
                    match request.recv_timeout(remaining(deadline)) {
                        Ok(id) => session = Some(id),
                        Err(error) => failures.push(format!(
                            "source agent session start unresolved: {}",
                            host_error_message(error)
                        )),
                    }
                }
                SourceAiWork::Proposing { session_id, .. } => session = Some(session_id),
                SourceAiWork::Cancelling { request, .. } => {
                    cancelled = true;
                    match request.recv_timeout(remaining(deadline)) {
                        Ok(value) => {
                            if let Err(error) = validate_remote_cancellation(&value) {
                                failures.push(error);
                            }
                        }
                        Err(error) => failures.push(format!(
                            "source agent cancellation unresolved: {}",
                            host_error_message(error)
                        )),
                    }
                }
                SourceAiWork::Unresolved { diagnostic, .. } => failures.push(diagnostic),
            }
        }
        if session.is_none() && !cancelled {
            session = self
                .source_ai_session
                .as_ref()
                .map(|(session_id, _)| session_id.clone());
        }
        if let Some(session_id) = session {
            match self.agent.as_ref().map(|host| host.cancel(&session_id)) {
                Some(Ok(request)) => match request.recv_timeout(remaining(deadline)) {
                    Ok(value) => {
                        if let Err(error) = validate_remote_cancellation(&value) {
                            failures.push(error);
                        }
                    }
                    Err(error) => failures.push(format!(
                        "source agent cancellation unresolved: {}",
                        host_error_message(error)
                    )),
                },
                Some(Err(error)) => failures.push(format!(
                    "source agent cancellation could not start: {}",
                    host_error_message(error)
                )),
                None => failures.push("source agent cleanup unavailable".into()),
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures.join("; "))
        }
    }

    fn shutdown_investigation(&mut self, timeout: Duration) -> Result<(), String> {
        let deadline = std::time::Instant::now() + timeout;
        let mut failures = Vec::new();
        let mut session = None;
        let mut cancellation_done = false;
        if let Some(work) = self.investigation_work.take() {
            match work {
                InvestigationWork::Snapshot { job, .. } => job.cancel(),
                InvestigationWork::Preparing { result, worker, .. } => {
                    if result.recv_timeout(remaining(deadline)).is_ok() {
                        let _ = worker.join();
                    } else {
                        failures.push("investigation snapshot worker did not stop".into());
                    }
                }
                InvestigationWork::Starting { request, .. } => {
                    match request.recv_timeout(remaining(deadline)) {
                        Ok(session_id) => session = Some(session_id),
                        Err(error) => failures.push(format!(
                            "investigation session start unresolved: {}",
                            host_error_message(error)
                        )),
                    }
                }
                InvestigationWork::Resuming { item, request, .. } => {
                    match request.recv_timeout(remaining(deadline)) {
                        Ok(session_id) if session_id == item.session_id => {
                            session = Some(session_id);
                        }
                        Ok(session_id) => {
                            session = Some(session_id);
                            failures.push(
                                "investigation resume returned a different session during shutdown"
                                    .into(),
                            );
                        }
                        Err(error) => {
                            session = Some(item.session_id);
                            failures.push(format!(
                                "investigation resume unresolved: {}",
                                host_error_message(error)
                            ));
                        }
                    }
                }
                InvestigationWork::Sending { item, .. }
                | InvestigationWork::Watching { item, .. } => session = Some(item.session_id),
                InvestigationWork::Cancelling { request, .. } => {
                    cancellation_done = true;
                    match request.recv_timeout(remaining(deadline)) {
                        Ok(value) => {
                            if let Err(error) = validate_remote_cancellation(&value) {
                                failures.push(error);
                            }
                        }
                        Err(error) => failures.push(format!(
                            "investigation cancellation unresolved: {}",
                            host_error_message(error)
                        )),
                    }
                }
                InvestigationWork::Unresolved {
                    item, diagnostic, ..
                } => {
                    session = Some(item.session_id);
                    failures.push(format!("retrying unresolved cleanup: {diagnostic}"));
                }
            }
        }
        if session.is_none() && !cancellation_done {
            session = self
                .investigation_session
                .as_ref()
                .map(|item| item.session_id.clone());
        }
        if let Some(session_id) = session {
            match self.agent.as_ref().map(|host| host.cancel(&session_id)) {
                Some(Ok(request)) => match request.recv_timeout(remaining(deadline)) {
                    Ok(value) => {
                        if let Err(error) = validate_remote_cancellation(&value) {
                            failures.push(error);
                        }
                    }
                    Err(error) => failures.push(format!(
                        "investigation cancellation unresolved: {}",
                        host_error_message(error)
                    )),
                },
                Some(Err(error)) => failures.push(format!(
                    "investigation cancellation could not start: {}",
                    host_error_message(error)
                )),
                None => failures.push("investigation session cleanup unavailable".into()),
            }
        }
        if let Some(mut load) = self.investigation_load.take() {
            match load.result.recv_timeout(remaining(deadline)) {
                Ok(result) => {
                    if let Some(worker) = load.worker.take() {
                        let _ = worker.join();
                    }
                    if let Err(error) = result {
                        failures.push(error);
                    }
                }
                Err(_) => failures.push("investigation metadata load did not finish".into()),
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures.join("; "))
        }
    }

    fn handle_source_controls(&mut self, app: &mut App, adapter: &NativeViewAdapter) -> bool {
        let mut changed = false;
        let mut completed = Vec::new();
        for (id, job) in &mut self.source_controls {
            // Let submissions against the old, still-pageable handle settle
            // before replacement cancels its query worker token.
            if job.restart
                && app.views().iter().any(|view| {
                    view.source_id == id.0.to_string() && app.view_has_pending_query(&view.id)
                })
            {
                continue;
            }
            let result = match job.result.try_recv() {
                Ok(result) => result,
                Err(tokio::sync::oneshot::error::TryRecvError::Empty) => continue,
                Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                    Err("source control worker disconnected".into())
                }
            };
            completed.push((*id, result));
        }
        for (id, result) in completed {
            changed = true;
            self.source_controls.remove(&id);
            let name = self
                .definitions
                .get(&id)
                .map_or_else(|| id.0.to_string(), |definition| definition.name.clone());
            app.action_notice = Some(match result {
                Ok(Some(handle)) => match adapter.register_source(handle.clone()) {
                    Ok(()) => format!("{name}: capture restarted; existing views retained"),
                    Err(error) => {
                        // A launched capture remains manager-owned until graceful cleanup settles.
                        let (sender, result) = tokio::sync::oneshot::channel();
                        let message = format!("register restarted source: {error}");
                        let worker = self.runtime.spawn(async move {
                            let stopped = handle.stop().await;
                            let diagnostic = match stopped {
                                Ok(report) if report.complete => message,
                                other => format!("{message}; cleanup incomplete: {other:?}"),
                            };
                            let _ = sender.send(Err(diagnostic));
                        });
                        self.source_controls.insert(
                            id,
                            SourceControlJob {
                                restart: false,
                                result,
                                worker,
                            },
                        );
                        format!("{name}: restart registration failed; stopping capture")
                    }
                },
                Ok(None) => format!("{name}: capture stopped; journal and views retained"),
                Err(error) => format!("{name}: {error}"),
            });
        }
        for request in app.take_source_controls() {
            changed = true;
            let id = match Uuid::parse_str(&request.source_id) {
                Ok(id) => SourceId(id),
                Err(_) => {
                    app.action_notice = Some("source control unavailable for this view".into());
                    continue;
                }
            };
            if self.source_controls.contains_key(&id) || self.pending_starts.contains(&id) {
                app.action_notice = Some("source operation already pending".into());
                continue;
            }
            if self.source_controls.len() >= 8 {
                app.action_notice =
                    Some("source control limit reached; wait for pending operations".into());
                continue;
            }
            let Some(definition) = self.definitions.get(&id).cloned() else {
                app.action_notice = Some("source definition is unavailable".into());
                continue;
            };
            let name = definition.name.clone();
            let manager = self.manager.clone();
            let (sender, result) = tokio::sync::oneshot::channel();
            let restart = request.restart;
            let worker = self.runtime.spawn(async move {
                let result = control_source(manager, definition, restart).await;
                let _ = sender.send(result);
            });
            self.source_controls.insert(
                id,
                SourceControlJob {
                    restart,
                    result,
                    worker,
                },
            );
            app.action_notice = Some(format!(
                "{name}: {} capture…",
                if request.restart {
                    "restarting"
                } else {
                    "stopping"
                }
            ));
        }
        changed
    }

    /// Creates the derived view an edit to a canonical view implies.
    ///
    /// Registration happens here, before any query for the candidate can be
    /// submitted, so the runtime never sees a query for a view it does not
    /// know. The candidate stays out of the view list until its query and its
    /// persistence have both succeeded.
    fn handle_view_forks(&mut self, app: &mut App, adapter: &mut NativeViewAdapter) -> bool {
        let requests = app.take_view_fork_requests();
        let ready = app.take_ready_forks();
        let discards = app.take_fork_discards();
        let changed = !requests.is_empty() || !ready.is_empty() || !discards.is_empty();
        for candidate in discards {
            adapter.unregister_view(&candidate);
        }
        for request in requests {
            let sources = request
                .source_ids
                .iter()
                .map(|id| Uuid::parse_str(id).map(SourceId))
                .collect::<Result<Vec<_>, _>>();
            let sources = match sources {
                Ok(sources) if !sources.is_empty() => sources,
                _ => {
                    app.discard_fork(
                        &request.candidate_view_id,
                        "view sources are unavailable".into(),
                    );
                    continue;
                }
            };
            if let Err(error) = adapter.register_view(&request.candidate_view_id, sources) {
                app.discard_fork(
                    &request.candidate_view_id,
                    format!("could not create a view for this filter: {error}"),
                );
                continue;
            }
            if !app.begin_fork_query(&request.candidate_view_id) {
                app.discard_fork(
                    &request.candidate_view_id,
                    "could not start the query for this filter".into(),
                );
            }
        }
        for fork in ready {
            let Ok(source_uuid) = Uuid::parse_str(&fork.source_id) else {
                app.discard_fork(&fork.candidate_view_id, "source identity is invalid".into());
                continue;
            };
            let source_id = SourceId(source_uuid);
            let Some(definition) = self.definitions.get(&source_id).cloned() else {
                app.discard_fork(&fork.candidate_view_id, "source is unavailable".into());
                continue;
            };
            let Some(mut state) = app.fork_persistent_state(&fork.candidate_view_id) else {
                app.discard_fork(&fork.candidate_view_id, "view state is unavailable".into());
                continue;
            };
            state.view_name = fork.name.clone();
            let Ok(view_uuid) = Uuid::parse_str(&fork.candidate_view_id) else {
                app.discard_fork(&fork.candidate_view_id, "view identity is invalid".into());
                continue;
            };
            self.memory_sequence = self.memory_sequence.saturating_add(1);
            let request = Box::new(memory::SaveRequest {
                sequence: self.memory_sequence,
                definition,
                view_id: lvu_core::ViewId(view_uuid),
                state,
            });
            if let Err(error) = self.memory.create_derived_view(request) {
                app.discard_fork(
                    &fork.candidate_view_id,
                    format!("could not save the new view: {error}"),
                );
            }
        }
        changed
    }

    /// Field correlation across sources: resolve the frozen record's typed
    /// value and each source's field names, then open the accepted mapping as
    /// its own merged view. The origin view is never modified.
    fn handle_correlation(&mut self, app: &mut App, adapter: &mut NativeViewAdapter) -> bool {
        let requests = app.take_correlation_requests();
        let mut changed = !requests.is_empty();
        for request in requests {
            match request {
                lvu::CorrelationRequest::Cancel { .. } => adapter.cancel_correlation_lookup(),
                lvu::CorrelationRequest::Resolve {
                    generation,
                    origin_view_id,
                    row_id,
                    field,
                } => {
                    let origin = match Uuid::parse_str(&row_id.source_id) {
                        Ok(uuid) => lvu_core::RecordId {
                            source_id: SourceId(uuid),
                            sequence: row_id.sequence,
                        },
                        Err(error) => {
                            app.finish_correlation(
                                generation,
                                &origin_view_id,
                                Err(format!("record identity: {error}")),
                            );
                            continue;
                        }
                    };
                    // Every open source is a correlation candidate; the user
                    // decides which of them carry this identity.
                    let mut sources = Vec::new();
                    let mut invalid = None;
                    for source in &app.sources {
                        match Uuid::parse_str(&source.id) {
                            Ok(uuid) => sources.push(SourceId(uuid)),
                            Err(error) => invalid = Some(error.to_string()),
                        }
                    }
                    if let Some(error) = invalid {
                        app.finish_correlation(
                            generation,
                            &origin_view_id,
                            Err(format!("source identity: {error}")),
                        );
                        continue;
                    }
                    if let Err(error) =
                        adapter.submit_correlation_lookup(lvu_view::CorrelationLookupRequest {
                            generation,
                            origin_view_id: origin_view_id.clone(),
                            origin,
                            field,
                            sources,
                        })
                    {
                        app.finish_correlation(generation, &origin_view_id, Err(error.to_string()));
                    }
                }
                lvu::CorrelationRequest::Accept {
                    generation,
                    origin_view_id: _,
                    name,
                    correlation,
                } => {
                    changed = true;
                    match self.open_correlated_view(app, adapter, &name, &correlation) {
                        Ok(notice) => {
                            app.correlation_accepted(generation, notice);
                        }
                        Err(message) => {
                            app.correlation_accept_failed(generation, message);
                        }
                    }
                }
            }
        }
        for lookup in adapter.take_correlation_lookups() {
            changed = true;
            let lvu_view::CorrelationLookup {
                generation,
                origin_view_id,
                result,
            } = lookup;
            match result {
                Err(message) => {
                    app.finish_correlation(generation, &origin_view_id, Err(message));
                }
                Ok(candidate) => {
                    let label = correlation_value_label(&candidate.value);
                    let sources = candidate
                        .sources
                        .iter()
                        .map(|source| {
                            let id = source.source_id.0.to_string();
                            let name = app
                                .sources
                                .iter()
                                .find(|item| item.id == id)
                                .map_or_else(|| id.clone(), |item| item.name.clone());
                            // A field with the same name in another source is
                            // an exact name match, shown and confirmed by the
                            // user; a differently named field stays unmapped
                            // until they choose it.
                            let chosen = source
                                .fields
                                .iter()
                                .find(|field| *field == &candidate.field)
                                .cloned();
                            lvu::CorrelationSourceChoice {
                                source_id: id,
                                name,
                                fields: source.fields.clone(),
                                chosen,
                                incomplete: source.incomplete,
                            }
                        })
                        .collect();
                    app.open_correlation_dialog(
                        generation,
                        &origin_view_id,
                        candidate.field,
                        candidate.value,
                        label,
                        sources,
                    );
                }
            }
        }
        changed
    }

    /// The accepted mapping becomes one merged view over exactly the mapped
    /// sources, with the correlating fields pinned. It is an ordinary view: the
    /// correlation is a constraint on it, not a new kind of membership.
    fn open_correlated_view(
        &mut self,
        app: &mut App,
        adapter: &mut NativeViewAdapter,
        name: &str,
        correlation: &lvu_core::FieldCorrelation,
    ) -> Result<String, String> {
        // Merged views order records by explicit source position. The mapping
        // is keyed by identity, so order it the way the user opened the
        // sources rather than by how the identities happen to sort.
        let mapped: std::collections::HashSet<&str> = correlation.source_ids().collect();
        let source_ids: Vec<String> = app
            .sources
            .iter()
            .filter(|source| mapped.contains(source.id.as_str()))
            .map(|source| source.id.clone())
            .collect();
        if source_ids.len() != mapped.len() {
            return Err("a mapped source is no longer open".into());
        }
        let primary = source_ids.first().cloned().ok_or("no source was mapped")?;
        if let Some(error) = view_admission_error(app, &primary) {
            return Err(error.into());
        }
        let mut uuids = Vec::with_capacity(source_ids.len());
        for id in &source_ids {
            let uuid = Uuid::parse_str(id).map_err(|error| format!("source identity: {error}"))?;
            if !self.sources.contains_key(&SourceId(uuid)) {
                return Err("a mapped source is no longer open".into());
            }
            uuids.push(SourceId(uuid));
        }
        self.memory_sequence = self.memory_sequence.saturating_add(1);
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let new_id = Uuid::new_v5(
            &SOURCE_NAMESPACE,
            format!(
                "correlated-view:{primary}:{nonce}:{}:{name}",
                self.memory_sequence
            )
            .as_bytes(),
        )
        .to_string();
        adapter
            .register_view(&new_id, uuids)
            .map_err(|error| format!("register view: {error}"))?;
        app.add_view(ViewItem {
            id: new_id.clone(),
            source_id: primary,
            name: name.to_owned(),
        });
        let restored = lvu::PersistentViewState {
            source_ids: source_ids.clone(),
            view_name: name.to_owned(),
            exact_field: Some(correlation.clone()),
            pinned_columns: correlation.fields(),
            ..lvu::PersistentViewState::default()
        };
        if !app.restore_persistent_view(&new_id, restored) {
            return Err("the correlated view could not be installed".into());
        }
        let memory_id =
            lvu_core::ViewId(Uuid::parse_str(&new_id).expect("generated view identity"));
        self.memory_load_fences.insert(
            memory_id,
            app.view_interaction_revision(&new_id).unwrap_or_default(),
        );
        app.select_view(&new_id);
        Ok(format!(
            "Correlating {} across {} source{}",
            correlation.origin_field(),
            source_ids.len(),
            if source_ids.len() == 1 { "" } else { "s" }
        ))
    }

    fn handle_view_requests(&mut self, app: &mut App, adapter: &mut NativeViewAdapter) -> bool {
        let requests = app.layers.view.outbox.take();
        let changed = !requests.is_empty();
        for request in requests {
            if request.mode == lvu::ViewDialogMode::Sources {
                let sources = match request
                    .source_ids
                    .iter()
                    .map(|id| Uuid::parse_str(id).map(SourceId))
                    .collect::<Result<Vec<_>, _>>()
                {
                    Ok(sources) => sources,
                    Err(error) => {
                        app.view_request_failed(format!("source identity: {error}"));
                        continue;
                    }
                };
                match app.begin_source_change(&request.view_id, request.source_ids) {
                    Ok(query) => {
                        if let Err(message) = adapter.submit_source_change(query.clone(), sources) {
                            app.apply_query_completion(lvu::QueryCompletion {
                                view_id: query.view_id,
                                generation: query.generation,
                                revision: query.revision,
                                purpose: query.purpose,
                                result: Err(lvu::QueryFailure {
                                    purpose: query.purpose,
                                    message: message.clone(),
                                }),
                            });
                            app.view_request_failed(message);
                        } else {
                            app.view_request_succeeded(&request.view_id);
                        }
                    }
                    Err(error) => app.view_request_failed(error),
                }
                continue;
            }
            if request.mode == lvu::ViewDialogMode::Rename {
                if app.views().iter().any(|view| {
                    view.id != request.view_id
                        && view.source_id == request.source_id
                        && view.name == request.name
                }) {
                    app.view_request_failed("a view with that name already exists".into());
                    continue;
                }
                if app.rename_view(&request.view_id, request.name.clone()) {
                    app.view_request_succeeded(&request.view_id);
                } else {
                    app.view_request_failed("selected view no longer exists".into());
                }
                continue;
            }
            if let Some(error) = view_admission_error(app, &request.source_id) {
                app.view_request_failed(error.into());
                continue;
            }
            if app
                .views()
                .iter()
                .any(|view| view.source_id == request.source_id && view.name == request.name)
            {
                app.view_request_failed("a view with that name already exists".into());
                continue;
            }
            let Ok(source_uuid) = Uuid::parse_str(&request.source_id) else {
                app.view_request_failed("selected source identity is invalid".into());
                continue;
            };
            let source_id = SourceId(source_uuid);
            if !self.sources.contains_key(&source_id) {
                app.view_request_failed("selected source is unavailable".into());
                continue;
            }
            self.memory_sequence = self.memory_sequence.saturating_add(1);
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let new_id = Uuid::new_v5(
                &SOURCE_NAMESPACE,
                format!(
                    "named-view:{}:{nonce}:{}:{}",
                    source_id.0, self.memory_sequence, request.name
                )
                .as_bytes(),
            )
            .to_string();
            let view_sources = if request.mode == lvu::ViewDialogMode::Clone {
                app.view_source_ids(&request.view_id)
                    .iter()
                    .map(|id| Uuid::parse_str(id).map(SourceId))
                    .collect::<Result<Vec<_>, _>>()
            } else {
                Ok(vec![source_id])
            };
            let view_sources = match view_sources {
                Ok(value) => value,
                Err(error) => {
                    app.view_request_failed(error.to_string());
                    continue;
                }
            };
            if let Err(error) = adapter.register_view(&new_id, view_sources) {
                app.view_request_failed(format!("register view: {error}"));
                continue;
            }
            let cloned = (request.mode == lvu::ViewDialogMode::Clone)
                .then(|| app.persistent_view_state(&request.view_id))
                .flatten();
            let mut copied_command = false;
            app.add_view(ViewItem {
                id: new_id.clone(),
                source_id: request.source_id,
                name: request.name,
            });
            if let Some(mut state) = cloned {
                copied_command = command_controller::clear_cloned_publication(&mut state);
                state.view_name = app
                    .views()
                    .iter()
                    .find(|view| view.id == new_id)
                    .map(|view| view.name.clone())
                    .unwrap_or_default();
                app.restore_persistent_view(&new_id, state);
                let memory_id =
                    lvu_core::ViewId(Uuid::parse_str(&new_id).expect("generated view identity"));
                self.memory_load_fences.insert(
                    memory_id,
                    app.view_interaction_revision(&new_id).unwrap_or_default(),
                );
                self.memory_restoring.insert(memory_id);
            }
            app.view_request_succeeded(&new_id);
            if copied_command {
                app.action_notice = Some(
                    "Command definition copied; results require an explicit run in this view."
                        .into(),
                );
            }
        }
        changed
    }

    fn request_restore(&mut self, app: &App, definition: SourceDefinition) -> Result<(), String> {
        let memory_id = lvu_core::ViewId(Uuid::new_v5(
            &SOURCE_NAMESPACE,
            format!("working-view:{}", definition.id.0).as_bytes(),
        ));
        let interaction = app
            .view_interaction_revision(&view_id(definition.id))
            .unwrap_or_default();
        self.memory_load_fences.insert(memory_id, interaction);
        self.memory.load(definition, memory_id)
    }

    fn poll_memory(&mut self, app: &mut App, adapter: &NativeViewAdapter) -> bool {
        let mut changed = false;
        for _ in 0..64 {
            let Some(event) = self.memory.poll() else {
                break;
            };
            changed = true;
            self.handle_memory_event(app, adapter, event);
        }
        let ready: Vec<_> = self
            .memory_deferred
            .iter()
            .filter(|(_, view)| {
                view.presentation
                    .source_ids
                    .iter()
                    .all(|id| self.sources.contains_key(id))
            })
            .map(|(id, _)| *id)
            .collect();
        for id in ready {
            if let Some(view) = self.memory_deferred.remove(&id) {
                self.handle_memory_event(
                    app,
                    adapter,
                    MemoryEvent::Loaded(view.source_id, id, vec![view]),
                );
                changed = true;
            }
        }
        if self.memory_deferred.is_empty()
            && app
                .action_notice
                .as_deref()
                .is_some_and(|notice| notice.starts_with("Waiting for sources:"))
        {
            app.action_notice = None;
            changed = true;
        }
        changed
    }

    fn handle_memory_event(
        &mut self,
        app: &mut App,
        adapter: &NativeViewAdapter,
        event: MemoryEvent,
    ) {
        self.handle_command_memory_event(app, &event);
        match event {
            MemoryEvent::Loaded(source_id, _requested, stored) => {
                for value in stored {
                    let id = value.id;
                    let ui_id = id.0.to_string();
                    let sources = if value.presentation.source_ids.is_empty() {
                        vec![source_id]
                    } else {
                        value.presentation.source_ids.clone()
                    };
                    let fence = self
                        .memory_load_fences
                        .get(&id)
                        .copied()
                        .unwrap_or_else(|| {
                            app.view_interaction_revision(&ui_id).unwrap_or_default()
                        });
                    if app
                        .view_interaction_revision(&ui_id)
                        .is_some_and(|current| current != fence)
                    {
                        continue;
                    }
                    if sources
                        .iter()
                        .any(|source| !self.sources.contains_key(source))
                    {
                        if self.memory_deferred.len() < 128
                            || self.memory_deferred.contains_key(&id)
                        {
                            app.defer_view_restore(&ui_id);
                            app.action_notice = Some(format!(
                                "Waiting for sources: view {:?}. Press n to open its other sources; remembered commands never start automatically.",
                                value.name
                            ));
                            self.memory_load_fences.insert(id, fence);
                            self.memory_deferred.insert(id, value);
                        } else {
                            memory_notice(app, "deferred view restoration limit reached".into());
                        }
                        continue;
                    }
                    if app.views().iter().all(|view| view.id != ui_id) {
                        if let Some(error) = view_admission_error(app, &source_id.0.to_string()) {
                            memory_notice(app, format!("restore view {:?}: {error}", value.name));
                            continue;
                        }
                        if let Err(error) = adapter.register_view(&ui_id, sources.clone()) {
                            memory_notice(app, format!("restore view: {error}"));
                            continue;
                        }
                        app.add_view(ViewItem {
                            id: ui_id.clone(),
                            source_id: source_id.0.to_string(),
                            name: value.name.clone(),
                        });
                    }
                    // The role always comes from persisted metadata, including
                    // for a view this session created before the load finished.
                    app.set_view_role(&ui_id, view_role(value.role));
                    if adapter.view_sources(&ui_id).as_ref() != Some(&sources)
                        && let Err(error) = adapter.register_view(&ui_id, sources)
                    {
                        memory_notice(app, format!("restore source membership: {error}"));
                        continue;
                    }
                    let fence = self
                        .memory_load_fences
                        .get(&id)
                        .copied()
                        .unwrap_or_else(|| {
                            app.view_interaction_revision(&ui_id).unwrap_or_default()
                        });
                    self.memory_load_fences.insert(id, fence);
                    let restored = memory::restored(value);
                    if app.restore_persistent_view_if_unmodified(&ui_id, fence, restored) {
                        self.memory_restoring.insert(id);
                    }
                }
                // Every view of this source has its remembered position now, so
                // reopen the one that was last in use rather than leaving the
                // user on All events.
                app.restore_source_selection(&source_id.0.to_string());
                self.memory_ready.insert(source_id);
            }
            MemoryEvent::DerivedViewCreated(view_id, result) => {
                let candidate = view_id.0.to_string();
                match result {
                    // Persisted: only now does the view exist for the user.
                    Ok(()) => {
                        if !app.install_fork(&candidate) {
                            app.discard_fork(&candidate, String::new());
                        }
                    }
                    Err(error) => {
                        app.discard_fork(
                            &candidate,
                            format!("the new view could not be saved: {error}"),
                        );
                    }
                }
            }
            MemoryEvent::LoadFailed(source_id, _view_id, error) => {
                self.memory_ready.insert(source_id);
                memory_notice(app, error);
            }
            MemoryEvent::Saved(_source_id, view_id, sequence) => {
                if let Some((_, state)) = self.memory_inflight.remove(&sequence)
                    && self
                        .memory_ack_sequence
                        .get(&view_id)
                        .is_none_or(|seen| sequence > *seen)
                {
                    self.memory_ack_sequence.insert(view_id, sequence);
                    self.memory_last.insert(view_id, state);
                    self.memory_failed.remove(&view_id);
                }
            }
            MemoryEvent::SaveFailed(_source_id, view_id, sequence, error) => {
                if let Some((_, state)) = self.memory_inflight.remove(&sequence) {
                    self.memory_failed.insert(view_id, state);
                }
                memory_notice(app, error);
            }
            MemoryEvent::Recent(values) => self.recent_sources = values,
            MemoryEvent::Recipes(meta, values, candidates) => {
                let mut items: Vec<_> = values
                    .into_iter()
                    .map(|(recipe, _hash)| recipe_item(recipe))
                    .collect();
                let observed = suggestion_context(app, adapter, &self.definitions, &self.cwd)
                    .map(|context| context.fields)
                    .unwrap_or_default();
                let suggestions: Vec<_> = candidates
                    .into_iter()
                    .map(|candidate| {
                        let recipe_id = candidate.recipe_id.0.to_string();
                        let mut missing_fields = items
                            .iter()
                            .find(|item| item.id == recipe_id)
                            .into_iter()
                            .flat_map(|item| {
                                item.config
                                    .pinned_columns
                                    .iter()
                                    .chain(item.config.color_field.iter())
                            })
                            .filter(|field| !observed.contains_key(*field))
                            .cloned()
                            .collect::<Vec<_>>();
                        missing_fields.sort();
                        missing_fields.dedup();
                        let mut evidence = candidate.evidence;
                        if !candidate.missing_fields.is_empty() {
                            evidence.push(format!(
                                "{} candidate fields not observed in sampled visible rows",
                                candidate.missing_fields.len()
                            ));
                        }
                        lvu::app::RecipeSuggestion {
                            recipe_id,
                            evidence,
                            missing_fields,
                        }
                    })
                    .collect();
                let order: HashMap<_, _> = suggestions
                    .iter()
                    .enumerate()
                    .map(|(index, value)| (value.recipe_id.as_str(), index))
                    .collect();
                items.sort_by_key(|item| {
                    (
                        order.get(item.id.as_str()).copied().unwrap_or(usize::MAX),
                        item.name.clone(),
                    )
                });
                app.set_recipes_with_suggestions(meta, items, suggestions, None);
            }
            MemoryEvent::RecipeHistory(meta, values) => {
                app.set_recipes(meta, values.into_iter().map(recipe_item).collect(), None)
            }
            MemoryEvent::RecipeSaved(meta, saved) => app.recipe_saved(
                meta,
                format!("saved immutable revision {}", saved.revision_id),
            ),
            MemoryEvent::RecipeExported(meta, saved) => app.recipe_exported(
                meta,
                format!("exported {} to {}", saved.revision_id, saved.path.display()),
            ),
            MemoryEvent::RecipeFailed(meta, error) => app.recipe_failed(meta, error),
            MemoryEvent::SuggestionFailed(error) => memory_notice(app, error),
            MemoryEvent::RecentFailed(error) => memory_notice(app, error),
            // The worker sends this and then exits: workspace state is gone for
            // this session and the user has been told. Later queue failures are
            // that same fact restated, not new information.
            MemoryEvent::Fatal(error) => {
                self.memory_unavailable = true;
                memory_notice(app, error)
            }
        }
    }

    fn queue_memory_saves(&mut self, app: &App, force: bool) -> bool {
        const AUTOSAVE_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(250);
        let mut changed = false;
        for view in app.views().to_vec() {
            let Ok(view_uuid) = Uuid::parse_str(&view.id) else {
                continue;
            };
            let memory_view_id = lvu_core::ViewId(view_uuid);
            if self.command_controller.suppresses_autosave(memory_view_id) {
                continue;
            }
            let Ok(source_uuid) = Uuid::parse_str(&view.source_id) else {
                continue;
            };
            let source_id = SourceId(source_uuid);
            if self.memory_deferred.contains_key(&memory_view_id) {
                continue;
            }
            let Some(definition) = self.definitions.get(&source_id) else {
                continue;
            };
            if !self.memory_ready.contains(&source_id) {
                continue;
            }
            if self.memory_restoring.contains(&memory_view_id) {
                let user_interacted = self.memory_load_fences.get(&memory_view_id).copied()
                    != app.view_interaction_revision(&view.id);
                if !user_interacted && app.view_has_pending_query(&view.id) {
                    continue;
                }
                self.memory_restoring.remove(&memory_view_id);
            }
            let Some(state) = app.persistent_view_state(&view.id) else {
                continue;
            };
            let already_tracked = reconcile_pending_state(
                &mut self.memory_pending,
                &self.memory_last,
                &self.memory_inflight,
                &self.memory_failed,
                memory_view_id,
                &state,
            );
            if already_tracked {
                continue;
            }
            self.memory_sequence = self.memory_sequence.saturating_add(1);
            let request = SaveRequest {
                sequence: self.memory_sequence,
                definition: definition.clone(),
                view_id: memory_view_id,
                state: state.clone(),
            };
            self.memory_pending.insert(
                memory_view_id,
                PendingMemorySave {
                    request: Box::new(request),
                    dirty_since: std::time::Instant::now(),
                },
            );
            changed = true;
        }
        let ids: Vec<_> = self.memory_pending.keys().copied().collect();
        for id in ids {
            let Some(pending) = self.memory_pending.remove(&id) else {
                continue;
            };
            if !force && pending.dirty_since.elapsed() < AUTOSAVE_DEBOUNCE
                || self.memory_inflight.values().any(|(view, _)| *view == id)
            {
                self.memory_pending.insert(id, pending);
                continue;
            }
            let sequence = pending.request.sequence;
            let state = pending.request.state.clone();
            if let Err(request) = self.memory.save(pending.request) {
                self.memory_pending.insert(
                    id,
                    PendingMemorySave {
                        request,
                        dirty_since: pending.dirty_since,
                    },
                );
                break;
            }
            self.memory_inflight.insert(sequence, (id, state));
        }
        changed
    }

    fn flush_memory(
        &mut self,
        app: &mut App,
        adapter: &NativeViewAdapter,
        timeout: std::time::Duration,
    ) -> Result<(), String> {
        // Nothing was ever queued and nothing can be: the session already ran
        // in the documented degraded mode and said so on screen. Reporting the
        // dead worker again here is what turned a deliberate quit into a
        // failed exit status.
        self.poll_memory(app, adapter);
        if self.memory_unavailable {
            return Ok(());
        }
        let deadline = std::time::Instant::now() + timeout;
        loop {
            self.queue_memory_saves(app, true);
            while !self.memory_pending.is_empty() {
                self.poll_memory(app, adapter);
                self.queue_memory_saves(app, true);
                if std::time::Instant::now() >= deadline {
                    return Err("memory autosave flush deadline exceeded".into());
                }
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
            let (events, result) = self
                .memory
                .flush(deadline.saturating_duration_since(std::time::Instant::now()));
            for event in events {
                self.handle_memory_event(app, adapter, event);
            }
            result?;
            self.queue_memory_saves(app, true);
            if self.memory_pending.is_empty() && self.memory_inflight.is_empty() {
                return Ok(());
            }
            if std::time::Instant::now() >= deadline {
                return Err("memory autosave acknowledgements incomplete".into());
            }
        }
    }

    fn start_path_completion(&mut self, request: PathCompletionRequest) {
        let generation = request.generation;
        let cancel = Arc::new(AtomicBool::new(false));
        self.active_completion = Some((generation, Arc::clone(&cancel)));
        let cwd = self.cwd.clone();
        let home = self.home.clone();
        let tx = self.completions_tx.clone();
        std::thread::spawn(move || {
            let result = complete_path(request, &cwd, home.as_deref(), &cancel);
            let _ = tx.blocking_send(result);
        });
    }

    fn admit_definition(
        &mut self,
        app: &mut App,
        definition: SourceDefinition,
        origin: StartOrigin,
    ) {
        let id = definition.id;
        if self.sources.contains_key(&id) {
            start_succeeded(app, &origin, &view_id(id));
        } else if self.pending_starts.contains(&id) {
            start_failed(app, origin, "source is already starting".into());
        } else if self.sources.len() + self.pending_starts.len() >= MAX_SOURCES
            || self.pending_starts.len() >= MAX_PENDING_STARTS
        {
            start_failed(app, origin, "source admission limit reached".into());
        } else if app.views().len() + self.pending_starts.len() >= MAX_VIEWS {
            // Each pending source reserves its default view before acquisition.
            start_failed(app, origin, "view admission limit reached".into());
        } else {
            self.spawn_start(definition, origin);
        }
    }

    fn spawn_start(&mut self, definition: SourceDefinition, origin: StartOrigin) {
        self.pending_starts.insert(definition.id);
        let manager = Arc::clone(&self.manager);
        let sender = self.starts_tx.clone();
        self.runtime.spawn(async move {
            let source_id = definition.id;
            let view_id = view_id(source_id);
            let result = match manager.start(definition.clone()).await {
                Ok(handle) => Ok(StartedSource {
                    definition,
                    view_id,
                    handle,
                    origin: Some(origin.clone()),
                }),
                Err(error) => Err(StartFailure {
                    source_id,
                    origin,
                    message: format!("start {}: {error}", definition.name),
                }),
            };
            let _ = sender.send(result).await;
        });
    }

    fn spawn_scan(&mut self, generation: u64) {
        if let Some((_, cancel)) = &self.active_scan {
            cancel.cancel();
            self.pending_scan = Some(generation);
            return;
        }
        self.start_scan_task(generation);
    }

    fn start_scan_task(&mut self, generation: u64) {
        let cancel = CancellationToken::default();
        self.active_scan = Some((generation, cancel.clone()));
        let sender = self.scans_tx.clone();
        let cwd = self.cwd.clone();
        self.runtime.spawn(async move {
            let request = discovery_request(cwd, cancel);
            let result = lvu_discovery::discover(request).await;
            let _ = sender.send(ScanResult { generation, result }).await;
        });
    }

    fn cancel_discovery(&mut self) {
        if let Some((_, cancel)) = self.active_scan.take() {
            cancel.cancel();
        }
        self.pending_scan = None;
        self.discovery_candidates.clear();
    }
}

/// How a correlated value reads in the dialog and the view name. The predicate
/// always uses the typed scalar; this is presentation.
fn correlation_value_label(value: &lvu_core::ExactScalar) -> String {
    match value {
        lvu_core::ExactScalar::Null => "null".into(),
        lvu_core::ExactScalar::Bool(value) => value.to_string(),
        lvu_core::ExactScalar::SignedInteger(value) => value.to_string(),
        lvu_core::ExactScalar::UnsignedInteger(value) => value.to_string(),
        lvu_core::ExactScalar::FloatBits(bits) => f64::from_bits(*bits).to_string(),
        lvu_core::ExactScalar::String(value) => {
            let bounded: String = value.chars().take(64).collect();
            format!("\"{bounded}\"")
        }
    }
}

fn view_admission_error(app: &App, source_id: &str) -> Option<&'static str> {
    let source_views = app
        .views()
        .iter()
        .filter(|view| view.source_id == source_id)
        .count();
    (app.views().len() >= MAX_VIEWS || source_views >= MAX_VIEWS_PER_SOURCE)
        .then_some("view admission limit reached")
}

fn ai_generation(work: &AiWork) -> u64 {
    match work {
        AiWork::Sampling { start, .. }
        | AiWork::Starting { start, .. }
        | AiWork::Proposing { start, .. } => start.generation,
        AiWork::Cancelling { generation, .. } => *generation,
    }
}

fn investigation_generation(work: &InvestigationWork) -> u64 {
    match work {
        InvestigationWork::Snapshot { start, .. }
        | InvestigationWork::Preparing { start, .. }
        | InvestigationWork::Starting { start, .. } => start.generation,
        InvestigationWork::Resuming { generation, .. }
        | InvestigationWork::Sending { generation, .. }
        | InvestigationWork::Watching { generation, .. }
        | InvestigationWork::Cancelling { generation, .. }
        | InvestigationWork::Unresolved { generation, .. } => *generation,
    }
}

fn investigation_session_id(work: Option<&InvestigationWork>) -> Option<&str> {
    match work? {
        InvestigationWork::Resuming { item, .. }
        | InvestigationWork::Sending { item, .. }
        | InvestigationWork::Watching { item, .. }
        | InvestigationWork::Cancelling { item, .. }
        | InvestigationWork::Unresolved { item, .. } => Some(&item.session_id),
        InvestigationWork::Snapshot { .. }
        | InvestigationWork::Preparing { .. }
        | InvestigationWork::Starting { .. } => None,
    }
}

fn investigation_turn_started(work: Option<&InvestigationWork>) -> bool {
    match work {
        Some(
            InvestigationWork::Sending { turn_started, .. }
            | InvestigationWork::Watching { turn_started, .. },
        ) => *turn_started,
        _ => false,
    }
}

fn investigation_observation(
    work: Option<&InvestigationWork>,
) -> Option<(u64, InvestigationItem, u64)> {
    match work? {
        InvestigationWork::Sending {
            generation,
            item,
            event_floor,
            ..
        }
        | InvestigationWork::Watching {
            generation,
            item,
            event_floor,
            ..
        } => Some((*generation, item.clone(), *event_floor)),
        _ => None,
    }
}

fn investigation_health_failure(
    state: HostState,
    dropped_events: u64,
    event_floor: u64,
) -> Option<String> {
    if dropped_events > event_floor {
        Some(format!(
            "agent event loss detected ({} events dropped); cancelling session",
            dropped_events - event_floor
        ))
    } else if matches!(
        state,
        HostState::Disconnected | HostState::Faulted | HostState::Stopped
    ) {
        Some("agent bridge disconnected while waiting for the turn".into())
    } else {
        None
    }
}

fn mark_investigation_turn_started(work: Option<&mut InvestigationWork>) {
    if let Some(
        InvestigationWork::Sending { turn_started, .. }
        | InvestigationWork::Watching { turn_started, .. },
    ) = work
    {
        *turn_started = true;
    }
}

fn finish_investigation_error(app: &mut App, generation: u64, message: &str) {
    app.update_investigation_progress(
        generation,
        InvestigationStage::Error,
        message.to_owned(),
        None,
        None,
        None,
    );
}

fn investigation_prompt(question: &str, context: &PreparedAiContext) -> String {
    let inspection = context.inspection_command.as_ref().map_or_else(String::new, |command| {
        format!(
            "\nBegin with this bounded typed schema/sample helper (executable argument vector): {}. It supports --source and --field selection, preserves separate schema variants, and reports coverage and omissions within 32 KiB. Report further full-data reads separately. Bounded decoding currently requires Linux; if unavailable, report the limitation.\n",
            serde_json::to_string(command).expect("string vector serializes")
        )
    });
    let datasets = context
        .datasets
        .iter()
        .map(|path| format!("- {}", path.display()))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "Investigate the fixed local lvu log snapshot described below. Use the manifest and Parquet files directly. Preserve raw record identity and distinguish captured data from inference. Do not modify the capture.{inspection}\n\nQuestion: {question}\nManifest: {}\nDatasets:\n{datasets}",
        context.manifest_path.display()
    )
}

fn settle_ai_work(
    active: Option<AiWork>,
    owned_session: Option<String>,
    host: Option<&AgentBridgeHost>,
    deadline: std::time::Instant,
) -> Vec<String> {
    let mut failures = Vec::new();
    let mut session_to_cancel = None;
    let mut cancellation_already_requested = false;
    if let Some(work) = active {
        match work {
            AiWork::Sampling { mut job, .. } => {
                job.cancel();
                loop {
                    if job.try_wait().is_some() {
                        break;
                    }
                    let left = remaining(deadline);
                    if left.is_zero() {
                        failures.push("assistance preparation did not stop before shutdown".into());
                        break;
                    }
                    std::thread::sleep(left.min(Duration::from_millis(5)));
                }
            }
            AiWork::Starting { request, .. } => match request.recv_timeout(remaining(deadline)) {
                Ok(session_id) => session_to_cancel = Some(session_id),
                Err(error) => failures.push(format!(
                    "agent session start did not settle before shutdown: {}",
                    host_error_message(error)
                )),
            },
            AiWork::Proposing { session_id, .. } => session_to_cancel = Some(session_id),
            AiWork::Cancelling {
                session_id,
                request,
                ..
            } => {
                cancellation_already_requested = true;
                match request.recv_timeout(remaining(deadline)) {
                    Ok(result) => {
                        if let Err(error) = validate_remote_cancellation(&result) {
                            failures.push(error);
                        }
                    }
                    Err(error) => failures.push(format!(
                        "agent session {session_id} cancellation did not settle: {}",
                        host_error_message(error)
                    )),
                }
            }
        }
    }
    if session_to_cancel.is_none() && !cancellation_already_requested {
        session_to_cancel = owned_session;
    }
    if let Some(session_id) = session_to_cancel {
        match host.map(|host| host.cancel(&session_id)) {
            Some(Ok(request)) => match request.recv_timeout(remaining(deadline)) {
                Ok(result) => {
                    if let Err(error) = validate_remote_cancellation(&result) {
                        failures.push(error);
                    }
                }
                Err(error) => failures.push(format!(
                    "agent session {session_id} cancellation did not settle: {}",
                    host_error_message(error)
                )),
            },
            Some(Err(error)) => failures.push(format!(
                "agent session {session_id} cancellation could not start: {}",
                host_error_message(error)
            )),
            None => failures.push(format!(
                "agent session {session_id} cancellation unavailable"
            )),
        }
    }
    failures
}

fn finish_ai_host_error(app: &mut App, start: &AiStart, error: agent::HostError) {
    // `diagnose` already returns a complete user sentence; prefixing it with
    // the service name only adds implementation jargon (AGENTS.md).
    finish_ai_error(app, start, host_error_message(error));
}

fn host_error_message(error: agent::HostError) -> String {
    agent::diagnose(&error)
}

fn validate_remote_cancellation(result: &serde_json::Value) -> Result<(), String> {
    match result
        .get("remote_agent_may_still_be_running")
        .and_then(serde_json::Value::as_bool)
    {
        Some(false) => Ok(()),
        Some(true) => Err(result
            .get("cancel_error")
            .and_then(serde_json::Value::as_str)
            .map_or_else(
                || "remote agent agent may still be running after cancellation".into(),
                |error| format!("remote agent agent may still be running: {error}"),
            )),
        None => Err("agent bridge cancellation response omitted remote lifecycle status".into()),
    }
}

fn remaining(deadline: std::time::Instant) -> Duration {
    deadline.saturating_duration_since(std::time::Instant::now())
}

fn finish_ai_error(app: &mut App, start: &AiStart, message: String) {
    app.finish_ask_ai(
        start.generation,
        &start.view_id,
        start.definition_revision,
        Err(message),
    );
}

fn collect_snapshot_datasets(root: &Path) -> Vec<PathBuf> {
    let mut result = Vec::new();
    let mut pending = vec![(root.to_path_buf(), 0_u8)];
    while let Some((directory, depth)) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(directory) else {
            continue;
        };
        for entry in entries.flatten().take(1024) {
            if result.len() >= MAX_AI_DATASETS {
                return result;
            }
            let path = entry.path();
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() && depth < 3 {
                pending.push((path, depth + 1));
            } else if kind.is_file() && path.extension().is_some_and(|value| value == "parquet") {
                result.push(path);
            }
        }
    }
    result.sort();
    result
}

fn proposal_expression(kind: AskAiKind, proposal: &ProposalEnvelope) -> Result<String, String> {
    match kind {
        AskAiKind::Filter => proposal
            .definition
            .get("expression")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| "filter proposal omitted expression".into()),
        AskAiKind::Enrichment => {
            let stages = proposal
                .definition
                .get("stages")
                .and_then(serde_json::Value::as_array)
                .ok_or_else(|| "enrichment proposal omitted stages".to_owned())?;
            if stages.len() != 1 {
                return Err("this editor accepts one enrichment stage at a time".into());
            }
            let expressions = stages[0]
                .get("expressions")
                .and_then(serde_json::Value::as_object)
                .ok_or_else(|| "enrichment proposal omitted expressions".to_owned())?;
            if expressions.len() != 1 {
                return Err("this editor accepts one derived field at a time".into());
            }
            let (name, expression) = expressions.iter().next().expect("one expression");
            let expression = expression
                .as_str()
                .ok_or_else(|| "enrichment expression is not text".to_owned())?;
            Ok(format!("{name} = {expression}"))
        }
        AskAiKind::Recipe => {
            let definition = proposal
                .definition
                .as_object()
                .ok_or_else(|| "view proposal definition is not an object".to_owned())?;
            const ALLOWED: [&str; 6] = [
                "schema_version",
                "id",
                "name",
                "source_ids",
                "filter",
                "recipe_stage_revisions",
            ];
            if definition.len()
                != ALLOWED.len() + usize::from(definition.contains_key("enrichments"))
                || !ALLOWED.iter().all(|field| definition.contains_key(*field))
            {
                return Err(
                    "adaptation proposed unsupported view settings; working view preserved".into(),
                );
            }
            let revisions = proposal
                .definition
                .get("recipe_stage_revisions")
                .and_then(serde_json::Value::as_array)
                .ok_or_else(|| "view proposal omitted recipe_stage_revisions".to_owned())?;
            if !revisions.is_empty() {
                return Err("adaptation cannot import unresolved recipe stages".into());
            }
            let filter = proposal
                .definition
                .get("filter")
                .ok_or_else(|| "view proposal omitted filter".to_owned())?;
            if filter.is_null() {
                Ok(String::new())
            } else {
                filter
                    .get("expression")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned)
                    .ok_or_else(|| "view proposal filter omitted expression".into())
            }
        }
    }
}

fn proposal_recipe_enrichments(
    proposal: &ProposalEnvelope,
) -> Result<Option<Vec<lvu::EnrichmentDefinition>>, String> {
    let Some(value) = proposal.definition.get("enrichments") else {
        return Ok(None);
    };
    let stages = value
        .as_array()
        .ok_or("enrichments must be an ordered array")?;
    if stages.len() > 32 {
        return Err("at most 32 enrichment stages are supported".into());
    }
    let mut ids = std::collections::HashSet::new();
    stages
        .iter()
        .map(|value| {
            let fields = value
                .as_object()
                .ok_or("enrichment stage must be an object")?;
            let id = fields
                .get("id")
                .and_then(serde_json::Value::as_str)
                .ok_or("missing stage id")?;
            let source = fields
                .get("source")
                .and_then(serde_json::Value::as_str)
                .ok_or("missing stage source")?;
            if fields.len() != 2
                || id.is_empty()
                || id.len() > 128
                || source.is_empty()
                || source.len() > 16_384
                || !ids.insert(id)
            {
                return Err("invalid, duplicate or oversized enrichment stage".into());
            }
            Ok(lvu::EnrichmentDefinition {
                id: lvu::EnrichmentStageId(id.into()),
                source: source.into(),
                command: None,
            })
        })
        .collect::<Result<Vec<_>, String>>()
        .map(Some)
}

fn validate_recipe_proposal_source(
    proposal: &ProposalEnvelope,
    expected: &[String],
) -> Result<(), String> {
    let sources = proposal
        .definition
        .get("source_ids")
        .and_then(serde_json::Value::as_array)
        .ok_or("view proposal omitted source_ids")?;
    if sources.len() == expected.len()
        && sources
            .iter()
            .zip(expected)
            .all(|(value, id)| value.as_str() == Some(id.as_str()))
    {
        Ok(())
    } else {
        Err(
            "adaptation proposed different source membership or order; working view preserved"
                .into(),
        )
    }
}

fn prepared_sample_context(
    start: &AiStart,
    prepared: AssistancePreparationResult,
) -> Result<(PathBuf, PreparedAiContext), String> {
    if prepared.view_id != start.view_id {
        return Err("prepared assistance belongs to a different view".into());
    }
    let encoded = serde_json::to_vec(&prepared.context)
        .map_err(|error| format!("prepared assistance context: {error}"))?;
    if !prepared.context.is_object()
        || encoded.len() > 32 * 1024
        || prepared.inline_context.len() > 32 * 1024
        || prepared.inline_context.len() != prepared.serialized_bytes
        || serde_json::from_str::<serde_json::Value>(&prepared.inline_context)
            .ok()
            .as_ref()
            != Some(&prepared.context)
    {
        return Err("prepared assistance context failed its complete byte contract".into());
    }
    if !prepared.context_path.is_absolute() {
        return Err("prepared assistance context path must be absolute".into());
    }
    let output_dir = prepared
        .context_path
        .parent()
        .ok_or_else(|| "prepared assistance context has no directory".to_owned())?
        .to_path_buf();
    let data = output_dir
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| "prepared assistance context has no snapshot identity".to_owned())?
        .to_owned();
    Ok((
        output_dir,
        PreparedAiContext {
            manifest_path: prepared.context_path,
            datasets: Vec::new(),
            inline_context: Some(prepared.context),
            // This artifact already contains the entire bounded context. Further
            // full-data inspection belongs to an explicit investigation snapshot.
            inspection_command: None,
            revision: OriginatingRevision {
                data,
                definition: format!("{}:{}", start.view_id, start.definition_revision),
            },
        },
    ))
}

fn prepare_ai_context(
    output_dir: PathBuf,
    manifest_path: PathBuf,
    view_id: String,
    definition_revision: u64,
) -> (
    std_mpsc::Receiver<Result<PreparedAiContext, String>>,
    JoinHandle<()>,
) {
    let (sender, result) = std_mpsc::sync_channel(1);
    let worker = std::thread::spawn(move || {
        let prepared = (|| {
            let manifest_path = std::fs::canonicalize(&manifest_path)
                .map_err(|error| format!("snapshot manifest: {error}"))?;
            let output_dir = std::fs::canonicalize(&output_dir)
                .map_err(|error| format!("snapshot directory: {error}"))?;
            let datasets = collect_snapshot_datasets(&output_dir)
                .into_iter()
                .map(|path| {
                    std::fs::canonicalize(path)
                        .map_err(|error| format!("snapshot dataset: {error}"))
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(PreparedAiContext {
                inline_context: None,
                // Omitted rather than guessed when the helper is absent; the
                // agent must not be handed a command that cannot run.
                inspection_command: resources::resolve_all().0.located().map(|helper| {
                    helper.python_command(
                        "lvu_expr_helper.inspection",
                        &[manifest_path.display().to_string()],
                    )
                }),
                revision: OriginatingRevision {
                    data: output_dir
                        .file_name()
                        .and_then(|value| value.to_str())
                        .unwrap_or("snapshot")
                        .to_owned(),
                    definition: format!("{view_id}:{definition_revision}"),
                },
                manifest_path,
                datasets,
            })
        })();
        let _ = sender.send(prepared);
    });
    (result, worker)
}

fn record_agent_session(directory: &Path, session_id: &str) -> SessionRecordJob {
    let directory = directory.to_path_buf();
    let session_id = session_id.to_owned();
    let (sender, result) = std_mpsc::sync_channel(1);
    let worker = std::thread::spawn(move || {
        let outcome = (|| {
            let path = directory.join("lvu-agent-session.json");
            let temporary = directory.join(".lvu-agent-session.json.tmp");
            let value = serde_json::json!({
                "schema_version": 1,
                "session_id": session_id,
                "snapshot_directory": directory.display().to_string(),
            });
            let bytes = serde_json::to_vec_pretty(&value)
                .map_err(|error| format!("encode session record: {error}"))?;
            if bytes.len() > 16 * 1024 {
                return Err("session record exceeds byte limit".into());
            }
            std::fs::write(&temporary, bytes)
                .map_err(|error| format!("write session record: {error}"))?;
            std::fs::rename(&temporary, path)
                .map_err(|error| format!("commit session record: {error}"))?;
            Ok(())
        })();
        let _ = sender.send(outcome);
    });
    SessionRecordJob {
        result,
        worker: Some(worker),
    }
}

fn admit_session_record(
    jobs: &mut Vec<SessionRecordJob>,
    directory: &Path,
    session_id: &str,
) -> Result<(), String> {
    if jobs.len() >= MAX_SESSION_RECORD_JOBS {
        return Err(format!(
            "at most {MAX_SESSION_RECORD_JOBS} session records may be pending"
        ));
    }
    jobs.push(record_agent_session(directory, session_id));
    Ok(())
}

fn admit_investigation_record(
    jobs: &mut Vec<SessionRecordJob>,
    directory: &Path,
    item: &InvestigationItem,
) -> Result<(), String> {
    if jobs.len() >= MAX_SESSION_RECORD_JOBS {
        return Err(format!(
            "at most {MAX_SESSION_RECORD_JOBS} session records may be pending"
        ));
    }
    let directory = directory.to_path_buf();
    let item = item.clone();
    let (sender, result) = std_mpsc::sync_channel(1);
    let worker = std::thread::spawn(move || {
        let outcome = (|| {
            let path = directory.join("lvu-investigation.json");
            let temporary = directory.join(".lvu-investigation.json.tmp");
            let value = serde_json::json!({
                "schema_version": 1,
                "investigation_id": item.id,
                "view_id": item.view_id,
                "session_id": item.session_id,
                "snapshot_dir": item.snapshot_dir,
                "manifest_path": item.manifest_path,
                "question": item.question,
            });
            let bytes = serde_json::to_vec_pretty(&value)
                .map_err(|error| format!("encode investigation record: {error}"))?;
            if bytes.len() > 32 * 1024 {
                return Err("investigation record exceeds byte limit".into());
            }
            std::fs::write(&temporary, bytes)
                .map_err(|error| format!("write investigation record: {error}"))?;
            std::fs::rename(&temporary, path)
                .map_err(|error| format!("commit investigation record: {error}"))?;
            Ok(())
        })();
        let _ = sender.send(outcome);
    });
    jobs.push(SessionRecordJob {
        result,
        worker: Some(worker),
    });
    Ok(())
}

fn load_investigations(root: PathBuf) -> InvestigationLoadJob {
    let (sender, result) = std_mpsc::sync_channel(1);
    let worker = std::thread::spawn(move || {
        let loaded = (|| {
            let mut items = Vec::new();
            if !root.exists() {
                return Ok(InvestigationLoadResult {
                    items,
                    diagnostic: None,
                });
            }
            let owned_root = std::fs::canonicalize(&root)
                .map_err(|error| format!("resolve investigation directory: {error}"))?;
            let mut entries = std::fs::read_dir(&owned_root)
                .map_err(|error| format!("read investigation directory: {error}"))?;
            let mut scanned = 0_usize;
            let mut rejected = 0_usize;
            while scanned < MAX_INVESTIGATION_SCAN_DIRS && items.len() < MAX_INVESTIGATIONS {
                let Some(entry) = entries.next() else {
                    break;
                };
                scanned += 1;
                let Ok(entry) = entry else {
                    rejected += 1;
                    continue;
                };
                let Ok(kind) = entry.file_type() else {
                    rejected += 1;
                    continue;
                };
                if !kind.is_dir() || kind.is_symlink() {
                    continue;
                }
                let Ok(snapshot) = std::fs::canonicalize(entry.path()) else {
                    rejected += 1;
                    continue;
                };
                if snapshot.parent() != Some(owned_root.as_path()) {
                    rejected += 1;
                    continue;
                }
                let path = snapshot.join("lvu-investigation.json");
                let Ok(metadata) = std::fs::symlink_metadata(&path) else {
                    continue;
                };
                if !metadata.file_type().is_file()
                    || metadata.len() > MAX_INVESTIGATION_RECORD_BYTES
                {
                    rejected += 1;
                    continue;
                }
                let Ok(file) = std::fs::File::open(&path) else {
                    rejected += 1;
                    continue;
                };
                let mut bytes = Vec::new();
                if file
                    .take(MAX_INVESTIGATION_RECORD_BYTES + 1)
                    .read_to_end(&mut bytes)
                    .is_err()
                    || bytes.len() as u64 > MAX_INVESTIGATION_RECORD_BYTES
                {
                    rejected += 1;
                    continue;
                }
                let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
                    rejected += 1;
                    continue;
                };
                if value
                    .get("schema_version")
                    .and_then(serde_json::Value::as_u64)
                    != Some(1)
                {
                    rejected += 1;
                    continue;
                };
                let string = |key: &str| {
                    value
                        .get(key)
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned)
                };
                let Some(item) = (|| {
                    Some(InvestigationItem {
                        id: string("investigation_id")?,
                        view_id: string("view_id")?,
                        session_id: string("session_id")?,
                        snapshot_dir: string("snapshot_dir")?,
                        manifest_path: string("manifest_path")?,
                        question: string("question")?,
                    })
                })() else {
                    rejected += 1;
                    continue;
                };
                if Uuid::parse_str(&item.id).is_err()
                    || Uuid::parse_str(&item.view_id).is_err()
                    || item.session_id.is_empty()
                    || item.session_id.len() > 512
                    || item.question.len() > 8 * 1024
                {
                    rejected += 1;
                    continue;
                }
                if std::fs::symlink_metadata(&item.snapshot_dir)
                    .is_ok_and(|metadata| metadata.file_type().is_symlink())
                    || std::fs::symlink_metadata(&item.manifest_path)
                        .is_ok_and(|metadata| metadata.file_type().is_symlink())
                {
                    rejected += 1;
                    continue;
                }
                let Ok(recorded_snapshot) = std::fs::canonicalize(&item.snapshot_dir) else {
                    rejected += 1;
                    continue;
                };
                let Ok(manifest) = std::fs::canonicalize(&item.manifest_path) else {
                    rejected += 1;
                    continue;
                };
                let manifest_regular = std::fs::symlink_metadata(&manifest)
                    .is_ok_and(|metadata| metadata.file_type().is_file());
                if recorded_snapshot != snapshot
                    || manifest.parent() != Some(snapshot.as_path())
                    || !manifest_regular
                {
                    rejected += 1;
                    continue;
                }
                items.push(InvestigationItem {
                    snapshot_dir: snapshot.display().to_string(),
                    manifest_path: manifest.display().to_string(),
                    ..item
                });
            }
            items.sort_by(|left, right| right.id.cmp(&left.id));
            let truncated = items.len() >= MAX_INVESTIGATIONS
                || (scanned >= MAX_INVESTIGATION_SCAN_DIRS && entries.next().is_some());
            let diagnostic = (truncated || rejected > 0).then(|| {
                format!(
                    "investigation list loaded {} entries{}{}",
                    items.len(),
                    if truncated {
                        "; listing limit reached"
                    } else {
                        ""
                    },
                    if rejected > 0 {
                        format!("; rejected {rejected} invalid records")
                    } else {
                        String::new()
                    }
                )
            });
            Ok(InvestigationLoadResult { items, diagnostic })
        })();
        let _ = sender.send(loaded);
    });
    InvestigationLoadJob {
        result,
        worker: Some(worker),
    }
}

fn start_succeeded(app: &mut App, origin: &StartOrigin, view_id: &str) {
    match origin {
        StartOrigin::Manual(request) => app.source_request_succeeded(request, view_id),
        StartOrigin::Discovery { generation } => {
            app.discovery_selection_succeeded(*generation, view_id);
        }
        StartOrigin::Ai { generation } => app.source_ai_launch_succeeded(*generation, view_id),
    }
}

fn memory_notice(app: &mut App, error: String) {
    app.source_notice = Some(format!(
        "memory error: {error}; raw browsing remains available"
    ));
}

/// The chain as a recipe stores it: expression steps by source, command
/// steps by program, arguments and environment (docs/command-enrichment.md).
/// Results never travel with a recipe.
fn recipe_extraction_stages(config: &lvu::RecipeConfig) -> Vec<lvu_memory::StageDefinition> {
    let definitions = config.enrichments.clone();
    definitions
        .into_iter()
        .map(|stage| match stage.command {
            Some(command) => lvu_memory::StageDefinition::Command {
                id: stage.id.0,
                name: stage.source,
                command,
            },
            None => lvu_memory::StageDefinition::Extraction {
                id: stage.id.0,
                source: stage.source,
            },
        })
        .collect()
}

fn recipe_item(recipe: lvu_memory::RecipeFile) -> lvu::RecipeItem {
    let incompatibility = recipe_incompatibility(&recipe.view);
    let enrichments = recipe
        .view
        .stages
        .iter()
        .map(|stage| match stage {
            lvu_memory::StageDefinition::Extraction { id, source } => {
                lvu::EnrichmentDefinition::expression(id.clone(), source.clone())
            }
            lvu_memory::StageDefinition::Polars {
                id,
                expression,
                output,
            } => lvu::EnrichmentDefinition::expression(
                id.to_string(),
                format!("{output} = {expression}"),
            ),
            lvu_memory::StageDefinition::Command { id, name, command } => {
                lvu::EnrichmentDefinition::command(
                    id.clone(),
                    if name.is_empty() {
                        lvu::app::DEFAULT_COMMAND_STEP_NAME.to_owned()
                    } else {
                        name.clone()
                    },
                    command.clone(),
                )
            }
        })
        .collect::<Vec<_>>();
    let enrichment = enrichments
        .iter()
        .rev()
        .find(|stage| !stage.is_command())
        .map_or_else(String::new, |stage| stage.source.clone());
    let color_field = recipe
        .view
        .color_rules
        .iter()
        .find(|rule| rule.style == "stable-value")
        .map(|rule| rule.expression.clone());
    let (capture_time, capture_time_policy) = match recipe.view.time_policy {
        lvu_memory::TimePolicy::Absolute {
            start_unix_nanos,
            end_unix_nanos,
        } => {
            let window = lvu::CaptureTimeRange {
                start_unix_nanos,
                end_unix_nanos,
            };
            (Some(window), Some(lvu::CaptureTimePolicy::Absolute(window)))
        }
        lvu_memory::TimePolicy::Recent { seconds } => {
            (None, Some(lvu::CaptureTimePolicy::Recent { seconds }))
        }
        lvu_memory::TimePolicy::All => (None, None),
    };
    lvu::RecipeItem {
        id: recipe.recipe_id.0.to_string(),
        revision: recipe.revision_id.to_string(),
        name: recipe.name,
        saved_at_unix_nanos: recipe.saved_at_unix_nanos,
        incompatibility,
        config: lvu::RecipeConfig {
            search: recipe.view.search,
            advanced: recipe
                .view
                .advanced_filter
                .map(|value| value.expression)
                .unwrap_or_default(),
            enrichment,
            enrichments,
            pinned_columns: recipe.view.pinned_columns,
            color_field,
            capture_time,
            capture_time_policy,
            time_basis: match recipe.view.time_basis {
                lvu_memory::TimeBasis::Capture => lvu::TimeBasis::Capture,
                lvu_memory::TimeBasis::Event => lvu::TimeBasis::Event,
                lvu_memory::TimeBasis::Extracted => lvu::TimeBasis::Extracted,
                // A recipe stores no field token, so it cannot name a field.
                lvu_memory::TimeBasis::Selected => lvu::TimeBasis::Capture,
            },
            grouping: recipe.view.grouping.unwrap_or_default(),
        },
    }
}

fn unix_now_nanos() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_nanos()).ok())
        .unwrap_or(i64::MAX)
}

fn recipe_incompatibility(view: &lvu_memory::NamedViewDefinition) -> Option<String> {
    if matches!(
        view.time_policy,
        lvu_memory::TimePolicy::Recent { seconds: 0 }
    ) || matches!(
        view.time_policy,
        lvu_memory::TimePolicy::Recent { seconds }
            if seconds > i64::MAX as u64 / 1_000_000_000
    ) {
        Some("rolling time-window recipe duration is unsupported".to_owned())
    } else if view.pinned_columns.len() > 8 {
        Some("recipe has more than 8 pinned columns".to_owned())
    } else if view.color_rules.len() > 1
        || view
            .color_rules
            .iter()
            .any(|rule| rule.style != "stable-value")
    {
        Some("recipe uses unsupported color rules".to_owned())
    } else if view.stages.len() > 32 {
        Some("recipe has more than 32 enrichment steps".to_owned())
    } else {
        None
    }
}

fn reconcile_pending_state(
    pending: &mut HashMap<lvu_core::ViewId, PendingMemorySave>,
    durable: &HashMap<lvu_core::ViewId, lvu::PersistentViewState>,
    inflight: &HashMap<u64, (lvu_core::ViewId, lvu::PersistentViewState)>,
    failed: &HashMap<lvu_core::ViewId, lvu::PersistentViewState>,
    view_id: lvu_core::ViewId,
    current: &lvu::PersistentViewState,
) -> bool {
    if pending
        .get(&view_id)
        .is_some_and(|value| value.request.state != *current)
    {
        pending.remove(&view_id);
    }
    durable.get(&view_id) == Some(current)
        || pending
            .get(&view_id)
            .is_some_and(|value| value.request.state == *current)
        || inflight
            .values()
            .any(|(id, state)| *id == view_id && state == current)
        || failed.get(&view_id) == Some(current)
}

fn start_failed(app: &mut App, origin: StartOrigin, message: String) {
    match origin {
        StartOrigin::Manual(request) => app.source_request_failed(request, message),
        StartOrigin::Discovery { generation } => {
            app.discovery_selection_failed(generation, message);
        }
        StartOrigin::Ai { generation } => app.source_ai_launch_failed(generation, message),
    }
}

fn complete_path(
    request: PathCompletionRequest,
    cwd: &Path,
    home: Option<&Path>,
    cancel: &AtomicBool,
) -> PathCompletionResult {
    let finish = |replacement, candidates, error| PathCompletionResult {
        generation: request.generation,
        draft: request.draft.clone(),
        replacement,
        candidates,
        error,
    };
    if request.draft == "~" {
        return finish(Some("~/".into()), Vec::new(), None);
    }
    if cancel.load(Ordering::Acquire) {
        return finish(None, Vec::new(), None);
    }

    let (display_parent, prefix) = split_completion_input(&request.draft);
    let resolved_parent = if display_parent == "~/" {
        match home {
            Some(home) => home.to_path_buf(),
            None => return finish(None, Vec::new(), Some("HOME is not available".into())),
        }
    } else if let Some(relative) = display_parent.strip_prefix("~/") {
        match home {
            Some(home) => home.join(relative),
            None => return finish(None, Vec::new(), Some("HOME is not available".into())),
        }
    } else {
        let path = Path::new(display_parent);
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            cwd.join(path)
        }
    };

    let entries = match std::fs::read_dir(&resolved_parent) {
        Ok(entries) => entries,
        Err(error) => {
            return finish(
                None,
                Vec::new(),
                Some(format!(
                    "cannot list {}: {error}",
                    resolved_parent.display()
                )),
            );
        }
    };
    let mut matches = Vec::new();
    let mut truncated = false;
    for (inspected, entry) in entries.enumerate() {
        if cancel.load(Ordering::Acquire) {
            return finish(None, Vec::new(), None);
        }
        if inspected == MAX_PATH_ENTRIES {
            truncated = true;
            break;
        }
        let Ok(entry) = entry else { continue };
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if !name.starts_with(prefix) {
            continue;
        }
        if matches.len() == MAX_PATH_CANDIDATES {
            truncated = true;
            break;
        }
        let slash = entry.path().is_dir();
        matches.push(format!(
            "{display_parent}{name}{}",
            if slash { "/" } else { "" }
        ));
    }
    matches.sort();
    if matches.is_empty() {
        return finish(None, matches, Some("no matching paths".into()));
    }
    let replacement = if matches.len() == 1 {
        Some(matches[0].clone())
    } else {
        let common = common_prefix(&matches);
        (common.len() > request.draft.len()).then_some(common)
    };
    let status = truncated.then(|| format!("showing first {MAX_PATH_CANDIDATES} matches"));
    finish(replacement, matches, status)
}

fn expand_tilde_path(input: &str, home: Option<&Path>) -> Result<PathBuf, String> {
    if input == "~" {
        return home
            .map(Path::to_path_buf)
            .ok_or_else(|| "HOME is not available".into());
    }
    if let Some(relative) = input.strip_prefix("~/") {
        return home
            .map(|home| home.join(relative))
            .ok_or_else(|| "HOME is not available".into());
    }
    if input.starts_with('~') {
        return Err("only ~/ home expansion is supported".into());
    }
    Ok(PathBuf::from(input))
}

fn split_completion_input(input: &str) -> (&str, &str) {
    match input.rfind('/') {
        Some(index) => input.split_at(index + 1),
        None => ("", input),
    }
}

fn common_prefix(values: &[String]) -> String {
    let Some(first) = values.first() else {
        return String::new();
    };
    let mut prefix = first.clone();
    for value in &values[1..] {
        prefix = prefix
            .chars()
            .zip(value.chars())
            .take_while(|(left, right)| left == right)
            .map(|(character, _)| character)
            .collect();
    }
    prefix
}

fn discovery_request(root: PathBuf, cancel: CancellationToken) -> DiscoveryRequest {
    DiscoveryRequest {
        limits: DiscoveryLimits {
            maximum_candidates: MAX_DISCOVERY_CANDIDATES,
            maximum_processes: 256,
            maximum_files: 512,
            // Enough for an ordinary process's whole descriptor table, so the
            // budget is spent on breadth rather than on one noisy neighbour.
            maximum_files_per_process: 32,
            maximum_output_bytes: 256 * 1024,
            maximum_duration: std::time::Duration::from_millis(1500),
        },
        cancel,
        docker: Some(DockerConfig::default()),
        procfs: Some(ProcConfig::default()),
        project: Some(ProjectConfig {
            roots: vec![root],
            recent_sources: Vec::new(),
            modified_within: std::time::Duration::from_secs(14 * 86400),
            maximum_depth: 6,
        }),
    }
}

fn spawn_source_ai_context_worker(
    cwd: PathBuf,
    directory: PathBuf,
    cancel: CancellationToken,
) -> (
    std_mpsc::Receiver<Result<SourceAiContext, String>>,
    JoinHandle<()>,
) {
    let (sender, result) = std_mpsc::sync_channel(1);
    let worker_cancel = cancel.clone();
    let worker = std::thread::spawn(move || {
        let outcome = (|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| format!("start source discovery worker: {error}"))?;
            let request = discovery_request(cwd.clone(), worker_cancel.clone());
            let discovered = runtime.block_on(lvu_discovery::discover(request));
            if worker_cancel.is_cancelled() {
                return Err("source agent context cancelled".into());
            }
            write_source_ai_context(directory, cwd, discovered, &worker_cancel)
        })();
        let _ = sender.send(outcome);
    });
    (result, worker)
}

fn write_source_ai_context(
    directory: PathBuf,
    cwd: PathBuf,
    discovered: DiscoveryResult,
    cancel: &CancellationToken,
) -> Result<SourceAiContext, String> {
    if cancel.is_cancelled() {
        return Err("source agent context cancelled".into());
    }
    if let Some(root) = directory.parent() {
        std::fs::create_dir_all(root)
            .map_err(|error| format!("create source agent context root: {error}"))?;
        let entries = std::fs::read_dir(root)
            .map_err(|error| format!("read source agent context root: {error}"))?
            .filter_map(Result::ok)
            .take(1025)
            .collect::<Vec<_>>();
        if entries.len() > 1024 {
            return Err("source agent context directory scan limit reached".into());
        }
        let count = entries
            .iter()
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("source-ai-")
            })
            .count();
        if count >= 64 {
            return Err(
                "source agent context limit reached (64); start with a fresh capture directory"
                    .into(),
            );
        }
    }
    std::fs::create_dir_all(&directory)
        .map_err(|error| format!("create source agent context: {error}"))?;
    let outcome = write_source_ai_manifest(&directory, &cwd, &discovered, cancel);
    if outcome.is_err() {
        let _ = std::fs::remove_file(directory.join(".manifest.json.tmp"));
        let _ = std::fs::remove_dir(&directory);
    }
    let manifest = outcome?;
    let directory = std::fs::canonicalize(&directory)
        .map_err(|error| format!("resolve source agent context: {error}"))?;
    Ok(SourceAiContext {
        directory,
        manifest,
        revision: OriginatingRevision {
            data: format!("discovery:{}", discovered.candidates.len()),
            definition: "source-dialog:1".into(),
        },
    })
}

fn write_source_ai_manifest(
    directory: &Path,
    cwd: &Path,
    discovered: &DiscoveryResult,
    cancel: &CancellationToken,
) -> Result<PathBuf, String> {
    #[derive(serde::Serialize)]
    struct Manifest<'a> {
        schema_version: u32,
        kind: &'static str,
        cwd: String,
        candidate_count: usize,
        candidates: Vec<&'a DiscoveryCandidate>,
        discovery_status: String,
        note: &'static str,
    }

    let manifest = directory.join("manifest.json");
    let temporary = directory.join(".manifest.json.tmp");
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(|error| format!("create source agent manifest: {error}"))?;
    let candidates = discovered
        .candidates
        .iter()
        .take(MAX_DISCOVERY_CANDIDATES)
        .collect::<Vec<_>>();
    let value = Manifest {
        schema_version: 1,
        kind: "source_discovery_context",
        cwd: cwd.display().to_string(),
        candidate_count: candidates.len(),
        candidates,
        discovery_status: discovery_status(discovered),
        note: "Read-only bounded discovery evidence; do not execute a proposed source during review.",
    };
    let mut writer = CappedWriter {
        inner: file,
        written: 0,
        limit: 2 * 1024 * 1024,
        cancel,
    };
    serde_json::to_writer_pretty(&mut writer, &value)
        .map_err(|error| format!("encode source agent context: {error}"))?;
    writer
        .inner
        .sync_all()
        .map_err(|error| format!("sync source agent context: {error}"))?;
    if cancel.is_cancelled() {
        return Err("source agent context cancelled".into());
    }
    std::fs::rename(&temporary, &manifest)
        .map_err(|error| format!("publish source agent context: {error}"))?;
    if cancel.is_cancelled() {
        let _ = std::fs::remove_file(&manifest);
        return Err("source agent context cancelled".into());
    }
    Ok(manifest)
}

fn cleanup_unstarted_source_ai_context(context: &SourceAiContext) {
    let _ = std::fs::remove_file(&context.manifest);
    let _ = std::fs::remove_dir(&context.directory);
}

struct CappedWriter<'a, W> {
    inner: W,
    written: usize,
    limit: usize,
    cancel: &'a CancellationToken,
}

impl<W: Write> Write for CappedWriter<'_, W> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self.cancel.is_cancelled() {
            // write_all retries Interrupted; cancellation must stop serialization.
            return Err(std::io::Error::other("source agent context cancelled"));
        }
        if bytes.len() > self.limit.saturating_sub(self.written) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::FileTooLarge,
                "source agent context exceeds 2 MiB",
            ));
        }
        let count = self.inner.write(bytes)?;
        self.written += count;
        Ok(count)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

fn parse_source_proposal(
    proposal: &ProposalEnvelope,
    application_cwd: &Path,
) -> Result<(SourceDefinition, SourceAiPreview), String> {
    let definition: SourceDefinition = serde_json::from_value(proposal.definition.clone())
        .map_err(|error| format!("invalid source definition: {error}"))?;
    validate_source_definition_for_launch(&definition)?;
    let (kind, launch, effective_path_or_cwd, restart, environment) = match &definition.acquisition
    {
        Acquisition::File { path, follow } => {
            let effective = if path.is_absolute() {
                path.clone()
            } else {
                application_cwd.join(path)
            };
            (
                "file".to_owned(),
                format!("{} (follow: {follow})", path.display()),
                if path.is_absolute() {
                    effective.display().to_string()
                } else {
                    format!(
                        "base {} -> {}",
                        application_cwd.display(),
                        effective.display()
                    )
                },
                "not applicable".to_owned(),
                Vec::new(),
            )
        }
        Acquisition::Command { command } => {
            let launch = match &command.program {
                CommandProgram::Shell { text } => format!("sh -c {text:?}"),
                CommandProgram::Exec { executable, args } => {
                    serde_json::to_string(&serde_json::json!({
                        "executable": executable,
                        "args": args,
                    }))
                    .map_err(|error| format!("render command preview: {error}"))?
                }
            };
            let effective_cwd = command
                .cwd
                .as_ref()
                .map(|cwd| {
                    if cwd.is_absolute() {
                        cwd.clone()
                    } else {
                        application_cwd.join(cwd)
                    }
                })
                .unwrap_or_else(|| application_cwd.to_path_buf());
            let environment = command
                .environment
                .iter()
                .map(|(key, value)| format!("{key}={value}"))
                .collect();
            (
                "command".to_owned(),
                launch,
                effective_cwd.display().to_string(),
                "never".to_owned(),
                environment,
            )
        }
        Acquisition::Http { .. } => {
            return Err(
                "HTTP sources are not launchable in this preview; request file or command".into(),
            );
        }
        Acquisition::Stdin => {
            return Err("stdin can only be attached explicitly from the command line".into());
        }
    };
    Ok((
        definition.clone(),
        SourceAiPreview {
            name: definition.name,
            kind,
            launch,
            effective_path_or_cwd,
            restart,
            environment,
            explanation: proposal.explanation.clone(),
        },
    ))
}

fn validate_source_definition_for_launch(definition: &SourceDefinition) -> Result<(), String> {
    if definition.schema_version != 1 || definition.name.is_empty() || definition.name.len() > 256 {
        return Err("source name/schema is invalid".into());
    }
    match &definition.acquisition {
        Acquisition::File { path, .. } if path.as_os_str().is_empty() => {
            Err("file path is empty".into())
        }
        Acquisition::Command { command } => {
            if command.restart != RestartPolicy::Never {
                return Err(
                    "only restart policy 'never' is supported for reviewed agent commands".into(),
                );
            }
            if command.environment.len() > 64 {
                return Err("command environment exceeds 64 entries".into());
            }
            let environment_bytes = command
                .environment
                .iter()
                .map(|(key, value)| key.len().saturating_add(value.len()))
                .sum::<usize>();
            if environment_bytes > 32 * 1024
                || command
                    .environment
                    .iter()
                    .any(|(key, value)| key.is_empty() || key.len() > 256 || value.len() > 4096)
            {
                return Err("command environment exceeds review limits".into());
            }
            match &command.program {
                CommandProgram::Shell { text } if text.is_empty() || text.len() > 131_072 => {
                    Err("shell command is invalid".into())
                }
                CommandProgram::Exec { executable, args }
                    if executable.as_os_str().is_empty()
                        || args.len() > 256
                        || args.iter().any(|arg| arg.len() > 16_384) =>
                {
                    Err("executable or arguments exceed limits".into())
                }
                _ => Ok(()),
            }
        }
        Acquisition::Http { .. } => Err("HTTP sources are not supported by this runtime".into()),
        Acquisition::Stdin => {
            Err("stdin can only be attached explicitly from the command line".into())
        }
        _ => Ok(()),
    }
}

fn source_ai_generation(work: &SourceAiWork) -> u64 {
    match work {
        SourceAiWork::Preparing { start, .. }
        | SourceAiWork::Starting { start, .. }
        | SourceAiWork::Proposing { start, .. } => start.generation,
        SourceAiWork::Cancelling { generation, .. }
        | SourceAiWork::Unresolved { generation, .. } => *generation,
    }
}

fn discovery_item(candidate: &DiscoveryCandidate) -> DiscoveryItem {
    let acquisition = match &candidate.source.acquisition {
        Acquisition::File { path, .. } => path.display().to_string(),
        Acquisition::Command { .. } => candidate
            .identity_hints
            .get("compose_service")
            .or_else(|| candidate.identity_hints.get("container_name"))
            .map_or_else(|| "managed command source".into(), |value| value.clone()),
        Acquisition::Http { url, .. } => url.clone(),
        Acquisition::Stdin => "one-shot standard input".into(),
    };
    let evidence = candidate
        .evidence
        .iter()
        .take(2)
        .map(|evidence| {
            let observed = ["status", "state", "path", "service"]
                .into_iter()
                .filter_map(|key| {
                    evidence
                        .attributes
                        .get(key)
                        .map(|value| format!("{key}={value}"))
                })
                .collect::<Vec<_>>()
                .join(", ");
            if observed.is_empty() {
                evidence.summary.clone()
            } else {
                format!("{} ({observed})", evidence.summary)
            }
        })
        .collect::<Vec<_>>()
        .join("; ");
    DiscoveryItem {
        key: candidate.fingerprint.clone(),
        label: candidate.display_label.clone(),
        detail: if evidence.is_empty() {
            acquisition
        } else {
            format!("{acquisition} — {evidence}")
        },
        status: format!(
            "{:?} {:?} {:?}",
            candidate.provider, candidate.confidence, candidate.availability
        ),
    }
}

fn recent_key(id: SourceId) -> String {
    format!("recent:{}", id.0)
}

fn recent_discovery_item(source: &lvu_memory::SourceMetadata) -> DiscoveryItem {
    let detail = match &source.definition.acquisition {
        Acquisition::File { path, .. } => path.display().to_string(),
        Acquisition::Command { command } => format!("{command:?}"),
        Acquisition::Http { url, .. } => url.clone(),
        Acquisition::Stdin => "one-shot standard input".into(),
    };
    DiscoveryItem {
        key: recent_key(source.definition.id),
        label: source.definition.name.clone(),
        detail: format!("{detail} — remembered source"),
        status: if source.missing {
            "Remembered Missing".into()
        } else {
            "Remembered Unknown".into()
        },
    }
}

fn discovery_status(result: &DiscoveryResult) -> String {
    let providers = result
        .statuses
        .iter()
        .map(provider_status)
        .collect::<Vec<_>>()
        .join("; ");
    let ending = if result.cancelled {
        "cancelled"
    } else if result.timed_out {
        "time limit reached"
    } else if result.candidates.is_empty() {
        "no candidates"
    } else {
        "complete"
    };
    format!(
        "{} candidates, {ending}; {providers}",
        result.candidates.len()
    )
}

fn provider_status(status: &ProviderStatus) -> String {
    format!(
        "{:?} {:?}: {}",
        status.provider, status.state, status.message
    )
}

fn suggestion_context(
    app: &App,
    adapter: &NativeViewAdapter,
    definitions: &HashMap<SourceId, SourceDefinition>,
    cwd: &Path,
) -> Option<SuggestionContext> {
    let view_id = app.active_view_id()?;
    suggestion_context_for_view(app, adapter, definitions, cwd, view_id)
}

fn suggestion_context_for_view(
    app: &App,
    adapter: &NativeViewAdapter,
    definitions: &HashMap<SourceId, SourceDefinition>,
    cwd: &Path,
    view_id: &str,
) -> Option<SuggestionContext> {
    let view = app.views().iter().find(|view| view.id == view_id)?;
    let source = SourceId(Uuid::parse_str(&view.source_id).ok()?);
    let definition = definitions.get(&source)?;
    // Suggestions are built from sampled fields; folding is presentation and
    // must not change which rows are sampled.
    let rows = adapter
        .rows()
        .unfolded_page(view_id, ViewportRequest { start: 0, len: 128 });
    let mut fields = BTreeMap::new();
    for row in rows.rows {
        for (name, value) in row.fields.into_iter().take(32) {
            if fields.len() >= 128 {
                break;
            }
            fields
                .entry(name)
                // DisplayRow values have already lost JSON scalar typing. Keep
                // this explicitly lexical: names are useful similarity hints,
                // but `"200"` and `200` must never become schema evidence.
                .or_insert_with(|| lexical_display_hint(&value).into());
        }
    }
    let command = match &definition.acquisition {
        Acquisition::File { path, .. } => Some(path.to_string_lossy().into_owned()),
        Acquisition::Command { command } => Some(format!("{command:?}")),
        Acquisition::Http { url, .. } => Some(url.clone()),
        Acquisition::Stdin => None,
    };
    Some(SuggestionContext {
        source,
        project: Some(cwd.to_string_lossy().into_owned()),
        command,
        fields,
    })
}

fn lexical_display_hint(_value: &str) -> &'static str {
    "display-text"
}

fn settings_values(value: &settings::Settings) -> SettingsValues {
    SettingsValues {
        provider: value.paseo.provider.clone(),
        mode: value.paseo.mode.clone(),
        thinking: value.paseo.thinking.clone(),
        theme: match value.appearance.theme {
            settings::Theme::Terminal => ThemeId::Terminal,
            settings::Theme::LoveDark => ThemeId::LoveDark,
            settings::Theme::LoveLight => ThemeId::LoveLight,
            settings::Theme::Dracula => ThemeId::Dracula,
            settings::Theme::Nord => ThemeId::Nord,
            settings::Theme::GruvboxDark => ThemeId::GruvboxDark,
        },
        delight_enabled: value.appearance.delight_enabled,
        reduced_motion: value.appearance.reduced_motion,
        ascii: value.appearance.ascii,
        rows_mib: value.cache.memory.rows_mib.to_string(),
        membership_mib: value.cache.memory.membership_mib.to_string(),
        disk_total_mib: value.cache.disk.total_mib.to_string(),
        index_per_source_mib: value.cache.disk.index_per_source_mib.to_string(),
    }
}

fn value_source_label(source: &settings::ValueSource) -> String {
    match source {
        settings::ValueSource::Default => "default".into(),
        settings::ValueSource::GlobalFile => "settings.toml".into(),
        settings::ValueSource::Environment(name) => format!("environment {name}"),
    }
}

fn settings_context(
    loaded: &settings::LoadedSettings,
    effective: &settings::EffectiveSettings,
    paths: &settings::AppPaths,
    capture_root: &Path,
    applied: &settings::ValidatedSettings,
) -> SettingsContext {
    SettingsContext {
        saved: settings_values(&loaded.validated.settings),
        effective_provider: effective.provider.value.clone(),
        effective_mode: effective.mode.value.clone(),
        effective_thinking: effective.thinking.value.clone(),
        effective_theme: match effective.theme.value {
            settings::Theme::Terminal => ThemeId::Terminal,
            settings::Theme::LoveDark => ThemeId::LoveDark,
            settings::Theme::LoveLight => ThemeId::LoveLight,
            settings::Theme::Dracula => ThemeId::Dracula,
            settings::Theme::Nord => ThemeId::Nord,
            settings::Theme::GruvboxDark => ThemeId::GruvboxDark,
        },
        effective_delight_enabled: effective.delight_enabled.value,
        effective_reduced_motion: effective.reduced_motion.value,
        effective_ascii: effective.ascii.value,
        provider_source: value_source_label(&effective.provider.source),
        mode_source: value_source_label(&effective.mode.source),
        thinking_source: value_source_label(&effective.thinking.source),
        delight_source: value_source_label(&effective.delight_enabled.source),
        reduced_motion_source: value_source_label(&effective.reduced_motion.source),
        ascii_source: value_source_label(&effective.ascii.source),
        settings_path: paths.settings_file.display().to_string(),
        data_path: paths.data_dir.display().to_string(),
        cache_path: paths.cache_dir.display().to_string(),
        capture_path: capture_root.display().to_string(),
        applied_rows_mib: applied.settings.cache.memory.rows_mib,
        applied_membership_mib: applied.settings.cache.memory.membership_mib,
        applied_disk_total_mib: applied.settings.cache.disk.total_mib,
        applied_index_per_source_mib: applied.settings.cache.disk.index_per_source_mib,
    }
}

fn parse_mib(field: &'static str, value: &str) -> Result<u64, String> {
    value
        .parse::<u64>()
        .map_err(|_| format!("{field} must be a positive integer MiB value"))
}

fn save_settings_request(
    request: SettingsRequest,
    path: &Path,
    paths: &settings::AppPaths,
    capture_root: &Path,
    applied: &settings::ValidatedSettings,
) -> Result<SettingsContext, String> {
    let value = request.values;
    let settings = settings::Settings {
        schema_version: settings::SETTINGS_SCHEMA_VERSION,
        paseo: settings::PaseoSettings {
            provider: value.provider,
            mode: value.mode,
            thinking: value.thinking,
        },
        appearance: settings::AppearanceSettings {
            theme: match value.theme {
                ThemeId::Terminal => settings::Theme::Terminal,
                ThemeId::LoveDark => settings::Theme::LoveDark,
                ThemeId::LoveLight => settings::Theme::LoveLight,
                ThemeId::Dracula => settings::Theme::Dracula,
                ThemeId::Nord => settings::Theme::Nord,
                ThemeId::GruvboxDark => settings::Theme::GruvboxDark,
            },
            delight_enabled: value.delight_enabled,
            reduced_motion: value.reduced_motion,
            ascii: value.ascii,
        },
        cache: settings::CacheSettings {
            memory: settings::MemoryCacheSettings {
                rows_mib: parse_mib("row cache", &value.rows_mib)?,
                membership_mib: parse_mib("membership", &value.membership_mib)?,
            },
            disk: settings::DiskCacheSettings {
                total_mib: parse_mib("derived total", &value.disk_total_mib)?,
                index_per_source_mib: parse_mib(
                    "derived index per source",
                    &value.index_per_source_mib,
                )?,
            },
        },
        // The Settings screen does not edit retention yet. Carrying the applied
        // section forward keeps a save from silently clearing a configured
        // retention policy out of the user's TOML.
        storage: applied.settings.storage.clone(),
    };
    settings::save_settings(path, &settings).map_err(|error| error.to_string())?;
    let loaded = settings::load_settings(path).map_err(|error| error.to_string())?;
    let effective = loaded.effective().map_err(|error| error.to_string())?;
    Ok(settings_context(
        &loaded,
        &effective,
        paths,
        capture_root,
        applied,
    ))
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() {
    let result = run().await;
    if let Err(error) = result {
        eprintln!("lvu-app: {error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), String> {
    let arguments: Vec<OsString> = env::args_os().skip(1).collect();
    if arguments
        .first()
        .is_some_and(|value| value == "--resources")
    {
        // Packaging verification seam: report resolution without a terminal.
        print!("{}", resources::report());
        return Ok(());
    }
    let Some(options) = parse_args(arguments, std::io::stdin().is_terminal())? else {
        print_help();
        return Ok(());
    };
    ensure_controlling_terminal()?;
    let cwd = env::current_dir().map_err(|error| format!("current directory: {error}"))?;
    let paths = settings::resolve_paths().map_err(|error| error.to_string())?;
    // Decide the capture root before anything can materialise a candidate root.
    // The assistance bridge used to create `$XDG_DATA_HOME/lvu/assistance` on
    // every launch, which then made the next launch abandon a legacy root and
    // its whole workspace. Every dependent path derives from this decision.
    let choice = select_capture_root(options.capture_dir, &paths.data_dir, &cwd)?;
    let capture_dir = choice.root.clone();
    let record_error = choice
        .record
        .as_ref()
        .and_then(|pointer| record_capture_root(pointer, &capture_dir).err());
    let legacy_notice = choice.notice(&paths.data_dir, record_error.as_deref());
    let owned_assistance_root = capture_dir.join("assistance");
    if !owned_assistance_root.is_absolute() {
        return Err(format!(
            "assistance storage root must be absolute: {}",
            owned_assistance_root.display()
        ));
    }
    let loaded_settings = settings::load_settings(&paths.settings_file)
        .map_err(|error| format!("load {}: {error}", paths.settings_file.display()))?;
    let effective_settings = loaded_settings
        .effective()
        .map_err(|error| format!("effective settings: {error}"))?;
    let row_cache_bytes = usize::try_from(effective_settings.row_cache_bytes.value)
        .map_err(|_| "row cache setting exceeds this platform".to_owned())?;
    let mut live_config = LiveConfig::new(paths.cache_dir.join("derived"));
    live_config.maximum_request_rows = 256;
    live_config.cache_bytes = row_cache_bytes;
    live_config.maximum_index_bytes_per_source = effective_settings.index_per_source_bytes.value;
    live_config.maximum_total_index_bytes = effective_settings.disk_total_bytes.value;
    let raw = Arc::new(
        LiveRowProvider::new(live_config).map_err(|error| format!("live row provider: {error}"))?,
    );
    let manager = Arc::new(
        SourceManager::new(&capture_dir, RuntimeConfig::default())
            .map_err(|error| format!("capture manager: {error}"))?,
    );
    // Resolve once during startup. Terminal ticks and agent workers only see
    // absolute snapshot/session paths, regardless of a relative --capture-dir.
    let snapshot_root = match std::fs::canonicalize(&capture_dir) {
        Ok(root) => root.join("investigations"),
        Err(error) => {
            let cleanup = cleanup(raw.as_ref(), &manager).await;
            return Err(combine_errors(
                format!("capture directory: {error}"),
                cleanup,
            ));
        }
    };
    let (helper_resource, bridge_resource) = resources::resolve_all();
    let mut view_config = ViewConfig::new(paths.cache_dir.join("views"));
    // Absent helper leaves advanced expressions unconfigured rather than
    // spawning a command built from a checkout that may not exist here.
    view_config.compiler = helper_resource.located().map(compiler_config);
    view_config.maximum_index_bytes = effective_settings.membership_bytes.value;
    let query_index_limit = view_config.maximum_index_bytes;
    let mut adapter = match NativeViewAdapter::new(Arc::clone(&raw), view_config) {
        Ok(adapter) => adapter,
        Err(error) => {
            let cleanup = cleanup(raw.as_ref(), &manager).await;
            return Err(combine_errors(
                format!("native view adapter: {error}"),
                cleanup,
            ));
        }
    };
    let mut app = App::new(Vec::new(), Vec::new(), false);
    app.title = "lvu live sources".into();
    app.show_startup_title = options.sources.is_empty();
    app.configure_ai(
        effective_settings.provider.value.clone(),
        effective_settings.mode.value.clone(),
        effective_settings.thinking.value.clone(),
    );
    app.configure_appearance(
        match effective_settings.theme.value {
            settings::Theme::Terminal => ThemeId::Terminal,
            settings::Theme::LoveDark => ThemeId::LoveDark,
            settings::Theme::LoveLight => ThemeId::LoveLight,
            settings::Theme::Dracula => ThemeId::Dracula,
            settings::Theme::Nord => ThemeId::Nord,
            settings::Theme::GruvboxDark => ThemeId::GruvboxDark,
        },
        effective_settings.delight_enabled.value,
        effective_settings.reduced_motion.value,
        effective_settings.ascii.value,
    );
    app.configure_settings(settings_context(
        &loaded_settings,
        &effective_settings,
        &paths,
        &capture_dir,
        &loaded_settings.validated,
    ));
    app.source_notice = legacy_notice.or_else(|| helper_resource.diagnostic());
    let mut source_ids = HashMap::new();
    let mut definitions = HashMap::new();
    let mut startup_error = None;
    for argument in options.sources {
        let is_stdin = matches!(argument, SourceArgument::Stdin);
        let definition = match definition(argument, &cwd) {
            Ok(definition) => definition,
            Err(error) => {
                startup_error = Some(error);
                break;
            }
        };
        if source_ids.contains_key(&definition.id) {
            continue;
        }
        let started = match if is_stdin {
            start_stdin_definition(&manager, definition).await
        } else {
            start_definition(&manager, definition).await
        } {
            Ok(started) => started,
            Err(error) => {
                startup_error = Some(error);
                break;
            }
        };
        let source_id = started.definition.id;
        definitions.insert(source_id, started.definition.clone());
        if let Err(error) = register_started(&adapter, &mut app, &mut source_ids, started) {
            if let Some(handle) = manager.source(source_id) {
                let _ = handle.stop().await;
            }
            startup_error = Some(error);
            break;
        }
    }
    if let Some(error) = startup_error {
        adapter.shutdown();
        let cleanup = cleanup(raw.as_ref(), &manager).await;
        return Err(combine_errors(error, cleanup));
    }
    if !app.views().is_empty() {
        app.close_source_layer();
    }

    let (starts_tx, starts_rx) = mpsc::channel(MAX_PENDING_STARTS);
    let (scans_tx, scans_rx) = mpsc::channel(2);
    let (completions_tx, completions_rx) = mpsc::channel(2);
    let memory = MemoryWorker::start(capture_dir.join("workspace"));
    let command_presentation = command_rows::CommandPresentation::default();
    let command_controller = command_controller::CommandController::new(
        capture_dir.join("workspace"),
        cwd.clone(),
        command_presentation.clone(),
    );
    let recent_error = memory.recent().err();
    let (agent, agent_error) = match bridge_resource.located() {
        Some(bridge) => {
            match AgentBridgeHost::launch(agent_config(bridge, &cwd, &owned_assistance_root)) {
                Ok(host) => (Some(host), None),
                Err(error) => (None, Some(agent::diagnose(&error))),
            }
        }
        None => (None, bridge_resource.diagnostic()),
    };
    let mut composition = Composition {
        manager: Arc::clone(&manager),
        raw: Arc::clone(&raw),
        runtime: tokio::runtime::Handle::current(),
        starts_tx,
        starts_rx,
        sources: source_ids,
        definitions,
        pending_starts: HashSet::new(),
        source_controls: HashMap::new(),
        cwd,
        scans_tx,
        scans_rx,
        active_scan: None,
        pending_scan: None,
        discovery_candidates: HashMap::new(),
        recent_sources: Vec::new(),
        memory,
        memory_ready: HashSet::new(),
        memory_restoring: HashSet::new(),
        memory_deferred: HashMap::new(),
        memory_load_fences: HashMap::new(),
        memory_last: HashMap::new(),
        memory_unavailable: false,
        memory_pending: HashMap::new(),
        memory_inflight: HashMap::new(),
        memory_failed: HashMap::new(),
        memory_ack_sequence: HashMap::new(),
        memory_sequence: 0,
        completions_tx,
        completions_rx,
        active_completion: None,
        pending_completion: None,
        home: env::var_os("HOME").map(PathBuf::from),
        snapshot_root: snapshot_root.clone(),
        agent,
        agent_error,
        active_ai: None,
        owned_ai_session: None,
        owned_ai_session_config: None,
        retire_ai_session: false,
        ai_session_busy: false,
        source_ai_work: None,
        source_ai_session: None,
        source_ai_session_config: None,
        deferred_owned_lifecycle_events: VecDeque::new(),
        deferred_owned_lifecycle_overflowed: false,
        source_ai_proposals: HashMap::new(),
        session_records: Vec::new(),
        investigation_work: None,
        investigation_session: None,
        investigation_load: Some(load_investigations(snapshot_root.clone())),
        storage_root: capture_dir.clone(),
        storage_job: None,
        pending_storage: None,
        query_index_limit,
        storage_review: Vec::new(),
        settings_file: paths.settings_file.clone(),
        settings_paths: paths,
        applied_settings: loaded_settings.validated,
        settings_job: None,
        capture_root: capture_dir,
        command_controller,
    };
    if let Some(error) = recent_error {
        app.source_notice = Some(format!(
            "memory error: {error}; raw browsing remains available"
        ));
    }
    for definition in composition
        .definitions
        .values()
        .cloned()
        .collect::<Vec<_>>()
    {
        if let Err(error) = composition.request_restore(&app, definition) {
            app.source_notice = Some(format!(
                "memory error: {error}; raw browsing remains available"
            ));
        }
    }
    let mut rows = command_rows::CommandRows {
        native: adapter.rows(),
        presentation: command_presentation,
    };
    let terminal_result = run_with_tick_mut(
        &mut app,
        &mut rows,
        &mut adapter,
        |_| false,
        |app, _rows, adapter| composition.tick(app, adapter),
    );
    composition.cancel_discovery();
    let mut timing = ShutdownTiming::start();
    // Independent subsystems settle together. Each keeps its own deadline, so a
    // subsystem that is stuck still costs only its own budget rather than
    // pushing every later one back: run in sequence these three alone bounded
    // shutdown at eight seconds, and one wedged worker spent all of it.
    let (storage_shutdown_result, settings_shutdown_result, command_shutdown_result) = {
        let storage = composition.storage_job.take();
        let settings = composition.settings_job.take();
        let controller = &mut composition.command_controller;
        settle_together(
            move || storage.map_or(Ok(()), |mut job| job.settle(Duration::from_secs(3))),
            move || settings.map_or(Ok(()), |mut job| job.settle(Duration::from_secs(2))),
            || controller.shutdown(Duration::from_secs(3)),
        )
    };
    timing.mark("storage+settings+command");
    let investigation_shutdown_result = composition.shutdown_investigation(Duration::from_secs(3));
    timing.mark("investigation");
    let source_ai_shutdown_result = composition.shutdown_source_ai(Duration::from_secs(3));
    timing.mark("source-ai");
    let ai_shutdown_result = composition.shutdown_ai(Duration::from_secs(3));
    timing.mark("ai");
    // A chain change the user submitted just before quitting (a command
    // step's save, for one) is a query in flight; its answer is what the
    // final autosave below should persist, so give it a bounded moment.
    settle_pending_queries(&mut app, &mut adapter, Duration::from_millis(1500));
    timing.mark("pending-queries");
    let command_persistence_result =
        composition.flush_command_persistence(&mut app, &adapter, Duration::from_millis(500));
    timing.mark("command-persistence");
    let memory_flush_result =
        composition.flush_memory(&mut app, &adapter, std::time::Duration::from_millis(500));
    timing.mark("memory-flush");
    composition.memory.stop();
    adapter.shutdown();
    timing.mark("view-adapter");
    let cleanup_result = cleanup(raw.as_ref(), &manager).await;
    timing.mark("capture-cleanup");
    // Shutdown has closed manager admission and settled owned acquisitions. No
    // source-control task may launch a replacement after this point.
    for (_, job) in composition.source_controls.drain() {
        job.worker.abort();
        let _ = job.worker.await;
    }
    timing.mark("source-controls");
    timing.report();
    let lifecycle_error = memory_flush_result
        .err()
        .into_iter()
        .chain(investigation_shutdown_result.err())
        .chain(source_ai_shutdown_result.err())
        .chain(ai_shutdown_result.err())
        .chain(command_shutdown_result.err())
        .chain(command_persistence_result.err())
        .chain(storage_shutdown_result.err())
        .chain(settings_shutdown_result.err())
        .collect::<Vec<_>>()
        .join("; ");
    let terminal_result = if lifecycle_error.is_empty() {
        terminal_result
    } else {
        terminal_result.and(Err(std::io::Error::other(lifecycle_error)))
    };
    match (terminal_result, cleanup_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) => Err(format!("terminal: {error}")),
        (Ok(()), Err(error)) => Err(error),
        (Err(terminal), Err(cleanup)) => Err(format!("terminal: {terminal}; {cleanup}")),
    }
}

/// Submits queued query requests and applies their completions until no view
/// has a query in flight or `timeout` passes. Bounded: a worker that never
/// answers costs this much and no more.
fn settle_pending_queries(app: &mut App, adapter: &mut NativeViewAdapter, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        lvu::terminal::submit_query_requests(app, adapter);
        adapter.drain_updates(MAX_TICK_UPDATES);
        lvu::terminal::poll_query_completions(app, adapter);
        let pending = app
            .views()
            .iter()
            .any(|view| app.view_has_pending_query(&view.id));
        if !pending || Instant::now() >= deadline {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[cfg(unix)]
fn ensure_controlling_terminal() -> Result<(), String> {
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
        .map(|_| ())
        .map_err(|error| format!("controlling terminal unavailable: {error}"))
}

#[cfg(not(unix))]
fn ensure_controlling_terminal() -> Result<(), String> {
    Ok(())
}

async fn start_definition(
    manager: &SourceManager,
    definition: SourceDefinition,
) -> Result<StartedSource, String> {
    let view_id = view_id(definition.id);
    let handle = manager
        .start(definition.clone())
        .await
        .map_err(|error| format!("start {}: {error}", definition.name))?;
    Ok(StartedSource {
        definition,
        view_id,
        handle,
        origin: None,
    })
}

async fn start_stdin_definition(
    manager: &SourceManager,
    definition: SourceDefinition,
) -> Result<StartedSource, String> {
    let view_id = view_id(definition.id);
    let reader = attached_stdin_reader()?;
    let handle = manager
        .start_with_reader(definition.clone(), reader)
        .await
        .map_err(|error| format!("start {}: {error}", definition.name))?;
    Ok(StartedSource {
        definition,
        view_id,
        handle,
        origin: None,
    })
}

#[cfg(unix)]
fn attached_stdin_reader() -> Result<Pin<Box<dyn AsyncRead + Send>>, String> {
    use std::os::{
        fd::{FromRawFd, RawFd},
        unix::fs::{FileTypeExt, MetadataExt},
    };

    // Crossterm independently opens /dev/tty when fd 0 is redirected. Duplicate
    // fd 0 so the capture task exclusively owns and closes its reader.
    let duplicated: RawFd = unsafe { libc::fcntl(libc::STDIN_FILENO, libc::F_DUPFD_CLOEXEC, 3) };
    if duplicated < 0 {
        return Err(format!(
            "attach redirected stdin: {}",
            std::io::Error::last_os_error()
        ));
    }
    let file = unsafe { std::fs::File::from_raw_fd(duplicated) };
    let metadata = file
        .metadata()
        .map_err(|error| format!("inspect redirected stdin: {error}"))?;
    let file_type = metadata.file_type();
    if file_type.is_fifo() {
        #[cfg(not(target_os = "linux"))]
        return Err(
            "stdin pipe capture requires isolated nonblocking descriptors on this platform".into(),
        );
        #[cfg(target_os = "linux")]
        let file = {
            use std::os::unix::fs::OpenOptionsExt;
            // F_DUPFD shares the pipe's open-file status flags. Tokio enables
            // O_NONBLOCK, which would otherwise leak to another holder of the
            // inherited endpoint. Reopening through procfs creates an isolated
            // open-file description while preserving the same pipe endpoint.
            drop(file);
            std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NONBLOCK)
                .open("/proc/self/fd/0")
                .map_err(|error| format!("isolate redirected stdin pipe: {error}"))?
        };
        let reader = tokio::net::unix::pipe::Receiver::from_file(file)
            .map_err(|error| format!("attach stdin pipe: {error}"))?;
        Ok(Box::pin(reader))
    } else if file_type.is_file() {
        Ok(Box::pin(tokio::fs::File::from_std(file)))
    } else if file_type.is_char_device() {
        let null = std::fs::metadata("/dev/null")
            .map_err(|error| format!("inspect /dev/null: {error}"))?;
        if metadata.dev() != null.dev() || metadata.rdev() != null.rdev() {
            return Err("redirected stdin character devices are not supported".into());
        }
        Ok(Box::pin(tokio::fs::File::from_std(file)))
    } else {
        Err("redirected stdin must be a pipe or regular file".into())
    }
}

#[cfg(not(unix))]
fn attached_stdin_reader() -> Result<Pin<Box<dyn AsyncRead + Send>>, String> {
    Err("stdin capture is not yet supported on this platform".into())
}

/// The identity a newly started source's canonical view is proposed under.
///
/// A workspace that predates roles keeps its own `working-view:` row as an
/// ordinary editable view; this is a different identity, so migration adds the
/// canonical view rather than converting one the user has been working in.
/// Maps a persisted role onto the application's view model.
fn view_role(role: lvu_memory::ViewRole) -> lvu::ViewRole {
    match role {
        lvu_memory::ViewRole::Canonical => lvu::ViewRole::Canonical,
        lvu_memory::ViewRole::Derived => lvu::ViewRole::Derived,
    }
}

fn view_id(source_id: SourceId) -> String {
    memory::canonical_view_id(source_id).0.to_string()
}

/// Why the rows on screen are not the whole answer, when that is worth saying.
///
/// The states a user cannot tell apart from a genuinely empty result: a read
/// that failed, a query that failed, rows that stopped arriving inside the retry
/// budget, and an index held by another owner. Without this the pane is blank
/// and the status line still reads "query ready".
/// Raw-row request counters for diagnosing a pane that never fills.
///
/// Off unless `LVU_ROW_DIAGNOSTICS` is set, because these are implementation
/// counters, not something an ordinary status line should carry. They separate
/// the three ways a missing row can be missing: a request that was never made
/// (`pending 0` while rows are absent), one refused by the bounded queue
/// (`dropped` rising), and one made but never answered (`pending` stuck).
/// Settles three independent subsystems at once, each on its own deadline.
///
/// Run in sequence, their deadlines add up: a subsystem that is wedged spends
/// its whole budget and every later one starts that much later, so the bound on
/// shutdown is the sum rather than the longest. They share no state, so the
/// only reason they were sequential is that they were written that way.
fn settle_together<A, B, C>(
    first: A,
    second: B,
    third: C,
) -> (Result<(), String>, Result<(), String>, Result<(), String>)
where
    A: FnOnce() -> Result<(), String> + Send,
    B: FnOnce() -> Result<(), String> + Send,
    C: FnOnce() -> Result<(), String>,
{
    std::thread::scope(|scope| {
        let first = scope.spawn(first);
        let second = scope.spawn(second);
        // The caller's thread takes the third rather than idling.
        let third = third();
        (
            first
                .join()
                .unwrap_or_else(|_| Err("shutdown worker panicked".to_owned())),
            second
                .join()
                .unwrap_or_else(|_| Err("shutdown worker panicked".to_owned())),
            third,
        )
    })
}

/// Where the time between `q` and the process exiting actually goes.
///
/// Shutdown settles a dozen independent subsystems, each with its own deadline.
/// Off unless `LVU_SHUTDOWN_TIMING` is set, because it is an implementation
/// breakdown, not something to print at a user; it exists so "quitting took
/// eight seconds" can be answered with which phase rather than a guess.
struct ShutdownTiming {
    enabled: bool,
    started: std::time::Instant,
    last: std::time::Instant,
    phases: Vec<(&'static str, Duration)>,
}

impl ShutdownTiming {
    fn start() -> Self {
        let now = std::time::Instant::now();
        if std::env::var_os("LVU_SHUTDOWN_TIMING").is_some() {
            // Printed the moment the terminal loop returns, so the time between
            // the keypress and this line is attributable to input handling
            // rather than to any of the settles that follow it.
            eprintln!("lvu-app shutdown begins");
        }
        Self {
            enabled: std::env::var_os("LVU_SHUTDOWN_TIMING").is_some(),
            started: now,
            last: now,
            phases: Vec::new(),
        }
    }

    fn mark(&mut self, phase: &'static str) {
        if !self.enabled {
            return;
        }
        let now = std::time::Instant::now();
        self.phases.push((phase, now.duration_since(self.last)));
        self.last = now;
    }

    fn report(&self) {
        if !self.enabled {
            return;
        }
        let total = self.last.duration_since(self.started);
        let detail = self
            .phases
            .iter()
            .map(|(phase, elapsed)| format!("{phase} {:.3}s", elapsed.as_secs_f64()))
            .collect::<Vec<_>>()
            .join(" ");
        eprintln!(
            "lvu-app shutdown {:.3}s total: {detail}",
            total.as_secs_f64()
        );
    }
}

fn row_request_counters(adapter: &NativeViewAdapter) -> Option<String> {
    std::env::var_os("LVU_ROW_DIAGNOSTICS")?;
    let stats = adapter.raw_stats();
    Some(format!(
        "rows pending {} dropped {} completed {} cached {}",
        stats.pending_requests, stats.dropped_requests, stats.completed_requests, stats.cached_rows
    ))
}

fn row_delivery_explanation(adapter: &NativeViewAdapter, view_id: &str) -> Option<String> {
    let readiness = adapter.readiness(view_id);
    match readiness {
        // Transient states already have a status word of their own, and adding a
        // second sentence to a status line that is about to change would only
        // crowd out the view's own facts.
        // Folding is one of them: it resolves on its own, and the view's own
        // fold indicator already reports how much of the stream is left, after
        // the position counter rather than in front of it.
        RowReadiness::Ready
        | RowReadiness::NoMatches
        | RowReadiness::QueryPending { .. }
        | RowReadiness::RowsPending { .. }
        | RowReadiness::Folding { .. }
        | RowReadiness::Indexing { .. } => None,
        // These do not resolve on their own. Left unsaid they read as an empty
        // result over a confident "query ready". Contended does resolve on its
        // own, but only after a wait with no other outward sign, so an empty
        // pane needs to say what it is waiting for.
        RowReadiness::LookupFailed { .. }
        | RowReadiness::Stalled { .. }
        | RowReadiness::IndexContended { .. }
        | RowReadiness::IndexBudgetUnverified { .. }
        | RowReadiness::QueryFailed { .. } => readiness.describe(),
    }
}

fn register_started(
    adapter: &NativeViewAdapter,
    app: &mut App,
    sources: &mut HashMap<SourceId, String>,
    started: StartedSource,
) -> Result<String, String> {
    let source_id = started.definition.id;
    adapter
        .register_source(started.handle)
        .map_err(|error| format!("register {}: {error}", started.definition.name))?;
    if let Err(error) = adapter.register_view(&started.view_id, vec![source_id]) {
        adapter.rollback_source_registration(source_id, &started.view_id);
        return Err(format!("view {}: {error}", started.definition.name));
    }
    let ui_id = source_id.0.to_string();
    app.add_source_view(
        SourceItem {
            id: ui_id.clone(),
            name: started.definition.name,
            health: "starting/indexing".into(),
        },
        ViewItem {
            id: started.view_id.clone(),
            source_id: ui_id.clone(),
            name: memory::CANONICAL_VIEW_NAME.into(),
        },
    );
    app.set_view_role(&started.view_id, lvu::ViewRole::Canonical);
    sources.insert(source_id, ui_id);
    Ok(view_id(source_id))
}

/// Builds the compiler host from an already resolved helper location, so an
/// installed copy never depends on the checkout that produced the binary.
fn compiler_config(helper: &resources::Located) -> CompilerHostConfig {
    let mut argv = helper.python_command("lvu_expr_helper", &[]);
    let program = argv.remove(0);
    let mut config = CompilerHostConfig::python_module(program, "lvu_expr_helper");
    config.args = argv;
    config
}

fn agent_config(bridge: &resources::Located, cwd: &Path, owned_root: &Path) -> AgentBridgeConfig {
    let (program, arguments, bridge_cwd) = bridge.bridge_command();
    let mut config = AgentBridgeConfig::mise_bridge(Path::new("."));
    config.program = program;
    config.args = arguments;
    config.cwd = bridge_cwd;
    config.environment.push((
        "LVU_PASEO_OWNED_ROOT".into(),
        owned_root.to_string_lossy().into_owned(),
    ));
    if let Some(program) = env::var_os("LVU_AGENT_BRIDGE_PROGRAM") {
        // Explicit process-only seam for deterministic protocol testing. No
        // shell is involved and production keeps the pinned built bridge.
        config.program = PathBuf::from(program);
        config.args.clear();
        config.cwd = env::var_os("LVU_AGENT_BRIDGE_CWD")
            .map(PathBuf::from)
            .unwrap_or_else(|| cwd.to_path_buf());
    }
    config
}

async fn cleanup(provider: &LiveRowProvider, manager: &SourceManager) -> Result<(), String> {
    provider.shutdown().await;
    let mut failures = Vec::new();
    for (source, report) in manager.shutdown().await {
        match report {
            Ok(report) if report.complete => {}
            Ok(report) => failures.push(format!(
                "source {} shutdown incomplete (discarded_bytes={}, known={})",
                source.0, report.discarded_bytes, report.discarded_bytes_known
            )),
            Err(error) => failures.push(format!("source {} shutdown: {error}", source.0)),
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; "))
    }
}

fn combine_errors(primary: String, cleanup: Result<(), String>) -> String {
    match cleanup {
        Ok(()) => primary,
        Err(cleanup) => format!("{primary}; {cleanup}"),
    }
}

fn definition(argument: SourceArgument, cwd: &Path) -> Result<SourceDefinition, String> {
    let (name, identity, acquisition) = match argument {
        SourceArgument::File(path) => {
            let absolute = if path.is_absolute() {
                path
            } else {
                cwd.join(path)
            };
            let canonical = absolute
                .canonicalize()
                .map_err(|error| format!("file {}: {error}", absolute.display()))?;
            (
                canonical
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("file")
                    .to_owned(),
                source_identity(b"file", &canonical, None),
                Acquisition::File {
                    path: canonical,
                    follow: true,
                },
            )
        }
        SourceArgument::Command(text) => {
            if text.is_empty() {
                return Err("command text is empty".into());
            }
            let cwd = cwd
                .canonicalize()
                .map_err(|error| format!("command cwd {}: {error}", cwd.display()))?;
            (
                "shell command".into(),
                source_identity(b"command", &cwd, Some(text.as_bytes())),
                Acquisition::Command {
                    command: CommandDefinition {
                        program: CommandProgram::Shell { text },
                        cwd: Some(cwd),
                        environment: BTreeMap::new(),
                        restart: RestartPolicy::Never,
                    },
                },
            )
        }
        SourceArgument::Stdin => (
            "standard input".into(),
            SourceId::new().0.as_bytes().to_vec(),
            Acquisition::Stdin,
        ),
    };
    Ok(SourceDefinition {
        schema_version: 1,
        id: SourceId(Uuid::new_v5(&SOURCE_NAMESPACE, &identity)),
        name,
        acquisition,
        identity_hints: BTreeMap::new(),
        retention: None,
    })
}

/// Name of the pre-XDG capture directory some working directories still hold.
const LEGACY_CAPTURE_DIR: &str = ".lvu-captures";
/// Records which root a working directory with a legacy capture directory uses.
const CAPTURE_ROOT_POINTER: &str = "capture-root.toml";
const CAPTURE_ROOT_POINTER_VERSION: u32 = 1;

#[derive(serde::Serialize, serde::Deserialize)]
struct CaptureRootPointer {
    schema_version: u32,
    root: String,
}

/// Why a capture root was chosen. The status line reports this so a changed
/// data directory is a stated decision rather than a silent one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CaptureRootReason {
    /// `--capture-dir` named it for this run only.
    Explicit,
    /// `--capture-dir` named one of the two automatic roots, which records the
    /// switch for this working directory.
    ExplicitMigration,
    /// No legacy directory here; the documented XDG default applies.
    Default,
    /// A previous run recorded this root for this working directory.
    Recorded,
    /// The legacy directory holds captures or a workspace.
    LegacyState,
    /// A legacy directory exists but is empty, and the XDG root has no state.
    LegacyEmpty,
    /// A legacy directory exists but holds nothing; XDG state is authoritative.
    DefaultOverEmptyLegacy,
}

#[derive(Debug)]
struct CaptureRootChoice {
    root: PathBuf,
    reason: CaptureRootReason,
    /// Absolute pointer path to write, when this decision should be recorded.
    record: Option<PathBuf>,
    legacy: PathBuf,
}

impl CaptureRootChoice {
    fn notice(&self, xdg_data: &Path, record_error: Option<&str>) -> Option<String> {
        let legacy = self.legacy.display();
        let mut notice = match self.reason {
            // The user named this root for this run; the settings dialog shows it.
            CaptureRootReason::Explicit | CaptureRootReason::Default => return None,
            CaptureRootReason::ExplicitMigration => format!(
                "data directory {} recorded for this working directory; nothing was moved",
                self.root.display()
            ),
            CaptureRootReason::Recorded => format!(
                "using recorded data directory {}; change it with --capture-dir",
                self.root.display()
            ),
            CaptureRootReason::LegacyState => format!(
                "using legacy data directory {legacy} because it holds this directory's \
                 workspace; nothing was moved (--capture-dir {} switches to the default)",
                xdg_data.display()
            ),
            CaptureRootReason::LegacyEmpty => format!(
                "using legacy data directory {legacy} found here; nothing was moved \
                 (--capture-dir {} switches to the default)",
                xdg_data.display()
            ),
            CaptureRootReason::DefaultOverEmptyLegacy => format!(
                "using XDG data {}; empty legacy {legacy} remains untouched",
                xdg_data.display()
            ),
        };
        if let Some(error) = record_error {
            notice.push_str(&format!("; could not record the choice: {error}"));
        }
        Some(notice)
    }
}

/// True when a root already holds durable lvu state: a workspace, retained
/// investigations, or a captured source directory. `assistance/` is deliberately
/// excluded — it is created as a side effect of the agent bridge, so treating it
/// as evidence is what made root selection unstable between runs.
fn capture_root_has_state(root: &Path) -> bool {
    if root.join("workspace").exists() || root.join("investigations").exists() {
        return true;
    }
    let Ok(entries) = std::fs::read_dir(root) else {
        return false;
    };
    entries.flatten().any(|entry| {
        let path = entry.path();
        path.join("source.json").exists() || path.join("capture.journal").exists()
    })
}

fn read_capture_root_pointer(path: &Path) -> Result<Option<PathBuf>, String> {
    let bytes = match std::fs::read_to_string(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("read {}: {error}", path.display())),
    };
    let pointer: CaptureRootPointer = toml::from_str(&bytes).map_err(|error| {
        format!(
            "{} is not a valid capture root record: {error}",
            path.display()
        )
    })?;
    if pointer.schema_version > CAPTURE_ROOT_POINTER_VERSION {
        return Err(format!(
            "{} was written by a newer lvu (schema {}); use --capture-dir to choose a root",
            path.display(),
            pointer.schema_version
        ));
    }
    let root = PathBuf::from(pointer.root);
    if !root.is_absolute() {
        return Err(format!(
            "{} records a relative root {}; use --capture-dir to choose a root",
            path.display(),
            root.display()
        ));
    }
    Ok(Some(root))
}

/// Writes the pointer only when it would change, so repeated launches neither
/// rewrite nor lose an existing record. A failure is diagnostic, not fatal: the
/// decision rules below are already stable without it.
fn record_capture_root(path: &Path, root: &Path) -> Result<(), String> {
    if read_capture_root_pointer(path).ok().flatten().as_deref() == Some(root) {
        return Ok(());
    }
    let pointer = CaptureRootPointer {
        schema_version: CAPTURE_ROOT_POINTER_VERSION,
        root: root
            .to_str()
            .ok_or_else(|| "capture root path is not UTF-8".to_owned())?
            .to_owned(),
    };
    let text = toml::to_string(&pointer).map_err(|error| error.to_string())?;
    let temporary = path.with_extension("toml.tmp");
    std::fs::write(&temporary, text.as_bytes()).map_err(|error| error.to_string())?;
    std::fs::rename(&temporary, path).map_err(|error| {
        let _ = std::fs::remove_file(&temporary);
        error.to_string()
    })
}

/// Decides the capture root exactly once per launch.
///
/// The decision must never depend on a directory lvu itself can materialise, so
/// it reads durable state (`capture_root_has_state`) and an explicit recorded
/// choice instead of bare directory existence. A working directory holding a
/// legacy `.lvu-captures` keeps using it, and switching roots is an explicit
/// `--capture-dir` decision that is recorded rather than an accident of startup
/// ordering. Nothing is moved, copied or deleted by this function.
fn select_capture_root(
    explicit: Option<PathBuf>,
    xdg_data: &Path,
    cwd: &Path,
) -> Result<CaptureRootChoice, String> {
    let legacy = cwd.join(LEGACY_CAPTURE_DIR);
    let pointer = legacy.join(CAPTURE_ROOT_POINTER);
    let legacy_present = legacy.is_dir();
    if let Some(path) = explicit {
        let root = if path.is_absolute() {
            path
        } else {
            cwd.join(path)
        };
        // Naming one of the two automatic roots is how a user migrates; any
        // other path stays a one-off override and must not repoint this
        // directory permanently.
        let migration = legacy_present && (root == legacy || root == xdg_data);
        return Ok(CaptureRootChoice {
            reason: if migration {
                CaptureRootReason::ExplicitMigration
            } else {
                CaptureRootReason::Explicit
            },
            record: migration.then_some(pointer),
            root,
            legacy,
        });
    }
    if !legacy_present {
        return Ok(CaptureRootChoice {
            root: xdg_data.to_path_buf(),
            reason: CaptureRootReason::Default,
            record: None,
            legacy,
        });
    }
    if let Some(root) = read_capture_root_pointer(&pointer)? {
        return Ok(CaptureRootChoice {
            root,
            reason: CaptureRootReason::Recorded,
            record: None,
            legacy,
        });
    }
    let (root, reason) = if capture_root_has_state(&legacy) {
        (legacy.clone(), CaptureRootReason::LegacyState)
    } else if capture_root_has_state(xdg_data) {
        (
            xdg_data.to_path_buf(),
            CaptureRootReason::DefaultOverEmptyLegacy,
        )
    } else {
        (legacy.clone(), CaptureRootReason::LegacyEmpty)
    };
    Ok(CaptureRootChoice {
        root,
        reason,
        record: Some(pointer),
        legacy,
    })
}

fn parse_args(
    arguments: Vec<OsString>,
    stdin_is_terminal: bool,
) -> Result<Option<Options>, String> {
    let mut capture_dir = None;
    let mut sources = Vec::new();
    let mut explicit_stdin = false;
    let mut options_ended = false;
    let mut index = 0;
    while index < arguments.len() {
        let argument = &arguments[index];
        let option = argument.to_str();
        if options_ended {
            push_cli_source(
                &mut sources,
                &mut explicit_stdin,
                positional_source(argument.clone()),
                stdin_is_terminal,
            )?;
            index += 1;
            continue;
        }
        match option {
            Some("--") => options_ended = true,
            Some("--help" | "-h") => return Ok(None),
            Some("--capture-dir") => {
                index += 1;
                capture_dir = Some(PathBuf::from(value_os(&arguments, index, "--capture-dir")?));
            }
            Some("--file") => {
                index += 1;
                push_cli_source(
                    &mut sources,
                    &mut explicit_stdin,
                    SourceArgument::File(PathBuf::from(value_os(&arguments, index, "--file")?)),
                    stdin_is_terminal,
                )?;
            }
            Some(option @ ("--command" | "-c")) => {
                index += 1;
                let text = value_os(&arguments, index, option)?
                    .clone()
                    .into_string()
                    .map_err(|_| "--command/-c requires valid UTF-8 shell text".to_owned())?;
                push_cli_source(
                    &mut sources,
                    &mut explicit_stdin,
                    SourceArgument::Command(text),
                    stdin_is_terminal,
                )?;
            }
            Some("--stdin" | "-") => push_cli_source(
                &mut sources,
                &mut explicit_stdin,
                SourceArgument::Stdin,
                stdin_is_terminal,
            )?,
            Some(value) if !value.starts_with('-') => push_cli_source(
                &mut sources,
                &mut explicit_stdin,
                SourceArgument::File(PathBuf::from(argument)),
                stdin_is_terminal,
            )?,
            Some(unknown) => return Err(format!("unknown argument {unknown:?}; use --help")),
            None => push_cli_source(
                &mut sources,
                &mut explicit_stdin,
                SourceArgument::File(PathBuf::from(argument)),
                stdin_is_terminal,
            )?,
        }
        index += 1;
    }
    if !stdin_is_terminal && !explicit_stdin {
        sources.push(SourceArgument::Stdin);
        ensure_source_bound(&sources)?;
    }
    Ok(Some(Options {
        capture_dir,
        sources,
    }))
}

fn positional_source(argument: OsString) -> SourceArgument {
    if argument == "-" {
        SourceArgument::Stdin
    } else {
        SourceArgument::File(PathBuf::from(argument))
    }
}

fn push_cli_source(
    sources: &mut Vec<SourceArgument>,
    explicit_stdin: &mut bool,
    source: SourceArgument,
    stdin_is_terminal: bool,
) -> Result<(), String> {
    if matches!(source, SourceArgument::Stdin) {
        if *explicit_stdin {
            return Err("stdin source may be specified only once".into());
        }
        if stdin_is_terminal {
            return Err(
                "stdin is a terminal; pipe data into lvu or omit --stdin/- to use keyboard input"
                    .into(),
            );
        }
        *explicit_stdin = true;
    }
    sources.push(source);
    ensure_source_bound(sources)
}

fn ensure_source_bound(sources: &[SourceArgument]) -> Result<(), String> {
    if sources.len() > MAX_SOURCES {
        Err(format!("at most {MAX_SOURCES} sources may be launched"))
    } else {
        Ok(())
    }
}

fn value_os<'a>(
    arguments: &'a [OsString],
    index: usize,
    option: &str,
) -> Result<&'a OsString, String> {
    arguments
        .get(index)
        .ok_or_else(|| format!("{option} requires a value"))
}

fn source_identity(prefix: &[u8], path: &Path, suffix: Option<&[u8]>) -> Vec<u8> {
    let mut identity = Vec::from(prefix);
    identity.push(0);
    identity.extend(path_identity_bytes(path));
    if let Some(suffix) = suffix {
        identity.push(0);
        identity.extend(suffix);
    }
    identity
}

#[cfg(unix)]
fn path_identity_bytes(path: &Path) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    path.as_os_str().as_bytes().to_vec()
}

#[cfg(not(unix))]
fn path_identity_bytes(path: &Path) -> Vec<u8> {
    path.to_string_lossy().into_owned().into_bytes()
}

fn print_help() {
    println!(
        "lvu — live local log viewer\n\n\
         Usage: lvu [OPTIONS] [FILE ...]\n\n\
         FILE                Capture and follow a file (repeatable; --file compatible)\n\
         --file PATH         Capture and follow a file (repeatable)\n\
         -c, --command TEXT  Capture `sh -c TEXT` in the current directory (repeatable)\n\
         --stdin, -          Capture redirected stdin once; non-terminal stdin is automatic\n\
         --capture-dir PATH  Durable capture root; naming the default or a legacy\n\
         \x20                   .lvu-captures root records that choice for this directory\n\
         --                  Treat remaining arguments as file paths\n\
         --help              Show this help\n\
         --resources         Report resolved helper/bridge resources and exit\n\n\
         With no sources, the terminal opens an Add source dialog. Tab completes file\n\
         paths; Alt-F/Alt-C selects file or command; Ctrl-D opens discovery; Ctrl-A asks agent for a reviewed source definition.\n\
         With sources, / opens literal search, p advanced Polars, e enrichment,
         A opens definition Ask agent, I opens a snapshot investigation, and v manages views."
    );
}

#[cfg(test)]
mod tests {

    /// One wedged subsystem must cost its own deadline, not everyone's.
    #[test]
    fn a_stuck_shutdown_subsystem_does_not_delay_the_others() {
        let stuck = std::time::Duration::from_millis(600);
        let started = std::time::Instant::now();
        let (first, second, third) = super::settle_together(
            || {
                // Ignores its cancellation and burns its whole budget, which is
                // what a wedged worker looks like from here.
                std::thread::sleep(stuck);
                Err("first did not stop".to_owned())
            },
            || {
                std::thread::sleep(std::time::Duration::from_millis(20));
                Ok(())
            },
            || {
                std::thread::sleep(std::time::Duration::from_millis(20));
                Ok(())
            },
        );
        let elapsed = started.elapsed();
        assert_eq!(first, Err("first did not stop".to_owned()));
        assert_eq!(second, Ok(()));
        assert_eq!(third, Ok(()));
        // Sequentially this is 640ms; the point is that it is the longest
        // deadline and not the sum, with generous headroom for a loaded host.
        assert!(
            elapsed < stuck + std::time::Duration::from_millis(400),
            "settles did not overlap: {elapsed:?}"
        );
        assert!(
            elapsed >= stuck,
            "the stuck subsystem must still be waited for: {elapsed:?}"
        );
    }
    use super::{
        AiStart, AiWork, AtomicBool, CaptureRootReason, Composition, MAX_SESSION_RECORD_JOBS,
        MAX_VIEWS, PendingMemorySave, SessionConfig, SourceArgument, StartOrigin, agent_config,
        apply_deferred_owned_session_events, apply_or_defer_owned_session_event,
        apply_owned_session_event, capture_root_has_state, common_prefix, compiler_config,
        complete_path, definition, discovery_item, discovery_status, expand_tilde_path,
        lexical_display_hint, owned_session_start_admission, parse_args, prepare_ai_context,
        proposal_expression, recipe_incompatibility, reconcile_pending_state, record_agent_session,
        record_capture_root, resources, select_capture_root, validate_recipe_proposal_source,
        validate_remote_cancellation, view_admission_error,
    };
    use lvu::{
        App, PathCompletionRequest, PersistentViewState, SourceItem, SourceKind,
        SourceLaunchRequest, ViewItem,
    };
    use lvu_core::{Acquisition, CommandProgram, SourceId, ViewId};
    use serde_json::json;
    use std::{
        collections::{HashMap, HashSet, VecDeque},
        path::PathBuf,
        sync::Arc,
        time::{Duration, Instant},
    };

    fn pending_memory_save(
        definition: lvu_core::SourceDefinition,
        state: PersistentViewState,
    ) -> PendingMemorySave {
        PendingMemorySave {
            request: Box::new(super::SaveRequest {
                sequence: 1,
                view_id: ViewId(uuid::Uuid::nil()),
                definition,
                state,
            }),
            dirty_since: Instant::now(),
        }
    }

    fn session_config() -> SessionConfig {
        SessionConfig {
            provider: "provider".into(),
            mode: "mode".into(),
            thinking: "thinking".into(),
        }
    }

    #[test]
    fn agent_bridge_receives_absolute_owned_assistance_root() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let owned_root = directory.path().join("data/assistance");
        let bridge = resources::resolve_all().1;
        let Some(bridge) = bridge.located() else {
            // Resource resolution itself is covered in `resources`; without a
            // built bridge here there is no configuration to inspect.
            return;
        };
        let config = agent_config(bridge, directory.path(), &owned_root);
        let expected = owned_root.to_string_lossy().into_owned();
        assert!(owned_root.is_absolute());
        assert!(
            config
                .environment
                .iter()
                .any(|(key, value)| { key == "LVU_PASEO_OWNED_ROOT" && value == &expected })
        );
    }

    #[test]
    fn owned_archive_event_releases_only_the_matching_ephemeral_session() {
        let mut ask = Some("ask-1".to_owned());
        let mut ask_config = Some(session_config());
        let mut retiring = true;
        let mut busy = true;
        let mut source = Some(("source-1".to_owned(), 7));
        let mut source_config = Some(session_config());
        let notice = apply_owned_session_event(
            &super::agent::BridgeEvent {
                session_id: "ask-1".into(),
                kind: "session_archived".into(),
                payload: json!({"activity_path": "/data/assistance/activity/ask.jsonl"}),
            },
            &mut ask,
            &mut ask_config,
            &mut retiring,
            &mut busy,
            &mut source,
            &mut source_config,
        );
        assert_eq!(ask, None);
        assert_eq!(ask_config, None);
        assert!(!retiring);
        assert!(!busy);
        assert_eq!(
            source.as_ref().map(|value| value.0.as_str()),
            Some("source-1")
        );
        assert!(source_config.is_some());
        assert_eq!(
            notice.as_deref(),
            Some("Agent activity saved: /data/assistance/activity/ask.jsonl")
        );
    }

    #[test]
    fn failed_owned_archive_remains_busy_and_retriable_by_bridge() {
        let mut ask = Some("ask-1".to_owned());
        let mut ask_config = Some(session_config());
        let mut retiring = false;
        let mut busy = false;
        let mut source = None;
        let mut source_config = None;
        let notice = apply_owned_session_event(
            &super::agent::BridgeEvent {
                session_id: "ask-1".into(),
                kind: "archive_failed".into(),
                payload: json!({"error": "remote confirmation timed out", "activity_path": "/activity/ask.jsonl"}),
            },
            &mut ask,
            &mut ask_config,
            &mut retiring,
            &mut busy,
            &mut source,
            &mut source_config,
        );
        assert_eq!(ask.as_deref(), Some("ask-1"));
        assert!(ask_config.is_some());
        assert!(busy);
        assert!(
            notice
                .as_deref()
                .is_some_and(|value| value.contains("archival failed"))
        );
    }

    #[test]
    fn owned_archive_before_start_response_is_applied_after_identity_registration() {
        let mut deferred = VecDeque::new();
        let mut ask = None;
        let mut ask_config = None;
        let mut retiring = false;
        let mut busy = false;
        let mut source = None;
        let mut source_config = None;
        let mut overflowed = false;
        let event = super::agent::BridgeEvent {
            session_id: "early-session".into(),
            kind: "session_archived".into(),
            payload: json!({"activity_path": "/activity/early.jsonl"}),
        };
        assert_eq!(
            apply_or_defer_owned_session_event(
                &event,
                &mut deferred,
                &mut overflowed,
                true,
                &mut ask,
                &mut ask_config,
                &mut retiring,
                &mut busy,
                &mut source,
                &mut source_config,
            ),
            None
        );
        assert_eq!(deferred.len(), 1);

        ask = Some("early-session".into());
        ask_config = Some(session_config());
        busy = true;
        let notice = apply_deferred_owned_session_events(
            "early-session",
            &mut deferred,
            &mut ask,
            &mut ask_config,
            &mut retiring,
            &mut busy,
            &mut source,
            &mut source_config,
        );
        assert!(deferred.is_empty());
        assert_eq!(ask, None);
        assert_eq!(ask_config, None);
        assert!(!busy);
        assert!(notice.is_some());
    }

    #[test]
    fn early_archive_failure_matches_only_its_later_owned_identity() {
        let mut deferred = VecDeque::new();
        let mut ask = None;
        let mut ask_config = None;
        let mut retiring = false;
        let mut busy = false;
        let mut source = None;
        let mut source_config = None;
        let mut overflowed = false;
        let event = super::agent::BridgeEvent {
            session_id: "expected".into(),
            kind: "archive_failed".into(),
            payload: json!({"error": "confirmation timeout"}),
        };
        apply_or_defer_owned_session_event(
            &event,
            &mut deferred,
            &mut overflowed,
            true,
            &mut ask,
            &mut ask_config,
            &mut retiring,
            &mut busy,
            &mut source,
            &mut source_config,
        );
        ask = Some("different".into());
        ask_config = Some(session_config());
        assert_eq!(
            apply_deferred_owned_session_events(
                "different",
                &mut deferred,
                &mut ask,
                &mut ask_config,
                &mut retiring,
                &mut busy,
                &mut source,
                &mut source_config,
            ),
            None
        );
        assert!(!busy);
        assert_eq!(deferred.len(), 1);

        ask = Some("expected".into());
        ask_config = Some(session_config());
        assert!(
            apply_deferred_owned_session_events(
                "expected",
                &mut deferred,
                &mut ask,
                &mut ask_config,
                &mut retiring,
                &mut busy,
                &mut source,
                &mut source_config,
            )
            .is_some()
        );
        assert_eq!(ask.as_deref(), Some("expected"));
        assert!(busy);
        assert!(deferred.is_empty());
    }

    #[test]
    fn deferred_owned_lifecycle_overflow_fails_closed_without_dropping_acknowledgements() {
        let mut deferred = VecDeque::new();
        let mut overflowed = false;
        let mut ask = None;
        let mut ask_config = None;
        let mut retiring = false;
        let mut busy = false;
        let mut source = None;
        let mut source_config = None;
        for index in 0..super::MAX_DEFERRED_OWNED_LIFECYCLE_EVENTS {
            let notice = apply_or_defer_owned_session_event(
                &super::agent::BridgeEvent {
                    session_id: format!("unregistered-{index}"),
                    kind: "session_archived".into(),
                    payload: json!({}),
                },
                &mut deferred,
                &mut overflowed,
                true,
                &mut ask,
                &mut ask_config,
                &mut retiring,
                &mut busy,
                &mut source,
                &mut source_config,
            );
            assert_eq!(notice, None);
        }
        let notice = apply_or_defer_owned_session_event(
            &super::agent::BridgeEvent {
                session_id: "would-have-been-dropped".into(),
                kind: "session_archived".into(),
                payload: json!({}),
            },
            &mut deferred,
            &mut overflowed,
            true,
            &mut ask,
            &mut ask_config,
            &mut retiring,
            &mut busy,
            &mut source,
            &mut source_config,
        );
        assert!(overflowed);
        assert_eq!(deferred.len(), super::MAX_DEFERRED_OWNED_LIFECYCLE_EVENTS);
        assert!(notice.is_some_and(|value| value.contains("restart lvu")));
    }

    #[test]
    fn recovered_archives_without_pending_registration_do_not_consume_the_buffer() {
        let mut deferred = VecDeque::new();
        let mut overflowed = false;
        let mut ask = None;
        let mut ask_config = None;
        let mut retiring = false;
        let mut busy = false;
        let mut source = None;
        let mut source_config = None;
        for index in 0..40 {
            let notice = apply_or_defer_owned_session_event(
                &super::agent::BridgeEvent {
                    session_id: format!("recovered-{index}"),
                    kind: "session_archived".into(),
                    payload: json!({
                        "activity_path": format!("/activity/recovered-{index}.jsonl"),
                        "recovered": true,
                    }),
                },
                &mut deferred,
                &mut overflowed,
                false,
                &mut ask,
                &mut ask_config,
                &mut retiring,
                &mut busy,
                &mut source,
                &mut source_config,
            );
            assert!(notice.is_some_and(|value| value.contains("Recovered agent activity")));
        }
        assert!(deferred.is_empty());
        assert!(!overflowed);
        assert!(owned_session_start_admission(overflowed).is_ok());
    }

    #[test]
    fn overflow_latch_rejects_before_remote_session_creation() {
        let mut remote_creations = 0;
        if owned_session_start_admission(true).is_ok() {
            remote_creations += 1;
        }
        assert_eq!(remote_creations, 0);
        assert!(
            owned_session_start_admission(true)
                .unwrap_err()
                .contains("restart lvu")
        );
    }

    #[test]
    fn recipes_support_recent_time_and_reject_unsupported_colors_and_pin_counts() {
        let mut view = lvu_memory::NamedViewDefinition {
            schema_version: 1,
            id: ViewId::new(),
            name: "portable".into(),
            source_ids: vec![SourceId::new()],
            stages: Vec::new(),
            search: String::new(),
            advanced_filter: None,
            pinned_columns: Vec::new(),
            color_rules: Vec::new(),
            time_policy: lvu_memory::TimePolicy::All,
            time_basis: lvu_memory::TimeBasis::Capture,
            grouping: None,
        };
        view.time_policy = lvu_memory::TimePolicy::Recent { seconds: 60 };
        assert!(recipe_incompatibility(&view).is_none());
        view.time_policy = lvu_memory::TimePolicy::Recent { seconds: 0 };
        assert!(recipe_incompatibility(&view).unwrap().contains("duration"));
        view.time_policy = lvu_memory::TimePolicy::Absolute {
            start_unix_nanos: 1,
            end_unix_nanos: 2,
        };
        assert!(recipe_incompatibility(&view).is_none());
        view.time_policy = lvu_memory::TimePolicy::All;
        view.color_rules.push(lvu_memory::ColorRule {
            expression: "level".into(),
            style: "gradient".into(),
        });
        assert!(recipe_incompatibility(&view).unwrap().contains("color"));
        view.color_rules.clear();
        view.pinned_columns = (0..9).map(|index| format!("field_{index}")).collect();
        assert!(recipe_incompatibility(&view).unwrap().contains("8 pinned"));
    }

    #[test]
    fn recipe_export_preserves_each_editable_extraction_and_empty_chain() {
        let mut config = lvu::RecipeConfig {
            enrichments: vec![
                lvu::EnrichmentDefinition {
                    id: lvu::EnrichmentStageId("regex-step".into()),
                    source: r"/id=(?P<id>\w+)/".into(),
                    command: None,
                },
                lvu::EnrichmentDefinition {
                    id: lvu::EnrichmentStageId("upper-step".into()),
                    source: "upper = pl.col('id').str.to_uppercase()".into(),
                    command: None,
                },
            ],
            ..Default::default()
        };
        let stages = super::recipe_extraction_stages(&config);
        assert_eq!(stages.len(), 2);
        for (stage, definition) in stages.iter().zip(&config.enrichments) {
            let lvu_memory::StageDefinition::Extraction { id, source } = stage else {
                panic!("lost editable extraction");
            };
            assert_eq!(id, &definition.id.0);
            assert_eq!(source, &definition.source);
        }
        config.enrichments.clear();
        config.enrichment = "stale = pl.lit(1)".into();
        assert!(super::recipe_extraction_stages(&config).is_empty());
    }

    #[test]
    fn ai_proposals_map_only_to_existing_native_editor_shapes() {
        let revision = super::OriginatingRevision {
            data: "snapshot".into(),
            definition: "view:1".into(),
        };
        let filter = super::ProposalEnvelope {
            kind: super::ProposalKind::Filter,
            definition: json!({"schema_version": 1, "expression": "pl.col('level') == 'ERROR'"}),
            explanation: "filter".into(),
            originating_revision: revision.clone(),
        };
        assert_eq!(
            proposal_expression(lvu::AskAiKind::Filter, &filter).unwrap(),
            "pl.col('level') == 'ERROR'"
        );
        let enrichment = super::ProposalEnvelope {
            kind: super::ProposalKind::Enrichment,
            definition: json!({
                "schema_version": 1,
                "stages": [{
                    "id": "11111111-1111-4111-8111-111111111111",
                    "name": "agent",
                    "expressions": {"status": "pl.col('raw').str.extract('(\\\\d+)', 1)"}
                }]
            }),
            explanation: "derive".into(),
            originating_revision: revision,
        };
        assert!(
            proposal_expression(lvu::AskAiKind::Enrichment, &enrichment)
                .unwrap()
                .starts_with("status = pl.col")
        );

        let multiple = super::ProposalEnvelope {
            definition: json!({
                "schema_version": 1,
                "stages": [{
                    "id": "11111111-1111-4111-8111-111111111111",
                    "name": "agent",
                    "expressions": {"one": "pl.lit(1)", "two": "pl.lit(2)"}
                }]
            }),
            ..enrichment
        };
        assert!(proposal_expression(lvu::AskAiKind::Enrichment, &multiple).is_err());

        let source = "22222222-2222-4222-8222-222222222222";
        let view = super::ProposalEnvelope {
            kind: super::ProposalKind::View,
            definition: json!({
                "schema_version": 1,
                "id": "33333333-3333-4333-8333-333333333333",
                "name": "Adapted",
                "source_ids": [source],
                "filter": {"schema_version": 1, "expression": "pl.col('service') == 'api'"},
                "recipe_stage_revisions": []
            }),
            explanation: "adapt".into(),
            originating_revision: super::OriginatingRevision {
                data: "snapshot".into(),
                definition: "view:2".into(),
            },
        };
        assert!(validate_recipe_proposal_source(&view, &[source.to_string()]).is_ok());
        assert!(
            validate_recipe_proposal_source(
                &view,
                &["44444444-4444-4444-8444-444444444444".into()]
            )
            .unwrap_err()
            .contains("different source")
        );
        assert_eq!(
            proposal_expression(lvu::AskAiKind::Recipe, &view).unwrap(),
            "pl.col('service') == 'api'"
        );
        let mut inline = view.clone();
        inline.definition["enrichments"] = json!([
            {"id":"first", "source":"/(?P<code>[0-9]+)/"},
            {"id":"second", "source":"number = pl.col('code').cast(pl.Int64)"}
        ]);
        assert_eq!(
            proposal_expression(lvu::AskAiKind::Recipe, &inline).unwrap(),
            "pl.col('service') == 'api'"
        );
        let stages = super::proposal_recipe_enrichments(&inline)
            .unwrap()
            .unwrap();
        assert_eq!(stages.len(), 2);
        assert_eq!(stages[1].id.0, "second");
        inline.definition["enrichments"][1]["id"] = json!("first");
        assert!(super::proposal_recipe_enrichments(&inline).is_err());
        let mut unsupported = view.clone();
        unsupported.definition["recipe_stage_revisions"] = json!(["unresolved"]);
        assert!(proposal_expression(lvu::AskAiKind::Recipe, &unsupported).is_err());
        let mut unsupported_setting = view;
        unsupported_setting.definition["time_policy"] = json!({"recent": 300});
        assert!(
            proposal_expression(lvu::AskAiKind::Recipe, &unsupported_setting)
                .unwrap_err()
                .contains("unsupported view settings")
        );
        assert_eq!(lexical_display_hint("200"), lexical_display_hint("null"));
        assert_eq!(lexical_display_hint("true"), "display-text");
    }

    #[test]
    fn source_proposal_preserves_exec_arguments_and_is_only_rendered_for_review() {
        let source_id = uuid::Uuid::from_u128(90);
        let proposal = super::ProposalEnvelope {
            kind: super::ProposalKind::Source,
            definition: json!({
                "schema_version": 1,
                "id": source_id,
                "name": "backend logs",
                "kind": "command",
                "command": {
                    "program": {"exec": {"executable": "docker", "args": ["logs", "-f", "backend api"]}},
                    "cwd": "/tmp/project with spaces",
                    "environment": {"MODE": "fixture", "REGION": "local"},
                    "restart": "never"
                },
                "identity_hints": {"compose_service": "backend api"},
                "retention": null
            }),
            explanation: "matched controlled discovery evidence".into(),
            originating_revision: super::OriginatingRevision {
                data: "discovery:1".into(),
                definition: "source-dialog:1".into(),
            },
        };
        let (definition, preview) =
            super::parse_source_proposal(&proposal, std::path::Path::new("/app")).unwrap();
        let lvu_core::Acquisition::Command { command } = definition.acquisition else {
            panic!("command")
        };
        assert!(matches!(
            command.program,
            lvu_core::CommandProgram::Exec { ref executable, ref args }
                if executable == std::path::Path::new("docker")
                    && args == &["logs", "-f", "backend api"]
        ));
        assert!(preview.launch.contains("backend api"));
        assert!(!preview.launch.contains("sh -c"));
        assert_eq!(preview.effective_path_or_cwd, "/tmp/project with spaces");
        assert_eq!(preview.restart, "never");
        assert_eq!(preview.environment, ["MODE=fixture", "REGION=local"]);

        let mut unsupported = proposal;
        unsupported.definition["command"]["restart"] = json!("always");
        assert!(
            super::parse_source_proposal(&unsupported, std::path::Path::new("/app"))
                .unwrap_err()
                .contains("restart policy")
        );

        let file = super::ProposalEnvelope {
            kind: super::ProposalKind::Source,
            definition: json!({
                "schema_version": 1,
                "id": uuid::Uuid::from_u128(91),
                "name": "relative file",
                "kind": "file",
                "path": "logs/backend.log",
                "follow": true,
                "identity_hints": {},
                "retention": null
            }),
            explanation: "project candidate".into(),
            originating_revision: unsupported.originating_revision,
        };
        let (_, preview) =
            super::parse_source_proposal(&file, std::path::Path::new("/app")).unwrap();
        assert_eq!(preview.launch, "logs/backend.log (follow: true)");
        assert_eq!(
            preview.effective_path_or_cwd,
            "base /app -> /app/logs/backend.log"
        );
    }

    #[test]
    fn cancelled_source_context_workers_settle_without_publishing_files() {
        for index in 0..4 {
            let directory = tempfile::tempdir().unwrap();
            let output = directory.path().join(format!("source-ai-{index}"));
            let cancel = lvu_discovery::CancellationToken::default();
            cancel.cancel();
            let (result, worker) = super::spawn_source_ai_context_worker(
                directory.path().to_path_buf(),
                output.clone(),
                cancel,
            );
            assert!(
                result
                    .recv_timeout(Duration::from_secs(2))
                    .unwrap()
                    .unwrap_err()
                    .contains("cancelled")
            );
            worker.join().unwrap();
            assert!(!output.exists());
        }
    }

    #[test]
    fn source_context_writer_rejects_bytes_before_exceeding_its_cap() {
        use std::io::Write as _;

        let cancel = lvu_discovery::CancellationToken::default();
        let mut writer = super::CappedWriter {
            inner: Vec::new(),
            written: 0,
            limit: 4,
            cancel: &cancel,
        };
        writer.write_all(b"1234").unwrap();
        assert!(writer.write_all(b"5").is_err());
        assert_eq!(writer.inner, b"1234");
        cancel.cancel();
        assert_ne!(
            writer.write(b"x").unwrap_err().kind(),
            std::io::ErrorKind::Interrupted
        );
        assert!(
            writer
                .write_all(b"x")
                .unwrap_err()
                .to_string()
                .contains("cancelled")
        );
        assert_eq!(writer.inner, b"1234");
    }

    #[test]
    fn short_assistance_uses_inline_typed_context_without_dataset_inventory() {
        let directory = tempfile::tempdir().unwrap();
        let start = super::AiStart {
            generation: 1,
            view_id: "view-fixture".into(),
            definition_revision: 7,
            kind: super::AskAiKind::Enrichment,
            instruction: "recognize timestamp".into(),
            provider: "fixture".into(),
            mode: "default".into(),
            thinking: "default".into(),
        };
        // The producer's field order may differ from Value's sorted map order.
        let inline = r#"{"version":1,"samples":[{"fields":{"observed_at":{"kind":"datetime","integer":"1700000000000000001","unit":"ns","timezone":"UTC"},"optional":null}}],"coverage":{"admitted_rows":1,"omitted_rows":5}}"#;
        let prepared = super::AssistancePreparationResult {
            inline_context: inline.into(),
            context: serde_json::from_str(inline).unwrap(),
            context_path: directory.path().join("fixed-context/context.json"),
            serialized_bytes: inline.len(),
            view_id: start.view_id.clone(),
            applied_revision: 3,
            applied_generation: 4,
            sources: Vec::new(),
        };
        let (root, context) = super::prepared_sample_context(&start, prepared.clone()).unwrap();
        assert_eq!(root, directory.path().join("fixed-context"));
        assert!(context.datasets.is_empty());
        assert!(context.inspection_command.is_none());
        assert_eq!(context.inline_context.as_ref(), Some(&prepared.context));
        assert_eq!(context.revision.definition, "view-fixture:7");
        assert_eq!(context.revision.data, "fixed-context");
        assert!(
            !context.manifest_path.exists(),
            "conversion performs no filesystem reads or export"
        );

        let mut invalid = prepared.clone();
        invalid.view_id = "other-view".into();
        assert!(super::prepared_sample_context(&start, invalid).is_err());
        let mut invalid = prepared.clone();
        invalid.context_path = "relative/context.json".into();
        assert!(super::prepared_sample_context(&start, invalid).is_err());
        let mut invalid = prepared;
        invalid.context["samples"] = serde_json::json!([]);
        assert!(super::prepared_sample_context(&start, invalid).is_err());
    }

    #[test]
    fn snapshot_context_paths_are_absolute_and_session_record_failures_surface() {
        let directory = tempfile::tempdir().expect("tempdir");
        let snapshot = directory.path().join("snapshot");
        std::fs::create_dir(&snapshot).unwrap();
        std::fs::write(snapshot.join("manifest.json"), b"{}").unwrap();
        std::fs::write(snapshot.join("part-0.parquet"), b"fixture").unwrap();
        let relative = snapshot
            .strip_prefix(std::env::current_dir().unwrap())
            .map(PathBuf::from)
            .unwrap_or_else(|_| snapshot.clone());
        let (result, worker) = prepare_ai_context(
            relative.clone(),
            relative.join("manifest.json"),
            "view-fixture".into(),
            7,
        );
        let prepared = result.recv().unwrap().unwrap();
        worker.join().unwrap();
        assert!(prepared.manifest_path.is_absolute());
        assert!(prepared.manifest_path.is_file());
        assert_eq!(prepared.datasets.len(), 1);
        assert!(prepared.datasets[0].is_absolute());

        let mut job = record_agent_session(&directory.path().join("missing"), "session-fixture");
        let error = job.result.recv().unwrap().unwrap_err();
        if let Some(worker) = job.worker.take() {
            worker.join().unwrap();
        }
        assert!(error.contains("write session record"));
    }

    #[test]
    fn investigation_metadata_round_trips_from_owned_snapshot_directory() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("investigations");
        let snapshot = root.join("snapshot-1");
        std::fs::create_dir_all(&snapshot).unwrap();
        let manifest = snapshot.join("manifest.json");
        std::fs::write(&manifest, b"{}").unwrap();
        let item = lvu::InvestigationItem {
            id: uuid::Uuid::from_u128(1).to_string(),
            view_id: uuid::Uuid::from_u128(2).to_string(),
            session_id: "session-1".into(),
            snapshot_dir: snapshot.display().to_string(),
            manifest_path: manifest.display().to_string(),
            question: "why did it fail?".into(),
        };
        let mut jobs = Vec::new();
        super::admit_investigation_record(&mut jobs, &snapshot, &item).unwrap();
        let mut record = jobs.pop().unwrap();
        record
            .result
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .unwrap();
        record.worker.take().unwrap().join().unwrap();

        let future = root.join("future");
        std::fs::create_dir(&future).unwrap();
        std::fs::write(future.join("manifest.json"), b"{}").unwrap();
        std::fs::write(
            future.join("lvu-investigation.json"),
            serde_json::to_vec(&json!({
                "schema_version": 2,
                "investigation_id": uuid::Uuid::from_u128(3).to_string(),
                "view_id": uuid::Uuid::from_u128(4).to_string(),
                "session_id": "future",
                "snapshot_dir": future,
                "manifest_path": future.join("manifest.json"),
                "question": "future",
            }))
            .unwrap(),
        )
        .unwrap();
        let oversized = root.join("oversized");
        std::fs::create_dir(&oversized).unwrap();
        std::fs::File::create(oversized.join("lvu-investigation.json"))
            .unwrap()
            .set_len(super::MAX_INVESTIGATION_RECORD_BYTES + 1)
            .unwrap();
        let escaped = root.join("escaped");
        let outside = directory.path().join("outside");
        std::fs::create_dir(&escaped).unwrap();
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("manifest.json"), b"{}").unwrap();
        std::fs::write(
            escaped.join("lvu-investigation.json"),
            serde_json::to_vec(&json!({
                "schema_version": 1,
                "investigation_id": uuid::Uuid::from_u128(5).to_string(),
                "view_id": uuid::Uuid::from_u128(6).to_string(),
                "session_id": "escape",
                "snapshot_dir": outside,
                "manifest_path": outside.join("manifest.json"),
                "question": "escape",
            }))
            .unwrap(),
        )
        .unwrap();

        let mut load = super::load_investigations(root);
        let loaded = load
            .result
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .unwrap();
        load.worker.take().unwrap().join().unwrap();
        assert_eq!(loaded.items, vec![item]);
        assert!(
            loaded
                .diagnostic
                .as_deref()
                .is_some_and(|message| message.contains("rejected 3 invalid records"))
        );
    }

    #[test]
    fn investigation_scan_limit_is_reported_instead_of_claiming_complete_history() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("investigations");
        std::fs::create_dir(&root).unwrap();
        for index in 0..=super::MAX_INVESTIGATION_SCAN_DIRS {
            std::fs::create_dir(root.join(format!("unrelated-{index:04}"))).unwrap();
        }

        let mut load = super::load_investigations(root);
        let loaded = load
            .result
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .unwrap();
        load.worker.take().unwrap().join().unwrap();
        assert!(loaded.items.is_empty());
        assert!(
            loaded
                .diagnostic
                .as_deref()
                .is_some_and(|message| message.contains("listing limit reached"))
        );
    }

    #[test]
    fn investigation_watch_health_detects_event_loss_and_disconnect() {
        assert!(
            super::investigation_health_failure(super::HostState::Running, 8, 7)
                .is_some_and(|message| message.contains("event loss"))
        );
        assert!(
            super::investigation_health_failure(super::HostState::Disconnected, 7, 7)
                .is_some_and(|message| message.contains("disconnected"))
        );
        assert!(super::investigation_health_failure(super::HostState::Running, 7, 7).is_none());
    }

    #[test]
    fn incomplete_remote_cancellation_and_slow_record_admission_are_bounded() {
        assert!(
            validate_remote_cancellation(&json!({
                "remote_cancelled": false,
                "remote_agent_may_still_be_running": true,
            }))
            .unwrap_err()
            .contains("may still be running")
        );
        assert!(
            validate_remote_cancellation(&json!({
                "remote_cancelled": true,
                "remote_agent_may_still_be_running": false,
            }))
            .is_ok()
        );

        let directory = tempfile::tempdir().unwrap();
        let mut senders = Vec::new();
        let mut jobs = Vec::new();
        for _ in 0..MAX_SESSION_RECORD_JOBS {
            let (sender, result) = std::sync::mpsc::sync_channel(1);
            senders.push(sender);
            jobs.push(super::SessionRecordJob {
                result,
                worker: None,
            });
        }
        let error = super::admit_session_record(
            &mut jobs,
            directory.path(),
            "must-not-spawn-an-untracked-worker",
        )
        .unwrap_err();
        assert!(error.contains("at most 4"));
        assert_eq!(jobs.len(), MAX_SESSION_RECORD_JOBS);
        drop(senders);
    }

    #[cfg(unix)]
    #[test]
    fn shutdown_settles_starting_and_proposing_sessions_remotely() {
        let directory = tempfile::tempdir().unwrap();
        let archive = directory.path().join("requests.jsonl");
        let script = directory.path().join("bridge.py");
        std::fs::write(
            &script,
            r#"import json, os, sys, time
archive = os.environ["ARCHIVE"]
for line in sys.stdin:
    request = json.loads(line)
    with open(archive, "a") as stream:
        stream.write(json.dumps(request) + "\n")
    method = request["method"]
    if method == "start_session":
        time.sleep(0.05)
        result = {"session_id": "session-shutdown"}
    elif method == "request_proposal":
        time.sleep(0.05)
        result = {"proposal": {"kind": request["kind"],
            "definition": {"schema_version": 1, "expression": "pl.lit(True)"},
            "explanation": "fixture",
            "originating_revision": request["originating_revision"]}}
    elif method == "cancel":
        result = {"cancelled": True, "remote_cancelled": True,
                  "remote_agent_may_still_be_running": False}
    else:
        result = {"accepted": True}
    print(json.dumps({"schema_version": 1, "request_id": request["request_id"],
                      "ok": True, "result": result}), flush=True)
"#,
        )
        .unwrap();
        let host = super::AgentBridgeHost::launch(super::agent::AgentBridgeConfig {
            program: "python3".into(),
            args: vec!["-u".into(), script.display().to_string()],
            cwd: directory.path().to_path_buf(),
            environment: vec![("ARCHIVE".into(), archive.display().to_string())],
            max_line_bytes: 262_144,
            max_pending: 4,
            event_capacity: 4,
            request_timeout: Duration::from_secs(2),
            shutdown_timeout: Duration::from_secs(1),
        })
        .unwrap();
        let start = AiStart {
            generation: 1,
            view_id: "view".into(),
            definition_revision: 1,
            kind: lvu::AskAiKind::Filter,
            instruction: "fixture".into(),
            provider: "fixture".into(),
            mode: "fixture".into(),
            thinking: "fixture".into(),
        };
        let starting = host
            .start_session("fixture", directory.path(), None, None, None)
            .unwrap();
        let failures = super::settle_ai_work(
            Some(AiWork::Starting {
                start: start.clone(),
                output_dir: directory.path().into(),
                manifest_path: directory.path().join("manifest.json"),
                datasets: Vec::new(),
                inline_context: None,
                inspection_command: None,
                revision: super::OriginatingRevision {
                    data: "snapshot".into(),
                    definition: "view:1".into(),
                },
                request: starting,
                cancelled: false,
            }),
            None,
            Some(&host),
            Instant::now() + Duration::from_secs(1),
        );
        assert!(failures.is_empty(), "{failures:?}");

        let session_id = host
            .start_session("fixture", directory.path(), None, None, None)
            .unwrap()
            .recv_timeout(Duration::from_secs(1))
            .unwrap();
        let revision = super::OriginatingRevision {
            data: "snapshot".into(),
            definition: "view:1".into(),
        };
        let proposal = host
            .propose(
                &session_id,
                super::ProposalKind::Filter,
                "fixture",
                revision,
                super::ProposalContext {
                    inline_context: None,
                    inspection_command: None,
                    manifest_path: directory.path().join("manifest.json"),
                    dataset_paths: Vec::new(),
                },
            )
            .unwrap();
        let failures = super::settle_ai_work(
            Some(AiWork::Proposing {
                start,
                output_dir: directory.path().into(),
                session_id,
                request: proposal,
            }),
            None,
            Some(&host),
            Instant::now() + Duration::from_secs(1),
        );
        assert!(failures.is_empty(), "{failures:?}");
        host.shutdown().unwrap();
        let requests = std::fs::read_to_string(archive).unwrap();
        assert_eq!(requests.matches("\"method\": \"cancel\"").count(), 2);
    }

    #[test]
    fn returning_to_tracked_state_cancels_obsolete_debounced_save() {
        let directory = std::env::current_dir().unwrap();
        let definition = definition(
            SourceArgument::File(directory.join("Cargo.toml")),
            &directory,
        )
        .unwrap();
        let memory_view_id = ViewId(uuid::Uuid::nil());
        let state_a = PersistentViewState {
            applied_search: "A".into(),
            ..PersistentViewState::default()
        };
        let state_b = PersistentViewState {
            applied_search: "B".into(),
            ..PersistentViewState::default()
        };

        let mut pending = HashMap::from([(
            memory_view_id,
            pending_memory_save(definition.clone(), state_b.clone()),
        )]);
        let durable = HashMap::from([(memory_view_id, state_a.clone())]);
        assert!(reconcile_pending_state(
            &mut pending,
            &durable,
            &HashMap::new(),
            &HashMap::new(),
            memory_view_id,
            &state_a,
        ));
        assert!(pending.is_empty(), "obsolete B must not reach the worker");

        let mut pending =
            HashMap::from([(memory_view_id, pending_memory_save(definition, state_b))]);
        let inflight = HashMap::from([(7, (memory_view_id, state_a.clone()))]);
        assert!(reconcile_pending_state(
            &mut pending,
            &HashMap::new(),
            &inflight,
            &HashMap::new(),
            memory_view_id,
            &state_a,
        ));
        assert!(pending.is_empty(), "obsolete B must not follow in-flight A");
    }

    #[test]
    fn per_source_view_limit_is_checked_before_registration() {
        let source_id = uuid::Uuid::from_u128(1).to_string();
        let views = (0..super::MAX_VIEWS_PER_SOURCE)
            .map(|index| ViewItem {
                id: uuid::Uuid::from_u128(index as u128 + 2).to_string(),
                source_id: source_id.clone(),
                name: format!("view {index}"),
            })
            .collect();
        let app = App::new(
            vec![SourceItem {
                id: source_id.clone(),
                name: "source".into(),
                health: "raw".into(),
            }],
            views,
            false,
        );
        assert_eq!(
            view_admission_error(&app, &source_id),
            Some("view admission limit reached")
        );
    }

    #[tokio::test]
    async fn global_view_limit_rejects_command_before_it_can_start() {
        let directory = tempfile::tempdir().expect("tempdir");
        let marker = directory.path().join("must-not-start");
        let command = format!("printf started > '{}'", marker.display());
        let definition = definition(SourceArgument::Command(command.clone()), directory.path())
            .expect("definition");
        let views = (0..MAX_VIEWS)
            .map(|index| ViewItem {
                id: format!("view-{index}"),
                source_id: format!("source-{}", index / super::MAX_VIEWS_PER_SOURCE),
                name: format!("View {index}"),
            })
            .collect();
        let mut app = App::new(Vec::new(), views, false);
        let manager = Arc::new(
            lvu_ingest::SourceManager::new(
                directory.path().join("captures"),
                lvu_ingest::RuntimeConfig::default(),
            )
            .expect("manager"),
        );
        let (starts_tx, starts_rx) = tokio::sync::mpsc::channel(2);
        let (scans_tx, scans_rx) = tokio::sync::mpsc::channel(2);
        let (completions_tx, completions_rx) = tokio::sync::mpsc::channel(2);
        let memory = super::MemoryWorker::start(directory.path().join("workspace"));
        let raw = Arc::new(
            lvu_live::LiveRowProvider::new(lvu_live::LiveConfig::new(
                directory.path().join("derived"),
            ))
            .unwrap(),
        );
        let mut composition = Composition {
            manager: Arc::clone(&manager),
            raw,
            runtime: tokio::runtime::Handle::current(),
            starts_tx,
            starts_rx,
            sources: HashMap::<SourceId, String>::new(),
            definitions: HashMap::new(),
            pending_starts: HashSet::new(),
            source_controls: HashMap::new(),
            cwd: directory.path().to_path_buf(),
            scans_tx,
            scans_rx,
            active_scan: None,
            pending_scan: None,
            discovery_candidates: HashMap::new(),
            recent_sources: Vec::new(),
            memory,
            memory_ready: HashSet::new(),
            memory_restoring: HashSet::new(),
            memory_deferred: HashMap::new(),
            memory_load_fences: HashMap::new(),
            memory_last: HashMap::new(),
            memory_unavailable: false,
            memory_pending: HashMap::new(),
            memory_inflight: HashMap::new(),
            memory_failed: HashMap::new(),
            memory_ack_sequence: HashMap::new(),
            memory_sequence: 0,
            completions_tx,
            completions_rx,
            active_completion: None,
            pending_completion: None,
            home: None,
            snapshot_root: directory.path().join("investigations"),
            agent: None,
            agent_error: Some("test offline".into()),
            active_ai: None,
            owned_ai_session: None,
            owned_ai_session_config: None,
            retire_ai_session: false,
            ai_session_busy: false,
            source_ai_work: None,
            source_ai_session: None,
            source_ai_session_config: None,
            deferred_owned_lifecycle_events: VecDeque::new(),
            deferred_owned_lifecycle_overflowed: false,
            source_ai_proposals: HashMap::new(),
            session_records: Vec::new(),
            investigation_work: None,
            investigation_session: None,
            investigation_load: None,
            storage_root: directory.path().into(),
            storage_job: None,
            pending_storage: None,
            query_index_limit: 1,
            storage_review: Vec::new(),
            settings_file: directory.path().join("config/settings.toml"),
            settings_paths: super::settings::AppPaths {
                config_dir: directory.path().join("config"),
                cache_dir: directory.path().join("cache"),
                data_dir: directory.path().join("data"),
                settings_file: directory.path().join("config/settings.toml"),
            },
            applied_settings: super::settings::Settings::default()
                .validate()
                .expect("settings"),
            settings_job: None,
            capture_root: directory.path().join("captures"),
            command_controller: super::command_controller::CommandController::new(
                directory.path().join("workspace"),
                directory.path().into(),
                super::command_rows::CommandPresentation::default(),
            ),
        };
        composition.admit_definition(
            &mut app,
            definition,
            StartOrigin::Manual(SourceLaunchRequest {
                kind: SourceKind::Command,
                text: command,
            }),
        );

        assert!(composition.pending_starts.is_empty());
        assert!(!marker.exists(), "rejected command must never be spawned");
        assert!(
            app.source_notice
                .as_deref()
                .is_some_and(|notice| notice.contains("view admission limit"))
        );
        composition.memory.stop();
        assert!(manager.shutdown().await.is_empty());
    }

    #[tokio::test]
    async fn stale_investigation_cancel_cannot_cancel_newer_turn() {
        let directory = tempfile::tempdir().expect("tempdir");
        let manager = Arc::new(
            lvu_ingest::SourceManager::new(
                directory.path().join("captures"),
                lvu_ingest::RuntimeConfig::default(),
            )
            .expect("manager"),
        );
        let (starts_tx, starts_rx) = tokio::sync::mpsc::channel(1);
        let (scans_tx, scans_rx) = tokio::sync::mpsc::channel(1);
        let (completions_tx, completions_rx) = tokio::sync::mpsc::channel(1);
        let item = lvu::InvestigationItem {
            id: uuid::Uuid::from_u128(11).to_string(),
            view_id: uuid::Uuid::from_u128(12).to_string(),
            session_id: "new-turn".into(),
            snapshot_dir: directory.path().display().to_string(),
            manifest_path: directory.path().join("manifest.json").display().to_string(),
            question: "new question".into(),
        };
        let memory = super::MemoryWorker::start(directory.path().join("workspace"));
        let raw = Arc::new(
            lvu_live::LiveRowProvider::new(lvu_live::LiveConfig::new(
                directory.path().join("derived"),
            ))
            .unwrap(),
        );
        let mut composition = Composition {
            manager: Arc::clone(&manager),
            raw,
            runtime: tokio::runtime::Handle::current(),
            starts_tx,
            starts_rx,
            sources: HashMap::new(),
            definitions: HashMap::new(),
            pending_starts: HashSet::new(),
            source_controls: HashMap::new(),
            cwd: directory.path().to_path_buf(),
            scans_tx,
            scans_rx,
            active_scan: None,
            pending_scan: None,
            discovery_candidates: HashMap::new(),
            recent_sources: Vec::new(),
            memory,
            memory_ready: HashSet::new(),
            memory_restoring: HashSet::new(),
            memory_deferred: HashMap::new(),
            memory_load_fences: HashMap::new(),
            memory_last: HashMap::new(),
            memory_unavailable: false,
            memory_pending: HashMap::new(),
            memory_inflight: HashMap::new(),
            memory_failed: HashMap::new(),
            memory_ack_sequence: HashMap::new(),
            memory_sequence: 0,
            completions_tx,
            completions_rx,
            active_completion: None,
            pending_completion: None,
            home: None,
            snapshot_root: directory.path().join("investigations"),
            agent: None,
            agent_error: Some("offline fixture".into()),
            active_ai: None,
            owned_ai_session: None,
            owned_ai_session_config: None,
            retire_ai_session: false,
            ai_session_busy: false,
            source_ai_work: None,
            source_ai_session: None,
            source_ai_session_config: None,
            deferred_owned_lifecycle_events: VecDeque::new(),
            deferred_owned_lifecycle_overflowed: false,
            source_ai_proposals: HashMap::new(),
            session_records: Vec::new(),
            investigation_work: Some(super::InvestigationWork::Watching {
                generation: 22,
                item: item.clone(),
                event_floor: 0,
                turn_started: true,
            }),
            investigation_session: Some(item),
            investigation_load: None,
            storage_root: directory.path().into(),
            storage_job: None,
            pending_storage: None,
            query_index_limit: 1,
            storage_review: Vec::new(),
            settings_file: directory.path().join("config/settings.toml"),
            settings_paths: super::settings::AppPaths {
                config_dir: directory.path().join("config"),
                cache_dir: directory.path().join("cache"),
                data_dir: directory.path().join("data"),
                settings_file: directory.path().join("config/settings.toml"),
            },
            applied_settings: super::settings::Settings::default()
                .validate()
                .expect("settings"),
            settings_job: None,
            capture_root: directory.path().join("captures"),
            command_controller: super::command_controller::CommandController::new(
                directory.path().join("workspace"),
                directory.path().into(),
                super::command_rows::CommandPresentation::default(),
            ),
        };

        composition.cancel_investigation(21);
        assert!(matches!(
            composition.investigation_work,
            Some(super::InvestigationWork::Watching { generation: 22, .. })
        ));
        composition.cancel_investigation(22);
        assert!(matches!(
            composition.investigation_work,
            Some(super::InvestigationWork::Unresolved { generation: 22, .. })
        ));

        composition.memory.stop();
        assert!(manager.shutdown().await.is_empty());
    }

    #[test]
    fn shutdown_preserves_settings_save_failures() {
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let worker = std::thread::spawn(move || {
            tx.send(Err("settings file is malformed; preserved".into()))
                .unwrap();
        });
        let mut job = super::SettingsSaveJob {
            generation: 1,
            result: rx,
            worker: Some(worker),
        };
        assert_eq!(
            job.settle(std::time::Duration::from_secs(2)),
            Err("settings file is malformed; preserved".into())
        );
        assert!(job.worker.is_none());
    }

    #[test]
    fn cli_is_repeatable_and_definitions_are_stable_and_explicit() {
        let directory = std::env::current_dir().expect("cwd");
        let file = directory.join("Cargo.toml");
        let options = parse_args(
            vec![
                "--capture-dir".into(),
                "captures".into(),
                "--file".into(),
                file.to_string_lossy().into_owned().into(),
                "--command".into(),
                "printf hello".into(),
            ],
            true,
        )
        .expect("parse")
        .expect("run");
        assert_eq!(options.sources.len(), 2);
        let first = definition(options.sources[0].clone(), &directory).expect("definition");
        let again = definition(SourceArgument::File(file), &directory).expect("definition");
        assert_eq!(first.id, again.id);
        let command = definition(options.sources[1].clone(), &directory).expect("command");
        let Acquisition::Command { command } = command.acquisition else {
            panic!("command acquisition");
        };
        assert!(matches!(command.program, CommandProgram::Shell { .. }));
        assert_eq!(command.cwd.as_deref(), Some(directory.as_path()));
    }

    #[test]
    fn cli_supports_positional_commands_end_options_and_redirected_stdin() {
        let options = parse_args(
            vec![
                "first.log".into(),
                "-c".into(),
                "printf short".into(),
                "--command".into(),
                "printf long".into(),
                "--file".into(),
                "compatible.log".into(),
                "--stdin".into(),
                "--".into(),
                "-looks-like-an-option".into(),
            ],
            false,
        )
        .expect("parse")
        .expect("run");
        assert!(matches!(options.sources[0], SourceArgument::File(_)));
        assert!(matches!(options.sources[1], SourceArgument::Command(_)));
        assert!(matches!(options.sources[2], SourceArgument::Command(_)));
        assert!(matches!(options.sources[3], SourceArgument::File(_)));
        assert!(matches!(options.sources[4], SourceArgument::Stdin));
        assert!(matches!(options.sources[5], SourceArgument::File(_)));
        assert_eq!(options.sources.len(), 6, "automatic stdin must deduplicate");

        let automatic = parse_args(vec!["only.log".into()], false)
            .expect("automatic stdin")
            .expect("run");
        assert_eq!(automatic.sources.len(), 2);
        assert!(matches!(automatic.sources[1], SourceArgument::Stdin));

        let interactive = parse_args(Vec::new(), true)
            .expect("interactive empty startup")
            .expect("run");
        assert!(interactive.sources.is_empty());
        assert!(
            parse_args(vec!["--help".into()], false)
                .expect("help")
                .is_none()
        );
        assert!(
            parse_args(vec!["--stdin".into()], true)
                .expect_err("terminal stdin rejected")
                .contains("stdin is a terminal")
        );
        assert!(
            parse_args(vec!["-".into(), "--stdin".into()], false)
                .expect_err("repeated stdin rejected")
                .contains("only once")
        );
        let first = definition(SourceArgument::Stdin, std::path::Path::new("/unused"))
            .expect("first stdin definition");
        let second = definition(SourceArgument::Stdin, std::path::Path::new("/unused"))
            .expect("second stdin definition");
        assert_ne!(first.id, second.id, "each pipeline gets a fresh source ID");
        assert!(matches!(first.acquisition, Acquisition::Stdin));
    }

    fn workspace_state(root: &std::path::Path) {
        std::fs::create_dir_all(root.join("workspace")).expect("workspace");
    }

    #[test]
    fn capture_root_prefers_explicit_then_preserves_legacy_without_moving_it() {
        let root = tempfile::tempdir().expect("root");
        let cwd = root.path().join("project");
        let xdg = root.path().join("xdg-data/lvu");
        std::fs::create_dir_all(&cwd).expect("cwd");

        let fresh = select_capture_root(None, &xdg, &cwd).expect("fresh");
        assert_eq!(fresh.root, xdg);
        assert_eq!(fresh.reason, CaptureRootReason::Default);
        assert!(fresh.notice(&xdg, None).is_none());
        assert!(fresh.record.is_none());

        let legacy = cwd.join(".lvu-captures");
        workspace_state(&legacy);
        let selected = select_capture_root(None, &xdg, &cwd).expect("legacy");
        assert_eq!(selected.root, legacy);
        assert_eq!(selected.reason, CaptureRootReason::LegacyState);
        assert!(
            selected
                .notice(&xdg, None)
                .expect("legacy notice")
                .contains("nothing was moved")
        );

        let explicit =
            select_capture_root(Some(PathBuf::from("chosen")), &xdg, &cwd).expect("explicit root");
        assert_eq!(explicit.root, cwd.join("chosen"));
        assert_eq!(explicit.reason, CaptureRootReason::Explicit);
        assert!(explicit.record.is_none(), "a one-off root is not recorded");
        assert!(legacy.is_dir());
    }

    /// The data-loss regression: the assistance bridge materialises the XDG data
    /// directory on every launch, so bare existence there must never move a
    /// working directory off the legacy root that holds its workspace.
    #[test]
    fn materialised_xdg_data_directory_does_not_abandon_a_legacy_workspace() {
        let root = tempfile::tempdir().expect("root");
        let cwd = root.path().join("project");
        let xdg = root.path().join("xdg-data/lvu");
        std::fs::create_dir_all(&cwd).expect("cwd");
        let legacy = cwd.join(".lvu-captures");
        workspace_state(&legacy);

        let first = select_capture_root(None, &xdg, &cwd).expect("first launch");
        assert_eq!(first.root, legacy);
        record_capture_root(
            first.record.as_ref().expect("recorded decision"),
            &first.root,
        )
        .expect("record");

        // What the bridge did behind the decision's back, plus an unrelated
        // XDG workspace, must not change the answer.
        std::fs::create_dir_all(xdg.join("assistance")).expect("assistance");
        workspace_state(&xdg);
        let second = select_capture_root(None, &xdg, &cwd).expect("second launch");
        assert_eq!(second.root, legacy, "the recorded root stays selected");
        assert_eq!(second.reason, CaptureRootReason::Recorded);
        assert!(second.record.is_none(), "an obeyed record is not rewritten");
    }

    #[test]
    fn assistance_only_xdg_root_is_not_durable_state() {
        let root = tempfile::tempdir().expect("root");
        let xdg = root.path().join("data/lvu");
        std::fs::create_dir_all(xdg.join("assistance")).expect("assistance");
        assert!(!capture_root_has_state(&xdg));
        std::fs::create_dir_all(xdg.join("investigations")).expect("investigations");
        assert!(capture_root_has_state(&xdg));

        let captured = root.path().join("captured");
        std::fs::create_dir_all(captured.join("2f7c-source")).expect("source directory");
        assert!(!capture_root_has_state(&captured));
        std::fs::write(captured.join("2f7c-source/source.json"), b"{}").expect("metadata");
        assert!(capture_root_has_state(&captured));
    }

    #[test]
    fn empty_legacy_directory_yields_to_an_xdg_workspace_and_records_it() {
        let root = tempfile::tempdir().expect("root");
        let cwd = root.path().join("project");
        let xdg = root.path().join("xdg-data/lvu");
        let legacy = cwd.join(".lvu-captures");
        std::fs::create_dir_all(&legacy).expect("legacy");

        let empty = select_capture_root(None, &xdg, &cwd).expect("both empty");
        assert_eq!(
            empty.root, legacy,
            "a directory the user made keeps its root"
        );
        assert_eq!(empty.reason, CaptureRootReason::LegacyEmpty);

        workspace_state(&xdg);
        let chosen = select_capture_root(None, &xdg, &cwd).expect("xdg state");
        assert_eq!(chosen.root, xdg);
        assert_eq!(chosen.reason, CaptureRootReason::DefaultOverEmptyLegacy);
        let pointer = chosen.record.clone().expect("recorded decision");
        record_capture_root(&pointer, &chosen.root).expect("record");
        // Recording is idempotent and later launches follow the record.
        record_capture_root(&pointer, &chosen.root).expect("re-record");
        let again = select_capture_root(None, &xdg, &cwd).expect("recorded");
        assert_eq!(again.root, xdg);
        assert_eq!(again.reason, CaptureRootReason::Recorded);
        assert!(legacy.is_dir(), "recording must not disturb legacy data");
    }

    #[test]
    fn explicit_known_root_records_a_migration_and_unknown_roots_do_not() {
        let root = tempfile::tempdir().expect("root");
        let cwd = root.path().join("project");
        let xdg = root.path().join("xdg-data/lvu");
        let legacy = cwd.join(".lvu-captures");
        workspace_state(&legacy);

        let migrate =
            select_capture_root(Some(xdg.clone()), &xdg, &cwd).expect("explicit migration");
        assert_eq!(migrate.root, xdg);
        assert_eq!(migrate.reason, CaptureRootReason::ExplicitMigration);
        let pointer = migrate.record.clone().expect("recorded migration");
        record_capture_root(&pointer, &migrate.root).expect("record");
        assert!(
            migrate
                .notice(&xdg, None)
                .expect("notice")
                .contains("nothing was moved")
        );

        // The recorded switch survives without the flag, and legacy data stays.
        let later = select_capture_root(None, &xdg, &cwd).expect("after migration");
        assert_eq!(later.root, xdg);
        assert_eq!(later.reason, CaptureRootReason::Recorded);
        assert!(legacy.join("workspace").is_dir());

        // A scratch root is a one-off and must not repoint the directory.
        let scratch = select_capture_root(Some(root.path().join("scratch")), &xdg, &cwd)
            .expect("scratch root");
        assert_eq!(scratch.reason, CaptureRootReason::Explicit);
        assert!(scratch.record.is_none());
        assert_eq!(
            select_capture_root(None, &xdg, &cwd)
                .expect("unchanged")
                .root,
            xdg
        );
    }

    #[test]
    fn malformed_or_future_capture_root_record_refuses_to_guess() {
        let root = tempfile::tempdir().expect("root");
        let cwd = root.path().join("project");
        let xdg = root.path().join("xdg-data/lvu");
        let legacy = cwd.join(".lvu-captures");
        workspace_state(&legacy);
        let pointer = legacy.join("capture-root.toml");

        std::fs::write(&pointer, b"not = [toml").expect("write");
        let error = select_capture_root(None, &xdg, &cwd).expect_err("malformed record");
        assert!(error.contains("capture root record"), "{error}");

        std::fs::write(
            &pointer,
            b"schema_version = 9
root = \"/tmp/elsewhere\"\n",
        )
        .expect("write");
        let error = select_capture_root(None, &xdg, &cwd).expect_err("future record");
        assert!(error.contains("newer lvu"), "{error}");
        // Refusing must not rewrite or delete the record it could not read.
        assert!(
            std::fs::read_to_string(&pointer)
                .expect("kept")
                .contains("9")
        );
    }

    #[test]
    fn advanced_compiler_uses_locked_python_project_through_mise_and_uv() {
        let helper = resources::resolve_all().0;
        let helper = helper.located().expect("checkout python helper");
        let config = compiler_config(helper);
        assert_eq!(config.executable, "mise");
        assert_eq!(&config.args[..4], ["exec", "--", "uv", "run"]);
        assert!(config.args.iter().any(|argument| argument == "--locked"));
        assert!(
            config
                .args
                .windows(2)
                .any(|pair| pair == ["-m", "lvu_expr_helper"])
        );
    }

    #[test]
    fn completion_preserves_spaces_unicode_nested_paths_and_tilde() {
        let root = tempfile::tempdir().expect("temp directory");
        std::fs::create_dir(root.path().join("nested space")).expect("nested directory");
        std::fs::write(root.path().join("nested space/über.log"), b"event\n")
            .expect("fixture file");
        std::fs::write(root.path().join("nested space/union.log"), b"event\n")
            .expect("second fixture");

        let nested = complete_path(
            PathCompletionRequest {
                generation: 3,
                draft: "nested sp".into(),
            },
            root.path(),
            None,
            &AtomicBool::new(false),
        );
        assert_eq!(nested.replacement.as_deref(), Some("nested space/"));
        assert_eq!(nested.candidates, vec!["nested space/"]);
        let inside = complete_path(
            PathCompletionRequest {
                generation: 31,
                draft: nested.replacement.expect("completed directory"),
            },
            root.path(),
            None,
            &AtomicBool::new(false),
        );
        assert_eq!(inside.candidates.len(), 2);

        let unicode = complete_path(
            PathCompletionRequest {
                generation: 4,
                draft: "~/nested space/ü".into(),
            },
            root.path(),
            Some(root.path()),
            &AtomicBool::new(false),
        );
        assert_eq!(
            unicode.replacement.as_deref(),
            Some("~/nested space/über.log")
        );
        assert_eq!(
            common_prefix(&unicode.candidates),
            "~/nested space/über.log"
        );
        assert_eq!(
            expand_tilde_path("~/nested space/über.log", Some(root.path())).expect("expand home"),
            root.path().join("nested space/über.log")
        );
        assert!(expand_tilde_path("~someone/log", Some(root.path())).is_err());

        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("nested space", root.path().join("linked logs"))
                .expect("directory symlink");
            let linked = complete_path(
                PathCompletionRequest {
                    generation: 41,
                    draft: "linked".into(),
                },
                root.path(),
                None,
                &AtomicBool::new(false),
            );
            assert_eq!(linked.replacement.as_deref(), Some("linked logs/"));
        }

        let missing = complete_path(
            PathCompletionRequest {
                generation: 5,
                draft: "missing/entry".into(),
            },
            root.path(),
            None,
            &AtomicBool::new(false),
        );
        assert!(
            missing
                .error
                .as_deref()
                .is_some_and(|error| error.contains("cannot list"))
        );

        for index in 0..70 {
            std::fs::write(root.path().join(format!("many-{index:02}.log")), b"")
                .expect("bounded candidate fixture");
        }
        let bounded = complete_path(
            PathCompletionRequest {
                generation: 6,
                draft: "many-".into(),
            },
            root.path(),
            None,
            &AtomicBool::new(false),
        );
        assert_eq!(bounded.candidates.len(), super::MAX_PATH_CANDIDATES);
        assert!(bounded.error.is_some());
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_file_arguments_have_distinct_byte_preserving_identities() {
        use std::{ffi::OsString, os::unix::ffi::OsStringExt};

        let directory = tempfile::tempdir().expect("tempdir");
        let left = directory.path().join(OsString::from_vec(vec![b'l', 0x80]));
        let right = directory.path().join(OsString::from_vec(vec![b'l', 0x81]));
        std::fs::write(&left, b"left\n").expect("left");
        std::fs::write(&right, b"right\n").expect("right");

        let options = parse_args(
            vec![
                left.clone().into_os_string(),
                OsString::from("--file"),
                right.clone().into_os_string(),
            ],
            true,
        )
        .expect("non-UTF-8 paths parse")
        .expect("run");
        let left_definition =
            definition(options.sources[0].clone(), directory.path()).expect("left definition");
        let right_definition =
            definition(options.sources[1].clone(), directory.path()).expect("right definition");
        assert_ne!(left_definition.id, right_definition.id);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn recorded_docker_result_exposes_service_evidence_and_authoritative_definition() {
        use lvu_discovery::{
            CancellationToken, DiscoveryLimits, DiscoveryRequest, DockerConfig, DockerRunner,
        };
        use std::{os::unix::fs::PermissionsExt, time::Duration};

        let directory = tempfile::tempdir().expect("tempdir");
        let script = directory.path().join("docker-fixture");
        std::fs::write(
            &script,
            "#!/bin/sh\nprintf '%s\\n' '{\"ID\":\"recorded-id\",\"Names\":\"shop-api-1\",\"State\":\"running\",\"Status\":\"Up\",\"Labels\":\"com.docker.compose.project=shop,com.docker.compose.service=api,com.docker.compose.container-number=1\"}'\n",
        )
        .expect("fixture");
        let mut permissions = std::fs::metadata(&script).expect("metadata").permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&script, permissions).expect("permissions");
        let result = lvu_discovery::discover(DiscoveryRequest {
            limits: DiscoveryLimits {
                maximum_candidates: 4,
                maximum_processes: 0,
                maximum_files: 0,
                maximum_files_per_process: 0,
                maximum_output_bytes: 16 * 1024,
                maximum_duration: Duration::from_secs(1),
            },
            cancel: CancellationToken::default(),
            docker: Some(DockerConfig {
                runner: DockerRunner { executable: script },
                context: Some("recorded".into()),
                history_lines: 20,
            }),
            procfs: None,
            project: None,
        })
        .await;
        assert_eq!(result.candidates.len(), 1, "{:?}", result.statuses);
        let candidate = &result.candidates[0];
        let authoritative_id = candidate.source.id;
        let item = discovery_item(candidate);
        assert!(item.label.contains("shop/api #1"));
        assert!(item.detail.contains("Docker container"));
        assert!(item.status.contains("Docker"));
        assert_eq!(candidate.source.id, authoritative_id);
        assert!(discovery_status(&result).contains("1 candidates"));
    }
}

#[cfg(test)]
mod source_control_tests {
    use super::*;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stop_and_restart_preserve_file_history_and_stdin_restart_is_refused() {
        let root = tempfile::TempDir::new().unwrap();
        let input = root.path().join("input.log");
        std::fs::write(&input, "one\n").unwrap();
        let manager = Arc::new(
            SourceManager::new(root.path().join("capture"), RuntimeConfig::default()).unwrap(),
        );
        let definition = definition(SourceArgument::File(input.clone()), root.path()).unwrap();
        let handle = manager.start(definition.clone()).await.unwrap();
        let mut progress = handle.subscribe();
        tokio::time::timeout(Duration::from_secs(3), async {
            while progress.borrow().records < 1 {
                progress.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
        control_source(manager.clone(), definition.clone(), false)
            .await
            .unwrap();
        assert!(handle.progress().state.is_terminal());
        assert_eq!(handle.read_page(0, 8, 4096).await.unwrap().records.len(), 1);
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(input)
            .unwrap();
        writeln!(file, "two").unwrap();
        let restarted = control_source(manager.clone(), definition, true)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(restarted.source_id(), handle.source_id());
        assert!(restarted.progress().generation > handle.progress().generation);
        let mut progress = restarted.subscribe();
        tokio::time::timeout(Duration::from_secs(3), async {
            while progress.borrow().records < 2 {
                progress.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
        assert_eq!(
            restarted.read_page(0, 8, 4096).await.unwrap().records.len(),
            2
        );
        let stdin = super::definition(SourceArgument::Stdin, root.path()).unwrap();
        let (_producer, reader) = tokio::io::duplex(64);
        let stdin_handle = manager
            .start_with_reader(stdin.clone(), reader)
            .await
            .unwrap();
        assert!(
            control_source(manager.clone(), stdin, true)
                .await
                .err()
                .unwrap()
                .contains("fresh pipeline")
        );
        assert!(
            !stdin_handle.progress().state.is_terminal(),
            "refusal must not stop the pipeline"
        );
        for (_, stopped) in manager.shutdown().await {
            assert!(stopped.unwrap().complete);
        }
    }
}
