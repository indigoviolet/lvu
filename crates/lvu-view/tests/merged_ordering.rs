//! Display order of a merged multi-source view (docs/merged-view-ordering.md).
//!
//! Today the engine concatenates: every record of the first source, then every
//! record of the second, whatever their times. The tests that hold under
//! concatenation are live, and pin the properties the interleaving change must
//! not break — above all that `index_of_id` agrees with `page`, which is what
//! every selection, scroll and jump rests on. The tests that describe
//! interleaving are `#[ignore]`d with their invariant named, so they compile
//! against the API from the start and are un-ignored by the commit that builds
//! the order.

use lvu::{RowProvider, ViewportRequest};
use lvu_core::{Acquisition, SourceDefinition, SourceId};
use lvu_ingest::{RuntimeConfig, RuntimeState, SourceHandle, SourceManager};
use lvu_live::{LiveConfig, LiveRowProvider};
use lvu_view::{NativeViewAdapter, ViewConfig};
use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::Write,
    sync::Arc,
    time::Duration,
};
use tempfile::TempDir;

fn source(id: SourceId, path: &std::path::Path, follow: bool) -> SourceDefinition {
    SourceDefinition {
        schema_version: 1,
        id,
        name: "merged fixture".into(),
        acquisition: Acquisition::File {
            path: path.into(),
            follow,
        },
        identity_hints: BTreeMap::new(),
        retention: None,
    }
}

fn runtime_config() -> RuntimeConfig {
    let mut value = RuntimeConfig::default();
    value.acquisition.read_chunk_bytes = 64;
    // Framing only on line boundaries: a partial-line fragment would be a
    // record with no time, which is I3's subject and not this fixture's.
    value.acquisition.partial_flush_interval = Duration::from_secs(3600);
    value.batch_records = 8;
    value.max_page_records = 16;
    value.max_page_bytes = 4096;
    value
}

fn configs(root: &TempDir) -> (LiveConfig, ViewConfig) {
    let mut live = LiveConfig::new(root.path().join("raw-index"));
    live.index_page_records = 8;
    live.index_page_bytes = 4096;
    live.cache_rows = 16;
    live.cache_bytes = 16 * 1024;
    let mut view = ViewConfig::new(root.path().join("view-index"));
    view.page_records = 4;
    view.page_bytes = 4096;
    // No Python: nothing here needs a compiled advanced definition, and the
    // helper would make an ordering test depend on a subprocess.
    view.compiler = None;
    (live, view)
}

async fn wait_runtime(handle: &SourceHandle, records: u64) {
    let mut progress = handle.subscribe();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let current = progress.borrow().clone();
            if current.records >= records || current.state == RuntimeState::Stopped {
                break;
            }
            progress.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
}

/// Two file sources whose event times interleave: `api` at the even seconds,
/// `worker` at the odd ones, so concatenation and time order differ in every
/// position after the first.
async fn merged(root: &TempDir) -> (SourceManager, SourceHandle, SourceHandle, NativeViewAdapter) {
    let api_path = root.path().join("api.log");
    let worker_path = root.path().join("worker.log");
    fs::write(
        &api_path,
        (0..6)
            .map(|index| {
                format!(
                    "{{\"ts\":\"2026-03-04T05:06:{:02}Z\",\"svc\":\"api\",\"n\":{index}}}\n",
                    index * 2
                )
            })
            .collect::<String>(),
    )
    .unwrap();
    fs::write(
        &worker_path,
        (0..6)
            .map(|index| {
                format!(
                    "{{\"ts\":\"2026-03-04T05:06:{:02}Z\",\"svc\":\"worker\",\"n\":{index}}}\n",
                    index * 2 + 1
                )
            })
            .collect::<String>(),
    )
    .unwrap();
    let manager = SourceManager::new(root.path().join("capture"), runtime_config()).unwrap();
    let api = manager
        .start(source(SourceId::new(), &api_path, true))
        .await
        .unwrap();
    let worker = manager
        .start(source(SourceId::new(), &worker_path, true))
        .await
        .unwrap();
    wait_runtime(&api, 6).await;
    wait_runtime(&worker, 6).await;
    let (live, view) = configs(root);
    let raw = Arc::new(LiveRowProvider::new(live).unwrap());
    let adapter = NativeViewAdapter::new(raw, view).unwrap();
    adapter.register_source(api.clone()).unwrap();
    adapter.register_source(worker.clone()).unwrap();
    adapter
        .register_view("view", vec![api.source_id(), worker.source_id()])
        .unwrap();
    (manager, api, worker, adapter)
}

