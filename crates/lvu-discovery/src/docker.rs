use crate::{
    CancellationToken, Confidence, DiscoveryCandidate, DiscoveryLimits, Evidence, Provider,
    ProviderState, ProviderStatus,
};
use lvu_core::{Acquisition, CommandDefinition, CommandProgram, RestartPolicy};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    path::PathBuf,
    process::Stdio,
    time::{Duration, Instant},
};
use tokio::{io::AsyncReadExt, process::Command};

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
    let context = match config.context.clone() {
        Some(value) => value,
        None => match run(
            &config.runner.executable,
            &["context", "show"],
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
    let args = [
        "--context",
        context.as_str(),
        "ps",
        "--all",
        "--format",
        "{{json .}}",
    ];
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
    for line in output
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
    {
        let Ok(value) = serde_json::from_slice::<Value>(line) else {
            malformed += 1;
            continue;
        };
        match candidate(&value, &config, &context) {
            Some(mut value) => {
                if let Some(index) = candidates
                    .iter()
                    .position(|existing: &DiscoveryCandidate| existing.dedup_key == value.dedup_key)
                {
                    candidates[index].merge(&mut value);
                } else if candidates.len() < limits.maximum_candidates {
                    candidates.push(value);
                } else {
                    candidate_limited = true;
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
    (
        candidates,
        status(
            state,
            if malformed > 0 {
                format!("ignored {malformed} malformed container records")
            } else {
                "docker discovery complete".into()
            },
        ),
    )
}

fn candidate(value: &Value, config: &DockerConfig, context: &str) -> Option<DiscoveryCandidate> {
    let object = value.as_object()?;
    let id = field(object, &["ID", "Id"])?;
    let name = field(object, &["Names", "Name"]).unwrap_or_else(|| id.clone());
    let image = field(object, &["Image"]).unwrap_or_default();
    let state = field(object, &["State"]).unwrap_or_default();
    let status_text = field(object, &["Status"]).unwrap_or_default();
    let labels = labels(object.get("Labels"));
    let project = labels.get("com.docker.compose.project").cloned();
    let service = labels.get("com.docker.compose.service").cloned();
    let replica = labels.get("com.docker.compose.container-number").cloned();
    let logical_instance = match (project.as_deref(), service.as_deref()) {
        (Some(project), Some(service)) => format!(
            "compose:{context}:{project}:{service}:{}",
            replica.as_deref().unwrap_or(&name)
        ),
        _ => format!("container:{context}:{name}"),
    };
    let args = vec![
        "--context".into(),
        context.into(),
        "logs".into(),
        "--follow".into(),
        "--timestamps".into(),
        "--tail".into(),
        config.history_lines.to_string(),
        id.clone(),
    ];
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
        ("container_id".into(), id),
        ("image".into(), image),
        ("state".into(), state),
        ("status".into(), status_text),
    ]);
    let mut candidate = DiscoveryCandidate::new(
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
    if !candidate.evidence[0]
        .attributes
        .get("state")
        .is_some_and(|state| state == "running")
    {
        candidate.availability = crate::Availability::Unavailable;
    }
    Some(candidate)
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
    args: &[&str],
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
