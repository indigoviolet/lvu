//! What a restart does to the record count.
//!
//! A stopped-and-restarted source is the shape behind `Alt-R`, behind a
//! workspace restore, and behind the fixture in
//! `lvu-view/tests/assistance_preparation.rs` whose record count moved under
//! load. The question that fixture raised — whether a restart re-reads records
//! it had already captured — is answered here, and pinned, because a
//! regression to resuming from the start of the file would be invisible in
//! every other test: the records would all be there, just twice.
//!
//! Two things are separated on purpose. A *restart* repeats nothing: the
//! resumed run continues from the durable cursor. A *partial line* is captured
//! as its own record before its terminator arrives, which is intended — raw
//! bytes are shown before derived data is ready — and is the only reason a
//! record count can exceed a line count. The second is bounded by what is in
//! flight, never by the size of the file.

use lvu_core::{Acquisition, SourceDefinition, SourceId};
use lvu_ingest::{RuntimeConfig, RuntimeState, SourceHandle, SourceManager};
use std::{collections::BTreeMap, fs, io::Write, time::Duration};
use tempfile::TempDir;

const LINES: usize = 8_192;
/// Where the first run stops, so the restart has a real prefix to skip.
const PREFIX: usize = 128;

fn definition(id: SourceId, path: &std::path::Path) -> SourceDefinition {
    SourceDefinition {
        schema_version: 1,
        id,
        name: "restart replay".into(),
        acquisition: Acquisition::File {
            path: path.into(),
            follow: false,
        },
        identity_hints: BTreeMap::new(),
        retention: None,
    }
}

/// `read_chunk_bytes` is not a multiple of the 13-byte line, so every chunk
/// boundary lands mid-line and the partial-line flush has something to flush.
/// That is what makes the fragment case reachable at all.
fn config(line_framed: bool) -> RuntimeConfig {
    let mut config = RuntimeConfig::default();
    config.acquisition.read_chunk_bytes = 4 * 1024;
    config.batch_records = 32;
    config.max_page_records = 64;
    config.max_page_bytes = 64 * 1024;
    if line_framed {
        // Longer than the whole run, so a record is only ever a complete line.
        config.acquisition.partial_flush_interval = Duration::from_secs(3600);
    }
    config
}

fn body(from: usize, to: usize) -> String {
    (from..to)
        .map(|index| format!("ordinal-{index:04}\n"))
        .collect()
}

async fn wait(handle: &SourceHandle, records: u64) {
    let mut progress = handle.subscribe();
    let _ = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let current = progress.borrow().clone();
            if current.records >= records || current.state == RuntimeState::Stopped {
                break;
            }
            if progress.changed().await.is_err() {
                break;
            }
        }
    })
    .await;
    // The state settles a moment after the threshold is crossed; reading the
    // count before then would measure the wait, not the run.
    tokio::time::sleep(Duration::from_millis(300)).await;
}

/// Ingests `LINES` lines, stopping after `PREFIX` of them and restarting, and
/// answers with (records the first run had reached, records in the end).
async fn stop_append_restart(root: &TempDir, line_framed: bool) -> (u64, u64) {
    let path = root.path().join("input.log");
    fs::write(&path, body(0, PREFIX)).unwrap();
    let manager = SourceManager::new(root.path().join("capture"), config(line_framed)).unwrap();
    let id = SourceId::new();

    let first = manager.start(definition(id, &path)).await.unwrap();
    wait(&first, PREFIX as u64).await;
    let stopped_at = first.progress().records;
    // A source that has reached the end of a non-followed file has already
    // finished, so `stop` reports `Closed`; that is the state under test.
    let _ = first.stop().await;

    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    file.write_all(body(PREFIX, LINES).as_bytes()).unwrap();
    file.flush().unwrap();

    let second = manager.start(definition(id, &path)).await.unwrap();
    wait(&second, LINES as u64).await;
    let total = second.progress().records;
    manager.shutdown().await;
    (stopped_at, total)
}

/// The bound: a restart repeats nothing at all.
///
/// With records framed only on line boundaries the count is exact, so this is
/// an equality rather than a range. Resuming from the start of the file would
/// give `LINES + PREFIX`; resuming from the wrong checkpoint would give
/// something between. Both are caught.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_restart_resumes_from_the_cursor_and_repeats_no_records() {
    let root = TempDir::new().unwrap();
    let (stopped_at, total) = stop_append_restart(&root, true).await;
    assert_eq!(
        stopped_at, PREFIX as u64,
        "the first run captured the prefix it was given"
    );
    assert_eq!(
        total, LINES as u64,
        "the restarted run ends at the line count: nothing lost, nothing repeated"
    );
}

/// The same file with no restart, so the restart's contribution is isolated.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_uninterrupted_run_reaches_the_same_count() {
    let root = TempDir::new().unwrap();
    let path = root.path().join("input.log");
    fs::write(&path, body(0, LINES)).unwrap();
    let manager = SourceManager::new(root.path().join("capture"), config(true)).unwrap();
    let handle = manager
        .start(definition(SourceId::new(), &path))
        .await
        .unwrap();
    wait(&handle, LINES as u64).await;
    let total = handle.progress().records;
    manager.shutdown().await;
    assert_eq!(
        total, LINES as u64,
        "a restart is the only difference between this and the test above"
    );
}

/// With the partial-line flush enabled, a record count can exceed the line
/// count — a fragment is a record, on purpose. What must stay true is that the
/// excess is what is in flight and not a function of the file: a run over
/// 8,192 lines may produce a handful of fragments, never a second copy of the
/// data.
///
/// The assertion is deliberately loose on the low side and tight on the high
/// side. Fragments are a race, so demanding one would be flaky; allowing the
/// file's own size back would defeat the point.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn partial_line_fragments_are_the_only_excess_and_stay_bounded() {
    /// One flush interval's worth of in-flight fragments, with room for the
    /// boundary of each of the two runs. Far below `PREFIX`, so a resumed
    /// prefix could not hide inside it.
    const MAX_FRAGMENTS: u64 = 16;
    let root = TempDir::new().unwrap();
    let (stopped_at, total) = stop_append_restart(&root, false).await;
    assert!(
        stopped_at >= PREFIX as u64,
        "the prefix is captured whole: {stopped_at}"
    );
    assert!(
        total >= LINES as u64,
        "no line is lost to framing: {total} of {LINES}"
    );
    assert!(
        total - LINES as u64 <= MAX_FRAGMENTS,
        "the excess is fragments in flight, not repeated records: {total} for {LINES} lines"
    );
}