/// Drains until the view reports at least `rows`, then returns the page.
fn settled_page(adapter: &mut NativeViewAdapter, rows: usize) -> lvu::RowPage {
    let started = std::time::Instant::now();
    loop {
        adapter.drain_updates(64);
        let page = adapter
            .rows()
            .page("view", ViewportRequest { start: 0, len: 256 });
        if page.rows.len() >= rows {
            return page;
        }
        assert!(
            started.elapsed() < Duration::from_secs(15),
            "only {} of {rows} rows arrived",
            page.rows.len()
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// The property every selection, scroll, bookmark jump and correlation origin
/// rests on: the index the provider reports for a record is where that record
/// actually is in the page. It holds under concatenation and must still hold
/// under interleaving, which is why it is live rather than ignored.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn index_of_id_agrees_with_the_page_for_every_record() {
    let root = TempDir::new().unwrap();
    let (manager, _api, _worker, mut adapter) = merged(&root).await;
    let page = settled_page(&mut adapter, 12);
    let rows = adapter.rows();
    for (position, row) in page.rows.iter().enumerate() {
        assert_eq!(
            rows.index_of_id("view", &row.id),
            Some(position),
            "row {position} ({:?}) does not index to its own position",
            row.id
        );
    }
    adapter.shutdown();
    manager.shutdown().await;
}

/// The page is a permutation of the merged records: every record once, none
/// invented, and `total` agreeing with what was handed over. Reordering must
/// not become an opportunity to drop or duplicate a record.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_page_is_a_permutation_of_both_sources() {
    let root = TempDir::new().unwrap();
    let (manager, api, worker, mut adapter) = merged(&root).await;
    let page = settled_page(&mut adapter, 12);
    assert_eq!(page.total, 12);
    assert_eq!(page.rows.len(), 12);

    let mut seen = page
        .rows
        .iter()
        .map(|row| row.id.clone())
        .collect::<Vec<_>>();
    let before = seen.len();
    seen.sort_by(|left, right| {
        left.source_id
            .cmp(&right.source_id)
            .then(left.sequence.cmp(&right.sequence))
    });
    seen.dedup();
    assert_eq!(seen.len(), before, "a record appeared twice");
    for handle in [&api, &worker] {
        assert_eq!(
            page.rows
                .iter()
                .filter(|row| row.id.source_id == handle.source_id().0.to_string())
                .count(),
            6,
            "both sources contribute all of their records"
        );
    }
    adapter.shutdown();
    manager.shutdown().await;
}

/// I7: identities survive whatever the order does. A record addressed before an
/// append still resolves afterwards, at whatever index it now has — this is
/// what keeps bookmarks, the selection and correlation origins meaningful.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_identity_still_resolves_after_a_live_append() {
    let root = TempDir::new().unwrap();
    let (manager, api, _worker, mut adapter) = merged(&root).await;
    let page = settled_page(&mut adapter, 12);
    let addressed = page.rows[3].id.clone();
    let before = adapter.rows().index_of_id("view", &addressed).unwrap();
    let addressed_text = page.rows[3].text.clone();

    let mut file = OpenOptions::new()
        .append(true)
        .open(root.path().join("api.log"))
        .unwrap();
    // Deliberately older than everything already displayed: under interleaving
    // this lands above the addressed record, under concatenation below it.
    writeln!(
        file,
        "{{\"ts\":\"2026-03-04T05:05:00Z\",\"svc\":\"api\",\"n\":99}}"
    )
    .unwrap();
    file.flush().unwrap();
    wait_runtime(&api, 7).await;
    let after_page = settled_page(&mut adapter, 13);

    let after = adapter
        .rows()
        .index_of_id("view", &addressed)
        .expect("the addressed record still resolves");
    assert_eq!(
        after_page.rows[after].id, addressed,
        "and resolves to itself"
    );
    assert_eq!(
        after_page.rows[after].text, addressed_text,
        "with its own bytes"
    );
    let _ = before;
    adapter.shutdown();
    manager.shutdown().await;
}

