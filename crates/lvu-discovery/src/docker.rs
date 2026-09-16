use crate::{
    CancellationToken, Confidence, DiscoveryCandidate, DiscoveryLimits, Evidence, Provider,
    ProviderState, ProviderStatus,
};
use lvu_core::{Acquisition, CommandDefinition, CommandProgram, RestartPolicy};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    env,
    path::PathBuf,
    process::Stdio,
    time::{Duration, Instant},
};
use tokio::{io::AsyncReadExt, process::Command};

// Request the Compose labels separately. `.Labels` is a comma-delimited
// presentation field, but `project.config_files` is itself comma-delimited;
// the flattened field therefore cannot preserve multiple config paths.
// Docker's documented `json` and `.Label` template functions keep every
// value independently escaped without invoking a shell.
const PS_FORMAT: &str = r#"{"ID":{{json .ID}},"Names":{{json .Names}},"Image":{{json .Image}},"State":{{json .State}},"Status":{{json .Status}},"ComposeProject":{{.Label "com.docker.compose.project" | json}},"ComposeService":{{.Label "com.docker.compose.service" | json}},"ComposeReplica":{{.Label "com.docker.compose.container-number" | json}},"ComposeOneoff":{{.Label "com.docker.compose.oneoff" | json}},"ComposeWorkingDir":{{.Label "com.docker.compose.project.working_dir" | json}},"ComposeConfigFiles":{{.Label "com.docker.compose.project.config_files" | json}}}"#;

