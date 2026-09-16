#![cfg(target_os = "linux")]

use lvu_core::{
    Acquisition,
    acquisition::capture_command,
    acquisition::{CaptureLimits, capture_file},
};
use lvu_discovery::*;
use std::{
    collections::BTreeMap,
    fs,
    os::unix::{
        fs::{PermissionsExt, symlink},
        process::CommandExt,
    },
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tempfile::TempDir;
use tokio::{io::AsyncReadExt, process::Command};

// Linux refuses to exec a file that any process holds open for writing, and the
// count is per inode: while one test writes a shebang fixture, a fork from any
// other test in this binary inherits that writable descriptor and holds the
// file busy until it execs. Only forks from *this* process can inherit it, so a
// lock shared by everything here that writes such a fixture or spawns a child
// closes the window entirely.
//
// It was previously held by the docker tests alone, which is half the
// participants: under fork pressure `docker ps failed: Text file busy (os error
// 26)` still reached the assertions as zero candidates, once in twenty runs.
// Every test that spawns holds it now.
static EXECUTABLE_FIXTURE_LIFECYCLE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct ProcessGroupGuard(tokio::process::Child);
impl Drop for ProcessGroupGuard {
    fn drop(&mut self) {
        if let Some(pid) = self.0.id() {
            // SAFETY: this fixture is started in its own process group.
            unsafe { libc::kill(-(pid as i32), libc::SIGKILL) };
        }
        let _ = self.0.start_kill();
    }
}

fn request() -> DiscoveryRequest {
    DiscoveryRequest {
        limits: DiscoveryLimits {
            maximum_duration: Duration::from_secs(2),
            ..Default::default()
        },
        cancel: CancellationToken::default(),
        docker: None,
        procfs: None,
        project: None,
    }
}

fn fake_process(root: &Path, pid: u32, cwd: &Path, args: &[&[u8]]) -> PathBuf {
    let dir = root.join(pid.to_string());
    fs::create_dir_all(dir.join("fdinfo")).unwrap();
    fs::create_dir_all(dir.join("fd")).unwrap();
    symlink(cwd, dir.join("cwd")).unwrap();
    let cmdline = args
        .iter()
        .flat_map(|arg| arg.iter().copied().chain([0]))
        .collect::<Vec<_>>();
    fs::write(dir.join("cmdline"), cmdline).unwrap();
    fs::write(
        dir.join("stat"),
        format!("{pid} (fixture name) S 42 0 0 0\n"),
    )
    .unwrap();
    dir
}

#[tokio::test]
async fn fake_proc_resolves_tee_redirects_tolerates_races_and_deduplicates() {
    let tmp = TempDir::new().unwrap();
    let proc_root = tmp.path().join("proc");
    let work = tmp.path().join("work");
    fs::create_dir_all(&proc_root).unwrap();
    fs::create_dir_all(&work).unwrap();
    let log = work.join("service.log");
    fs::write(&log, b"do not inspect payload").unwrap();
    let process = fake_process(
        &proc_root,
        123,
        &work,
        &[b"/usr/bin/tee", b"-a", b"service.log", b"-"],
    );
    symlink(&log, process.join("fd/1")).unwrap();
    fs::write(process.join("fdinfo/1"), "flags:\t0100001\n").unwrap();
    symlink(&log, process.join("fd/3")).unwrap();
    fs::write(process.join("fdinfo/3"), "flags:\t0100000\n").unwrap();
    symlink("pipe:[987]", process.join("fd/4")).unwrap();
    fs::write(process.join("fdinfo/4"), "flags:\t01\n").unwrap();
    symlink(&log, process.join("fd/5")).unwrap();
    fs::write(process.join("fdinfo/5"), vec![b'x'; 200]).unwrap();
    fake_process(&proc_root, 122, &work, &[b"bad\xff-command"]);
    fs::create_dir(proc_root.join("124")).unwrap(); // vanished/restricted-shaped race: files absent
    let mut req = request();
    req.procfs = Some(ProcConfig { root: proc_root });
    req.limits.maximum_output_bytes = 64;
    let result = discover(req).await;
    assert_eq!(result.candidates.len(), 1);
    let candidate = &result.candidates[0];
    assert_eq!(candidate.evidence.len(), 2);
    assert!(candidate.evidence.iter().any(|e| e.summary.contains("tee")));
    assert!(
        candidate
            .evidence
            .iter()
            .any(|e| e.summary.contains("stdout"))
    );
    assert!(
        matches!(&candidate.source.acquisition, Acquisition::File { path, follow: true } if path == &log)
    );
}

#[tokio::test]
async fn proc_admits_producer_evidence_but_rejects_database_lock_and_binary_fds() {
    let tmp = TempDir::new().unwrap();
    let proc_root = tmp.path().join("proc");
    let work = tmp.path().join("work");
    fs::create_dir_all(&proc_root).unwrap();
    fs::create_dir_all(&work).unwrap();
    let custom_stdout = work.join("custom-output");
    let sqlite = work.join("events.sqlite");
    let wal = work.join("events.sqlite-wal");
    let lock = work.join("service.lock");
    let text_fd = work.join("activity");
    let binary_fd = work.join("cache-data");
    let named_log = work.join("service.log.2026-09-05");
    let own_capture = work.join(".lvu-captures/source/capture.journal");
    fs::create_dir_all(own_capture.parent().unwrap()).unwrap();
    fs::write(&custom_stdout, b"custom bytes").unwrap();
    fs::write(&sqlite, b"SQLite format 3\0").unwrap();
    fs::write(&wal, b"binary\0wal").unwrap();
    fs::write(&lock, b"123").unwrap();
    fs::write(&text_fd, b"first line\nsecond line\n").unwrap();
    fs::write(&binary_fd, [0, 1, 2, 3]).unwrap();
    fs::write(&named_log, b"").unwrap();
    fs::write(&own_capture, b"LVUJ").unwrap();
    let process = fake_process(&proc_root, 200, &work, &[b"server"]);
    for (fd, path) in [
        (1, &custom_stdout),
        (2, &sqlite),
        (3, &wal),
        (4, &lock),
        (5, &text_fd),
        (6, &binary_fd),
        (7, &named_log),
        (8, &own_capture),
    ] {
        symlink(path, process.join(format!("fd/{fd}"))).unwrap();
        fs::write(process.join(format!("fdinfo/{fd}")), "flags:\t0100001\n").unwrap();
    }
    let tee = fake_process(
        &proc_root,
        201,
        &work,
        &[b"tee", b"tee-custom", b"tee.sqlite", b"service.lck"],
    );
    fs::write(work.join("tee-custom"), b"").unwrap();
    fs::write(work.join("tee.sqlite"), b"").unwrap();
    fs::write(work.join("service.lck"), b"").unwrap();
    // No descriptor is required for tee argument discovery.
    assert!(tee.join("fd").is_dir());

    let mut req = request();
    req.procfs = Some(ProcConfig { root: proc_root });
    let result = discover(req).await;
    let paths = result
        .candidates
        .iter()
        .filter_map(|candidate| match &candidate.source.acquisition {
            Acquisition::File { path, .. } => Some(path.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(paths.contains(&custom_stdout));
    assert!(paths.contains(&text_fd));
    assert!(paths.contains(&named_log));
    assert!(paths.contains(&work.join("tee-custom")));
    for rejected in [sqlite, wal, lock, binary_fd, own_capture] {
        assert!(
            !paths.contains(&rejected),
            "unexpected candidate {rejected:?}"
        );
    }
    assert!(result.candidates.iter().any(|candidate| {
        candidate.evidence.iter().any(|evidence| {
            evidence.attributes.get("log_admission").map(String::as_str)
                == Some("bounded_text_probe")
        })
    }));
}

/// A busy machine must still get the candidates the scan did find.
///
/// Descriptors are examined in pid order, so a handful of long-running system
/// processes with many open files can consume the whole budget before the
/// scan reaches anything a person would want to follow. Returning an empty
/// list and blaming a "file descriptor limit" then reads as though the machine
/// is out of descriptors, when it is this scan's own bound and the interesting
/// process was simply never looked at.
#[tokio::test]
async fn a_spent_scan_budget_still_reports_what_it_found_and_says_which_budget() {
    let tmp = TempDir::new().unwrap();
    let proc_root = tmp.path().join("proc");
    let work = tmp.path().join("work");
    fs::create_dir_all(&proc_root).unwrap();
    fs::create_dir_all(&work).unwrap();

    // A low-pid process holding far more descriptors than the budget, none of
    // them admissible: exactly the shape that starves the rest of the scan.
    let noise = work.join("noise-data");
    fs::write(&noise, [0u8, 1, 2, 3]).unwrap();
    let noisy = fake_process(&proc_root, 100, &work, &[b"systemd-noise"]);
    for fd in 3..80u32 {
        symlink(&noise, noisy.join(format!("fd/{fd}"))).unwrap();
        fs::write(noisy.join(format!("fdinfo/{fd}")), "flags:\t0100001\n").unwrap();
    }

    // The process a person actually cares about, behind all that noise.
    let wanted = work.join("service.log");
    fs::write(&wanted, b"first line\nsecond line\n").unwrap();
    let server = fake_process(&proc_root, 900, &work, &[b"server"]);
    symlink(&wanted, server.join("fd/1")).unwrap();
    fs::write(server.join("fdinfo/1"), "flags:\t0100001\n").unwrap();

    let mut req = request();
    req.limits.maximum_files = 24;
    req.limits.maximum_files_per_process = 8;
    req.procfs = Some(ProcConfig { root: proc_root });
    let result = discover(req).await;

    let paths = result
        .candidates
        .iter()
        .filter_map(|candidate| match &candidate.source.acquisition {
            Acquisition::File { path, .. } => Some(path.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(
        paths.contains(&wanted),
        "a partial scan must still reach past one noisy process: {paths:?}"
    );
    let procfs = result
        .statuses
        .iter()
        .find(|status| status.provider == Provider::Procfs)
        .expect("procfs provider status");
    assert_eq!(procfs.state, ProviderState::Limited);
    let detail = &procfs.message;
    assert!(
        detail.contains("scan budget"),
        "the reason must name the scan's own bound, not a system limit: {detail}"
    );
    assert!(
        detail.contains("processes"),
        "the reason must say how much of the machine was examined: {detail}"
    );
    assert!(
        !detail.contains("file descriptor limit reached"),
        "that wording reads as the system descriptor limit: {detail}"
    );
}

#[tokio::test]
async fn real_tee_is_discovered_and_core_capture_preserves_expected_bytes() {
    let _fixture_lifecycle = EXECUTABLE_FIXTURE_LIFECYCLE.lock().await;
    let tmp = TempDir::new().unwrap();
    let log = tmp.path().join("owned.log");
    let mut command = Command::new("sh");
    command.arg("-c").arg("for x in alpha beta gamma; do printf '%s\\n' \"$x\"; sleep .15; done | tee owned.log >/dev/null; sleep 5")
        .current_dir(tmp.path()).stdout(Stdio::null()).stderr(Stdio::null());
    command.as_std_mut().process_group(0);
    let mut child = ProcessGroupGuard(command.spawn().unwrap());
    let candidate = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let mut req = request();
            req.procfs = Some(ProcConfig::default());
            req.limits.maximum_processes = 100_000;
            if let Some(candidate) = discover(req).await.candidates.into_iter().find(
                |candidate| matches!(&candidate.source.acquisition, Acquisition::File { path, .. } if path == &log),
            ) {
                break candidate;
            }
            tokio::task::yield_now().await;
        }
    }).await.expect("tee readiness timed out");
    tokio::time::timeout(Duration::from_secs(2), async {
        while fs::read(&log).unwrap_or_default() != b"alpha\nbeta\ngamma\n" {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("producer completion timed out");
    let Acquisition::File { path, .. } = &candidate.source.acquisition else {
        unreachable!()
    };
    let (handle, mut events) = capture_file(path.clone(), false, CaptureLimits::default()).unwrap();
    let mut bytes = Vec::new();
    while let Some(event) = events.recv().await {
        for record in event.records() {
            bytes.extend_from_slice(&record.bytes);
            bytes.extend_from_slice(&record.delimiter);
        }
    }
    handle.wait().await.unwrap();
    assert_eq!(bytes, b"alpha\nbeta\ngamma\n");
    if let Some(pid) = child.0.id() {
        unsafe { libc::kill(-(pid as i32), libc::SIGKILL) };
    }
    let _ = child.0.wait().await;
}

#[tokio::test]
async fn discovery_never_consumes_an_owned_process_pipe() {
    let _fixture_lifecycle = EXECUTABLE_FIXTURE_LIFECYCLE.lock().await;
    let mut child = Command::new("sh")
        .arg("-c")
        .arg("sleep .2; printf controlled-pipe-bytes")
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdout = child.stdout.take().unwrap();
    let mut req = request();
    req.procfs = Some(ProcConfig::default());
    req.limits.maximum_processes = 100_000;
    let _ = discover(req).await;
    let mut received = Vec::new();
    stdout.read_to_end(&mut received).await.unwrap();
    assert_eq!(received, b"controlled-pipe-bytes");
    assert!(child.wait().await.unwrap().success());
}

fn docker_script(tmp: &TempDir, body: &str) -> PathBuf {
    let path = tmp.path().join("docker-fixture");
    fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    let mut permissions = fs::metadata(&path).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&path, permissions).unwrap();
    path
}
fn docker_request(executable: PathBuf) -> DiscoveryRequest {
    let mut req = request();
    req.docker = Some(DockerConfig {
        runner: DockerRunner { executable },
        context: Some("ctx".into()),
        history_lines: 77,
    });
    req
}

struct EnvironmentGuard {
    name: &'static str,
    previous: Option<std::ffi::OsString>,
}
impl EnvironmentGuard {
    fn set(name: &'static str, value: &str) -> Self {
        let previous = std::env::var_os(name);
        // SAFETY: every Docker fixture that spawns a process holds
        // EXECUTABLE_FIXTURE_LIFECYCLE, so no sibling Docker child can inherit
        // this test-only routing value.
        unsafe { std::env::set_var(name, value) };
        Self { name, previous }
    }

    fn unset(name: &'static str) -> Self {
        let previous = std::env::var_os(name);
        // SAFETY: see `set`; the shared fixture lock covers this mutation.
        unsafe { std::env::remove_var(name) };
        Self { name, previous }
    }
}
impl Drop for EnvironmentGuard {
    fn drop(&mut self) {
        // SAFETY: the fixture lock remains held until guards declared after it
        // have restored their variables.
        unsafe {
            match &self.previous {
                Some(value) => std::env::set_var(self.name, value),
                None => std::env::remove_var(self.name),
            }
        }
    }
}

#[tokio::test]
async fn docker_implicit_routing_preserves_environment_and_omits_context_argument() {
    let _fixture_lifecycle = EXECUTABLE_FIXTURE_LIFECYCLE.lock().await;
    let _docker_context = EnvironmentGuard::unset("DOCKER_CONTEXT");
    let secret_host = "tcp://user:secret@docker.example:2376";
    let _docker_host = EnvironmentGuard::set("DOCKER_HOST", secret_host);
    let tmp = TempDir::new().unwrap();
    let body = r#"
if [ "$1" = context ] && [ "$2" = show ]; then printf 'default\n'; exit 0; fi
if [ "$1" = --context ]; then exit 41; fi
if [ "$1" = ps ] && [ "$DOCKER_HOST" = 'tcp://user:secret@docker.example:2376' ]; then
  printf '%s\n' '{"ID":"routed-id","Names":"routed","State":"running","Status":"Up"}'
  exit 0
fi
exit 42
"#;
    let mut req = request();
    req.docker = Some(DockerConfig {
        runner: DockerRunner {
            executable: docker_script(&tmp, body),
        },
        context: None,
        history_lines: 77,
    });
    let result = discover(req).await;
    assert_eq!(result.candidates.len(), 1, "{:?}", result.statuses);
    let candidate = &result.candidates[0];
    let Acquisition::Command { command } = &candidate.source.acquisition else {
        panic!()
    };
    let lvu_core::CommandProgram::Exec { args, .. } = &command.program else {
        panic!()
    };
    assert_eq!(args.first().map(String::as_str), Some("logs"));
    assert!(!args.iter().any(|arg| arg == "--context"));
    assert!(!candidate.dedup_key.contains(secret_host));
    assert!(!candidate.fingerprint.contains(secret_host));
}

#[tokio::test]
async fn compose_service_merges_replicas_and_executes_exact_multi_file_command() {
    let _fixture_lifecycle = EXECUTABLE_FIXTURE_LIFECYCLE.lock().await;
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("shop");
    fs::create_dir(&project).unwrap();
    let base = project.join("compose.yaml");
    let override_file = project.join("compose.override.yaml");
    fs::write(&base, "services: {api: {image: example}}\n").unwrap();
    fs::write(&override_file, "services: {api: {}}\n").unwrap();
    let config_files = format!("{},{}", base.display(), override_file.display());
    let row = |id: &str, name: &str, replica: &str, state: &str, oneoff: &str| {
        serde_json::json!({
            "ID": id,
            "Names": name,
            "Image": "example",
            "State": state,
            "Status": if state == "running" { "Up" } else { "Exited" },
            "ComposeProject": "shop",
            "ComposeService": "api",
            "ComposeReplica": replica,
            "ComposeOneoff": oneoff,
            "ComposeWorkingDir": project.to_string_lossy(),
            "ComposeConfigFiles": config_files,
        })
        .to_string()
    };
    let standalone = serde_json::json!({
        "ID": "standalone-id",
        "Names": "standalone",
        "State": "exited",
        "Status": "Exited (0)",
    })
    .to_string();
    let invocation = tmp.path().join("compose-argv");
    let body = format!(
        "if [ \"$3\" = ps ]; then printf '%s\\n' '{}' '{}' '{}' '{}'; elif [ \"$3\" = compose ]; then printf '%s\\n' \"$@\" > '{}'; printf 'service-follow-ok\\n'; else exit 23; fi",
        row("old-id", "shop-api-1", "1", "exited", "False"),
        row("new-id", "shop-api-2", "2", "running", "False"),
        row("run-id", "shop-api-run", "9", "exited", "True"),
        standalone,
        invocation.display(),
    );
    let result = discover(docker_request(docker_script(&tmp, &body))).await;
    assert_eq!(result.candidates.len(), 5, "{:?}", result.statuses);
    let service = result
        .candidates
        .iter()
        .find(|candidate| candidate.display_label == "shop/api (Docker service)")
        .expect("Compose service aggregate");
    assert_eq!(service.evidence.len(), 2);
    assert_eq!(service.availability, crate::Availability::Available);
    assert!(
        result.statuses[0]
            .message
            .contains("1 Compose service log sources and 4 container log sources")
    );
    assert_eq!(
        result
            .candidates
            .iter()
            .filter(|candidate| candidate.display_label.contains("Docker service"))
            .count(),
        1,
        "one-off containers must not create another service aggregate"
    );
    let stopped = result
        .candidates
        .iter()
        .find(|candidate| candidate.display_label == "standalone (Docker)")
        .unwrap();
    assert_eq!(stopped.availability, crate::Availability::Unavailable);

    let Acquisition::Command { command } = &service.source.acquisition else {
        panic!()
    };
    let lvu_core::CommandProgram::Exec { args, .. } = &command.program else {
        panic!()
    };
    assert_eq!(
        args,
        &[
            "--context",
            "ctx",
            "compose",
            "--project-name",
            "shop",
            "--project-directory",
            project.to_str().unwrap(),
            "--file",
            base.to_str().unwrap(),
            "--file",
            override_file.to_str().unwrap(),
            "logs",
            "--follow",
            "--timestamps",
            "--tail",
            "77",
            "api",
        ]
    );
    assert_eq!(command.cwd.as_deref(), Some(project.as_path()));
    let (handle, mut events) = capture_command(command.clone(), CaptureLimits::default()).unwrap();
    let mut observed = false;
    while let Some(event) = events.recv().await {
        observed |= event
            .records()
            .iter()
            .any(|record| record.bytes == b"service-follow-ok");
    }
    handle.wait().await.unwrap();
    assert!(observed, "service command did not execute fixture");
    let invoked = fs::read_to_string(invocation)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    assert_eq!(invoked.as_slice(), args.as_slice());
}

#[tokio::test]
async fn compose_service_requires_locally_addressable_configuration() {
    let _fixture_lifecycle = EXECUTABLE_FIXTURE_LIFECYCLE.lock().await;
    let tmp = TempDir::new().unwrap();
    let row = serde_json::json!({
        "ID": "remote-id",
        "Names": "remote-api-1",
        "State": "running",
        "Status": "Up",
        "ComposeProject": "remote",
        "ComposeService": "api",
        "ComposeReplica": "1",
        "ComposeWorkingDir": "/remote/host/project",
        "ComposeConfigFiles": "/remote/host/project/compose.yaml",
    });
    let body = format!("printf '%s\\n' '{}'", row);
    let result = discover(docker_request(docker_script(&tmp, &body))).await;
    assert_eq!(result.candidates.len(), 1, "{:?}", result.statuses);
    assert_eq!(
        result.candidates[0]
            .identity_hints
            .get("docker_log_scope")
            .map(String::as_str),
        Some("container")
    );
}

#[tokio::test]
async fn docker_candidate_limit_prioritizes_each_container_over_aggregates() {
    let _fixture_lifecycle = EXECUTABLE_FIXTURE_LIFECYCLE.lock().await;
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("project");
    fs::create_dir(&project).unwrap();
    let compose = project.join("compose.yaml");
    fs::write(&compose, "services: {}\n").unwrap();
    let row = |id: &str, service: &str| {
        serde_json::json!({
            "ID": id,
            "Names": id,
            "State": "running",
            "ComposeProject": "bounded",
            "ComposeService": service,
            "ComposeReplica": "1",
            "ComposeWorkingDir": project.to_string_lossy(),
            "ComposeConfigFiles": compose.to_string_lossy(),
        })
        .to_string()
    };
    let body = format!(
        "printf '%s\\n' '{}' '{}'",
        row("one", "api"),
        row("two", "worker")
    );
    let mut req = docker_request(docker_script(&tmp, &body));
    req.limits.maximum_candidates = 2;
    let result = discover(req).await;
    assert_eq!(result.statuses[0].state, ProviderState::Limited);
    assert_eq!(result.candidates.len(), 2);
    assert!(result.candidates.iter().all(|candidate| {
        candidate
            .identity_hints
            .get("docker_log_scope")
            .is_some_and(|scope| scope == "container")
    }));
}

#[tokio::test]
async fn docker_fixture_keeps_compose_replicas_and_uses_working_container_args() {
    let _fixture_lifecycle = EXECUTABLE_FIXTURE_LIFECYCLE.lock().await;
    let tmp = TempDir::new().unwrap();
    let labels_one = "com.docker.compose.project=shop,com.docker.compose.service=api,com.docker.compose.container-number=1";
    let labels_two = "com.docker.compose.project=shop,com.docker.compose.service=api,com.docker.compose.container-number=2";
    let body = format!(
        "if [ \"$3\" = ps ] && [ \"$4\" = --all ]; then printf '%s\\n' 'not-json' '{{\"ID\":\"old-id\",\"Names\":\"shop-api-1\",\"Image\":\"img\",\"State\":\"running\",\"Status\":\"Up\",\"Labels\":\"{labels_one}\"}}' '{{\"ID\":\"new-id\",\"Names\":\"shop-api-2\",\"State\":\"running\",\"Labels\":\"{labels_two}\"}}'; elif [ \"$3\" = logs ] && [ \"$8\" = old-id ]; then printf 'follow-ok\\n'; else exit 23; fi"
    );
    let result = discover(docker_request(docker_script(&tmp, &body))).await;
    assert_eq!(
        result.candidates.len(),
        2,
        "docker provider reported {:?}",
        result.statuses
    );
    assert!(
        result
            .candidates
            .iter()
            .all(|candidate| candidate.evidence.len() == 1)
    );
    assert_eq!(result.statuses[0].state, ProviderState::Limited);
    let candidate = result
        .candidates
        .iter()
        .find(|candidate| {
            candidate
                .identity_hints
                .get("compose_replica")
                .is_some_and(|value| value == "1")
        })
        .unwrap();
    let Acquisition::Command { command } = &candidate.source.acquisition else {
        panic!()
    };
    let lvu_core::CommandProgram::Exec {
        executable: _,
        args,
    } = &command.program
    else {
        panic!()
    };
    assert_eq!(
        args,
        &[
            "--context",
            "ctx",
            "logs",
            "--follow",
            "--timestamps",
            "--tail",
            "77",
            "old-id"
        ]
    );
    assert!(command.cwd.is_none());
    let (handle, mut events) = capture_command(command.clone(), CaptureLimits::default()).unwrap();
    let mut observed = false;
    while let Some(event) = events.recv().await {
        observed |= event
            .records()
            .iter()
            .any(|record| record.bytes == b"follow-ok");
    }
    handle.wait().await.unwrap();
    assert!(
        observed,
        "fake Docker CLI rejected generated logs invocation"
    );
}

#[tokio::test]
async fn docker_fingerprint_survives_instance_change_and_separates_projects() {
    let _fixture_lifecycle = EXECUTABLE_FIXTURE_LIFECYCLE.lock().await;
    let tmp1 = TempDir::new().unwrap();
    let tmp2 = TempDir::new().unwrap();
    let fixture = |id: &str, project: &str| {
        format!(
            "printf '%s\\n' '{{\"ID\":\"{id}\",\"Names\":\"x\",\"Labels\":\"com.docker.compose.project={project},com.docker.compose.service=api\"}}'"
        )
    };
    let a = discover(docker_request(docker_script(
        &tmp1,
        &fixture("one", "shop"),
    )))
    .await;
    assert!(!a.candidates.is_empty(), "{:?}", a.statuses);
    let b = discover(docker_request(docker_script(
        &tmp2,
        &fixture("two", "shop"),
    )))
    .await;
    assert!(!b.candidates.is_empty(), "{:?}", b.statuses);
    assert_eq!(a.candidates[0].fingerprint, b.candidates[0].fingerprint);
    assert_eq!(a.candidates[0].source.id, b.candidates[0].source.id);
    let tmp3 = TempDir::new().unwrap();
    let c = discover(docker_request(docker_script(
        &tmp3,
        &fixture("one", "other"),
    )))
    .await;
    assert_ne!(a.candidates[0].fingerprint, c.candidates[0].fingerprint);
}

#[tokio::test]
async fn docker_output_timeout_and_cancellation_are_bounded_and_reap_child() {
    let _fixture_lifecycle = EXECUTABLE_FIXTURE_LIFECYCLE.lock().await;
    let huge = TempDir::new().unwrap();
    let mut req = docker_request(docker_script(&huge, "yes x | head -c 50000"));
    req.limits.maximum_output_bytes = 100;
    let overflow_started = std::time::Instant::now();
    assert_eq!(
        discover(req).await.statuses[0].state,
        ProviderState::Limited
    );
    assert!(overflow_started.elapsed() < Duration::from_secs(1));

    let slow = TempDir::new().unwrap();
    let pidfile = slow.path().join("pid");
    let script = docker_script(
        &slow,
        &format!("echo $$ > '{}'; sleep 30", pidfile.display()),
    );
    let mut req = docker_request(script);
    req.limits.maximum_duration = Duration::from_millis(100);
    let timed = discover(req).await;
    assert_eq!(
        timed.statuses[0].state,
        ProviderState::TimedOut,
        "{:?}",
        timed.statuses
    );
    let pid: u32 = fs::read_to_string(&pidfile)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    for _ in 0..50 {
        if !Path::new(&format!("/proc/{pid}")).exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        !Path::new(&format!("/proc/{pid}")).exists(),
        "timed-out docker fixture still alive"
    );

    let cancelled = TempDir::new().unwrap();
    let token = CancellationToken::default();
    let mut req = docker_request(docker_script(&cancelled, "sleep 30"));
    req.cancel = token.clone();
    let task = tokio::spawn(discover(req));
    tokio::time::sleep(Duration::from_millis(40)).await;
    token.cancel();
    assert!(task.await.unwrap().cancelled);

    let inherited = TempDir::new().unwrap();
    let token = CancellationToken::default();
    let mut req = docker_request(docker_script(&inherited, "sleep 2 & exit 0"));
    req.cancel = token.clone();
    let task = tokio::spawn(discover(req));
    tokio::time::sleep(Duration::from_millis(40)).await;
    token.cancel();
    let result = tokio::time::timeout(Duration::from_millis(500), task)
        .await
        .expect("cancellation after direct child exit hung")
        .unwrap();
    assert!(result.cancelled);
}

#[tokio::test]
async fn project_scan_is_bounded_cautious_and_merges_recent_definition() {
    let tmp = TempDir::new().unwrap();
    fs::write(tmp.path().join("app.log"), b"secret payload not read").unwrap();
    fs::write(tmp.path().join("ignore.txt"), b"not a log").unwrap();
    symlink("/", tmp.path().join("outside")).unwrap();
    let source = lvu_core::SourceDefinition {
        schema_version: 1,
        id: lvu_core::SourceId::new(),
        name: "remembered".into(),
        acquisition: Acquisition::File {
            path: tmp.path().join("app.log"),
            follow: true,
        },
        identity_hints: BTreeMap::new(),
        retention: None,
    };
    let mut req = request();
    req.project = Some(ProjectConfig {
        roots: vec![tmp.path().to_owned()],
        recent_sources: vec![source],
        ..Default::default()
    });
    let result = discover(req).await;
    assert_eq!(result.candidates.len(), 1);
    assert_eq!(result.candidates[0].evidence.len(), 2);
    let mut limited = request();
    limited.project = Some(ProjectConfig {
        roots: vec![tmp.path().to_owned()],
        ..Default::default()
    });
    limited.limits.maximum_files = 1;
    assert_eq!(
        discover(limited).await.statuses[0].state,
        ProviderState::Limited
    );
}

#[tokio::test]
async fn project_scan_uses_strict_log_names_and_preserves_explicit_unusual_recent() {
    let tmp = TempDir::new().unwrap();
    for accepted in [
        "app.log",
        "app.log.1",
        "app.log.2026-09-05",
        "logfile",
        "stderr.err",
        "app.log.gz",
        "app.log.1.gz",
        "worker.out.gz",
        "logfile.gz",
    ] {
        fs::write(tmp.path().join(accepted), b"fixture").unwrap();
    }
    for rejected in [
        "log.db",
        "log.lock",
        "events.sqlite",
        "events.sqlite-wal",
        "dialog.txt",
        "backup.sqlite.gz",
        "log.lock.gz",
        "archive.tar.gz",
        "app.log.xz",
        "catalog.log.sqlite3",
    ] {
        fs::write(tmp.path().join(rejected), b"fixture").unwrap();
    }
    let own = tmp.path().join(".lvu-captures/source");
    fs::create_dir_all(&own).unwrap();
    fs::write(own.join("internal.log"), b"fixture").unwrap();
    let remembered_path = tmp.path().join("remembered.sqlite");
    fs::write(&remembered_path, b"SQLite format 3\0").unwrap();
    let remembered = lvu_core::SourceDefinition {
        schema_version: 1,
        id: lvu_core::SourceId::new(),
        name: "remembered database path".into(),
        acquisition: Acquisition::File {
            path: remembered_path.clone(),
            follow: true,
        },
        identity_hints: BTreeMap::new(),
        retention: None,
    };
    let mut req = request();
    req.project = Some(ProjectConfig {
        roots: vec![tmp.path().to_owned()],
        recent_sources: vec![remembered.clone()],
        ..Default::default()
    });
    let result = discover(req).await;
    let names = result
        .candidates
        .iter()
        .filter_map(|candidate| match &candidate.source.acquisition {
            Acquisition::File { path, .. } => path.file_name().map(|name| name.to_string_lossy()),
            _ => None,
        })
        .collect::<Vec<_>>();
    for accepted in [
        "app.log",
        "app.log.1",
        "app.log.2026-09-05",
        "logfile",
        "stderr.err",
        "app.log.gz",
        "app.log.1.gz",
        "worker.out.gz",
        "logfile.gz",
        "remembered.sqlite",
    ] {
        assert!(
            names.iter().any(|name| name == accepted),
            "missing {accepted}"
        );
    }
    for rejected in [
        "log.db",
        "log.lock",
        "events.sqlite-wal",
        "backup.sqlite.gz",
        "log.lock.gz",
        "archive.tar.gz",
        "app.log.xz",
        "internal.log",
    ] {
        assert!(
            !names.iter().any(|name| name == rejected),
            "included {rejected}"
        );
    }
    let recent = result
        .candidates
        .iter()
        .find(|candidate| candidate.source.id == remembered.id)
        .unwrap();
    assert_eq!(recent.confidence, Confidence::Low);
    assert!(recent.evidence.iter().any(|evidence| {
        evidence.attributes.get("admission").map(String::as_str)
            == Some("remembered_explicit_unusual_artifact")
    }));
}

#[tokio::test]
async fn distinct_invalid_utf8_paths_do_not_collide() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let tmp = TempDir::new().unwrap();
    for name in [b"same\x80.log".to_vec(), b"same\x81.log".to_vec()] {
        fs::write(tmp.path().join(OsString::from_vec(name)), b"").unwrap();
    }
    let mut req = request();
    req.project = Some(ProjectConfig {
        roots: vec![tmp.path().to_owned()],
        ..Default::default()
    });
    let result = discover(req).await;
    assert_eq!(result.candidates.len(), 2);
    assert_ne!(
        result.candidates[0].fingerprint,
        result.candidates[1].fingerprint
    );
}

#[tokio::test]
async fn tiny_caps_bound_many_tee_operands_and_project_files() {
    let tmp = TempDir::new().unwrap();
    let proc_root = tmp.path().join("proc");
    let work = tmp.path().join("work");
    fs::create_dir_all(&proc_root).unwrap();
    fs::create_dir_all(&work).unwrap();
    let names = (0..20)
        .map(|index| format!("{index}.log"))
        .collect::<Vec<_>>();
    for name in &names {
        fs::write(work.join(name), b"").unwrap();
    }
    let mut args = vec![b"tee".as_slice()];
    args.extend(names.iter().map(|name| name.as_bytes()));
    fake_process(&proc_root, 10, &work, &args);
    let mut req = request();
    req.procfs = Some(ProcConfig { root: proc_root });
    req.limits.maximum_candidates = 1;
    req.limits.maximum_files = 2;
    let result = discover(req).await;
    assert!(result.candidates.len() <= 1);
    assert_eq!(result.statuses[0].state, ProviderState::Limited);

    let mut req = request();
    req.project = Some(ProjectConfig {
        roots: vec![work],
        ..Default::default()
    });
    req.limits.maximum_candidates = 1;
    let result = discover(req).await;
    assert_eq!(result.candidates.len(), 1);
    assert_eq!(result.statuses[0].state, ProviderState::Limited);
}

#[tokio::test]
async fn persisted_source_wins_merge_and_full_evidence_keys_are_retained() {
    let tmp = TempDir::new().unwrap();
    let proc_root = tmp.path().join("proc");
    let work = tmp.path().join("work");
    fs::create_dir_all(&proc_root).unwrap();
    fs::create_dir_all(&work).unwrap();
    let log = work.join("same.log");
    fs::write(&log, b"").unwrap();
    fake_process(&proc_root, 10, &work, &[b"tee", b"same.log"]);
    fake_process(&proc_root, 11, &work, &[b"tee", b"same.log"]);
    let saved = lvu_core::SourceDefinition {
        schema_version: 1,
        id: lvu_core::SourceId::new(),
        name: "authoritative saved name".into(),
        acquisition: Acquisition::File {
            path: log,
            follow: false,
        },
        identity_hints: BTreeMap::from([("saved".into(), "yes".into())]),
        retention: Some(lvu_core::RetentionPolicy {
            maximum_bytes: Some(42),
            maximum_age_seconds: None,
        }),
    };
    let mut req = request();
    req.procfs = Some(ProcConfig { root: proc_root });
    req.project = Some(ProjectConfig {
        recent_sources: vec![saved.clone()],
        ..Default::default()
    });
    let result = discover(req).await;
    assert_eq!(result.candidates.len(), 1);
    assert_eq!(result.candidates[0].source, saved);
    assert_eq!(
        result.candidates[0]
            .evidence
            .iter()
            .filter(|e| e.summary == "tee output argument")
            .count(),
        2
    );
}

/// Reproduces the real host failure: `docker context show` succeeds but
/// `docker ps` exits 1 with a permission-denied socket error and zero rows.
/// The Docker category must report Unavailable with the actionable reason,
/// never an empty success or an untried/starved status.
#[tokio::test]
async fn docker_permission_denied_reports_unavailable_with_actionable_reason() {
    let _fixture_lifecycle = EXECUTABLE_FIXTURE_LIFECYCLE.lock().await;
    let tmp = TempDir::new().unwrap();
    let body = "if [ \"$1\" = \"context\" ]; then echo default; exit 0; fi\nprintf 'permission denied while trying to connect to the docker API at unix:///var/run/docker.sock\n' >&2; exit 1";
    let result = discover(docker_request(docker_script(&tmp, body))).await;
    assert!(result.candidates.is_empty(), "{:?}", result.candidates);
    assert_eq!(result.statuses.len(), 1);
    let status = &result.statuses[0];
    assert_eq!(status.provider, Provider::Docker);
    assert_eq!(status.state, ProviderState::Unavailable);
    assert!(
        status.message.contains("docker ps"),
        "must name the failed operation: {}",
        status.message
    );
    assert!(
        status.message.contains("permission denied"),
        "must preserve the actionable OS reason: {}",
        status.message
    );
}

/// A failing `docker context show` surfaces explicitly with its own operation
/// name rather than a generic Docker error.
#[tokio::test]
async fn docker_context_failure_names_context_show() {
    let _fixture_lifecycle = EXECUTABLE_FIXTURE_LIFECYCLE.lock().await;
    let tmp = TempDir::new().unwrap();
    let body = "printf 'permission denied while trying to connect to the docker API at unix:///var/run/docker.sock\n' >&2; exit 1";
    let mut req = request();
    req.docker = Some(DockerConfig {
        runner: DockerRunner {
            executable: docker_script(&tmp, body),
        },
        context: None,
        history_lines: 77,
    });
    let result = discover(req).await;
    assert!(result.candidates.is_empty());
    let status = &result.statuses[0];
    assert_eq!(status.state, ProviderState::Unavailable);
    assert!(
        status.message.contains("docker context show"),
        "must name the failed operation: {}",
        status.message
    );
    assert!(
        status.message.contains("permission denied"),
        "{}",
        status.message
    );
}

/// Current `docker ps --format '{{json .}}'` keys for a running container
/// produce a usable candidate with the expected evidence.
#[tokio::test]
async fn docker_running_container_keys_produce_candidate() {
    let _fixture_lifecycle = EXECUTABLE_FIXTURE_LIFECYCLE.lock().await;
    let tmp = TempDir::new().unwrap();
    let body = "printf '%s\\n' '{\"ID\":\"abc123\",\"Names\":\"web\",\"Image\":\"nginx:latest\",\"State\":\"running\",\"Status\":\"Up 5 minutes\",\"Labels\":\"\"}'";
    let result = discover(docker_request(docker_script(&tmp, body))).await;
    assert_eq!(result.candidates.len(), 1, "{:?}", result.statuses);
    let candidate = &result.candidates[0];
    assert_eq!(candidate.provider, Provider::Docker);
    assert!(candidate.display_label.contains("web"));
    assert!(candidate.display_label.contains("(Docker)"));
    assert_eq!(candidate.availability, crate::Availability::Available);
    let evidence = &candidate.evidence[0];
    assert_eq!(
        evidence.attributes.get("container_id").map(String::as_str),
        Some("abc123")
    );
    assert_eq!(
        evidence.attributes.get("image").map(String::as_str),
        Some("nginx:latest")
    );
    assert_eq!(
        evidence.attributes.get("state").map(String::as_str),
        Some("running")
    );
    assert_eq!(
        evidence.attributes.get("status").map(String::as_str),
        Some("Up 5 minutes")
    );
    assert_eq!(result.statuses[0].state, ProviderState::Complete);
}

/// Regression for sequential shared-deadline starvation: Project is slow
/// enough to consume the old leftover budget while a fake Docker runner
/// returns a running container immediately. Docker runs concurrently against
/// the same global deadline, so its candidate must appear even though
/// Project hits the time limit.
#[tokio::test]
async fn slow_project_does_not_starve_docker() {
    let _fixture_lifecycle = EXECUTABLE_FIXTURE_LIFECYCLE.lock().await;
    let tmp = TempDir::new().unwrap();
    let work = tmp.path().join("work");
    fs::create_dir_all(&work).unwrap();
    // Enough log files that Project cannot finish inside the deadline even on
    // a fast disk; each needs stat + canonicalize + candidate insert work.
    // The scan aborts via its own deadline checks (bounded), it never hangs.
    for index in 0..30_000 {
        fs::write(work.join(format!("service-{index:05}.log")), b"line\n").unwrap();
    }
    let docker_body = "printf '%s\\n' '{\"ID\":\"web-id\",\"Names\":\"web\",\"Image\":\"nginx\",\"State\":\"running\",\"Status\":\"Up\",\"Labels\":\"\"}'";
    let docker_path = tmp.path().join("docker-fixture");
    fs::write(&docker_path, format!("#!/bin/sh\n{docker_body}\n")).unwrap();
    let mut permissions = fs::metadata(&docker_path).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&docker_path, permissions).unwrap();
    let mut req = request();
    req.limits.maximum_duration = Duration::from_millis(40);
    req.limits.maximum_files = 200_000;
    req.limits.maximum_candidates = 50_000;
    req.project = Some(ProjectConfig {
        roots: vec![work],
        ..Default::default()
    });
    req.docker = Some(DockerConfig {
        runner: DockerRunner {
            executable: docker_path,
        },
        context: Some("ctx".into()),
        history_lines: 77,
    });
    let result = discover(req).await;
    let docker_status = result
        .statuses
        .iter()
        .find(|status| status.provider == Provider::Docker)
        .expect("docker provider status");
    assert_eq!(
        docker_status.state,
        ProviderState::Complete,
        "docker must get a real attempt, not starvation: {:?}",
        result.statuses
    );
    assert!(
        result.candidates.iter().any(|candidate| {
            candidate.provider == Provider::Docker
                && candidate
                    .evidence
                    .iter()
                    .flat_map(|evidence| evidence.attributes.get("container_id"))
                    .any(|id| id == "web-id")
        }),
        "docker candidate missing; statuses: {:?}",
        result.statuses
    );
    // Statuses stay in established provider order regardless of finish order.
    let order: Vec<Provider> = result
        .statuses
        .iter()
        .map(|status| status.provider.clone())
        .collect();
    assert_eq!(order, vec![Provider::Project, Provider::Docker]);
}

#[tokio::test]
async fn cancellation_and_zero_budget_do_not_start_providers() {
    let tmp = TempDir::new().unwrap();
    let marker = tmp.path().join("spawned");
    let executable = docker_script(&tmp, &format!("touch '{}'; exit 0", marker.display()));
    let mut req = docker_request(executable);
    req.limits.maximum_candidates = 0;
    let result = discover(req).await;
    assert!(result.candidates.is_empty());
    assert!(!marker.exists());
    assert_eq!(result.statuses[0].state, ProviderState::Limited);

    let root = tmp.path().join("many-proc");
    fs::create_dir(&root).unwrap();
    for pid in 1..500 {
        fs::create_dir(root.join(pid.to_string())).unwrap();
    }
    let token = CancellationToken::default();
    let mut req = request();
    req.procfs = Some(ProcConfig { root });
    req.cancel = token.clone();
    let task = tokio::spawn(discover(req));
    token.cancel();
    let result = task.await.unwrap();
    assert!(result.cancelled);
    assert!(
        result
            .statuses
            .iter()
            .any(|status| status.state == ProviderState::Cancelled)
    );
}
