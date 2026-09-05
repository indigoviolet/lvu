use std::{
    collections::{BTreeMap, HashMap, HashSet},
    env,
    ffi::OsString,
    path::{Path, PathBuf},
    sync::Arc,
};

use lvu::{
    App, Focus, SourceItem, SourceKind, SourceLaunchRequest, ViewItem,
    terminal::{UnwiredQueryDispatcher, run_with_tick},
};
use lvu_core::{
    Acquisition, CommandDefinition, CommandProgram, RestartPolicy, SourceDefinition, SourceId,
};
use lvu_ingest::{RuntimeConfig, SourceHandle, SourceManager};
use lvu_live::{LiveConfig, LiveRowProvider};
use tokio::sync::mpsc;
use uuid::Uuid;

const SOURCE_NAMESPACE: Uuid = Uuid::from_bytes([
    0x8a, 0x57, 0xd8, 0xc1, 0x2e, 0x99, 0x44, 0x64, 0xb7, 0x03, 0x0e, 0xba, 0xd7, 0xf0, 0x03, 0x11,
]);
const MAX_TICK_UPDATES: usize = 64;
const MAX_PENDING_STARTS: usize = 8;
const MAX_SOURCES: usize = 16;

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
    request: Option<SourceLaunchRequest>,
}

struct StartFailure {
    source_id: SourceId,
    request: SourceLaunchRequest,
    message: String,
}

type StartResult = Result<StartedSource, StartFailure>;

struct Composition {
    manager: Arc<SourceManager>,
    runtime: tokio::runtime::Handle,
    starts_tx: mpsc::Sender<StartResult>,
    starts_rx: mpsc::Receiver<StartResult>,
    sources: HashMap<SourceId, String>,
    pending_starts: HashSet<SourceId>,
    cwd: PathBuf,
}

impl Composition {
    fn tick(&mut self, app: &mut App, provider: &mut LiveRowProvider) -> bool {
        let mut changed = provider.drain_ready_updates(MAX_TICK_UPDATES) > 0;
        for request in app.take_source_requests() {
            changed = true;
            let argument = match request.kind {
                SourceKind::File => SourceArgument::File(PathBuf::from(&request.text)),
                SourceKind::Command => SourceArgument::Command(request.text.clone()),
            };
            let definition = match definition(argument, &self.cwd) {
                Ok(definition) => definition,
                Err(message) => {
                    app.source_request_failed(request, message);
                    continue;
                }
            };
            let view_id = view_id(definition.id);
            if self.sources.contains_key(&definition.id) {
                app.source_request_succeeded(&request, &view_id);
                continue;
            }
            if self.pending_starts.contains(&definition.id) {
                app.source_request_failed(request, "source is already starting".into());
                continue;
            }
            if self.sources.len() + self.pending_starts.len() >= MAX_SOURCES
                || self.pending_starts.len() >= MAX_PENDING_STARTS
            {
                app.source_request_failed(request, "source admission limit reached".into());
                continue;
            }
            self.spawn_start(definition, request);
        }
        while let Ok(result) = self.starts_rx.try_recv() {
            changed = true;
            match result {
                Ok(started) => {
                    let source_id = started.definition.id;
                    self.pending_starts.remove(&source_id);
                    let request = started.request.clone().expect("dynamic request");
                    match register_started(provider, app, &mut self.sources, started) {
                        Ok(view_id) => app.source_request_succeeded(&request, &view_id),
                        Err(message) => {
                            if let Some(handle) = self.manager.source(source_id) {
                                self.runtime.spawn(async move {
                                    let _ = handle.stop().await;
                                });
                            }
                            app.source_request_failed(request, message);
                        }
                    }
                }
                Err(failure) => {
                    self.pending_starts.remove(&failure.source_id);
                    app.source_request_failed(failure.request, failure.message);
                }
            }
        }
        for (source_id, ui_id) in &self.sources {
            if let Some(status) = provider.source_status(*source_id) {
                let health = format!(
                    "{:?}/{:?} {} rows",
                    status.acquisition, status.index, status.indexed_records
                );
                app.update_source_health(ui_id, health);
            }
        }
        changed
    }

    fn spawn_start(&mut self, definition: SourceDefinition, request: SourceLaunchRequest) {
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
                    request: Some(request.clone()),
                }),
                Err(error) => Err(StartFailure {
                    source_id,
                    request,
                    message: format!("start {}: {error}", definition.name),
                }),
            };
            let _ = sender.send(result).await;
        });
    }
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
    let mut provider =
        LiveRowProvider::new(live_config).map_err(|error| format!("live row provider: {error}"))?;
    let manager = Arc::new(
        SourceManager::new(&options.capture_dir, RuntimeConfig::default())
            .map_err(|error| format!("capture manager: {error}"))?,
    );
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
        if let Err(error) = register_started(&provider, &mut app, &mut source_ids, started) {
            if let Some(handle) = manager.source(source_id) {
                let _ = handle.stop().await;
            }
            startup_error = Some(error);
            break;
        }
    }
    if let Some(error) = startup_error {
        let cleanup = cleanup(&provider, &manager).await;
        return Err(combine_errors(error, cleanup));
    }
    if !app.views.is_empty() {
        app.source_dialog = None;
        app.focus = Focus::Logs;
    }

    let (starts_tx, starts_rx) = mpsc::channel(MAX_PENDING_STARTS);
    let mut composition = Composition {
        manager: Arc::clone(&manager),
        runtime: tokio::runtime::Handle::current(),
        starts_tx,
        starts_rx,
        sources: source_ids,
        pending_starts: HashSet::new(),
        cwd,
    };
    let mut dispatcher = UnwiredQueryDispatcher::new();
    let terminal_result = run_with_tick(
        app,
        &mut provider,
        &mut dispatcher,
        |_| false,
        |app, provider| composition.tick(app, provider),
    );
    let cleanup_result = cleanup(&provider, &manager).await;
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
        request: None,
    })
}

fn view_id(source_id: SourceId) -> String {
    format!("raw-{}", source_id.0)
}

fn register_started(
    provider: &LiveRowProvider,
    app: &mut App,
    sources: &mut HashMap<SourceId, String>,
    started: StartedSource,
) -> Result<String, String> {
    let source_id = started.definition.id;
    provider
        .register_source(started.handle)
        .map_err(|error| format!("register {}: {error}", started.definition.name))?;
    provider
        .register_raw_view(&started.view_id, vec![source_id])
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
         With no sources, the terminal opens an Add source dialog. Native search and\n\
         advanced Polars queries are not connected yet and report an actionable error."
    );
}

#[cfg(test)]
mod tests {
    use super::{SourceArgument, definition, parse_args};
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
}