#[derive(Clone, Debug)]
pub struct DockerRunner {
    pub executable: PathBuf,
}
impl Default for DockerRunner {
    fn default() -> Self {
        Self {
            executable: "docker".into(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct DockerConfig {
    pub runner: DockerRunner,
    pub context: Option<String>,
    pub history_lines: usize,
}
impl Default for DockerConfig {
    fn default() -> Self {
        Self {
            runner: DockerRunner::default(),
            context: None,
            history_lines: 200,
        }
    }
}

pub(crate) async fn discover(
    config: DockerConfig,
    limits: &DiscoveryLimits,
    cancel: &CancellationToken,
    deadline: Instant,
) -> (Vec<DiscoveryCandidate>, ProviderStatus) {
    if cancel.is_cancelled() {
        return (Vec::new(), status(ProviderState::Cancelled, "cancelled"));
    }
    let explicit_context = config.context.clone();
    let context = match explicit_context.clone() {
        Some(value) => value,
        None => match run(
            &config.runner.executable,
            &["context".into(), "show".into()],
            limits,
            cancel,
            deadline,
        )
        .await
        {
            Ok(bytes) => String::from_utf8_lossy(&bytes).trim().to_owned(),
            Err(RunError::NotFound) => {
                return (
                    Vec::new(),
                    status(ProviderState::Unavailable, "docker executable not found"),
                );
            }
            Err(error) => return (Vec::new(), run_status(error, "docker context show")),
        },
    };
    // An explicit `--context` overrides DOCKER_HOST. When the caller did not
    // choose a context, leave routing to the Docker CLI just as ordinary
    // `docker ps` and lazydocker do; `context show` above is descriptive and
    // must not silently change the daemon being queried.
    let args = docker_args(
        explicit_context.as_deref(),
        ["ps", "--all", "--format", PS_FORMAT],
    );
    let output = match run(&config.runner.executable, &args, limits, cancel, deadline).await {
        Ok(value) => value,
        Err(RunError::NotFound) => {
            return (
                Vec::new(),
                status(ProviderState::Unavailable, "docker executable not found"),
            );
        }
        Err(error) => return (Vec::new(), run_status(error, "docker ps")),
    };
    let mut candidates = Vec::new();
    let mut malformed = 0usize;
    let mut candidate_limited = false;
    let daemon_scope = daemon_scope(&context, explicit_context.as_deref());
    for line in output
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
    {
        let Ok(value) = serde_json::from_slice::<Value>(line) else {
            malformed += 1;
            continue;
        };
        match candidates_for_container(
            &value,
            &config,
            &context,
            explicit_context.as_deref(),
            &daemon_scope,
        ) {
            Some(found) => {
                for mut value in found {
                    if let Some(index) =
                        candidates.iter().position(|existing: &DiscoveryCandidate| {
                            existing.dedup_key == value.dedup_key
                        })
                    {
                        candidates[index].merge(&mut value);
                    } else if candidates.len() < limits.maximum_candidates {
                        candidates.push(value);
                    } else if is_container_candidate(&value)
                        && let Some(index) = candidates.iter().rposition(is_service_candidate)
                    {
                        // Container sources are the non-negotiable base result.
                        // If the shared bound fills with optional aggregates,
                        // replace the newest aggregate so a later container is
                        // never hidden merely because its service was seen first.
                        candidates.remove(index);
                        candidates.push(value);
                        candidate_limited = true;
                    } else {
                        candidate_limited = true;
                    }
                }
            }
            None => malformed += 1,
        }
    }
    let state =
        if malformed > 0 || candidate_limited || candidates.len() >= limits.maximum_candidates {
            ProviderState::Limited
        } else {
            ProviderState::Complete
        };
    let services = candidates
        .iter()
        .filter(|candidate| is_service_candidate(candidate))
        .count();
    let containers = candidates.len().saturating_sub(services);
    (
        candidates,
        status(
            state,
            if malformed > 0 {
                format!(
                    "found {services} Compose service log sources and {containers} container log sources; ignored {malformed} malformed container records"
                )
            } else {
                format!(
                    "found {services} Compose service log sources and {containers} container log sources"
                )
            },
        ),
    )
}

fn candidates_for_container(
    value: &Value,
    config: &DockerConfig,
    context: &str,
    explicit_context: Option<&str>,
    daemon_scope: &str,
) -> Option<Vec<DiscoveryCandidate>> {
    let object = value.as_object()?;
    let id = field(object, &["ID", "Id"])?;
    let name = field(object, &["Names", "Name"]).unwrap_or_else(|| id.clone());
    let image = field(object, &["Image"]).unwrap_or_default();
    let state = field(object, &["State"]).unwrap_or_default();
    let status_text = field(object, &["Status"]).unwrap_or_default();
    let labels = labels(object.get("Labels"));
    let project = label(
        object,
        &labels,
        "ComposeProject",
        "com.docker.compose.project",
    );
    let service = label(
        object,
        &labels,
        "ComposeService",
        "com.docker.compose.service",
    );
    let replica = label(
        object,
        &labels,
        "ComposeReplica",
        "com.docker.compose.container-number",
    );
    let one_off = label(
        object,
        &labels,
        "ComposeOneoff",
        "com.docker.compose.oneoff",
    )
    .as_deref()
    .is_some_and(|value| value.eq_ignore_ascii_case("true") || value == "1");
    let logical_instance = match (project.as_deref(), service.as_deref()) {
        (Some(project), Some(service)) => format!(
            "compose:{daemon_scope}:{project}:{service}:{}",
            replica.as_deref().unwrap_or(&name)
        ),
        _ => format!("container:{daemon_scope}:{name}"),
    };
    let args = docker_args(
        explicit_context,
        [
            "logs".into(),
            "--follow".into(),
            "--timestamps".into(),
            "--tail".into(),
            config.history_lines.to_string(),
            id.clone(),
        ],
    );
    let program = CommandProgram::Exec {
        executable: config.runner.executable.clone(),
        args,
    };
    let display = match (project.as_deref(), service.as_deref(), replica.as_deref()) {
        (Some(project), Some(service), Some(replica)) => {
            format!("{project}/{service} #{replica} (Docker)")
        }
        _ => format!("{name} (Docker)"),
    };
    let mut hints = BTreeMap::from([
        ("docker_context".into(), context.into()),
        ("docker_container_name".into(), name.clone()),
        ("docker_log_scope".into(), "container".into()),
    ]);
    if let Some(value) = &project {
        hints.insert("compose_project".into(), value.clone());
    }
    if let Some(value) = &service {
        hints.insert("compose_service".into(), value.clone());
    }
    if let Some(value) = &replica {
        hints.insert("compose_replica".into(), value.clone());
    }
    let attributes = BTreeMap::from([
        ("container_id".into(), id.clone()),
        ("image".into(), image),
        ("state".into(), state.clone()),
        ("status".into(), status_text.clone()),
    ]);
    let mut container = DiscoveryCandidate::new(
        display.clone(),
        Acquisition::Command {
            command: CommandDefinition {
                program,
                cwd: None,
                environment: BTreeMap::new(),
                restart: RestartPolicy::Never,
            },
        },
        Provider::Docker,
        Confidence::High,
        hints,
        format!("docker:{logical_instance}"),
        Evidence {
            provider: Provider::Docker,
            summary: "Docker container log source".into(),
            attributes,
        },
    );
    if !container.evidence[0]
        .attributes
        .get("state")
        .is_some_and(|state| state == "running")
    {
        container.availability = crate::Availability::Unavailable;
    }
    let mut found = vec![container];
    if !one_off
        && let (Some(project), Some(service)) = (project.as_deref(), service.as_deref())
        && let Some(service_candidate) = compose_service_candidate(
            object,
            &labels,
            config,
            context,
            explicit_context,
            daemon_scope,
            project,
            service,
            &id,
            &name,
            &state,
            &status_text,
        )
    {
        found.push(service_candidate);
    }
    Some(found)
}

#[allow(clippy::too_many_arguments)]
fn compose_service_candidate(
    object: &serde_json::Map<String, Value>,
    labels: &BTreeMap<String, String>,
    config: &DockerConfig,
    context: &str,
    explicit_context: Option<&str>,
    daemon_scope: &str,
    project: &str,
    service: &str,
    container_id: &str,
    container_name: &str,
    state: &str,
    status_text: &str,
) -> Option<DiscoveryCandidate> {
    let working_dir = label(
        object,
        labels,
        "ComposeWorkingDir",
        "com.docker.compose.project.working_dir",
    );
    let config_files = label(
        object,
        labels,
        "ComposeConfigFiles",
        "com.docker.compose.project.config_files",
    );

    let invocation = compose_invocation(working_dir.as_deref(), config_files.as_deref())?;

    let mut suffix = vec![
        "compose".to_owned(),
        "--project-name".to_owned(),
        project.to_owned(),
    ];
    if let Some(directory) = &invocation.project_directory {
        suffix.extend([
            "--project-directory".to_owned(),
            directory.to_string_lossy().into_owned(),
        ]);
    }
    for file in &invocation.config_files {
        suffix.extend(["--file".to_owned(), file.to_string_lossy().into_owned()]);
    }
    suffix.extend([
        "logs".to_owned(),
        "--follow".to_owned(),
        "--timestamps".to_owned(),
        "--tail".to_owned(),
        config.history_lines.to_string(),
        service.to_owned(),
    ]);
    let program = CommandProgram::Exec {
        executable: config.runner.executable.clone(),
        args: docker_args(explicit_context, suffix),
    };
    let cwd = invocation.project_directory.clone();
    let mut hints = BTreeMap::from([
        ("docker_context".into(), context.into()),
        ("docker_log_scope".into(), "compose_service".into()),
        ("compose_project".into(), project.into()),
        ("compose_service".into(), service.into()),
    ]);
    if let Some(directory) = &working_dir {
        hints.insert("compose_working_dir".into(), directory.clone());
    }
    if let Some(files) = &config_files {
        hints.insert("compose_config_files".into(), files.clone());
    }
    let attributes = BTreeMap::from([
        ("container_id".into(), container_id.into()),
        ("container_name".into(), container_name.into()),
        ("state".into(), state.into()),
        ("status".into(), status_text.into()),
        ("service".into(), service.into()),
    ]);
    let mut candidate = DiscoveryCandidate::new(
        format!("{project}/{service} (Docker service)"),
        Acquisition::Command {
            command: CommandDefinition {
                program,
                cwd,
                environment: BTreeMap::new(),
                restart: RestartPolicy::Never,
            },
        },
        Provider::Docker,
        Confidence::High,
        hints,
        format!("docker:compose-service:{daemon_scope}:{project}:{service}"),
        Evidence {
            provider: Provider::Docker,
            summary: "Docker Compose service log source".into(),
            attributes,
        },
    );
    if state != "running" {
        candidate.availability = crate::Availability::Unavailable;
    }
    Some(candidate)
}

#[derive(Debug)]
struct ComposeInvocation {
    project_directory: Option<PathBuf>,
    config_files: Vec<PathBuf>,
}

fn compose_invocation(
    working_dir: Option<&str>,
    config_files: Option<&str>,
) -> Option<ComposeInvocation> {
    let project_directory = working_dir
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .filter(|path| path.is_dir());
    let declared_files = config_files
        .into_iter()
        .flat_map(|files| files.split(','))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();

    if !declared_files.is_empty() {
        let mut resolved = Vec::with_capacity(declared_files.len());
        for file in declared_files {
            let path = PathBuf::from(file);
            let path = if path.is_absolute() {
                path
            } else {
                project_directory.as_ref()?.join(path)
            };
            if !path.is_file() {
                return None;
            }
            resolved.push(path);
        }
        return Some(ComposeInvocation {
            project_directory,
            config_files: resolved,
        });
    }

    let directory = project_directory?;
    let has_default_file = [
        "compose.yaml",
        "compose.yml",
        "docker-compose.yaml",
        "docker-compose.yml",
    ]
    .into_iter()
    .any(|name| directory.join(name).is_file());
    has_default_file.then_some(ComposeInvocation {
        project_directory: Some(directory),
        // Let Compose perform its documented default-file and override-file
        // discovery when no exact config-file list was recorded.
        config_files: Vec::new(),
    })
}

fn is_service_candidate(candidate: &DiscoveryCandidate) -> bool {
    candidate
        .identity_hints
        .get("docker_log_scope")
        .is_some_and(|scope| scope == "compose_service")
}

fn is_container_candidate(candidate: &DiscoveryCandidate) -> bool {
    candidate
        .identity_hints
        .get("docker_log_scope")
        .is_some_and(|scope| scope == "container")
}

fn label(
    object: &serde_json::Map<String, Value>,
    labels: &BTreeMap<String, String>,
    field_name: &str,
    label_name: &str,
) -> Option<String> {
    field(object, &[field_name]).or_else(|| labels.get(label_name).cloned())
}

fn docker_args<I, S>(explicit_context: Option<&str>, suffix: I) -> Vec<String>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let mut args = Vec::new();
    if let Some(context) = explicit_context {
        args.extend(["--context".to_owned(), context.to_owned()]);
    }
    args.extend(suffix.into_iter().map(Into::into));
    args
}

fn daemon_scope(context: &str, explicit_context: Option<&str>) -> String {
    if explicit_context.is_some() || env::var_os("DOCKER_CONTEXT").is_some() {
        return format!("context:{context}");
    }
    if let Some(host) = env::var_os("DOCKER_HOST") {
        // Keep endpoint credentials/paths out of discovery manifests while
        // still preventing two environment-routed daemons from sharing IDs.
        return format!(
            "host:{}",
            crate::model::stable_fingerprint(&host.to_string_lossy())
        );
    }
    format!("context:{context}")
}

fn field(object: &serde_json::Map<String, Value>, names: &[&str]) -> Option<String> {
    names
        .iter()
        .find_map(|name| object.get(*name))
        .and_then(|value| value.as_str())
        .map(str::to_owned)
}
fn labels(value: Option<&Value>) -> BTreeMap<String, String> {
    if let Some(Value::Object(values)) = value {
        return values
            .iter()
            .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_owned())))
            .collect();
    }
    value
        .and_then(Value::as_str)
        .map(|text| {
            text.split(',')
                .filter_map(|part| part.split_once('='))
                .map(|(k, v)| (k.to_owned(), v.to_owned()))
                .collect()
        })
        .unwrap_or_default()
}

