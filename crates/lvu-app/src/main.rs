use std::{
    collections::{BTreeMap, HashMap, HashSet},
    env,
    ffi::OsString,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc as std_mpsc,
    },
    thread::JoinHandle,
    time::Duration,
};

use lvu::{
    App, AskAiKind, AskAiRequest, AskAiStage, DiscoveryItem, DiscoveryUiRequest, Focus,
    InvestigationItem, InvestigationRequest, InvestigationStage, PathCompletionRequest,
    SourceAiPreview, SourceAiRequest, SourceAiStage, SourceItem, SourceKind, SourceLaunchRequest,
    ViewItem, terminal::run_with_tick_mut,
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
    NativeViewAdapter, ScanState, SnapshotJob, SnapshotLimits, SnapshotState, ViewConfig,
};
use tokio::sync::mpsc;
use uuid::Uuid;

pub mod agent;
mod memory;
use agent::{
    AgentBridgeConfig, AgentBridgeHost, HostState, OriginatingRevision, ProposalContext,
    ProposalEnvelope, ProposalKind, Request as AgentRequest,
};
use memory::{Event as MemoryEvent, MemoryWorker, SaveRequest};

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
    Snapshot {
        start: AiStart,
        job: SnapshotJob,
    },
    Preparing {
        start: AiStart,
        output_dir: PathBuf,
        cancelled: bool,
        result: std_mpsc::Receiver<Result<PreparedAiContext, String>>,
        worker: JoinHandle<()>,
    },
    Starting {
        start: AiStart,
        output_dir: PathBuf,
        manifest_path: PathBuf,
        datasets: Vec<PathBuf>,
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

#[derive(Clone, Debug)]
struct Options {
    capture_dir: PathBuf,
    sources: Vec<SourceArgument>,
}

#[derive(Clone, Debug)]
enum SourceArgument {
    File(PathBuf),
    Command(String),
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

struct Composition {
    manager: Arc<SourceManager>,
    runtime: tokio::runtime::Handle,
    starts_tx: mpsc::Sender<StartResult>,
    starts_rx: mpsc::Receiver<StartResult>,
    sources: HashMap<SourceId, String>,
    definitions: HashMap<SourceId, SourceDefinition>,
    pending_starts: HashSet<SourceId>,
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
    memory_load_fences: HashMap<lvu_core::ViewId, u64>,
    memory_last: HashMap<lvu_core::ViewId, lvu::PersistentViewState>,
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
    ai_session_busy: bool,
    source_ai_work: Option<SourceAiWork>,
    source_ai_session: Option<(String, u64)>,
    source_ai_proposals: HashMap<u64, SourceDefinition>,
    session_records: Vec<SessionRecordJob>,
    investigation_work: Option<InvestigationWork>,
    investigation_session: Option<InvestigationItem>,
    investigation_load: Option<InvestigationLoadJob>,
}

impl Composition {
    fn tick(&mut self, app: &mut App, adapter: &mut NativeViewAdapter) -> bool {
        let mut changed = adapter.drain_updates(MAX_TICK_UPDATES) > 0;
        changed |= self.poll_memory(app, adapter);
        changed |= self.handle_recipe_requests(app);
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
        changed |= self.handle_view_requests(app, adapter);
        changed |= self.queue_memory_saves(app, false);
        for view in app.views.clone() {
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
                app.update_source_health(&view.source_id, health.clone());
                app.update_view_runtime_status(&view.id, health);
            }
        }
        changed
    }

    fn handle_recipe_requests(&mut self, app: &mut App) -> bool {
        let requests = app.take_recipe_requests();
        let changed = !requests.is_empty();
        for request in requests {
            match request {
                lvu::RecipeRequest::List { meta } => {
                    if let Err(error) = self.memory.list_recipes(meta) {
                        app.recipe_failed(meta, error);
                    }
                }
                lvu::RecipeRequest::Save {
                    meta,
                    name,
                    view_id,
                    config: state,
                } => {
                    let duplicate = app
                        .recipe_dialog
                        .as_ref()
                        .is_some_and(|dialog| dialog.items.iter().any(|item| item.name == name));
                    if duplicate {
                        app.recipe_failed(meta, "a recipe with that name already exists; revisions require an explicit future edit action".into());
                        continue;
                    }
                    let Some(view) = app.views.iter().find(|view| view.id == view_id) else {
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
                    let stages = if state.enrichment.is_empty() {
                        Vec::new()
                    } else if let Some((output, expression)) = state.enrichment.split_once('=') {
                        let output = output.trim();
                        let expression = expression.trim();
                        if output.is_empty() || expression.is_empty() {
                            app.recipe_failed(
                                meta,
                                "accepted enrichment cannot be represented as a recipe".into(),
                            );
                            continue;
                        }
                        vec![lvu_memory::StageDefinition::Polars {
                            id: Uuid::new_v4(),
                            expression: expression.to_owned(),
                            output: output.to_owned(),
                        }]
                    } else {
                        app.recipe_failed(
                            meta,
                            "accepted enrichment cannot be represented as a recipe".into(),
                        );
                        continue;
                    };
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
                        recipe_id: lvu_core::RecipeId::new(),
                        revision_id: Uuid::new_v4(),
                        name: name.clone(),
                        description: String::new(),
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
                            time_policy: state.capture_time.map_or(
                                lvu_memory::TimePolicy::All,
                                |window| lvu_memory::TimePolicy::Absolute {
                                    start_unix_nanos: window.start_unix_nanos,
                                    end_unix_nanos: window.end_unix_nanos,
                                },
                            ),
                        },
                    };
                    if let Err(error) = self.memory.save_recipe(meta, recipe) {
                        app.recipe_failed(meta, error);
                    }
                }
                lvu::RecipeRequest::Import { meta, path } => {
                    if let Err(error) = self.memory.import_recipe(meta, PathBuf::from(path)) {
                        app.recipe_failed(meta, error);
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
                            Err("another source AI request is settling".into()),
                        );
                        continue;
                    }
                    if let Some(error) = &self.agent_error {
                        app.finish_source_ai(
                            generation,
                            Err(format!("local Paseo bridge unavailable: {error}")),
                        );
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
                    if let Err(error) = admit_session_record(
                        &mut self.session_records,
                        &context.directory,
                        &session_id,
                    ) {
                        app.source_notice = Some(format!("source AI session record: {error}"));
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
                    }
                }
                Some(Err(error)) => {
                    let diagnostic =
                        format!("source AI cancellation: {}", host_error_message(error));
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
        if let Some((session_id, _)) = self.source_ai_session.clone() {
            self.source_ai_session = Some((session_id.clone(), start.generation));
            if let Err(error) =
                admit_session_record(&mut self.session_records, &context.directory, &session_id)
            {
                app.source_notice = Some(format!("source AI session record: {error}"));
            }
            self.begin_source_ai_proposal(app, start, context, session_id);
            return;
        }
        let Some(host) = &self.agent else {
            app.finish_source_ai(
                start.generation,
                Err("local Paseo bridge unavailable".into()),
            );
            return;
        };
        match host.start_session(
            &start.provider,
            &self.cwd,
            Some(&start.mode),
            Some(&start.thinking),
            Some("lvu source definition assistance"),
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
                Err("local Paseo bridge unavailable".into()),
            );
            return;
        };
        match host.propose(
            &session_id,
            ProposalKind::Source,
            &start.instruction,
            context.revision.clone(),
            ProposalContext {
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
                diagnostic: "bridge unavailable during source AI cleanup".into(),
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
                let message = format!("source AI cleanup: {}", host_error_message(error));
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
                            Err("another AI request is still settling".into()),
                        );
                        continue;
                    }
                    if let Some(error) = &self.agent_error {
                        app.finish_ask_ai(
                            generation,
                            &view_id,
                            definition_revision,
                            Err(format!("local Paseo bridge unavailable: {error}")),
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
                    let limits = SnapshotLimits {
                        maximum_rows: 50_000,
                        maximum_input_bytes: 512 * 1024 * 1024,
                        maximum_disk_bytes: 512 * 1024 * 1024,
                        maximum_parts: 512,
                        ..SnapshotLimits::default()
                    };
                    match adapter.start_snapshot(&view_id, &self.snapshot_root, limits) {
                        Ok(job) => {
                            app.update_ask_ai_progress(
                                generation,
                                AskAiStage::Snapshot,
                                "exporting fixed applied view".into(),
                                None,
                                Some(job.output_dir().display().to_string()),
                            );
                            self.active_ai = Some(AiWork::Snapshot { start, job });
                        }
                        Err(error) => {
                            app.finish_ask_ai(
                                generation,
                                &view_id,
                                definition_revision,
                                Err(format!("snapshot: {error}")),
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
            AiWork::Snapshot { start, job } => {
                let status = job.poll();
                match status.state {
                    SnapshotState::Pending | SnapshotState::Running => {
                        app.update_ask_ai_progress(
                            start.generation,
                            AskAiStage::Snapshot,
                            format!(
                                "snapshot: {} scanned, {} matched",
                                status.source_rows_scanned, status.filtered_rows_written
                            ),
                            None,
                            None,
                        );
                        self.active_ai = Some(AiWork::Snapshot { start, job });
                    }
                    SnapshotState::Complete => {
                        let output_dir = job.output_dir().to_path_buf();
                        let manifest_path = status
                            .manifest_path
                            .unwrap_or_else(|| output_dir.join("manifest.json"));
                        let (result, worker) = prepare_ai_context(
                            output_dir.clone(),
                            manifest_path,
                            start.view_id.clone(),
                            start.definition_revision,
                        );
                        app.update_ask_ai_progress(
                            start.generation,
                            AskAiStage::Snapshot,
                            "preparing absolute snapshot paths".into(),
                            None,
                            None,
                        );
                        self.active_ai = Some(AiWork::Preparing {
                            start,
                            output_dir,
                            cancelled: false,
                            result,
                            worker,
                        });
                    }
                    SnapshotState::Limited => finish_ai_error(
                        app,
                        &start,
                        status.diagnostic.unwrap_or_else(|| {
                            format!(
                                "snapshot limit reached after {} rows; narrow the view and retry",
                                status.source_rows_scanned
                            )
                        }),
                    ),
                    SnapshotState::Cancelled | SnapshotState::Failed => finish_ai_error(
                        app,
                        &start,
                        status
                            .diagnostic
                            .unwrap_or_else(|| format!("snapshot {:?}", status.state)),
                    ),
                }
                changed = true;
            }
            AiWork::Preparing {
                start,
                output_dir,
                cancelled,
                result,
                worker,
            } => match result.try_recv() {
                Err(std_mpsc::TryRecvError::Empty) => {
                    self.active_ai = Some(AiWork::Preparing {
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
                        finish_ai_error(app, &start, "snapshot path worker disconnected".into());
                    }
                    changed = true;
                }
                Ok(prepared) => {
                    let _ = worker.join();
                    if cancelled {
                        return true;
                    }
                    match prepared {
                        Err(error) => finish_ai_error(app, &start, error),
                        Ok(context) => self.begin_agent_request(app, start, output_dir, context),
                    }
                    changed = true;
                }
            },
            AiWork::Starting {
                start,
                output_dir,
                manifest_path,
                datasets,
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
                    self.ai_session_busy = true;
                    if let Err(error) =
                        admit_session_record(&mut self.session_records, &output_dir, &session_id)
                    {
                        app.source_notice = Some(format!("AI session record error: {error}"));
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
                        Some((start.clone(), format!("local Paseo bridge: {message}"))),
                        start.generation,
                    ) {
                        finish_ai_error(
                            app,
                            &start,
                            format!("local Paseo bridge: {message}; {cleanup}"),
                        );
                    }
                    changed = true;
                }
                Some(Ok(proposal)) => {
                    self.ai_session_busy = false;
                    let expression = proposal_expression(start.kind, &proposal);
                    app.finish_ask_ai(
                        start.generation,
                        &start.view_id,
                        start.definition_revision,
                        expression.map(|value| (value, proposal.explanation)),
                    );
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
                            if let Some((start, message)) = failure {
                                finish_ai_error(app, &start, message);
                            }
                        }
                        Err(error) => {
                            self.agent_error = Some(error.clone());
                            if let Some((start, message)) = failure {
                                finish_ai_error(app, &start, format!("{message}; {error}"));
                            } else {
                                app.source_notice = Some(format!("AI session cleanup: {error}"));
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
                        app.source_notice = Some(format!("AI session cleanup: {cleanup}"));
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
                        finish_investigation_error(
                            app,
                            generation,
                            &format!("local Paseo bridge unavailable: {error}"),
                        );
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
                            "local Paseo bridge unavailable",
                        );
                        continue;
                    };
                    match host.resume_session(&item.session_id) {
                        Ok(request) => {
                            app.update_investigation_progress(
                                generation,
                                InvestigationStage::Resuming,
                                "resuming local Paseo session".into(),
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
                            "local Paseo bridge unavailable",
                        );
                        return true;
                    };
                    match host.start_session(
                        &start.provider,
                        &output_dir,
                        Some(&start.mode),
                        Some(&start.thinking),
                        Some("lvu investigation"),
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
            finish_investigation_error(app, generation, "local Paseo bridge unavailable");
            self.investigation_session = Some(item.clone());
            self.investigation_work = Some(InvestigationWork::Unresolved {
                generation,
                item,
                diagnostic: "local Paseo bridge unavailable; remote session ownership unresolved"
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
            let work = self.active_ai.take().expect("active AI checked above");
            match work {
                AiWork::Snapshot { job, .. } => job.cancel(),
                AiWork::Preparing {
                    start,
                    output_dir,
                    result,
                    worker,
                    ..
                } => {
                    self.active_ai = Some(AiWork::Preparing {
                        start,
                        output_dir,
                        cancelled: true,
                        result,
                        worker,
                    });
                }
                AiWork::Starting {
                    start,
                    output_dir,
                    manifest_path,
                    datasets,
                    revision,
                    request,
                    ..
                } => {
                    self.active_ai = Some(AiWork::Starting {
                        start,
                        output_dir,
                        manifest_path,
                        datasets,
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
                        "agent is waiting for a local permission decision in Paseo".into(),
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
            if event.kind.contains("permission")
                && let Some(work) = &self.active_ai
            {
                app.update_ask_ai_progress(
                    ai_generation(work),
                    AskAiStage::Proposing,
                    "agent is waiting for a local permission decision in Paseo".into(),
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
        if let Some(session_id) = self.owned_ai_session.clone() {
            self.ai_session_busy = true;
            if let Err(error) =
                admit_session_record(&mut self.session_records, &output_dir, &session_id)
            {
                app.source_notice = Some(format!("AI session record error: {error}"));
            }
            self.begin_proposal(app, start, output_dir, context, session_id);
            return;
        }
        let Some(host) = &self.agent else {
            finish_ai_error(app, &start, "local Paseo bridge unavailable".into());
            return;
        };
        match host.start_session(
            &start.provider,
            &output_dir,
            Some(&start.mode),
            Some(&start.thinking),
            Some("lvu Ask AI"),
        ) {
            Ok(request) => {
                app.update_ask_ai_progress(
                    start.generation,
                    AskAiStage::StartingSession,
                    "starting local Paseo session".into(),
                    None,
                    None,
                );
                self.active_ai = Some(AiWork::Starting {
                    start,
                    output_dir,
                    manifest_path: context.manifest_path,
                    datasets: context.datasets,
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
        };
        let Some(host) = &self.agent else {
            self.ai_session_busy = false;
            finish_ai_error(app, &start, "local Paseo bridge unavailable".into());
            return;
        };
        match host.propose(
            &session_id,
            kind,
            &start.instruction,
            context.revision,
            ProposalContext {
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
                let message = format!("local Paseo bridge: {}", host_error_message(error));
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
                        app.source_notice = Some(format!("AI session record error: {error}"));
                    }
                    changed = true;
                }
                Err(std_mpsc::TryRecvError::Disconnected) => {
                    let mut job = self.session_records.swap_remove(index);
                    if let Some(worker) = job.worker.take() {
                        let _ = worker.join();
                    }
                    app.source_notice = Some("AI session record worker disconnected".into());
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
                Err(_) => failures.push("AI session record did not finish".into()),
            }
        }
        if let Some(host) = &self.agent
            && let Err(error) = host.shutdown()
        {
            failures.push(format!("AI bridge shutdown: {}", host_error_message(error)));
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
                                failures.push("source AI context worker panicked".into());
                            }
                        }
                        Err(std_mpsc::RecvTimeoutError::Timeout) => {
                            failures.push(
                                "source AI context worker did not stop before deadline".into(),
                            );
                        }
                    }
                }
                SourceAiWork::Starting { request, .. } => {
                    match request.recv_timeout(remaining(deadline)) {
                        Ok(id) => session = Some(id),
                        Err(error) => failures.push(format!(
                            "source AI session start unresolved: {}",
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
                            "source AI cancellation unresolved: {}",
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
                        "source AI cancellation unresolved: {}",
                        host_error_message(error)
                    )),
                },
                Some(Err(error)) => failures.push(format!(
                    "source AI cancellation could not start: {}",
                    host_error_message(error)
                )),
                None => failures.push("source AI cleanup unavailable".into()),
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

    fn handle_view_requests(&mut self, app: &mut App, adapter: &NativeViewAdapter) -> bool {
        let requests = app.take_view_requests();
        let changed = !requests.is_empty();
        for request in requests {
            if request.mode == lvu::ViewDialogMode::Rename {
                if app.views.iter().any(|view| {
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
                .views
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
            if let Err(error) = adapter.register_view(&new_id, vec![source_id]) {
                app.view_request_failed(format!("register view: {error}"));
                continue;
            }
            let cloned = (request.mode == lvu::ViewDialogMode::Clone)
                .then(|| app.persistent_view_state(&request.view_id))
                .flatten();
            app.add_view(ViewItem {
                id: new_id.clone(),
                source_id: request.source_id,
                name: request.name,
            });
            if let Some(mut state) = cloned {
                state.view_name = app
                    .views
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
        changed
    }

    fn handle_memory_event(
        &mut self,
        app: &mut App,
        adapter: &NativeViewAdapter,
        event: MemoryEvent,
    ) {
        match event {
            MemoryEvent::Loaded(source_id, _requested, stored) => {
                for value in stored {
                    let id = value.id;
                    let ui_id = id.0.to_string();
                    if app.views.iter().all(|view| view.id != ui_id) {
                        if let Some(error) = view_admission_error(app, &source_id.0.to_string()) {
                            memory_notice(app, format!("restore view {:?}: {error}", value.name));
                            continue;
                        }
                        if let Err(error) = adapter.register_view(&ui_id, vec![source_id]) {
                            memory_notice(app, format!("restore view: {error}"));
                            continue;
                        }
                        app.add_view(ViewItem {
                            id: ui_id.clone(),
                            source_id: source_id.0.to_string(),
                            name: value.name.clone(),
                        });
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
                self.memory_ready.insert(source_id);
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
            MemoryEvent::Recipes(meta, values) => {
                let items = values
                    .into_iter()
                    .map(|(recipe, _hash)| recipe_item(recipe))
                    .collect();
                app.set_recipes(meta, items, None);
            }
            MemoryEvent::RecipeSaved(meta, saved) => app.recipe_saved(
                meta,
                format!("saved immutable revision {}", saved.revision_id),
            ),
            MemoryEvent::RecipeFailed(meta, error) => app.recipe_failed(meta, error),
            MemoryEvent::RecentFailed(error) | MemoryEvent::Fatal(error) => {
                memory_notice(app, error)
            }
        }
    }

    fn queue_memory_saves(&mut self, app: &App, force: bool) -> bool {
        const AUTOSAVE_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(250);
        let mut changed = false;
        for view in app.views.clone() {
            let Ok(view_uuid) = Uuid::parse_str(&view.id) else {
                continue;
            };
            let memory_view_id = lvu_core::ViewId(view_uuid);
            let Ok(source_uuid) = Uuid::parse_str(&view.source_id) else {
                continue;
            };
            let source_id = SourceId(source_uuid);
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
        } else if app.views.len() + self.pending_starts.len() >= MAX_VIEWS {
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

fn view_admission_error(app: &App, source_id: &str) -> Option<&'static str> {
    let source_views = app
        .views
        .iter()
        .filter(|view| view.source_id == source_id)
        .count();
    (app.views.len() >= MAX_VIEWS || source_views >= MAX_VIEWS_PER_SOURCE)
        .then_some("view admission limit reached")
}

fn ai_generation(work: &AiWork) -> u64 {
    match work {
        AiWork::Snapshot { start, .. }
        | AiWork::Preparing { start, .. }
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
    let datasets = context
        .datasets
        .iter()
        .map(|path| format!("- {}", path.display()))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "Investigate the fixed local lvu log snapshot described below. Use the manifest and Parquet files directly. Preserve raw record identity and distinguish captured data from inference. Do not modify the capture.\n\nQuestion: {question}\nManifest: {}\nDatasets:\n{datasets}",
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
            AiWork::Snapshot { job, .. } => job.cancel(),
            AiWork::Preparing { result, worker, .. } => {
                match result.recv_timeout(remaining(deadline)) {
                    Ok(_) => {
                        let _ = worker.join();
                    }
                    Err(_) => failures.push("AI snapshot path preparation did not stop".into()),
                }
            }
            AiWork::Starting { request, .. } => match request.recv_timeout(remaining(deadline)) {
                Ok(session_id) => session_to_cancel = Some(session_id),
                Err(error) => failures.push(format!(
                    "AI session start did not settle before shutdown: {}",
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
                        "AI session {session_id} cancellation did not settle: {}",
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
                    "AI session {session_id} cancellation did not settle: {}",
                    host_error_message(error)
                )),
            },
            Some(Err(error)) => failures.push(format!(
                "AI session {session_id} cancellation could not start: {}",
                host_error_message(error)
            )),
            None => failures.push(format!("AI session {session_id} cancellation unavailable")),
        }
    }
    failures
}

fn finish_ai_host_error(app: &mut App, start: &AiStart, error: agent::HostError) {
    let message = host_error_message(error);
    finish_ai_error(app, start, format!("local Paseo bridge: {message}"));
}

fn host_error_message(error: agent::HostError) -> String {
    match error {
        agent::HostError::NotRunning(message)
        | agent::HostError::Io(message)
        | agent::HostError::Protocol(message) => message,
        agent::HostError::Capacity => "bridge request capacity reached".into(),
        agent::HostError::Timeout => "bridge request timed out".into(),
        agent::HostError::Bridge(failure) => {
            format!("{}: {}", failure.code, failure.message)
        }
    }
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
                || "remote AI agent may still be running after cancellation".into(),
                |error| format!("remote AI agent may still be running: {error}"),
            )),
        None => Err("AI bridge cancellation response omitted remote lifecycle status".into()),
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
    }
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

fn recipe_item(recipe: lvu_memory::RecipeFile) -> lvu::RecipeItem {
    let incompatibility = recipe_incompatibility(&recipe.view);
    let enrichment = recipe
        .view
        .stages
        .iter()
        .find_map(|stage| match stage {
            lvu_memory::StageDefinition::Polars {
                expression, output, ..
            } => Some(format!("{output} = {expression}")),
            _ => None,
        })
        .unwrap_or_default();
    let color_field = recipe
        .view
        .color_rules
        .iter()
        .find(|rule| rule.style == "stable-value")
        .map(|rule| rule.expression.clone());
    let capture_time = match recipe.view.time_policy {
        lvu_memory::TimePolicy::Absolute {
            start_unix_nanos,
            end_unix_nanos,
        } => Some(lvu::CaptureTimeRange {
            start_unix_nanos,
            end_unix_nanos,
        }),
        _ => None,
    };
    lvu::RecipeItem {
        id: recipe.recipe_id.0.to_string(),
        revision: recipe.revision_id.to_string(),
        name: recipe.name,
        incompatibility,
        config: lvu::RecipeConfig {
            search: recipe.view.search,
            advanced: recipe
                .view
                .advanced_filter
                .map(|value| value.expression)
                .unwrap_or_default(),
            enrichment,
            pinned_columns: recipe.view.pinned_columns,
            color_field,
            capture_time,
        },
    }
}

fn recipe_incompatibility(view: &lvu_memory::NamedViewDefinition) -> Option<String> {
    if matches!(view.time_policy, lvu_memory::TimePolicy::Recent { .. }) {
        Some("rolling time-window recipes are not supported by this viewer".to_owned())
    } else if view.pinned_columns.len() > 8 {
        Some("recipe has more than 8 pinned columns".to_owned())
    } else if view.color_rules.len() > 1
        || view
            .color_rules
            .iter()
            .any(|rule| rule.style != "stable-value")
    {
        Some("recipe uses unsupported color rules".to_owned())
    } else if view.stages.len() > 1 {
        Some("recipe has multiple enrichment stages; this viewer supports one".to_owned())
    } else if view
        .stages
        .iter()
        .any(|stage| matches!(stage, lvu_memory::StageDefinition::Command { .. }))
    {
        Some("command enrichment recipes are not supported".to_owned())
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
                return Err("source AI context cancelled".into());
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
        return Err("source AI context cancelled".into());
    }
    if let Some(root) = directory.parent() {
        std::fs::create_dir_all(root)
            .map_err(|error| format!("create source AI context root: {error}"))?;
        let entries = std::fs::read_dir(root)
            .map_err(|error| format!("read source AI context root: {error}"))?
            .filter_map(Result::ok)
            .take(1025)
            .collect::<Vec<_>>();
        if entries.len() > 1024 {
            return Err("source AI context directory scan limit reached".into());
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
                "source AI context limit reached (64); start with a fresh capture directory".into(),
            );
        }
    }
    std::fs::create_dir_all(&directory)
        .map_err(|error| format!("create source AI context: {error}"))?;
    let outcome = write_source_ai_manifest(&directory, &cwd, &discovered, cancel);
    if outcome.is_err() {
        let _ = std::fs::remove_file(directory.join(".manifest.json.tmp"));
        let _ = std::fs::remove_dir(&directory);
    }
    let manifest = outcome?;
    let directory = std::fs::canonicalize(&directory)
        .map_err(|error| format!("resolve source AI context: {error}"))?;
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
        .map_err(|error| format!("create source AI manifest: {error}"))?;
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
        .map_err(|error| format!("encode source AI context: {error}"))?;
    writer
        .inner
        .sync_all()
        .map_err(|error| format!("sync source AI context: {error}"))?;
    if cancel.is_cancelled() {
        return Err("source AI context cancelled".into());
    }
    std::fs::rename(&temporary, &manifest)
        .map_err(|error| format!("publish source AI context: {error}"))?;
    if cancel.is_cancelled() {
        let _ = std::fs::remove_file(&manifest);
        return Err("source AI context cancelled".into());
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
            return Err(std::io::Error::other("source AI context cancelled"));
        }
        if bytes.len() > self.limit.saturating_sub(self.written) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::FileTooLarge,
                "source AI context exceeds 2 MiB",
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
                    "only restart policy 'never' is supported for reviewed AI commands".into(),
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

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() {
    let result = run().await;
    if let Err(error) = result {
        eprintln!("lvu-app: {error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), String> {
    let Some(options) = parse_args(env::args_os().skip(1).collect())? else {
        print_help();
        return Ok(());
    };
    let cwd = env::current_dir().map_err(|error| format!("current directory: {error}"))?;
    let mut live_config = LiveConfig::new(options.capture_dir.join("derived"));
    live_config.maximum_request_rows = 256;
    let raw = Arc::new(
        LiveRowProvider::new(live_config).map_err(|error| format!("live row provider: {error}"))?,
    );
    let manager = Arc::new(
        SourceManager::new(&options.capture_dir, RuntimeConfig::default())
            .map_err(|error| format!("capture manager: {error}"))?,
    );
    // Resolve once during startup. Terminal ticks and agent workers only see
    // absolute snapshot/session paths, regardless of a relative --capture-dir.
    let snapshot_root = match std::fs::canonicalize(&options.capture_dir) {
        Ok(root) => root.join("investigations"),
        Err(error) => {
            let cleanup = cleanup(raw.as_ref(), &manager).await;
            return Err(combine_errors(
                format!("capture directory: {error}"),
                cleanup,
            ));
        }
    };
    let mut view_config = ViewConfig::new(options.capture_dir.join("views"));
    view_config.compiler = Some(compiler_config());
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
    let ai_provider = env::var("LVU_AI_PROVIDER").unwrap_or_else(|_| "codex/gpt-5.6-sol".into());
    let ai_mode = env::var("LVU_AI_MODE").unwrap_or_else(|_| "full-access".into());
    let ai_thinking = env::var("LVU_AI_THINKING").unwrap_or_else(|_| "medium".into());
    app.configure_ai(ai_provider, ai_mode, ai_thinking);
    let mut source_ids = HashMap::new();
    let mut definitions = HashMap::new();
    let mut startup_error = None;
    for argument in options.sources {
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
        let started = match start_definition(&manager, definition).await {
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
    if !app.views.is_empty() {
        app.source_dialog = None;
        app.focus = Focus::Logs;
    }

    let (starts_tx, starts_rx) = mpsc::channel(MAX_PENDING_STARTS);
    let (scans_tx, scans_rx) = mpsc::channel(2);
    let (completions_tx, completions_rx) = mpsc::channel(2);
    let memory = MemoryWorker::start(options.capture_dir.join("workspace"));
    let recent_error = memory.recent().err();
    let (agent, agent_error) = match AgentBridgeHost::launch(agent_config(&cwd)) {
        Ok(host) => (Some(host), None),
        Err(error) => (None, Some(format!("{error:?}"))),
    };
    let mut composition = Composition {
        manager: Arc::clone(&manager),
        runtime: tokio::runtime::Handle::current(),
        starts_tx,
        starts_rx,
        sources: source_ids,
        definitions,
        pending_starts: HashSet::new(),
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
        memory_load_fences: HashMap::new(),
        memory_last: HashMap::new(),
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
        ai_session_busy: false,
        source_ai_work: None,
        source_ai_session: None,
        source_ai_proposals: HashMap::new(),
        session_records: Vec::new(),
        investigation_work: None,
        investigation_session: None,
        investigation_load: Some(load_investigations(snapshot_root.clone())),
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
    let mut rows = adapter.rows();
    let terminal_result = run_with_tick_mut(
        &mut app,
        &mut rows,
        &mut adapter,
        |_| false,
        |app, _rows, adapter| composition.tick(app, adapter),
    );
    composition.cancel_discovery();
    let investigation_shutdown_result = composition.shutdown_investigation(Duration::from_secs(3));
    let source_ai_shutdown_result = composition.shutdown_source_ai(Duration::from_secs(3));
    let ai_shutdown_result = composition.shutdown_ai(Duration::from_secs(3));
    let memory_flush_result =
        composition.flush_memory(&mut app, &adapter, std::time::Duration::from_millis(500));
    composition.memory.stop();
    adapter.shutdown();
    let cleanup_result = cleanup(raw.as_ref(), &manager).await;
    let lifecycle_error = memory_flush_result
        .err()
        .into_iter()
        .chain(investigation_shutdown_result.err())
        .chain(source_ai_shutdown_result.err())
        .chain(ai_shutdown_result.err())
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

fn view_id(source_id: SourceId) -> String {
    Uuid::new_v5(
        &SOURCE_NAMESPACE,
        format!("working-view:{}", source_id.0).as_bytes(),
    )
    .to_string()
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
            id: started.view_id,
            source_id: ui_id.clone(),
            name: "Raw events".into(),
        },
    );
    sources.insert(source_id, ui_id);
    Ok(view_id(source_id))
}

fn compiler_config() -> CompilerHostConfig {
    let project = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../python");
    let mut config = CompilerHostConfig::python_module("mise", "lvu_expr_helper");
    config.args = vec![
        "exec".into(),
        "--".into(),
        "uv".into(),
        "run".into(),
        "--project".into(),
        project.to_string_lossy().into_owned(),
        "--locked".into(),
        "python".into(),
        "-m".into(),
        "lvu_expr_helper".into(),
    ];
    config
}

fn agent_config(cwd: &Path) -> AgentBridgeConfig {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut config = AgentBridgeConfig::mise_bridge(&repository);
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

fn parse_args(arguments: Vec<OsString>) -> Result<Option<Options>, String> {
    let mut capture_dir = PathBuf::from(".lvu-captures");
    let mut sources = Vec::new();
    let mut index = 0;
    while index < arguments.len() {
        let option = arguments[index]
            .to_str()
            .ok_or_else(|| "option names must be valid UTF-8".to_owned())?;
        match option {
            "--help" | "-h" => return Ok(None),
            "--capture-dir" => {
                index += 1;
                capture_dir = PathBuf::from(value_os(&arguments, index, "--capture-dir")?);
            }
            "--file" => {
                index += 1;
                sources.push(SourceArgument::File(PathBuf::from(value_os(
                    &arguments, index, "--file",
                )?)));
                ensure_source_bound(&sources)?;
            }
            "--command" => {
                index += 1;
                let text = value_os(&arguments, index, "--command")?
                    .clone()
                    .into_string()
                    .map_err(|_| "--command requires valid UTF-8 shell text".to_owned())?;
                sources.push(SourceArgument::Command(text));
                ensure_source_bound(&sources)?;
            }
            unknown => return Err(format!("unknown argument {unknown:?}; use --help")),
        }
        index += 1;
    }
    Ok(Some(Options {
        capture_dir,
        sources,
    }))
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
        "lvu-app — live local log viewer\n\n\
         Usage: lvu-app [--capture-dir PATH] [--file PATH]... [--command SHELL_TEXT]...\n\n\
         --file PATH       Capture and follow a file (repeatable)\n\
         --command TEXT    Capture `sh -c TEXT` in the current directory (repeatable)\n\
         --capture-dir PATH  Durable journals and derived indexes\n\
         --help            Show this help\n\n\
         With no sources, the terminal opens an Add source dialog. Tab completes file\n\
         paths; Alt-F/Alt-C selects file or command; Ctrl-D opens discovery; Ctrl-A asks AI for a reviewed source definition.\n\
         With sources, / opens literal search, p advanced Polars, e enrichment,
         A opens definition Ask AI, I opens a snapshot investigation, and v manages views."
    );
}

#[cfg(test)]
mod tests {
    use super::{
        AiStart, AiWork, AtomicBool, Composition, MAX_SESSION_RECORD_JOBS, MAX_VIEWS,
        PendingMemorySave, SourceArgument, StartOrigin, common_prefix, compiler_config,
        complete_path, definition, discovery_item, discovery_status, expand_tilde_path, parse_args,
        prepare_ai_context, proposal_expression, recipe_incompatibility, reconcile_pending_state,
        record_agent_session, validate_remote_cancellation, view_admission_error,
    };
    use lvu::{
        App, PathCompletionRequest, PersistentViewState, SourceItem, SourceKind,
        SourceLaunchRequest, ViewItem,
    };
    use lvu_core::{Acquisition, CommandProgram, SourceId, ViewId};
    use serde_json::json;
    use std::{
        collections::{HashMap, HashSet},
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

    #[test]
    fn recipes_reject_unsupported_time_colors_and_pin_counts_without_partial_projection() {
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
        };
        view.time_policy = lvu_memory::TimePolicy::Recent { seconds: 60 };
        assert!(
            recipe_incompatibility(&view)
                .unwrap()
                .contains("time-window")
        );
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
                    "name": "AI",
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
                    "name": "AI",
                    "expressions": {"one": "pl.lit(1)", "two": "pl.lit(2)"}
                }]
            }),
            ..enrichment
        };
        assert!(proposal_expression(lvu::AskAiKind::Enrichment, &multiple).is_err());
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
        let mut composition = Composition {
            manager: Arc::clone(&manager),
            runtime: tokio::runtime::Handle::current(),
            starts_tx,
            starts_rx,
            sources: HashMap::<SourceId, String>::new(),
            definitions: HashMap::new(),
            pending_starts: HashSet::new(),
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
            memory_load_fences: HashMap::new(),
            memory_last: HashMap::new(),
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
            ai_session_busy: false,
            source_ai_work: None,
            source_ai_session: None,
            source_ai_proposals: HashMap::new(),
            session_records: Vec::new(),
            investigation_work: None,
            investigation_session: None,
            investigation_load: None,
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
        let mut composition = Composition {
            manager: Arc::clone(&manager),
            runtime: tokio::runtime::Handle::current(),
            starts_tx,
            starts_rx,
            sources: HashMap::new(),
            definitions: HashMap::new(),
            pending_starts: HashSet::new(),
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
            memory_load_fences: HashMap::new(),
            memory_last: HashMap::new(),
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
            ai_session_busy: false,
            source_ai_work: None,
            source_ai_session: None,
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
    fn cli_is_repeatable_and_definitions_are_stable_and_explicit() {
        let directory = std::env::current_dir().expect("cwd");
        let file = directory.join("Cargo.toml");
        let options = parse_args(vec![
            "--capture-dir".into(),
            "captures".into(),
            "--file".into(),
            file.to_string_lossy().into_owned().into(),
            "--command".into(),
            "printf hello".into(),
        ])
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
    fn advanced_compiler_uses_locked_python_project_through_mise_and_uv() {
        let config = compiler_config();
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

        let options = parse_args(vec![
            OsString::from("--file"),
            left.clone().into_os_string(),
            OsString::from("--file"),
            right.clone().into_os_string(),
        ])
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
