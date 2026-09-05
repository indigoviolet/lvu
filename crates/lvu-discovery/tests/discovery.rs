#![cfg(target_os = "linux")]

use lvu_core::{
    Acquisition, CaptureEvent,
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

// Linux can return ETXTBSY when one parallel test forks while another briefly
// has a shebang fixture open for writing: the child inherits that writable fd
// until exec closes it. Keep each executable fixture's create/use/drop lifecycle
// together instead of adding a production retry for a test-only race.
static DOCKER_FIXTURE_LIFECYCLE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

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
async fn real_tee_is_discovered_and_core_capture_preserves_expected_bytes() {
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
        if let CaptureEvent::Record(record) = event {
            bytes.extend(record.bytes);
            bytes.extend(record.delimiter);
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

#[tokio::test]
async fn docker_fixture_keeps_compose_replicas_and_uses_working_container_args() {
    let _fixture_lifecycle = DOCKER_FIXTURE_LIFECYCLE.lock().await;
    let tmp = TempDir::new().unwrap();
    let labels_one = "com.docker.compose.project=shop,com.docker.compose.service=api,com.docker.compose.container-number=1";
    let labels_two = "com.docker.compose.project=shop,com.docker.compose.service=api,com.docker.compose.container-number=2";
    let body = format!(
        "if [ \"$3\" = ps ] && [ \"$4\" = --all ]; then printf '%s\\n' 'not-json' '{{\"ID\":\"old-id\",\"Names\":\"shop-api-1\",\"Image\":\"img\",\"State\":\"running\",\"Status\":\"Up\",\"Labels\":\"{labels_one}\"}}' '{{\"ID\":\"new-id\",\"Names\":\"shop-api-2\",\"State\":\"running\",\"Labels\":\"{labels_two}\"}}'; elif [ \"$3\" = logs ] && [ \"$8\" = old-id ]; then printf 'follow-ok\\n'; else exit 23; fi"
    );
    let result = discover(docker_request(docker_script(&tmp, &body))).await;
    assert_eq!(result.candidates.len(), 2);
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
        observed |= matches!(event, CaptureEvent::Record(record) if record.bytes == b"follow-ok");
    }
    handle.wait().await.unwrap();
    assert!(
        observed,
        "fake Docker CLI rejected generated logs invocation"
    );
}

#[tokio::test]
async fn docker_fingerprint_survives_instance_change_and_separates_projects() {
    let _fixture_lifecycle = DOCKER_FIXTURE_LIFECYCLE.lock().await;
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
    let _fixture_lifecycle = DOCKER_FIXTURE_LIFECYCLE.lock().await;
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
