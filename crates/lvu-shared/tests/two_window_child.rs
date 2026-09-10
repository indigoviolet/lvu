//! Executable shared-capture proof: real worker child processes, real
//! election, real sockets. Window A spawns the worker by attaching, starts
//! a file capture, and saves a view; a rival worker process exits
//! `INCUMBENT`; window B attaches to the same worker, sees the source in
//! presence, tails the same journal through its own reader, and wins a
//! cross-window save race with CAS semantics; both windows drain (flush +
//! goodbye) and the worker exits clean after the last detach.
//!
//! Every wait is bounded; any failure names the phase so the preserved
//! worker log (path printed at start) can be correlated.

use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::{Duration, Instant},
};

use lvu_shared::{
    FileJournalTail, SourceSummary, StartOutcome, StoreEvent, StoreMethod, WorkerClient,
    election::WorkerPaths,
    spawn::{SpawnSpec, exit},
};

fn worker_bin() -> PathBuf {
    // Cargo sets `CARGO_BIN_EXE_<name>` for integration tests when the
    // harness binary is in the build graph; otherwise resolve it beside
    // this test executable (`<target>/debug/deps` -> `<target>/debug`).
    // The build fails loudly if neither resolves, never silently probing.
    if let Some(path) = option_env!("CARGO_BIN_EXE_lvu-shared-worker") {
        return PathBuf::from(path);
    }
    let exe = std::env::current_exe().expect("test executable path");
    let bin = exe
        .parent()
        .and_then(|deps| deps.parent())
        .map(|debug| debug.join("lvu-shared-worker"))
        .expect("target layout");
    assert!(
        bin.is_file(),
        "worker harness binary missing at {}",
        bin.display()
    );
    bin
}

/// A spawned fixture that is always reaped: drop kills and waits, so a
/// failing assertion never leaves a worker behind holding the election.
struct KillOnDrop(Option<std::process::Child>);

