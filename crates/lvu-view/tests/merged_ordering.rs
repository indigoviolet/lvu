//! Display order of a merged multi-source view (docs/merged-view-ordering.md).
//!
//! The engine merges the sources' runs by the basis time. The tests that held
//! under the old concatenation are still here and still pin the properties the
//! change had to keep — above all that `index_of_id` agrees with `page`, which
//! is what every selection, scroll and jump rests on. The tests that describe
//! interleaving are `#[ignore]`d with their invariant named, so they compile
//! against the API from the start and are un-ignored by the commit that builds
//! the order.

use lvu::{
    QueryConstraints, QueryPurpose, QueryRequest, RowProvider, TextConstraint, ViewportRequest,
    terminal::QueryDispatcher,
};
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
    let mut adapter = NativeViewAdapter::new(raw, view).unwrap();
    adapter
        .register_source(lvu_shared::AnySourceHandle::Local(api.clone()))
        .unwrap();
    adapter
        .register_source(lvu_shared::AnySourceHandle::Local(worker.clone()))
        .unwrap();
    adapter
        .register_view("view", vec![api.source_id(), worker.source_id()])
        .unwrap();
    // An applied filter is what produces the membership the order is built in.
    // Every record carries `ts`, so the filter selects all of them and the
    // subject stays the order rather than which rows survived.
    apply(&mut adapter, 1);
    wait_applied(&mut adapter, 1).await;
    (manager, api, worker, adapter)
}

/// Waits for a submitted revision to be applied.
///
/// A refresh's base is the revision before it, so submitting the next one
/// before this one lands is refused with "query base snapshot does not match
/// the applied view" — the engine protecting itself, not a timing hint.
async fn wait_applied(adapter: &mut NativeViewAdapter, revision: u64) {
    let done = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            adapter.drain_updates(64);
            if let Some(done) = adapter.poll()
                && done.revision == revision
            {
                break done;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the revision is applied");
    assert!(
        done.result.is_ok(),
        "revision {revision}: {:?}",
        done.result
    );
}

/// The same filter at a new revision.
///
/// The fixture's `ts` is an event time. Under the capture basis the merge key
/// would be arrival, both sources would be captured in the same instant, and
/// the tie rule — earliest source position — would correctly reproduce
/// concatenation, which is what the old order was.
fn apply(adapter: &mut NativeViewAdapter, revision: u64) {
    let constraints = QueryConstraints {
        text: Some(TextConstraint {
            literal: "ts".into(),
            case_insensitive: true,
        }),
        time_basis: lvu::TimeBasis::Event,
        ..QueryConstraints::default()
    };
    adapter
        .submit(QueryRequest {
            view_id: "view".into(),
            // The generation is the source generation, not the revision: it
            // changes only when a source restarts, and bumping it would make
            // every refresh discard the membership it should be extending.
            generation: 1,
            revision,
            base_revision: revision.saturating_sub(1),
            base_constraints: if revision <= 1 {
                QueryConstraints::default()
            } else {
                constraints.clone()
            },
            purpose: QueryPurpose::Search,
            constraints,
        })
        .unwrap();
}

/// Two sources logging the *same* service at interleaving times, so a fold on
/// `svc` has a run that only exists once the sources are merged.
async fn same_service(
    root: &TempDir,
) -> (SourceManager, SourceHandle, SourceHandle, NativeViewAdapter) {
    let api_path = root.path().join("api.log");
    let worker_path = root.path().join("worker.log");
    for (path, offset) in [(&api_path, 0), (&worker_path, 1)] {
        fs::write(
            path,
            (0..6)
                .map(|index| {
                    format!(
                        "{{\"ts\":\"2026-03-04T05:06:{:02}Z\",\"svc\":\"gateway\",\"n\":{index}}}\n",
                        index * 2 + offset
                    )
                })
                .collect::<String>(),
        )
        .unwrap();
    }
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
    let mut adapter = NativeViewAdapter::new(raw, view).unwrap();
    adapter
        .register_source(lvu_shared::AnySourceHandle::Local(api.clone()))
        .unwrap();
    adapter
        .register_source(lvu_shared::AnySourceHandle::Local(worker.clone()))
        .unwrap();
    adapter
        .register_view("view", vec![api.source_id(), worker.source_id()])
        .unwrap();
    apply(&mut adapter, 1);
    wait_applied(&mut adapter, 1).await;
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
    apply(&mut adapter, 2);
    wait_applied(&mut adapter, 2).await;
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
    apply(&mut adapter, 2);
    wait_applied(&mut adapter, 2).await;
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
    apply(&mut adapter, 2);
    wait_applied(&mut adapter, 2).await;
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

/// I6, I7, and the edge where I6 meets I2: a record that arrives late with an
/// older time is placed among the *other* sources by its time, but inside its
/// own source it stays where it arrived.
///
/// Reordering it inside its source is what I2 forbids, so appending an old
/// record to one file does not move it to the top of the view. It appears
/// where that source's stream has reached, that source becomes one that
/// "arrives out of order", and the order row says so. Meanwhile the record
/// that was addressed before the insert is still the same record — which is
/// what lets the shell hold the selection by identity.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
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
    apply(&mut adapter, 2);
    wait_applied(&mut adapter, 2).await;
    let after_page = settled_page(&mut adapter, 13);

    let late = after_page
        .rows
        .iter()
        .position(|row| row.text.contains("05:05:00"))
        .expect("the late record is shown");
    let last_earlier_api = after_page
        .rows
        .iter()
        .rposition(|row| row.text.contains("\"api\"") && row.text.contains("05:06:10"))
        .expect("its source's previous record");
    assert_eq!(
        late,
        last_earlier_api + 1,
        "it follows its own source's previous record rather than jumping to the top"
    );

    // Its source is now out of order in this basis, and that is reported.
    let order = adapter.rows().view_order("view").expect("a merged view");
    assert_eq!(order.sources, 2);
    assert_eq!(order.out_of_order, 1);
    assert!(!order.fully_ordered());

    let after = adapter.rows().index_of_id("view", &addressed).unwrap();
    assert_eq!(
        after_page.rows[after].id, addressed,
        "still the same record"
    );
    assert!(
        after >= before,
        "nothing moved above it: {before} to {after}"
    );
    adapter.shutdown();
    manager.shutdown().await;
}

/// I5: folding groups rows adjacent *in the merged order*, so a run drawn
/// alternately from two sources folds into one line.
///
/// This is the invariant that is a capability rather than a port. Both files
/// here log the same service at interleaving times. Concatenated, the run is
/// two blocks of six and folds to two lines; merged, it is one run of twelve
/// and folds to one. Nothing about the records changed — only which rows are
/// adjacent.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_fold_run_spanning_two_sources_is_contiguous() {
    let root = TempDir::new().unwrap();
    let (manager, _api, _worker, mut adapter) = same_service(&root).await;
    settled_page(&mut adapter, 12);
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
    let folded = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            adapter.drain_updates(64);
            let page = adapter
                .rows()
                .page("view", ViewportRequest { start: 0, len: 256 });
            if page.rows.len() <= 2 {
                break page;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the run folds");
    assert_eq!(
        folded.rows.len(),
        1,
        "twelve records of one service, adjacent only once merged, are one run"
    );
    adapter.shutdown();
    manager.shutdown().await;
}