#[derive(Debug)]
enum RunError {
    NotFound,
    Failed(String),
    TooLarge,
    TimedOut,
    Cancelled,
}

type ReaderOutput = std::io::Result<(Vec<u8>, bool)>;
type ReaderTask = tokio::task::JoinHandle<ReaderOutput>;

async fn run(
    executable: &PathBuf,
    args: &[String],
    limits: &DiscoveryLimits,
    cancel: &CancellationToken,
    deadline: Instant,
) -> Result<Vec<u8>, RunError> {
    let mut command = Command::new(executable);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    configure_process_group(&mut command);
    let mut child = command.spawn().map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            RunError::NotFound
        } else {
            RunError::Failed(error.to_string())
        }
    })?;
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let process_group = child.id();
    let max = limits.maximum_output_bytes;
    let mut stdout_task = Some(tokio::spawn(read_bounded(stdout, max)));
    let mut stderr_task = Some(tokio::spawn(read_bounded(stderr, max.min(64 * 1024))));
    let mut stdout_value = None;
    let mut stderr_value = None;
    let mut exit_status = None;
    loop {
        if cancel.is_cancelled() {
            cleanup_child(
                &mut child,
                process_group,
                &mut stdout_task,
                &mut stderr_task,
            )
            .await;
            return Err(RunError::Cancelled);
        }
        if Instant::now() >= deadline {
            cleanup_child(
                &mut child,
                process_group,
                &mut stdout_task,
                &mut stderr_task,
            )
            .await;
            return Err(RunError::TimedOut);
        }
        if exit_status.is_none() {
            match child.try_wait() {
                Ok(Some(value)) => exit_status = Some(value),
                Ok(None) => {}
                Err(error) => {
                    cleanup_child(
                        &mut child,
                        process_group,
                        &mut stdout_task,
                        &mut stderr_task,
                    )
                    .await;
                    return Err(RunError::Failed(error.to_string()));
                }
            }
        }
        if stdout_task.as_ref().is_some_and(|task| task.is_finished()) {
            match take_reader(&mut stdout_task).await {
                Ok((value, false)) => stdout_value = Some(value),
                Ok((_, true)) => {
                    cleanup_child(
                        &mut child,
                        process_group,
                        &mut stdout_task,
                        &mut stderr_task,
                    )
                    .await;
                    return Err(RunError::TooLarge);
                }
                Err(error) => {
                    cleanup_child(
                        &mut child,
                        process_group,
                        &mut stdout_task,
                        &mut stderr_task,
                    )
                    .await;
                    return Err(RunError::Failed(error));
                }
            }
        }
        if stderr_task.as_ref().is_some_and(|task| task.is_finished()) {
            match take_reader(&mut stderr_task).await {
                Ok((value, false)) => stderr_value = Some(value),
                Ok((_, true)) => {
                    cleanup_child(
                        &mut child,
                        process_group,
                        &mut stdout_task,
                        &mut stderr_task,
                    )
                    .await;
                    return Err(RunError::TooLarge);
                }
                Err(error) => {
                    cleanup_child(
                        &mut child,
                        process_group,
                        &mut stdout_task,
                        &mut stderr_task,
                    )
                    .await;
                    return Err(RunError::Failed(error));
                }
            }
        }
        if let (Some(status), Some(stdout), Some(stderr)) = (
            exit_status.as_ref(),
            stdout_value.as_ref(),
            stderr_value.as_ref(),
        ) {
            if !status.success() {
                let message = format!("exit {status}: {}", String::from_utf8_lossy(stderr));
                cleanup_child(
                    &mut child,
                    process_group,
                    &mut stdout_task,
                    &mut stderr_task,
                )
                .await;
                return Err(RunError::Failed(message));
            }
            kill_process_group(process_group);
            return Ok(stdout.clone());
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

async fn take_reader(task: &mut Option<ReaderTask>) -> Result<(Vec<u8>, bool), String> {
    task.take()
        .expect("finished reader exists")
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())
}

async fn cleanup_child(
    child: &mut tokio::process::Child,
    process_group: Option<u32>,
    stdout: &mut Option<ReaderTask>,
    stderr: &mut Option<ReaderTask>,
) {
    kill_process_group(process_group);
    let _ = child.kill().await;
    let _ = child.wait().await;
    for task in [stdout, stderr] {
        if let Some(task) = task.take() {
            task.abort();
            let _ = task.await;
        }
    }
}

#[cfg(unix)]
fn configure_process_group(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    command.as_std_mut().process_group(0);
}
#[cfg(not(unix))]
fn configure_process_group(_: &mut Command) {}

#[cfg(unix)]
fn kill_process_group(pid: Option<u32>) {
    if let Some(pid) = pid {
        // SAFETY: negative pid targets only the process group created above.
        unsafe { libc::kill(-(pid as i32), libc::SIGKILL) };
    }
}
#[cfg(not(unix))]
fn kill_process_group(_: Option<u32>) {}

async fn read_bounded<R: tokio::io::AsyncRead + Unpin>(
    mut input: R,
    maximum: usize,
) -> std::io::Result<(Vec<u8>, bool)> {
    let mut output = Vec::new();
    let mut buffer = [0u8; 8192];
    let mut overflow = false;
    loop {
        let count = input.read(&mut buffer).await?;
        if count == 0 {
            break;
        }
        let remaining = maximum.saturating_sub(output.len());
        output.extend_from_slice(&buffer[..count.min(remaining)]);
        overflow |= count > remaining;
        if overflow {
            return Ok((output, true));
        }
    }
    Ok((output, overflow))
}
fn run_status(error: RunError, operation: &str) -> ProviderStatus {
    match error {
        RunError::TimedOut => status(ProviderState::TimedOut, format!("{operation} timed out")),
        RunError::Cancelled => status(ProviderState::Cancelled, format!("{operation} cancelled")),
        RunError::TooLarge => status(
            ProviderState::Limited,
            format!("{operation} exceeded output limit"),
        ),
        RunError::NotFound => status(ProviderState::Unavailable, "docker executable not found"),
        RunError::Failed(message) => status(
            ProviderState::Unavailable,
            format!("{operation} failed: {message}"),
        ),
    }
}
fn status(state: ProviderState, message: impl Into<String>) -> ProviderStatus {
    ProviderStatus {
        provider: Provider::Docker,
        state,
        message: message.into(),
    }
}
