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
    assert!(matches!(
        host.compile(
            "pl.col('x')",
            ExpressionKind::Enrichment,
            &AtomicBool::new(false)
        ),
        Err(HostError::Timeout)
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
        Err(HostError::Timeout)
    ));
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
        Err(HostError::Timeout)
    ));
    assert!(
        started.elapsed() < Duration::from_millis(800),
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
    assert!(
        status.is_empty()
            || status
                .lines()
                .any(|line| line.starts_with("State:") && line.contains("Z")),
        "live descendant in compiler process group survived restart: {status}"
    );
}

#[test]
fn repeated_restarts_do_not_accumulate_threads() {
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
    // Other tests in this binary run concurrently, so allow their three host
    // workers; twelve restarts must not add another twelve persistent workers.
    assert!(
        after <= before + 3,
        "threads leaked across restarts: before={before} after={after}"
    );
}
