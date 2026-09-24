use std::{
    fs,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use lvu_query::{CompilerHost, CompilerHostConfig, ExpressionKind, HostError};
use tempfile::TempDir;

fn config(script: &std::path::Path, timeout: Duration, output_limit: usize) -> CompilerHostConfig {
    CompilerHostConfig {
        executable: "python".into(),
        args: vec![script.display().to_string()],
        request_limit: 64 * 1024,
        output_limit,
        stderr_limit: 4096,
        timeout,
    }
}

#[test]
fn hang_times_out_and_child_is_reaped_then_host_restarts() {
    let temp = TempDir::new().unwrap();
    let script = temp.path().join("helper.py");
    let pid_file = temp.path().join("pid");
    fs::write(
        &script,
        format!(
            r#"import os,sys,time
open({:?},'w').write(str(os.getpid()))
for line in sys.stdin:
 time.sleep(60)
"#,
            pid_file.display().to_string()
        ),
    )
    .unwrap();
    let mut host = CompilerHost::new(config(&script, Duration::from_millis(100), 1024));
    // A freshly spawned helper gets the cold-start budget (8x steady here),
    // so the detail must name the cold start and the retry.
    assert!(matches!(
        host.compile(
            "pl.col('x')",
            ExpressionKind::Enrichment,
            &AtomicBool::new(false)
        ),
        Err(HostError::Timeout(detail))
            if detail.contains("cold start") && detail.contains("retry the action")
    ));
    let pid = fs::read_to_string(&pid_file).unwrap();
    assert!(
        !std::path::Path::new(&format!("/proc/{pid}")).exists(),
        "timed-out child must be waited/reaped"
    );
    assert!(matches!(
        host.compile(
            "pl.col('x')",
            ExpressionKind::Enrichment,
            &AtomicBool::new(false)
        ),
        Err(HostError::Timeout(_))
    ));
}

#[test]
fn fresh_child_gets_cold_budget_but_warmed_child_uses_steady_timeout() {
    // A slow first answer within the cold-start budget succeeds on a fresh
    // helper; the same slowness on the warmed helper exhausts the steady
    // budget. This is the installed-helper shape: uv/Python/Polars startup is
    // paid once, never per request.
    let temp = TempDir::new().unwrap();
    let script = temp.path().join("slow_first.py");
    fs::write(
        &script,
        r#"import json,sys,time
for index,line in enumerate(sys.stdin):
 r=json.loads(line)
 if index==0: time.sleep(0.3)
 else: time.sleep(60)
 print(json.dumps({'schema_version':1,'request_id':r['request_id'],'ok':False,'error':{'code':'slow','message':'slow'}}),flush=True)
"#,
    )
    .unwrap();
    let mut host = CompilerHost::new(config(&script, Duration::from_millis(100), 1024));
    assert!(matches!(
        host.compile(
            "pl.col('x')",
            ExpressionKind::Enrichment,
            &AtomicBool::new(false)
        ),
        Err(HostError::Rejected { code, .. }) if code == "slow"
    ));
    let started = Instant::now();
    assert!(matches!(
        host.compile(
            "pl.col('x')",
            ExpressionKind::Enrichment,
            &AtomicBool::new(false)
        ),
        Err(HostError::Timeout(detail))
            if detail.contains("already running helper")
                && detail.contains("budget 0.1s")
                && detail.contains("retry the action")
    ));
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "warmed-helper timeout must use the steady budget, not the cold one: {:?}",
        started.elapsed()
    );
}

#[test]
fn cancellation_reaps_child() {
    let temp = TempDir::new().unwrap();
    let script = temp.path().join("helper.py");
    fs::write(
        &script,
        "import sys,time\nfor line in sys.stdin: time.sleep(60)\n",
    )
    .unwrap();
    let mut host = CompilerHost::new(config(&script, Duration::from_secs(5), 1024));
    let cancelled = Arc::new(AtomicBool::new(false));
    let setter = Arc::clone(&cancelled);
    thread::spawn(move || {
        thread::sleep(Duration::from_millis(50));
        setter.store(true, Ordering::Release);
    });
    assert!(matches!(
        host.compile("pl.col('x')", ExpressionKind::Enrichment, &cancelled),
        Err(HostError::Cancelled)
    ));
}

