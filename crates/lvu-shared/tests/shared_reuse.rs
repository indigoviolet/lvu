//! Shared-capture reuse across windows through the real worker: concurrent
//! fresh-ID starts of one acquisition share the first capture, an explicit
//! start after stop resumes the original capture instead of re-presenting a
//! dead handle, and direct/symlink spellings of one file share one capture.
//!
//! Every wait is bounded; any failure names the phase so the preserved
//! worker log (path printed at start) can be correlated. Record-count
//! assertions use lower bounds only: a partial line in flight is its own
//! record, never a function of the file's size.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use lvu_shared::{FileJournalTail, StartOutcome, WorkerClient};

fn worker_bin() -> PathBuf {
    // Same resolution as `two_window_child.rs`: prefer the Cargo-provided
    // harness binary path, else resolve beside this test executable. The
    // build fails loudly if neither resolves, never silently probing.
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

fn started_id(outcome: StartOutcome) -> (lvu_core::SourceId, PathBuf) {
    match outcome {
        StartOutcome::Started {
            source_id,
            journal_path,
            ..
        } => (source_id, journal_path),
        StartOutcome::StdinBound { .. } => panic!("file start must not bind stdin"),
    }
}

async fn wait_journal_growth(tail: &FileJournalTail, beyond: usize, journal: &Path, what: &str) {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Ok(page) = tail.read_page(0, 128, 1024 * 1024)
            && page.records.len() > beyond
        {
            return;
        }
        if Instant::now() >= deadline {
            panic!("{what} never arrived at {}", journal.display());
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

async fn wait_not_live(client: &mut WorkerClient, id: lvu_core::SourceId, what: &str) {
    // A refused progress poll also proves the point: no live handle answers
    // for the id anymore. Either signal is terminal for our purposes.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match client.poll_progress(id).await {
            Err(_) => return,
            Ok(progress) => {
                if progress.state.is_terminal() {
                    return;
                }
            }
        }
        if Instant::now() >= deadline {
            panic!("{what} never reached a terminal state");
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// Two windows racing fresh-ID starts of one acquisition share the first
/// capture: both replies carry the winner's id and journal, presence lists
/// exactly one source, and that journal receives the file's bytes.
#[tokio::test]
async fn concurrent_fresh_id_starts_share_the_first_capture() {
    let scenario = tokio::time::timeout(Duration::from_secs(120), async {
        let root = tempfile::tempdir().expect("scratch root");
        let capture_root = root.path().join("captures");
        let log = root.path().join("app.log");
        std::fs::write(&log, "one\n").expect("seed log");
        let bin = worker_bin();

        let (mut first, presence) = WorkerClient::attach(&bin, &capture_root, "window-a", 5001)
            .await
            .expect("window A attaches");
        assert!(presence.is_empty(), "fresh worker has no sources");
        let (mut second, _) = WorkerClient::attach(&bin, &capture_root, "window-b", 5002)
            .await
            .expect("window B attaches");

        // One barrier for three parties so both starters are already inside
        // `request_start` before either can settle: post-fix every
        // interleaving elects one leader and joins the rest.
        let barrier = Arc::new(tokio::sync::Barrier::new(3));
        let barrier_a = Arc::clone(&barrier);
        let definition_a = file_definition(101, &log);
        let task_a = tokio::spawn(async move {
            barrier_a.wait().await;
            first.request_start(&definition_a).await
        });
        let barrier_b = Arc::clone(&barrier);
        let definition_b = file_definition(102, &log);
        let task_b = tokio::spawn(async move {
            barrier_b.wait().await;
            second.request_start(&definition_b).await
        });
        barrier.wait().await;
        let (id_a, journal_a) = started_id(
            task_a
                .await
                .expect("starter A joins")
                .expect("starter A starts"),
        );
        let (id_b, journal_b) = started_id(
            task_b
                .await
                .expect("starter B joins")
                .expect("starter B starts"),
        );
        assert_eq!(
            id_a, id_b,
            "concurrent fresh-ID starts must share one capture identity"
        );
        assert_eq!(
            journal_a, journal_b,
            "concurrent fresh-ID starts must share one journal"
        );

        // Exactly one manager source exists: whichever id lost never even
        // gained a journal directory on disk, and a third window's presence
        // proves no second capture was started for it.
        let loser = if id_a == lvu_core::SourceId(uuid::Uuid::from_u128(101)) {
            102u128
        } else {
            101u128
        };
        assert!(
            !capture_root
                .join(uuid::Uuid::from_u128(loser).to_string())
                .exists(),
            "the loser id must not own a journal directory"
        );
        let (mut third, presence) = WorkerClient::attach(&bin, &capture_root, "window-c", 5003)
            .await
            .expect("window C attaches");
        assert_eq!(
            presence.len(),
            1,
            "exactly one capture exists: {presence:?}"
        );
        assert_eq!(presence[0].id, id_a.0.to_string());

        // The shared journal receives the file's bytes after both replies.
        std::fs::write(&log, "one\ntwo\nthree\n").expect("append log");
        let tail = FileJournalTail::new(id_a, &journal_a);
        wait_journal_growth(&tail, 0, &journal_a, "shared rows").await;

        first.shutdown().await.expect("window A drains");
        second.shutdown().await.expect("window B drains");
        third.shutdown().await.expect("window C drains");
    })
    .await;
    assert!(scenario.is_ok(), "concurrent-start scenario exceeded 120s");
}

/// An explicit start after stop resumes the original capture: the reply
/// carries the original id and journal, and newly appended bytes arrive
/// there (durable cursor, no recapture). A `Started` paired with terminal
/// progress would leave the appended bytes unread forever and fail below.
#[tokio::test]
async fn stopped_source_restarts_under_original_id_on_explicit_start() {
    let scenario = tokio::time::timeout(Duration::from_secs(120), async {
        let root = tempfile::tempdir().expect("scratch root");
        let capture_root = root.path().join("captures");
        let log = root.path().join("app.log");
        std::fs::write(&log, "one\n").expect("seed log");
        let bin = worker_bin();

        let (mut first, _) = WorkerClient::attach(&bin, &capture_root, "window-a", 5101)
            .await
            .expect("window A attaches");
        let definition = file_definition(111, &log);
        let (original, journal) = started_id(
            first
                .request_start(&definition)
                .await
                .expect("window A starts capture"),
        );
        assert_eq!(original, definition.id);

        let tail = FileJournalTail::new(original, &journal);
        wait_journal_growth(&tail, 0, &journal, "initial rows").await;
        let before = tail
            .read_page(0, 128, 1024 * 1024)
            .expect("journal readable")
            .records
            .len();

        first
            .request_stop(original)
            .await
            .expect("explicit stop works");
        wait_not_live(&mut first, original, "stopped capture");

        // Same acquisition, fresh id: must come back as the ORIGINAL id
        // with its journal, observably live.
        let mut revived = file_definition(112, &log);
        revived.name = "window-b-label".into();
        let (restarted, restarted_journal) = started_id(
            first
                .request_start(&revived)
                .await
                .expect("explicit start after stop works"),
        );
        assert_eq!(
            restarted, original,
            "restart preserves the original identity"
        );
        assert_eq!(
            restarted_journal, journal,
            "restart preserves the durable journal path"
        );
        std::fs::write(&log, "one\ntwo\nthree\nfour\n").expect("append after restart");
        wait_journal_growth(&tail, before, &journal, "post-restart rows").await;

        first.shutdown().await.expect("window A drains");
    })
    .await;
    assert!(scenario.is_ok(), "stop/start scenario exceeded 120s");
}

/// Direct and symlink spellings of one file share one capture even under
/// fresh ids: the worker canonicalizes absolute file identity instead of
/// comparing lexical spellings.
#[tokio::test]
async fn direct_and_symlink_spellings_share_one_capture() {
    let scenario = tokio::time::timeout(Duration::from_secs(120), async {
        let root = tempfile::tempdir().expect("scratch root");
        let capture_root = root.path().join("captures");
        let log = root.path().join("app.log");
        std::fs::write(&log, "one\n").expect("seed log");
        let link = root.path().join("alias.log");
        std::os::unix::fs::symlink(&log, &link).expect("symlink fixture");
        let bin = worker_bin();

        let (mut first, _) = WorkerClient::attach(&bin, &capture_root, "window-a", 5201)
            .await
            .expect("window A attaches");
        let (direct, direct_journal) = started_id(
            first
                .request_start(&file_definition(121, &log))
                .await
                .expect("direct start works"),
        );
        let (aliased, alias_journal) = started_id(
            first
                .request_start(&file_definition(122, &link))
                .await
                .expect("symlink start works"),
        );
        assert_eq!(
            direct, aliased,
            "alias spellings must share one capture identity"
        );
        assert_eq!(direct_journal, alias_journal);

        let (mut second, presence) = WorkerClient::attach(&bin, &capture_root, "window-b", 5202)
            .await
            .expect("window B attaches");
        assert_eq!(
            presence.len(),
            1,
            "aliases start no second capture: {presence:?}"
        );
        assert_eq!(presence[0].id, direct.0.to_string());

        first.shutdown().await.expect("window A drains");
        second.shutdown().await.expect("window B drains");
    })
    .await;
    assert!(scenario.is_ok(), "alias scenario exceeded 120s");
}