impl KillOnDrop {
    fn wait_for_exit(&mut self, what: &str, timeout: Duration) -> i32 {
        let deadline = Instant::now() + timeout;
        loop {
            match self.0.as_mut().expect("child taken").try_wait() {
                Ok(Some(status)) => {
                    return status.code().unwrap_or(-1);
                }
                Ok(None) => {}
                Err(error) => panic!("{what}: wait failed: {error}"),
            }
            if Instant::now() >= deadline {
                panic!("{what}: no exit within {timeout:?}");
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn spawn_worker(bin: &Path, capture_root: &Path, socket: &Path) -> KillOnDrop {
    let spec = SpawnSpec::new(bin, capture_root, socket);
    let mut argv = spec.argv();
    argv.remove(0);
    let child = std::process::Command::new(bin)
        .args(&argv)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("worker child must spawn");
    KillOnDrop(Some(child))
}

fn file_definition(id: u128, path: &Path) -> lvu_core::SourceDefinition {
    lvu_core::SourceDefinition {
        schema_version: 1,
        id: lvu_core::SourceId(uuid::Uuid::from_u128(id)),
        name: format!("log-{id}"),
        acquisition: lvu_core::Acquisition::File {
            path: path.to_path_buf(),
            follow: true,
        },
        identity_hints: Default::default(),
        retention: None,
    }
}

fn test_view(source: lvu_core::SourceId, view: u128, version: u64) -> lvu_memory::WorkingView {
    lvu_memory::WorkingView {
        id: lvu_core::ViewId(uuid::Uuid::from_u128(view)),
        source_id: source,
        name: "All events".into(),
        role: lvu_memory::ViewRole::Derived,
        applied_revision_id: None,
        applied_search: String::new(),
        search_draft: None,
        applied_advanced_filter: None,
        advanced_filter_draft: None,
        navigation: lvu_memory::NavigationState {
            selected: None,
            anchor: None,
            follow: true,
        },
        presentation: lvu_memory::PresentationState::default(),
        version,
    }
}

fn save_method(
    window: &str,
    sequence: u64,
    definition: &lvu_core::SourceDefinition,
    view: lvu_core::ViewId,
    expected_version: Option<u64>,
) -> StoreMethod {
    StoreMethod::Save {
        request_id: format!("{window}-save-{sequence}"),
        window_id: window.to_owned(),
        sequence,
        definition: definition.clone(),
        view_id: view,
        state: test_view(
            definition.id,
            view.0.as_u128(),
            expected_version.unwrap_or(0),
        ),
        expected_version,
    }
}

#[tokio::test]
async fn two_windows_share_one_capture_through_real_worker() {
    let scenario = tokio::time::timeout(Duration::from_secs(120), async {
        let root = tempfile::tempdir().expect("scratch root");
        let capture_root = root.path().join("captures");
        let paths = WorkerPaths::new(&capture_root);
        let log = root.path().join("app.log");
        std::fs::write(&log, "one\n").expect("seed log");
        let bin = worker_bin();

        // Window A cold-attaches: the client elects and spawns the worker.
        // Distinct viewer PIDs per logical window: the election refuses two
        // takes of one PID slot in a process, exactly as for real windows.
        let (mut first, presence) = WorkerClient::attach(&bin, &capture_root, "window-a", 4001)
            .await
            .expect("window A attaches");
        assert!(presence.is_empty(), "fresh worker has no sources");

        // A rival worker loses the election promptly with INCUMBENT.
        let mut rival = spawn_worker(&bin, &capture_root, &paths.socket_path());
        assert_eq!(
            rival.wait_for_exit("rival worker", Duration::from_secs(15)),
            exit::INCUMBENT,
            "second worker must yield, not serve"
        );

        // Window A starts a file capture through the worker.
        let definition = file_definition(11, &log);
        let (source_id, journal_path) = match first
            .request_start(&definition)
            .await
            .expect("window A starts capture")
        {
            StartOutcome::Started {
                source_id,
                journal_path,
                ..
            } => (source_id, journal_path),
            StartOutcome::StdinBound { .. } => panic!("file start must not bind stdin"),
        };
        assert_eq!(source_id, definition.id);

        // The capture follows appends; window B tails the same journal
        // through its own reader: one capture, two readers.
        std::fs::write(&log, "one\ntwo\n").expect("append log");
        let tail = FileJournalTail::new(source_id, &journal_path);
        let deadline = Instant::now() + Duration::from_secs(15);
        let page = loop {
            if let Ok(page) = tail.read_page(0, 128, 1024 * 1024)
                && page.records.len() >= 2
            {
                break page;
            }
            if Instant::now() >= deadline {
                panic!("shared rows never arrived at {}", journal_path.display());
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        };
        assert_eq!(page.records.len(), 2);

        // Window B attaches to the same worker and sees the source.
        let (mut second, presence) = WorkerClient::attach(&bin, &capture_root, "window-b", 4002)
            .await
            .expect("window B attaches");
        assert!(
            presence.iter().any(
                |SourceSummary {
                     id,
                     journal_path: journal,
                     ..
                 }| id == &source_id.0.to_string()
                    && Path::new(journal) == journal_path.as_path()
            ),
            "window B sees the shared source: {presence:?}"
        );

        // Cross-window save race with CAS: A creates version 0, B wins
        // version 1 against it, A's stale retry loses with the committed
        // version echoed.
        let view = lvu_core::ViewId(uuid::Uuid::from_u128(12));
        match first
            .store(save_method("window-a", 1, &definition, view, None))
            .await
            .expect("window A saves")
        {
            StoreEvent::Saved { version: 0, .. } => {}
            other => panic!("expected version 0, got {other:?}"),
        }
        match second
            .store(save_method("window-b", 1, &definition, view, Some(0)))
            .await
            .expect("window B saves")
        {
            StoreEvent::Saved { version: 1, .. } => {}
            other => panic!("expected version 1, got {other:?}"),
        }
        match first
            .store(save_method("window-a", 2, &definition, view, Some(0)))
            .await
            .expect("stale save answers")
        {
            StoreEvent::SaveFailed {
                current_version: Some(1),
                ..
            } => {}
            other => panic!("expected versioned conflict, got {other:?}"),
        }

        // Both windows drain (flush + goodbye); detach is explicit.
        first.shutdown().await.expect("window A drains");
        second.shutdown().await.expect("window B drains");

        // The worker notices the empty audience past grace and exits
        // clean: drained viewers, stopped captures, unlinked socket. `root`
        // rides along so the scratch tree outlives the verification below:
        // dropping it first would delete the socket and log under us.
        let log_path = paths.worker_log();
        (root, log_path, paths.socket_path())
    })
    .await;
    let (scratch, log_path, socket_path) = match scenario {
        Ok(paths) => paths,
        Err(_) => panic!("two-window scenario exceeded 120s"),
    };
    let _scratch = scratch;
    // Reap is by KillOnDrop in the outer scope is impossible here (moved);
    // instead poll the socket's owner indirectly: the worker unlinks the
    // socket on clean shutdown, so wait for that, then confirm the log.
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if !socket_path.exists() {
            break;
        }
        if Instant::now() >= deadline {
            panic!("worker never unlinked its socket after both drains");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let log_text = std::fs::read_to_string(&log_path).unwrap_or_default();
    assert!(
        log_text.contains("clean shutdown"),
        "worker log must record clean shutdown: {log_text:?}"
    );
}
