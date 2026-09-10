//! Executable union worker: register, submit, freeze, visit, merge, publish.
//!
//! Two real file sources with interleaving event times, two accepted filtered
//! views, one union over them. Pumps the real adapter tick (freeze traffic
//! rides `drain_updates`) and asserts the published membership: timestamp
//! order across sources, first-input dedup, revision fencing and live
//! refresh on input advance. No mocks: real capture, real frozen replay,
//! real Polars merge.

use lvu::{
    QueryConstraints, QueryPurpose, QueryRequest, RowProvider, TextConstraint, ViewportRequest,
    terminal::QueryDispatcher,
};
use lvu_core::{Acquisition, SourceDefinition, SourceId};
use lvu_ingest::{RuntimeConfig, RuntimeState, SourceHandle, SourceManager};
use lvu_live::{LiveConfig, LiveRowProvider};
use lvu_view::{
    NativeViewAdapter, StoredUnionInput, UnionCandidateSpec, UnionFilterSpec, UnionPhaseTestProbe,
    UnionPublishTestBarrier, UnionTestBarrier, ViewConfig,
};
use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::Write,
    sync::{Arc, atomic::AtomicUsize},
    time::Duration,
};
use tempfile::TempDir;

fn source(id: SourceId, path: &std::path::Path, follow: bool) -> SourceDefinition {
    SourceDefinition {
        schema_version: 1,
        id,
        name: "union fixture".into(),
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
    live.cache_rows = 64;
    live.cache_bytes = 64 * 1024;
    let mut view = ViewConfig::new(root.path().join("view-index"));
    view.page_records = 4;
    view.page_bytes = 4096;
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

fn apply(adapter: &mut NativeViewAdapter, view: &str, revision: u64) {
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
            view_id: view.into(),
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

async fn setup() -> (
    TempDir,
    SourceManager,
    SourceHandle,
    SourceHandle,
    NativeViewAdapter,
) {
    let root = TempDir::new().unwrap();
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
    let (live, view) = configs(&root);
    let raw = Arc::new(LiveRowProvider::new(live).unwrap());
    let mut adapter = NativeViewAdapter::new(raw, view).unwrap();
    adapter.register_source(api.clone()).unwrap();
    adapter.register_source(worker.clone()).unwrap();
    adapter
        .register_view("view-a", vec![api.source_id()])
        .unwrap();
    adapter
        .register_view("view-b", vec![worker.source_id()])
        .unwrap();
    apply(&mut adapter, "view-a", 1);
    wait_applied(&mut adapter, 1).await;
    apply(&mut adapter, "view-b", 1);
    wait_applied(&mut adapter, 1).await;
    (root, manager, api, worker, adapter)
}

async fn setup_raw_with_budget(
    maximum_index_bytes: u64,
) -> (
    TempDir,
    SourceManager,
    SourceHandle,
    SourceHandle,
    NativeViewAdapter,
) {
    let root = TempDir::new().unwrap();
    let api_path = root.path().join("api.log");
    let worker_path = root.path().join("worker.log");
    fs::write(&api_path, "api row\n").unwrap();
    fs::write(&worker_path, "worker row\n").unwrap();
    let manager = SourceManager::new(root.path().join("capture"), runtime_config()).unwrap();
    let api = manager
        .start(source(SourceId::new(), &api_path, true))
        .await
        .unwrap();
    let worker = manager
        .start(source(SourceId::new(), &worker_path, true))
        .await
        .unwrap();
    wait_runtime(&api, 1).await;
    wait_runtime(&worker, 1).await;
    let (live, mut view) = configs(&root);
    view.maximum_index_bytes = maximum_index_bytes;
    let raw = Arc::new(LiveRowProvider::new(live).unwrap());
    let adapter = NativeViewAdapter::new(raw, view).unwrap();
    adapter.register_source(api.clone()).unwrap();
    adapter.register_source(worker.clone()).unwrap();
    adapter
        .register_view("raw-a", vec![api.source_id()])
        .unwrap();
    adapter
        .register_view("raw-b", vec![worker.source_id()])
        .unwrap();
    (root, manager, api, worker, adapter)
}

/// Pump the tick until the union job for `revision` completes, then return
/// its error (if any). Freeze traffic rides `drain_updates`; the worker
/// thread does the replay and merge.
fn wait_union(adapter: &mut NativeViewAdapter, revision: u64) -> Option<lvu_view::UnionCompletion> {
    let started = std::time::Instant::now();
    loop {
        adapter.drain_updates(64);
        let mut completions = adapter.take_union_completions();
        if let Some(done) = completions
            .iter()
            .position(|completion| completion.union_revision == revision)
        {
            return Some(completions.remove(done));
        }
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "union revision {revision} did not complete"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn candidate(
    revision: u64,
    api: &SourceHandle,
    worker: &SourceHandle,
    api_revision: u64,
    worker_revision: u64,
) -> UnionCandidateSpec {
    candidate_filtered(revision, api, worker, api_revision, worker_revision, "")
}

fn candidate_with_generation(
    revision: u64,
    generation: u64,
    api: &SourceHandle,
    worker: &SourceHandle,
    api_revision: u64,
    worker_revision: u64,
) -> UnionCandidateSpec {
    let mut candidate =
        candidate_filtered(revision, api, worker, api_revision, worker_revision, "");
    candidate.generation = generation;
    candidate
}

fn candidate_filtered(
    revision: u64,
    api: &SourceHandle,
    worker: &SourceHandle,
    api_revision: u64,
    worker_revision: u64,
    search: &str,
) -> UnionCandidateSpec {
    UnionCandidateSpec {
        union_view_id: "union".into(),
        union_revision: revision,
        generation: 1,
        inputs: vec![
            StoredUnionInput {
                view_id: "view-a".into(),
                accepted_revision: api_revision,
                applied_generation: api.progress().generation,
            },
            StoredUnionInput {
                view_id: "view-b".into(),
                accepted_revision: worker_revision,
                applied_generation: worker.progress().generation,
            },
        ],
        filter: UnionFilterSpec {
            search: search.into(),
            exact_key: None,
            ..UnionFilterSpec::default()
        },
    }
}

fn raw_candidate(revision: u64) -> UnionCandidateSpec {
    UnionCandidateSpec {
        union_view_id: "union".into(),
        union_revision: revision,
        generation: revision,
        inputs: vec![
            StoredUnionInput {
                view_id: "raw-a".into(),
                accepted_revision: 0,
                applied_generation: 0,
            },
            StoredUnionInput {
                view_id: "raw-b".into(),
                accepted_revision: 0,
                applied_generation: 0,
            },
        ],
        filter: UnionFilterSpec::default(),
    }
}

fn union_rows(adapter: &mut NativeViewAdapter) -> Vec<lvu::DisplayRow> {
    // Rows page in behind a fresh publication; pump the tick like the
    // product loop does — fetched rows only enter the cache through the
    // drain — and poll briefly.
    let started = std::time::Instant::now();
    loop {
        adapter.drain_updates(64);
        let page = adapter
            .rows()
            .page("union", ViewportRequest { start: 0, len: 256 });
        if page.rows.len() >= page.total {
            return page.rows;
        }
        assert!(
            started.elapsed() < Duration::from_secs(15),
            "only {} of {} union rows arrived; status={:?} order={:?}",
            page.rows.len(),
            page.total,
            adapter.status("union"),
            adapter.rows().view_order("union"),
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn union_texts(adapter: &mut NativeViewAdapter) -> Vec<String> {
    union_rows(adapter)
        .into_iter()
        .map(|row| row.text)
        .collect()
}

fn phase_probe() -> (UnionPhaseTestProbe, [Arc<AtomicUsize>; 4]) {
    let counters = std::array::from_fn(|_| Arc::new(AtomicUsize::new(0)));
    (
        UnionPhaseTestProbe {
            retained_rows: Arc::clone(&counters[0]),
            polars_builds: Arc::clone(&counters[1]),
            grouping_indexed_rows: Arc::clone(&counters[2]),
            grouping_lookups: Arc::clone(&counters[3]),
        },
        counters,
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn union_publishes_both_sources_in_ts_order() {
    let (_root, manager, api, worker, mut adapter) = setup().await;
    adapter
        .register_union_view("union", vec![api.source_id(), worker.source_id()])
        .unwrap();
    // Structural validation happens before any work is queued.
    assert!(
        adapter
            .submit_union_candidate(
                UnionCandidateSpec {
                    union_view_id: "nope".into(),
                    union_revision: 1,
                    generation: 1,
                    inputs: vec![],
                    filter: UnionFilterSpec::default(),
                },
                &|_| None,
            )
            .is_err()
    );
    adapter
        .submit_union_candidate(candidate(1, &api, &worker, 1, 1), &|_| None)
        .unwrap();
    let completion = wait_union(&mut adapter, 1).expect("a completion");
    assert_eq!(completion.error, None);
    assert_eq!(
        adapter.union_inputs("union").unwrap().len(),
        2,
        "the fence baseline is recorded"
    );
    let texts = union_texts(&mut adapter);
    assert_eq!(texts.len(), 12);
    let mut sorted = texts.clone();
    sorted.sort();
    assert_eq!(
        texts, sorted,
        "the union page is in timestamp order, not source order"
    );
    // A resubmit at the published revision is a no-op, not a second job.
    adapter
        .submit_union_candidate(candidate(1, &api, &worker, 1, 1), &|_| None)
        .unwrap();
    adapter.drain_updates(64);
    assert!(adapter.take_union_completions().is_empty());
    adapter.shutdown();
    manager.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exhausted_shared_budget_rejects_before_carrier_retention_or_polars() {
    let (_root, manager, api, worker, mut adapter) = setup_raw_with_budget(128).await;
    adapter
        .register_union_view("union", vec![api.source_id(), worker.source_id()])
        .unwrap();
    let (probe, counters) = phase_probe();
    adapter.arm_union_phase_test_probe("union", probe).unwrap();
    adapter
        .submit_union_candidate(raw_candidate(1), &|_| None)
        .unwrap();
    let error = wait_union(&mut adapter, 1)
        .unwrap()
        .error
        .expect("the exhausted global budget rejects");
    assert!(error.contains("shared memory budget"), "{error}");
    assert_eq!(
        counters[0].load(std::sync::atomic::Ordering::Acquire),
        0,
        "no proportional carrier is retained before global admission"
    );
    assert_eq!(
        counters[1].load(std::sync::atomic::Ordering::Acquire),
        0,
        "no Polars builder is entered after admission fails"
    );
    assert!(adapter.union_inputs("union").unwrap().is_empty());
    adapter.shutdown();
    manager.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn union_refreshes_when_an_input_advances() {
    let (root, manager, api, worker, mut adapter) = setup().await;
    adapter
        .register_union_view("union", vec![api.source_id(), worker.source_id()])
        .unwrap();
    adapter
        .submit_union_candidate(candidate(1, &api, &worker, 1, 1), &|_| None)
        .unwrap();
    let completion = wait_union(&mut adapter, 1).expect("a completion");
    assert_eq!(completion.error, None);
    assert_eq!(union_texts(&mut adapter).len(), 12);
    // A late older record lands mid-stream in its own source.
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
    apply(&mut adapter, "view-a", 2);
    wait_applied(&mut adapter, 2).await;
    adapter
        .submit_union_candidate(candidate(2, &api, &worker, 2, 1), &|_| None)
        .unwrap();
    let completion = wait_union(&mut adapter, 2).expect("a completion");
    assert_eq!(completion.error, None);
    let texts = union_texts(&mut adapter);
    assert_eq!(texts.len(), 13);
    assert!(
        texts.iter().any(|text| text.contains("\"n\":99")),
        "the appended record is in the refreshed union"
    );
    adapter.shutdown();
    manager.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn union_rejects_input_source_set_drift_and_preserves_last_good() {
    let (root, manager, api, worker, mut adapter) = setup().await;
    adapter
        .register_union_view("union", vec![api.source_id(), worker.source_id()])
        .unwrap();
    adapter
        .submit_union_candidate(candidate(1, &api, &worker, 1, 1), &|_| None)
        .unwrap();
    assert_eq!(wait_union(&mut adapter, 1).unwrap().error, None);
    let accepted_inputs = adapter.union_inputs("union").unwrap();
    let accepted_rows = union_texts(&mut adapter);

    let third_path = root.path().join("third.log");
    fs::write(
        &third_path,
        "{\"ts\":\"2026-03-04T05:06:30Z\",\"svc\":\"third\",\"n\":30}\n",
    )
    .unwrap();
    let third = manager
        .start(source(SourceId::new(), &third_path, true))
        .await
        .unwrap();
    wait_runtime(&third, 1).await;
    adapter.register_source(third.clone()).unwrap();
    let constraints = QueryConstraints {
        text: Some(TextConstraint {
            literal: "ts".into(),
            case_insensitive: true,
        }),
        time_basis: lvu::TimeBasis::Event,
        ..QueryConstraints::default()
    };
    adapter
        .submit_source_change(
            QueryRequest {
                view_id: "view-a".into(),
                generation: 1,
                revision: 2,
                base_revision: 1,
                base_constraints: constraints.clone(),
                purpose: QueryPurpose::Search,
                constraints,
            },
            vec![api.source_id(), third.source_id()],
        )
        .unwrap();
    wait_applied(&mut adapter, 2).await;

    adapter
        .submit_union_candidate(candidate(2, &api, &worker, 2, 1), &|_| None)
        .unwrap();
    let rejected = wait_union(&mut adapter, 2).unwrap();
    let error = rejected.error.expect("moved source set must reject");
    assert!(error.contains("source set moved"), "{error}");
    assert_eq!(adapter.union_inputs("union").unwrap(), accepted_inputs);
    assert_eq!(union_texts(&mut adapter), accepted_rows);
    assert!(
        adapter
            .rows()
            .page("union", ViewportRequest { start: 0, len: 256 })
            .rows
            .iter()
            .all(|row| row.id.source_id != third.source_id().0.to_string())
    );
    for _ in 0..20 {
        adapter.drain_updates(64);
        assert_eq!(
            adapter.union_needs_refresh("union"),
            Some(false),
            "the identical rejected dependency state must not enqueue forever"
        );
        assert!(adapter.take_union_completions().is_empty());
    }

    let mut third_file = OpenOptions::new().append(true).open(&third_path).unwrap();
    writeln!(
        third_file,
        "{{\"ts\":\"2026-03-04T05:06:31Z\",\"svc\":\"third\",\"n\":31}}"
    )
    .unwrap();
    third_file.flush().unwrap();
    wait_runtime(&third, 2).await;
    let started = std::time::Instant::now();
    loop {
        adapter.drain_updates(64);
        if adapter.union_needs_refresh("union") == Some(true) {
            break;
        }
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "a real later source update must become retryable"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    adapter
        .submit_union_candidate(candidate(3, &api, &worker, 2, 1), &|_| None)
        .unwrap();
    assert!(wait_union(&mut adapter, 3).unwrap().error.is_some());
    assert_eq!(adapter.union_needs_refresh("union"), Some(false));
    adapter.shutdown();
    manager.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn raw_source_publication_linearizes_with_final_union_install() {
    let (root, manager, api, worker, mut adapter) = setup().await;
    adapter
        .register_view("raw-a", vec![api.source_id()])
        .unwrap();
    adapter
        .register_view("raw-b", vec![worker.source_id()])
        .unwrap();
    adapter
        .register_union_view("union", vec![api.source_id(), worker.source_id()])
        .unwrap();
    adapter
        .submit_union_candidate(raw_candidate(1), &|_| None)
        .unwrap();
    assert_eq!(wait_union(&mut adapter, 1).unwrap().error, None);
    assert_eq!(union_texts(&mut adapter).len(), 12);
    let original = lvu::RowId::new(api.source_id().0.to_string(), 0);
    assert!(adapter.rows().index_of_id("union", &original).is_some());

    let (checked_tx, checked_rx) = std::sync::mpsc::sync_channel(1);
    let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
    adapter
        .arm_union_publish_test_barrier(
            "union",
            UnionPublishTestBarrier {
                checked: checked_tx,
                release: release_rx,
            },
        )
        .unwrap();
    adapter
        .submit_union_candidate(raw_candidate(2), &|_| None)
        .unwrap();
    let started = std::time::Instant::now();
    loop {
        adapter.drain_updates(64);
        if checked_rx.try_recv().is_ok() {
            break;
        }
        assert!(started.elapsed() < Duration::from_secs(60));
        std::thread::sleep(Duration::from_millis(5));
    }
    let path = root.path().join("api.log");
    let writer = std::thread::spawn(move || {
        let mut file = OpenOptions::new().append(true).open(path).unwrap();
        writeln!(
            file,
            "{{\"ts\":\"2026-03-04T05:05:00Z\",\"svc\":\"api\",\"n\":99}}"
        )
        .unwrap();
        file.flush().unwrap();
    });
    writer.join().unwrap();
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(
        api.progress().records,
        6,
        "source progress publication must wait behind the final union guard"
    );
    release_tx.send(()).unwrap();
    assert_eq!(wait_union(&mut adapter, 2).unwrap().error, None);
    wait_runtime(&api, 7).await;
    assert_eq!(union_texts(&mut adapter).len(), 12);
    assert_eq!(adapter.union_needs_refresh("union"), Some(true));
    adapter
        .submit_union_candidate(raw_candidate(3), &|_| None)
        .unwrap();
    assert_eq!(wait_union(&mut adapter, 3).unwrap().error, None);
    assert_eq!(union_texts(&mut adapter).len(), 13);
    assert!(adapter.rows().index_of_id("union", &original).is_some());
    adapter.shutdown();
    manager.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn union_applies_its_own_text_search() {
    let (_root, manager, api, worker, mut adapter) = setup().await;
    adapter
        .register_union_view("union", vec![api.source_id(), worker.source_id()])
        .unwrap();
    adapter
        .submit_union_candidate(
            candidate_filtered(1, &api, &worker, 1, 1, "worker"),
            &|_| None,
        )
        .unwrap();
    let completion = wait_union(&mut adapter, 1).expect("a completion");
    assert_eq!(completion.error, None);
    let texts = union_texts(&mut adapter);
    assert_eq!(texts.len(), 6);
    assert!(
        texts.iter().all(|text| text.contains("\"worker\"")),
        "only worker rows survive the union search, in ts order"
    );
    let mut sorted = texts.clone();
    sorted.sort();
    assert_eq!(texts, sorted);
    adapter.shutdown();
    manager.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn union_grouping_collapses_exact_constituents_and_repeats_stably() {
    let (root, manager, api, worker, mut adapter) = setup().await;
    let mut file = OpenOptions::new()
        .append(true)
        .open(root.path().join("api.log"))
        .unwrap();
    writeln!(file, "  continuation after api event").unwrap();
    file.flush().unwrap();
    wait_runtime(&api, 7).await;
    adapter
        .register_view("raw-a", vec![api.source_id()])
        .unwrap();
    adapter
        .register_view("raw-b", vec![worker.source_id()])
        .unwrap();
    adapter
        .register_union_view("union", vec![api.source_id(), worker.source_id()])
        .unwrap();

    let (probe, counters) = phase_probe();
    adapter.arm_union_phase_test_probe("union", probe).unwrap();
    let mut grouped = raw_candidate(1);
    grouped.filter.grouping = Some(r"^\s+".into());
    adapter.submit_union_candidate(grouped, &|_| None).unwrap();
    assert_eq!(wait_union(&mut adapter, 1).unwrap().error, None);
    let rows = union_rows(&mut adapter);
    assert_eq!(rows.len(), 12, "one continuation is collapsed: {rows:#?}");
    assert_eq!(adapter.status("union").unwrap().matched_records, 13);
    assert_eq!(
        counters[2].load(std::sync::atomic::Ordering::Acquire),
        13,
        "every late-decoded physical row is indexed once"
    );
    assert_eq!(
        counters[3].load(std::sync::atomic::Ordering::Acquire),
        13,
        "grouping performs exactly one hash lookup per physical survivor"
    );
    let head = rows
        .iter()
        .find(|row| row.id.source_id == api.source_id().0.to_string() && row.id.sequence == 5)
        .expect("the event immediately before the continuation is the group head");
    assert_eq!(
        head.details
            .iter()
            .find(|(name, _)| name == "group_record_count")
            .map(|(_, value)| value.as_str()),
        Some("2")
    );
    let constituents = head
        .details
        .iter()
        .filter(|(name, _)| name.starts_with("group_line_") && name != "group_line_count")
        .map(|(_, value)| value.split_once(": ").unwrap().0.to_owned())
        .collect::<Vec<_>>();
    assert_eq!(
        constituents,
        [
            format!("{}:5", api.source_id().0),
            format!("{}:6", api.source_id().0),
        ]
    );
    let accepted = rows
        .iter()
        .map(|row| (row.id.clone(), row.text.clone(), row.details.clone()))
        .collect::<Vec<_>>();

    let mut repeated = raw_candidate(2);
    repeated.filter.grouping = Some(r"^\s+".into());
    adapter.submit_union_candidate(repeated, &|_| None).unwrap();
    assert_eq!(wait_union(&mut adapter, 2).unwrap().error, None);
    assert_eq!(
        union_rows(&mut adapter)
            .iter()
            .map(|row| (row.id.clone(), row.text.clone(), row.details.clone()))
            .collect::<Vec<_>>(),
        accepted
    );

    let mut invalid = raw_candidate(3);
    invalid.filter.grouping = Some(r"(?=lookaround)".into());
    adapter.submit_union_candidate(invalid, &|_| None).unwrap();
    let error = wait_union(&mut adapter, 3)
        .unwrap()
        .error
        .expect("invalid grouping must reject");
    assert!(error.contains("grouping"), "{error}");
    assert_eq!(
        union_rows(&mut adapter)
            .iter()
            .map(|row| (row.id.clone(), row.text.clone(), row.details.clone()))
            .collect::<Vec<_>>(),
        accepted
    );
    adapter.shutdown();
    manager.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn union_completion_reports_the_candidate_generation() {
    // The completion must carry the caller's submission generation (here 7),
    // not the adapter's private per-union worker ordinal (here 1): routing
    // on the wrong one discards valid results or attributes them wrongly.
    let (_root, manager, api, worker, mut adapter) = setup().await;
    adapter
        .register_union_view("union", vec![api.source_id(), worker.source_id()])
        .unwrap();
    adapter
        .submit_union_candidate(
            candidate_with_generation(1, 7, &api, &worker, 1, 1),
            &|_| None,
        )
        .unwrap();
    let completion = wait_union(&mut adapter, 1).expect("a completion");
    assert_eq!(completion.error, None);
    assert_eq!(completion.generation, 7);
    adapter.shutdown();
    manager.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn submitted_definition_revision_and_generation_are_authoritative() {
    let (_root, manager, api, worker, mut adapter) = setup().await;
    adapter
        .register_union_view("union", vec![api.source_id(), worker.source_id()])
        .unwrap();
    adapter
        .submit_union_candidate(candidate(1, &api, &worker, 1, 1), &|_| None)
        .unwrap();
    assert_eq!(wait_union(&mut adapter, 1).unwrap().error, None);
    let accepted = adapter.union_inputs("union").unwrap();
    let before = union_texts(&mut adapter);

    let mut stale_revision = candidate(2, &api, &worker, 1, 1);
    stale_revision.inputs[0].accepted_revision = 0;
    adapter
        .submit_union_candidate(stale_revision, &|_| None)
        .unwrap();
    let error = wait_union(&mut adapter, 2).unwrap().error.unwrap();
    assert!(error.contains("submitted revision 0"), "{error}");
    assert_eq!(adapter.union_inputs("union").unwrap(), accepted);
    assert_eq!(union_texts(&mut adapter), before);

    let mut stale_generation = candidate(3, &api, &worker, 1, 1);
    stale_generation.inputs[0].applied_generation = 0;
    adapter
        .submit_union_candidate(stale_generation, &|_| None)
        .unwrap();
    let error = wait_union(&mut adapter, 3).unwrap().error.unwrap();
    assert!(error.contains("generation 0"), "{error}");
    assert_eq!(adapter.union_inputs("union").unwrap(), accepted);
    assert_eq!(union_texts(&mut adapter), before);
    adapter.shutdown();
    manager.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn union_rejects_input_advanced_between_freeze_and_publish() {
    // Deterministic interleaving through the test barrier: the union freezes
    // both inputs, the test then advances input A WITHOUT a new query
    // revision (a live append plus the ordinary incremental refresh), and
    // only then releases the worker. Revision and generation fences still
    // match, so only the per-source high-watermark fence can catch this —
    // publishing would omit the newly accepted row as current.
    let (root, manager, api, worker, mut adapter) = setup().await;
    adapter
        .register_union_view("union", vec![api.source_id(), worker.source_id()])
        .unwrap();
    let (frozen_tx, frozen_rx) = std::sync::mpsc::sync_channel(1);
    let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
    adapter
        .arm_union_test_barrier(
            "union",
            UnionTestBarrier {
                frozen: frozen_tx,
                release: release_rx,
            },
        )
        .unwrap();
    adapter
        .submit_union_candidate(candidate(1, &api, &worker, 1, 1), &|_| None)
        .unwrap();
    let started = std::time::Instant::now();
    loop {
        adapter.drain_updates(64);
        if frozen_rx.try_recv().is_ok() {
            break;
        }
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "union never reached the barrier"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    // Live append; the ordinary incremental refresh publishes it at the SAME
    // accepted revision the union fenced on.
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
    let started = std::time::Instant::now();
    loop {
        adapter.drain_updates(64);
        let advanced: Option<u64> = adapter
            .status("view-a")
            .and_then(|status| {
                status
                    .high_watermarks
                    .iter()
                    .find_map(|(id, high)| (*id == api.source_id()).then_some(*high))
            })
            .flatten();
        if advanced == Some(6) {
            break;
        }
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "input membership never advanced (high={advanced:?})"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    release_tx.send(()).unwrap();
    let completion = wait_union(&mut adapter, 1).expect("a completion");
    let error = completion
        .error
        .expect("the stale candidate must be rejected, not published");
    assert!(
        error.contains("advanced"),
        "expected a high-watermark stale rejection, got: {error}"
    );
    // The prior union is preserved (nothing was ever published here, so the
    // view is still Raw), and a fresh candidate fences the new state fine.
    assert!(adapter.union_inputs("union").unwrap().is_empty());
    adapter
        .submit_union_candidate(candidate(2, &api, &worker, 1, 1), &|_| None)
        .unwrap();
    let completion = wait_union(&mut adapter, 2).expect("a completion");
    assert_eq!(completion.error, None);
    assert_eq!(union_texts(&mut adapter).len(), 13);
    adapter.shutdown();
    manager.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn union_published_view_renders_without_hanging() {
    use lvu::{App, SourceItem, ViewItem, theme::Theme};
    use ratatui::{Terminal, backend::TestBackend};
    let (_root, manager, api, worker, mut adapter) = setup().await;
    adapter
        .register_union_view("union", vec![api.source_id(), worker.source_id()])
        .unwrap();
    adapter
        .submit_union_candidate(candidate(1, &api, &worker, 1, 1), &|_| None)
        .unwrap();
    let completion = wait_union(&mut adapter, 1).expect("a completion");
    assert_eq!(completion.error, None);
    // Settle raw rows first (as every paged test does): render asserts
    // content, not delivery, and must not race the row cache.
    assert_eq!(union_texts(&mut adapter).len(), 12);
    // Full UI render over the published union membership: rows, sidebar,
    // status and health paths all read it generically. Runs on the calling
    // thread with TestBackend; any render-side loop over union structures
    // hangs here instead of freezing a live terminal with its last frame.
    let sources = vec![
        SourceItem {
            id: api.source_id().0.to_string(),
            name: "api".into(),
            health: String::new(),
        },
        SourceItem {
            id: worker.source_id().0.to_string(),
            name: "worker".into(),
            health: String::new(),
        },
    ];
    let views = vec![
        ViewItem {
            id: "view-a".into(),
            source_id: api.source_id().0.to_string(),
            name: "api view".into(),
        },
        ViewItem {
            id: "view-b".into(),
            source_id: worker.source_id().0.to_string(),
            name: "worker view".into(),
        },
        ViewItem {
            id: "union".into(),
            source_id: api.source_id().0.to_string(),
            name: "union view".into(),
        },
    ];
    let mut app = App::new(sources, views, true);
    app.select_view("union");
    app.sync_provider(&adapter, 24);
    let mut terminal = Terminal::new(TestBackend::new(150, 32)).unwrap();
    terminal
        .draw(|frame| {
            lvu::ui::render_with_theme(frame, &mut app, &adapter, Theme::TERMINAL, None);
        })
        .unwrap();
    let buffer = terminal.backend().buffer().clone();
    let text: String = (0..buffer.area.height)
        .map(|y| {
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("union view"), "{text}");
    assert!(text.contains("\"svc\":\"api\""), "{text}");
    assert!(text.contains("\"svc\":\"worker\""), "{text}");
    adapter.shutdown();
    manager.shutdown().await;
}