// --- Interleaving. Un-ignore these with the commit that builds the order. ---

/// I1, I2: with both sources ascending in the basis, the merged page is
/// nondecreasing in it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "merged views concatenate by source; see docs/merged-view-ordering.md"]
async fn the_merged_page_is_nondecreasing_in_the_basis_time() {
    let root = TempDir::new().unwrap();
    let (manager, _api, _worker, mut adapter) = merged(&root).await;
    let page = settled_page(&mut adapter, 12);
    let seconds = page
        .rows
        .iter()
        .map(|row| row.text.clone())
        .collect::<Vec<_>>();
    let mut sorted = seconds.clone();
    sorted.sort();
    assert_eq!(
        seconds, sorted,
        "the merged page should be in time order, not source order"
    );
    adapter.shutdown();
    manager.shutdown().await;
}

/// I1: equal times break by source position, then by sequence, and the order
/// is the same every time the same membership is published.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "merged views concatenate by source; see docs/merged-view-ordering.md"]
async fn equal_times_break_by_source_position_then_sequence() {
    let root = TempDir::new().unwrap();
    let (manager, api, worker, mut adapter) = merged(&root).await;
    for (path, service) in [("api.log", "api"), ("worker.log", "worker")] {
        let mut file = OpenOptions::new()
            .append(true)
            .open(root.path().join(path))
            .unwrap();
        writeln!(
            file,
            "{{\"ts\":\"2026-03-04T05:07:00Z\",\"svc\":\"{service}\",\"n\":50}}"
        )
        .unwrap();
        file.flush().unwrap();
    }
    wait_runtime(&api, 7).await;
    wait_runtime(&worker, 7).await;
    let page = settled_page(&mut adapter, 14);
    let tail = &page.rows[page.rows.len() - 2..];
    assert!(tail[0].text.contains("\"api\""), "{:?}", tail[0].text);
    assert!(tail[1].text.contains("\"worker\""), "{:?}", tail[1].text);

    let again = settled_page(&mut adapter, 14);
    assert_eq!(
        again
            .rows
            .iter()
            .map(|row| row.id.clone())
            .collect::<Vec<_>>(),
        page.rows
            .iter()
            .map(|row| row.id.clone())
            .collect::<Vec<_>>(),
        "the same membership publishes the same order"
    );
    adapter.shutdown();
    manager.shutdown().await;
}

/// I3: a record with no readable value in the basis follows the last timed
/// record of its own source rather than sorting to an end.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "merged views concatenate by source; see docs/merged-view-ordering.md"]
async fn an_untimed_record_keeps_its_place_in_its_own_source() {
    let root = TempDir::new().unwrap();
    let (manager, api, _worker, mut adapter) = merged(&root).await;
    let mut file = OpenOptions::new()
        .append(true)
        .open(root.path().join("api.log"))
        .unwrap();
    writeln!(
        file,
        "{{\"svc\":\"api\",\"n\":404,\"note\":\"no ts here\"}}"
    )
    .unwrap();
    file.flush().unwrap();
    wait_runtime(&api, 7).await;
    let page = settled_page(&mut adapter, 13);

    let untimed = page
        .rows
        .iter()
        .position(|row| row.text.contains("no ts here"))
        .expect("the untimed record is shown, never dropped");
    let last_api = page
        .rows
        .iter()
        .rposition(|row| row.text.contains("\"api\"") && row.text.contains("\"ts\""))
        .unwrap();
    assert_eq!(
        untimed,
        last_api + 1,
        "it follows its own source's last timed record"
    );
    // And it is *mid-stream*, not at the end of an api block: under
    // concatenation every api record precedes every worker record, so this is
    // the half of the assertion that distinguishes the two orders.
    let first_worker = page
        .rows
        .iter()
        .position(|row| row.text.contains("\"worker\""))
        .unwrap();
    assert!(
        first_worker < untimed,
        "worker records interleave before it: untimed at {untimed}, first worker at {first_worker}"
    );
    adapter.shutdown();
    manager.shutdown().await;
}