#[test]
fn noisy_stderr_does_not_deadlock_and_is_bounded() {
    let temp = TempDir::new().unwrap();
    let script = temp.path().join("helper.py");
    fs::write(&script, r#"import json,sys
for line in sys.stdin:
 r=json.loads(line); sys.stderr.write('z'*100000); sys.stderr.flush()
 print(json.dumps({'schema_version':1,'request_id':r['request_id'],'ok':False,'error':{'code':'fixture','message':'expected'}}),flush=True)
"#).unwrap();
    let mut host = CompilerHost::new(config(&script, Duration::from_secs(2), 1024));
    assert!(matches!(
        host.compile(
            "pl.col('x')",
            ExpressionKind::Enrichment,
            &AtomicBool::new(false)
        ),
        Err(HostError::Rejected { .. })
    ));
    thread::sleep(Duration::from_millis(20));
    assert!(host.stderr_snapshot().len() <= 4096);
}

#[test]
fn malformed_and_oversized_responses_poison_child_and_restart() {
    let temp = TempDir::new().unwrap();
    let script = temp.path().join("helper.py");
    let marker = temp.path().join("marker");
    fs::write(&script, format!(r#"import json,os,sys
marker={:?}
first=not os.path.exists(marker)
open(marker,'a').close()
for line in sys.stdin:
 r=json.loads(line)
 if first: print('{{bad',flush=True); first=False
 else: print(json.dumps({{'schema_version':1,'request_id':r['request_id'],'ok':False,'error':{{'code':'restarted','message':'yes'}}}}),flush=True)
"#, marker.display().to_string())).unwrap();
    let mut host = CompilerHost::new(config(&script, Duration::from_secs(2), 1024));
    assert!(matches!(
        host.compile(
            "pl.col('x')",
            ExpressionKind::Enrichment,
            &AtomicBool::new(false)
        ),
        Err(HostError::Malformed(_))
    ));
    assert!(
        matches!(host.compile("pl.col('x')", ExpressionKind::Enrichment, &AtomicBool::new(false)), Err(HostError::Rejected { code, .. }) if code == "restarted")
    );

    let oversized = temp.path().join("oversized.py");
    fs::write(
        &oversized,
        "import sys\nfor line in sys.stdin: print('x'*2048,flush=True)\n",
    )
    .unwrap();
    let mut host = CompilerHost::new(config(&oversized, Duration::from_secs(2), 128));
    assert!(matches!(
        host.compile(
            "pl.col('x')",
            ExpressionKind::Enrichment,
            &AtomicBool::new(false)
        ),
        Err(HostError::OutputTooLarge)
    ));
}

#[test]
fn stale_request_id_is_rejected() {
    let temp = TempDir::new().unwrap();
    let script = temp.path().join("stale.py");
    fs::write(&script, r#"import json,sys
for line in sys.stdin:
 print(json.dumps({'schema_version':1,'request_id':'old-generation','ok':False,'error':{'code':'old','message':'old'}}),flush=True)
"#).unwrap();
    let mut host = CompilerHost::new(config(&script, Duration::from_secs(2), 1024));
    assert!(matches!(
        host.compile(
            "pl.col('x')",
            ExpressionKind::Enrichment,
            &AtomicBool::new(false)
        ),
        Err(HostError::MismatchedResponse)
    ));
}

#[test]
fn timeout_covers_blocked_large_stdin_write() {
    let mut cfg = CompilerHostConfig::python_module("python", "unused");
    cfg.args = vec!["-c".into(), "import time; time.sleep(2)".into()];
    cfg.timeout = Duration::from_millis(100);
    cfg.request_limit = 1024 * 1024;
    let mut host = CompilerHost::new(cfg);
    let started = Instant::now();
    assert!(matches!(
        host.compile(
            &"x".repeat(512_000),
            ExpressionKind::Filter,
            &AtomicBool::new(false)
        ),
        Err(HostError::Timeout(_))
    ));
    // A blocked stdin write on a freshly spawned helper waits out the
    // cold-start budget (8x the 100 ms steady timeout here), never the steady
    // one alone, and stays bounded regardless.
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "blocked write exceeded wall timeout: {:?}",
        started.elapsed()
    );
}

#[test]
fn short_line_flood_and_descendant_pipe_holder_are_bounded_and_reaped() {
    let temp = TempDir::new().unwrap();
    let script = temp.path().join("flood.py");
    let descendant_pid = temp.path().join("descendant-pid");
    fs::write(
        &script,
        format!(
            r#"import json,os,subprocess,sys,time
p=subprocess.Popen([sys.executable,'-c','import time; time.sleep(60)'])
open({:?},'w').write(str(p.pid))
for line in sys.stdin:
 for _ in range(10000): print('{{}}',flush=True)
 time.sleep(60)
"#,
            descendant_pid.display().to_string()
        ),
    )
    .unwrap();
    let mut host = CompilerHost::new(config(&script, Duration::from_millis(100), 1024));
    assert!(matches!(
        host.compile(
            "pl.col('x')",
            ExpressionKind::Filter,
            &AtomicBool::new(false)
        ),
        Err(HostError::MismatchedResponse | HostError::Malformed(_))
    ));
    let pid = fs::read_to_string(descendant_pid).unwrap();
    let status = fs::read_to_string(format!("/proc/{pid}/status")).unwrap_or_default();
    // A reaped descendant leaves no /proc entry; one caught mid-teardown reports
    // Z (zombie) or X (dead). Treating only Z as terminated made this race on the
    // exiting state and report an already-dead process as alive.
    let terminated = status.is_empty()
        || status
            .lines()
            .any(|line| line.starts_with("State:") && (line.contains('Z') || line.contains('X')));
    assert!(
        terminated,
        "live descendant in compiler process group survived restart: {status}"
    );
}

/// Probe flag: when set, this test measures in an isolated child process
/// instead of asserting in the shared harness process.
const LEAK_PROBE_ENV: &str = "LVU_HOST_LEAK_PROBE";

#[test]
fn repeated_restarts_do_not_accumulate_threads() {
    // The harness runs this binary's tests on concurrent threads, so a
    // thread count taken here also sees the other tests' live compiler hosts
    // (each owns three workers). Re-exec this same test binary as an isolated
    // probe child that runs only this measurement: no threshold inflation,
    // no serialization of the suite, and the count proves exactly the twelve
    // restarts' own workers.
    if std::env::var_os(LEAK_PROBE_ENV).is_none() {
        let exe = std::env::current_exe().expect("test binary path");
        let output = std::process::Command::new(exe)
            .arg("--exact")
            .arg("repeated_restarts_do_not_accumulate_threads")
            .arg("--nocapture")
            .env(LEAK_PROBE_ENV, "1")
            .output()
            .expect("leak probe child");
        assert!(
            output.status.success(),
            "isolated leak probe failed: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        return;
    }
    let before = fs::read_dir("/proc/self/task").unwrap().count();
    let temp = TempDir::new().unwrap();
    let script = temp.path().join("bad.py");
    fs::write(
        &script,
        "import sys\nfor line in sys.stdin: print('{bad',flush=True)\n",
    )
    .unwrap();
    let mut host = CompilerHost::new(config(&script, Duration::from_secs(1), 1024));
    for _ in 0..12 {
        assert!(matches!(
            host.compile(
                "pl.col('x')",
                ExpressionKind::Filter,
                &AtomicBool::new(false)
            ),
            Err(HostError::Malformed(_))
        ));
    }
    drop(host);
    thread::sleep(Duration::from_millis(150));
    let after = fs::read_dir("/proc/self/task").unwrap().count();
    // Twelve restarts must not leave twelve persistent workers behind. The
    // allowance covers only harness/worker noise inside the isolated probe.
    assert!(
        after <= before + 3,
        "threads leaked across restarts: before={before} after={after}"
    );
}
