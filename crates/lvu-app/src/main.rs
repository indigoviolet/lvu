use std::{
    collections::{BTreeMap, HashMap, HashSet},
    env,
    ffi::OsString,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use lvu::{
    App, DiscoveryItem, DiscoveryUiRequest, Focus, PathCompletionRequest, SourceItem, SourceKind,
    SourceLaunchRequest, ViewItem, terminal::run_with_tick,
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
use lvu_view::{NativeViewAdapter, ScanState, ViewConfig};
use tokio::sync::mpsc;
use uuid::Uuid;

const SOURCE_NAMESPACE: Uuid = Uuid::from_bytes([
    0x8a, 0x57, 0xd8, 0xc1, 0x2e, 0x99, 0x44, 0x64, 0xb7, 0x03, 0x0e, 0xba, 0xd7, 0xf0, 0x03, 0x11,
]);
const MAX_TICK_UPDATES: usize = 64;
const MAX_PENDING_STARTS: usize = 8;
const MAX_SOURCES: usize = 16;
const MAX_DISCOVERY_CANDIDATES: usize = 128;
const MAX_PATH_CANDIDATES: usize = 64;
const MAX_PATH_ENTRIES: usize = 1024;

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
}

struct StartFailure {
    source_id: SourceId,
    origin: StartOrigin,
    message: String,
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
    pending_starts: HashSet<SourceId>,
    cwd: PathBuf,
    scans_tx: mpsc::Sender<ScanResult>,
    scans_rx: mpsc::Receiver<ScanResult>,
    active_scan: Option<(u64, CancellationToken)>,
    pending_scan: Option<u64>,
    discovery_candidates: HashMap<String, DiscoveryCandidate>,
    completions_tx: mpsc::Sender<PathCompletionResult>,
    completions_rx: mpsc::Receiver<PathCompletionResult>,
    active_completion: Option<(u64, Arc<AtomicBool>)>,
    pending_completion: Option<PathCompletionRequest>,
    home: Option<PathBuf>,
}

impl Composition {
    fn tick(&mut self, app: &mut App, adapter: &mut NativeViewAdapter) -> bool {
        let mut changed = adapter.drain_updates(MAX_TICK_UPDATES) > 0;
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
                    self.admit_definition(
                        app,
                        candidate.source,
                        StartOrigin::Discovery { generation },
                    );
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
                let items = scan.result.candidates.iter().map(discovery_item).collect();
                if app.apply_discovery_result(
                    scan.generation,
                    items,
                    discovery_status(&scan.result),
                ) {
                    self.discovery_candidates = scan
                        .result
                        .candidates
                        .iter()
                        .map(|candidate| (candidate.fingerprint.clone(), candidate.clone()))
                        .collect();
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
                    self.pending_starts.remove(&source_id);
                    let origin = started.origin.clone().expect("dynamic start origin");
                    match register_started(adapter, app, &mut self.sources, started) {
                        Ok(view_id) => start_succeeded(app, &origin, &view_id),
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
        for (source_id, ui_id) in &self.sources {
            if let Some(status) = adapter.status(&view_id(*source_id)) {
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
                app.update_source_health(ui_id, health.clone());
                app.update_view_runtime_status(&view_id(*source_id), health);
            }
        }
        changed
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

fn start_succeeded(app: &mut App, origin: &StartOrigin, view_id: &str) {
    match origin {
        StartOrigin::Manual(request) => app.source_request_succeeded(request, view_id),
        StartOrigin::Discovery { generation } => {
            app.discovery_selection_succeeded(*generation, view_id);
        }
    }
}

fn start_failed(app: &mut App, origin: StartOrigin, message: String) {
    match origin {
        StartOrigin::Manual(request) => app.source_request_failed(request, message),
        StartOrigin::Discovery { generation } => {
            app.discovery_selection_failed(generation, message);
        }
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
    let mut source_ids = HashMap::new();
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
    let mut composition = Composition {
        manager: Arc::clone(&manager),
        runtime: tokio::runtime::Handle::current(),
        starts_tx,
        starts_rx,
        sources: source_ids,
        pending_starts: HashSet::new(),
        cwd,
        scans_tx,
        scans_rx,
        active_scan: None,
        pending_scan: None,
        discovery_candidates: HashMap::new(),
        completions_tx,
        completions_rx,
        active_completion: None,
        pending_completion: None,
        home: env::var_os("HOME").map(PathBuf::from),
    };
    let mut rows = adapter.rows();
    let terminal_result = run_with_tick(
        app,
        &mut rows,
        &mut adapter,
        |_| false,
        |app, _rows, adapter| composition.tick(app, adapter),
    );
    composition.cancel_discovery();
    adapter.shutdown();
    let cleanup_result = cleanup(raw.as_ref(), &manager).await;
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
    format!("raw-{}", source_id.0)
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
    adapter
        .register_view(&started.view_id, vec![source_id])
        .map_err(|error| format!("view {}: {error}", started.definition.name))?;
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
         paths; Alt-F/Alt-C selects file or command; Ctrl-D opens discovery.\n\
         With sources, / opens literal search and p opens advanced Polars."
    );
}

#[cfg(test)]
mod tests {
    use super::{
        AtomicBool, SourceArgument, common_prefix, compiler_config, complete_path, definition,
        discovery_item, discovery_status, expand_tilde_path, parse_args,
    };
    use lvu::PathCompletionRequest;
    use lvu_core::{Acquisition, CommandProgram};

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