/// I6, I7: a late record with an older time lands at its time position, and
/// the record that was addressed before the insert is still the same record at
/// its new index — which is what lets the shell hold the selection by identity
/// and re-anchor the viewport around it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "merged views concatenate by source; see docs/merged-view-ordering.md"]
async fn a_late_older_record_inserts_above_the_selection() {
    let root = TempDir::new().unwrap();
    let (manager, api, _worker, mut adapter) = merged(&root).await;
    let page = settled_page(&mut adapter, 12);
    let addressed = page.rows[6].id.clone();
    let before = adapter.rows().index_of_id("view", &addressed).unwrap();

    let mut file = OpenOptions::new()
        .append(true)
        .open(root.path().join("api.log"))
        .unwrap();
    writeln!(
        file,
        "{{\"ts\":\"2026-03-04T05:05:00Z\",\"svc\":\"api\",\"n\":99}}"
    )
    .unwrap();
    file.flush().unwrap();
    wait_runtime(&api, 7).await;
    let after_page = settled_page(&mut adapter, 13);

    assert!(
        after_page.rows[0].text.contains("05:05:00"),
        "the older record leads the merged view: {:?}",
        after_page.rows[0].text
    );
    let after = adapter.rows().index_of_id("view", &addressed).unwrap();
    assert_eq!(after, before + 1, "one record was inserted above it");
    assert_eq!(after_page.rows[after].id, addressed);
    adapter.shutdown();
    manager.shutdown().await;
}

/// I5: folding groups rows adjacent *in the merged order*, so a run that
/// alternates between two sources by arrival but is contiguous in time folds
/// into one line.
///
/// This is the invariant that is a capability rather than a port: under
/// concatenation the two sources' halves of the run can never be adjacent, so
/// a chatty service logging into two files cannot be folded at all.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "merged views concatenate by source; see docs/merged-view-ordering.md"]
async fn a_fold_run_spanning_two_sources_is_contiguous() {
    let root = TempDir::new().unwrap();
    let (manager, _api, _worker, mut adapter) = merged(&root).await;
    settled_page(&mut adapter, 12);
    // Both fixtures carry `svc`, and their times alternate, so folding on a
    // constant key would collapse everything; folding on `svc` must instead
    // produce runs of one, because no two adjacent merged rows share a service.
    adapter.rows().set_fold(
        "view",
        &lvu::FoldRequest {
            enabled: true,
            minimum_run: 2,
            key_column: Some("svc".into()),
            expanded: Vec::new(),
            ..lvu::FoldRequest::default()
        },
    );
    let folded = settled_page(&mut adapter, 12);
    assert_eq!(
        folded.rows.len(),
        12,
        "alternating services give no run of two, so nothing folds"
    );

    // Now make one service contiguous in time across both files: three worker
    // records inside the api sequence's gap. They are adjacent in the merged
    // order and in no source's own order, so only a merged fold sees the run.
    let mut file = OpenOptions::new()
        .append(true)
        .open(root.path().join("worker.log"))
        .unwrap();
    for offset in 0..3 {
        writeln!(
            file,
            "{{\"ts\":\"2026-03-04T05:06:{:02}Z\",\"svc\":\"worker\",\"n\":{}}}",
            20 + offset,
            60 + offset
        )
        .unwrap();
    }
    file.flush().unwrap();
    let merged_fold = settled_page(&mut adapter, 13);
    let worker_lines = merged_fold
        .rows
        .iter()
        .filter(|row| row.text.contains("\"worker\""))
        .count();
    assert!(
        worker_lines < 9,
        "the contiguous worker run folds into fewer lines than its records: {worker_lines}"
    );
    adapter.shutdown();
    manager.shutdown().await;
}
