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
use lvu_core::{Acquisition, ExactFieldConstraint, ExactScalar, SourceDefinition, SourceId};
use lvu_ingest::{RuntimeConfig, RuntimeState, SourceHandle, SourceManager};
use lvu_live::{LiveConfig, LiveRowProvider};
use lvu_query::CompilerHostConfig;
use lvu_view::{
    FrozenInputLimits, NativeViewAdapter, RemoteUnionCommitTransport, StoredUnionInput,
    UnionCandidateSpec, UnionFilterSpec, UnionPhaseTestProbe, UnionPublishTestBarrier,
    UnionTestBarrier, ViewConfig,
};
use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::Write,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize},
        mpsc::{Receiver, Sender, SyncSender, channel, sync_channel},
    },
    time::Duration,
};
use tempfile::TempDir;

#[derive(Clone, Copy)]
enum TestCommitVerdict {
    Commit,
    WrongDigest,
}

struct TestCommitTransport {
    requests: Mutex<Vec<(String, lvu_shared::union_commit::CommitRequest)>>,
    verdict: TestCommitVerdict,
    before_reply: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl TestCommitTransport {
    fn committing() -> Self {
        Self {
            requests: Mutex::new(Vec::new()),
            verdict: TestCommitVerdict::Commit,
            before_reply: None,
        }
    }
}

impl RemoteUnionCommitTransport for TestCommitTransport {
    fn submit(
        &self,
        expected_worker_session: &str,
        request: lvu_shared::union_commit::CommitRequest,
    ) -> Result<Receiver<Result<lvu_shared::union_commit::CommitReceipt, String>>, String> {
        self.requests
            .lock()
            .expect("requests poisoned")
            .push((expected_worker_session.to_owned(), request.clone()));
        let (tx, rx) = channel();
        if let Some(before_reply) = &self.before_reply {
            before_reply();
        }
        let outcome = lvu_shared::union_commit::CommitOutcome::Committed {
            current: request.frozen.clone(),
        };
        let mut receipt = lvu_shared::union_commit::CommitReceipt::answer(
            expected_worker_session,
            &request,
            outcome,
        );
        if matches!(self.verdict, TestCommitVerdict::WrongDigest) {
            receipt.digest[0] ^= 0xff;
        }
        tx.send(Ok(receipt)).expect("test receiver alive");
        Ok(rx)
    }
}

type CommitCall = (
    String,
    lvu_shared::union_commit::CommitRequest,
    Sender<Result<lvu_shared::union_commit::CommitReceipt, String>>,
);

struct RendezvousCommitTransport {
    calls: SyncSender<CommitCall>,
}

impl RemoteUnionCommitTransport for RendezvousCommitTransport {
    fn submit(
        &self,
        expected_worker_session: &str,
        request: lvu_shared::union_commit::CommitRequest,
    ) -> Result<Receiver<Result<lvu_shared::union_commit::CommitReceipt, String>>, String> {
        let (reply, result) = channel();
        self.calls
            .try_send((expected_worker_session.to_owned(), request, reply))
            .map_err(|_| "test commit rendezvous is full".to_owned())?;
        Ok(result)
    }
}

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
    view.compiler = Some(CompilerHostConfig {
        executable: "uv".into(),
        args: vec![
            "run".into(),
            "--project".into(),
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../python")
                .display()
                .to_string(),
            "--locked".into(),
            "python".into(),
            "-m".into(),
            "lvu_expr_helper".into(),
        ],
        request_limit: 64 * 1024,
        output_limit: 384 * 1024,
        stderr_limit: 32 * 1024,
        timeout: Duration::from_secs(10),
    });
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

fn apply_slash_enrichment(adapter: &mut NativeViewAdapter, view: &str, source: &str) {
    let constraints = QueryConstraints {
        enrichments: vec![lvu::EnrichmentDefinition {
            id: lvu::EnrichmentStageId(format!("{view}-capture")),
            source: source.into(),
            command: None,
        }],
        ..QueryConstraints::default()
    };
    adapter
        .submit(QueryRequest {
            view_id: view.into(),
            generation: 1,
            revision: 1,
            base_revision: 0,
            base_constraints: QueryConstraints::default(),
            purpose: QueryPurpose::Enrichment,
            constraints,
        })
        .unwrap();
}

fn apply_enrichment_after_filter(
    adapter: &mut NativeViewAdapter,
    view: &str,
    source: &str,
    revision: u64,
) {
    let base = QueryConstraints {
        text: Some(TextConstraint {
            literal: "ts".into(),
            case_insensitive: true,
        }),
        time_basis: lvu::TimeBasis::Event,
        ..QueryConstraints::default()
    };
    let mut constraints = base.clone();
    constraints.enrichments = vec![lvu::EnrichmentDefinition {
        id: lvu::EnrichmentStageId(format!("{view}-capture")),
        source: source.into(),
        command: None,
    }];
    adapter
        .submit(QueryRequest {
            view_id: view.into(),
            generation: revision,
            revision,
            base_revision: revision - 1,
            base_constraints: base,
            purpose: QueryPurpose::Enrichment,
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
    adapter.register_source(lvu_shared::AnySourceHandle::Local(api.clone())).unwrap();
    adapter.register_source(lvu_shared::AnySourceHandle::Local(worker.clone())).unwrap();
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
    setup_raw_bytes_with_budget(maximum_index_bytes, b"api row\n", b"worker row\n").await
}

async fn setup_raw_bytes_with_budget(
    maximum_index_bytes: u64,
    api_bytes: &[u8],
    worker_bytes: &[u8],
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
    fs::write(&api_path, api_bytes).unwrap();
    fs::write(&worker_path, worker_bytes).unwrap();
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
    adapter.register_source(lvu_shared::AnySourceHandle::Local(api.clone())).unwrap();
    adapter.register_source(lvu_shared::AnySourceHandle::Local(worker.clone())).unwrap();
    adapter
        .register_view("raw-a", vec![api.source_id()])
        .unwrap();
    adapter
        .register_view("raw-b", vec![worker.source_id()])
        .unwrap();
    (root, manager, api, worker, adapter)
}

async fn setup_remote_raw() -> (
    TempDir,
    SourceManager,
    lvu_shared::RemoteSourceHandle,
    NativeViewAdapter,
) {
    let root = TempDir::new().unwrap();
    let path = root.path().join("remote.log");
    fs::write(&path, b"remote-one\nremote-two\n").unwrap();
    let capture_root = root.path().join("capture");
    let manager = SourceManager::new(&capture_root, runtime_config()).unwrap();
    let source_id = SourceId::new();
    let local = manager
        .start(source(source_id, &path, false))
        .await
        .unwrap();
    wait_runtime(&local, 2).await;
    let progress = local.progress();
    assert_eq!(progress.records, 2, "remote fixture captured both rows");
    let journal = capture_root
        .join(source_id.0.to_string())
        .join("capture.journal");
    let remote = lvu_shared::RemoteSourceHandle::new(
        source_id,
        &journal,
        "worker-session-a".into(),
        progress,
        lvu_shared::RemoteConfig::default(),
    )
    .expect("bind remote source");
    let (live, view) = configs(&root);
    let raw = Arc::new(LiveRowProvider::new(live).unwrap());
    let adapter = NativeViewAdapter::new(raw, view).unwrap();
    adapter
        .register_source(lvu_shared::AnySourceHandle::Remote(remote.clone()))
        .unwrap();
    adapter
        .register_view("remote-raw-a", vec![source_id])
        .unwrap();
    adapter
        .register_view("remote-raw-b", vec![source_id])
        .unwrap();
    adapter
        .register_union_view("union", vec![source_id])
        .unwrap();
    (root, manager, remote, adapter)
}

fn remote_raw_candidate(revision: u64) -> UnionCandidateSpec {
    UnionCandidateSpec {
        union_view_id: "union".into(),
        union_revision: revision,
        generation: revision,
        inputs: vec![
            StoredUnionInput {
                view_id: "remote-raw-a".into(),
                accepted_revision: 0,
                applied_generation: 0,
            },
            StoredUnionInput {
                view_id: "remote-raw-b".into(),
                accepted_revision: 0,
                applied_generation: 0,
            },
        ],
        filter: UnionFilterSpec::default(),
        color_rules: Vec::new(),
    }
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

fn wait_commit_call(adapter: &mut NativeViewAdapter, calls: &Receiver<CommitCall>) -> CommitCall {
    let started = std::time::Instant::now();
    loop {
        adapter.drain_updates(64);
        if let Ok(call) = calls.try_recv() {
            return call;
        }
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "remote union did not submit its commit request"
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
        color_rules: Vec::new(),
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
        color_rules: Vec::new(),
    }
}

fn duplicate_projection_candidate(revision: u64, filter: UnionFilterSpec) -> UnionCandidateSpec {
    UnionCandidateSpec {
        union_view_id: "union".into(),
        union_revision: revision,
        generation: revision,
        inputs: vec![
            StoredUnionInput {
                view_id: "view-loser".into(),
                accepted_revision: 1,
                applied_generation: 1,
            },
            StoredUnionInput {
                view_id: "view-winner".into(),
                accepted_revision: 1,
                applied_generation: 1,
            },
        ],
        filter,
        color_rules: Vec::new(),
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
async fn remote_raw_union_installs_only_the_exact_receipted_candidate() {
    let (_root, manager, remote, mut adapter) = setup_remote_raw().await;
    let committed = Arc::new(TestCommitTransport::committing());
    adapter
        .set_remote_union_commit_transport("window-test".into(), committed.clone())
        .unwrap();
    adapter
        .submit_union_candidate(remote_raw_candidate(1), &|_| None)
        .unwrap();
    let completion = wait_union(&mut adapter, 1).expect("remote completion");
    assert_eq!(completion.error, None);
    assert_eq!(union_texts(&mut adapter), ["remote-one", "remote-two"]);
    {
        let requests = committed.requests.lock().expect("requests poisoned");
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].0, "worker-session-a");
        assert_eq!(requests[0].1.window_id, "window-test");
        assert_eq!(requests[0].1.frozen.len(), 1);
        assert_eq!(
            requests[0].1.frozen[0].source_id,
            remote.source_id().0.to_string()
        );
    }

    let rejected = Arc::new(TestCommitTransport {
        requests: Mutex::new(Vec::new()),
        verdict: TestCommitVerdict::WrongDigest,
        before_reply: None,
    });
    adapter
        .set_remote_union_commit_transport("window-test".into(), rejected)
        .unwrap();
    adapter
        .submit_union_candidate(remote_raw_candidate(2), &|_| None)
        .unwrap();
    let completion = wait_union(&mut adapter, 2).expect("rejected completion");
    assert!(
        completion
            .error
            .as_deref()
            .is_some_and(|error| error.contains("does not match the pending candidate")),
        "unexpected receipt verdict: {:?}",
        completion.error
    );
    assert_eq!(
        union_texts(&mut adapter),
        ["remote-one", "remote-two"],
        "a mismatched receipt preserves the last-good membership"
    );
    let mut after_install = remote.progress();
    after_install.records = after_install.records.saturating_add(1);
    after_install.high_watermark = Some(lvu_core::RecordId {
        source_id: remote.source_id(),
        sequence: 2,
    });
    assert!(remote.update_progress("worker-session-a", after_install));
    assert_eq!(adapter.union_needs_refresh("union"), Some(true));
    adapter.shutdown();
    manager.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_progress_before_receipt_installs_then_requests_refresh() {
    let (_root, manager, remote, mut adapter) = setup_remote_raw().await;
    let mut advanced = remote.progress();
    advanced.records = advanced.records.saturating_add(1);
    advanced.high_watermark = Some(lvu_core::RecordId {
        source_id: remote.source_id(),
        sequence: 2,
    });
    let remote_for_reply = remote.clone();
    let transport = Arc::new(TestCommitTransport {
        requests: Mutex::new(Vec::new()),
        verdict: TestCommitVerdict::Commit,
        before_reply: Some(Arc::new(move || {
            assert!(remote_for_reply.update_progress("worker-session-a", advanced.clone()));
        })),
    });
    adapter
        .set_remote_union_commit_transport("window-test".into(), transport)
        .unwrap();
    adapter
        .submit_union_candidate(remote_raw_candidate(1), &|_| None)
        .unwrap();
    let completion = wait_union(&mut adapter, 1).expect("remote completion");
    assert_eq!(completion.error, None);
    assert_eq!(union_texts(&mut adapter), ["remote-one", "remote-two"]);
    assert_eq!(
        adapter.union_needs_refresh("union"),
        Some(true),
        "progress observed after worker linearization is a pending refresh, not a rejection"
    );
    adapter.shutdown();
    manager.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn superseded_remote_waiter_cannot_clear_the_new_candidate() {
    let (_root, manager, _remote, mut adapter) = setup_remote_raw().await;
    let (calls_tx, calls_rx) = sync_channel(4);
    adapter
        .set_remote_union_commit_transport(
            "window-test".into(),
            Arc::new(RendezvousCommitTransport { calls: calls_tx }),
        )
        .unwrap();
    adapter
        .submit_union_candidate(remote_raw_candidate(1), &|_| None)
        .unwrap();
    let (_session_one, request_one, reply_one) = wait_commit_call(&mut adapter, &calls_rx);

    adapter
        .submit_union_candidate(remote_raw_candidate(2), &|_| None)
        .unwrap();
    let (session_two, request_two, reply_two) = wait_commit_call(&mut adapter, &calls_rx);
    let receipt_two = lvu_shared::union_commit::CommitReceipt::answer(
        &session_two,
        &request_two,
        lvu_shared::union_commit::CommitOutcome::Committed {
            current: request_two.frozen.clone(),
        },
    );
    reply_two.send(Ok(receipt_two)).expect("new waiter alive");
    let completion = wait_union(&mut adapter, 2).expect("new completion");
    assert_eq!(completion.error, None);
    assert_eq!(union_texts(&mut adapter), ["remote-one", "remote-two"]);

    // The old waiter is generation-scoped. Whether it has already observed
    // cancellation or observes it on this wake, it cannot clear or replace
    // the generation-2 publication.
    let receipt_one = lvu_shared::union_commit::CommitReceipt::answer(
        "worker-session-a",
        &request_one,
        lvu_shared::union_commit::CommitOutcome::Committed {
            current: request_one.frozen.clone(),
        },
    );
    let _ = reply_one.send(Ok(receipt_one));
    std::thread::sleep(Duration::from_millis(150));
    adapter.drain_updates(64);
    assert_eq!(union_texts(&mut adapter), ["remote-one", "remote-two"]);
    assert_eq!(
        adapter.union_inputs("union"),
        Some(remote_raw_candidate(2).inputs)
    );
    adapter.shutdown();
    manager.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_receipt_cannot_cross_a_filtered_input_publication() {
    let (_root, manager, _remote, mut adapter) = setup_remote_raw().await;
    let (calls_tx, calls_rx) = sync_channel(2);
    adapter
        .set_remote_union_commit_transport(
            "window-test".into(),
            Arc::new(RendezvousCommitTransport { calls: calls_tx }),
        )
        .unwrap();
    adapter
        .submit_union_candidate(remote_raw_candidate(1), &|_| None)
        .unwrap();
    let (session, request, reply) = wait_commit_call(&mut adapter, &calls_rx);

    let constraints = QueryConstraints {
        text: Some(TextConstraint {
            literal: "remote".into(),
            case_insensitive: true,
        }),
        ..QueryConstraints::default()
    };
    adapter
        .submit(QueryRequest {
            view_id: "remote-raw-a".into(),
            generation: 1,
            revision: 1,
            base_revision: 0,
            base_constraints: QueryConstraints::default(),
            purpose: QueryPurpose::Search,
            constraints,
        })
        .unwrap();
    wait_applied(&mut adapter, 1).await;
    let receipt = lvu_shared::union_commit::CommitReceipt::answer(
        &session,
        &request,
        lvu_shared::union_commit::CommitOutcome::Committed {
            current: request.frozen.clone(),
        },
    );
    reply.send(Ok(receipt)).expect("union waiter alive");
    let completion = wait_union(&mut adapter, 1).expect("union completion");
    assert!(
        completion
            .error
            .as_deref()
            .is_some_and(|error| error.contains("moved during the merge")),
        "unexpected final-fence verdict: {:?}",
        completion.error
    );
    assert_eq!(adapter.union_inputs("union"), Some(Vec::new()));
    adapter.shutdown();
    manager.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_receipt_cannot_cross_a_worker_session_replacement() {
    let (root, manager, remote, mut adapter) = setup_remote_raw().await;
    let (calls_tx, calls_rx) = sync_channel(2);
    adapter
        .set_remote_union_commit_transport(
            "window-test".into(),
            Arc::new(RendezvousCommitTransport { calls: calls_tx }),
        )
        .unwrap();
    adapter
        .submit_union_candidate(remote_raw_candidate(1), &|_| None)
        .unwrap();
    let (session, request, reply) = wait_commit_call(&mut adapter, &calls_rx);

    let replacement = lvu_shared::RemoteSourceHandle::new(
        remote.source_id(),
        &root
            .path()
            .join("capture")
            .join(remote.source_id().0.to_string())
            .join("capture.journal"),
        "worker-session-b".into(),
        remote.progress(),
        lvu_shared::RemoteConfig::default(),
    )
    .expect("bind replacement session");
    adapter
        .register_source(lvu_shared::AnySourceHandle::Remote(replacement))
        .unwrap();
    let receipt = lvu_shared::union_commit::CommitReceipt::answer(
        &session,
        &request,
        lvu_shared::union_commit::CommitOutcome::Committed {
            current: request.frozen.clone(),
        },
    );
    reply.send(Ok(receipt)).expect("union waiter alive");
    let completion = wait_union(&mut adapter, 1).expect("union completion");
    assert!(
        completion
            .error
            .as_deref()
            .is_some_and(|error| error.contains("changed worker session")),
        "unexpected session-fence verdict: {:?}",
        completion.error
    );
    assert_eq!(adapter.union_inputs("union"), Some(Vec::new()));
    adapter.shutdown();
    manager.shutdown().await;
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
                    color_rules: Vec::new(),
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
async fn malformed_utf8_expansion_is_charged_before_carrier_retention() {
    let mut malformed = vec![0xff; 4_096];
    malformed.push(b'\n');
    // The obsolete charge (raw bytes twice plus fixed slack) fits in 12 KiB;
    // retaining exact bytes plus the three-byte replacement string does not.
    // This makes the phase probe distinguish pre-retention admission from a
    // later workspace rejection.
    let (_root, manager, api, worker, mut adapter) =
        setup_raw_bytes_with_budget(12 * 1_024, &malformed, b"worker row\n").await;
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
        .expect("lossy UTF-8 carrier expansion must exceed the shared budget");
    assert!(error.contains("shared memory budget"), "{error}");
    assert_eq!(
        counters[0].load(std::sync::atomic::Ordering::Acquire),
        0,
        "the expanding lossy string is admitted before either carrier is retained"
    );
    assert_eq!(
        counters[1].load(std::sync::atomic::Ordering::Acquire),
        0,
        "Polars is never entered after carrier admission fails"
    );
    adapter.shutdown();
    manager.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn overlapping_colour_rule_vectors_are_reserved_before_native_evaluation() {
    let api_rows = (0..20)
        .map(|index| format!("api row {index}\n"))
        .collect::<String>();
    let worker_rows = (0..20)
        .map(|index| format!("worker row {index}\n"))
        .collect::<String>();
    // Forty one-rule rows fit this budget; sixteen complete native match
    // vectors alone require 40 * 16 * 96 bytes, before the selected-ID set
    // and final map. The reservation must reject before the Polars boundary.
    let (_root, manager, api, worker, mut adapter) =
        setup_raw_bytes_with_budget(48 * 1_024, api_rows.as_bytes(), worker_rows.as_bytes()).await;
    wait_runtime(&api, 20).await;
    wait_runtime(&worker, 20).await;
    adapter
        .register_union_view("union", vec![api.source_id(), worker.source_id()])
        .unwrap();
    let (probe, counters) = phase_probe();
    adapter.arm_union_phase_test_probe("union", probe).unwrap();
    let mut candidate = raw_candidate(1);
    candidate.color_rules = (0..lvu::MAX_COLOR_RULES)
        .map(|_| lvu::ColorRule {
            predicate: "row".into(),
            color: lvu::RuleColor::Red,
            column: None,
            value: None,
        })
        .collect();
    adapter
        .submit_union_candidate(candidate, &|_| None)
        .unwrap();
    let error = wait_union(&mut adapter, 1)
        .unwrap()
        .error
        .expect("the rule-multiplied workspace must reject");
    assert!(error.contains("memory budget"), "{error}");
    assert_eq!(
        counters[0].load(std::sync::atomic::Ordering::Acquire),
        40,
        "the bounded inputs were visited before workspace admission"
    );
    assert_eq!(
        counters[1].load(std::sync::atomic::Ordering::Acquire),
        0,
        "native merge/colour evaluation must not start before reservation"
    );

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
async fn transient_refresh_failure_retries_same_dependency_after_backoff() {
    let (root, manager, api, worker, mut adapter) = setup_raw_with_budget(8 * 1024 * 1024).await;
    adapter
        .register_union_view("union", vec![api.source_id(), worker.source_id()])
        .unwrap();
    adapter
        .submit_union_candidate(raw_candidate(1), &|_| None)
        .unwrap();
    assert_eq!(wait_union(&mut adapter, 1).unwrap().error, None);
    let original = lvu::RowId::new(api.source_id().0.to_string(), 0);

    let mut file = OpenOptions::new()
        .append(true)
        .open(root.path().join("api.log"))
        .unwrap();
    writeln!(file, "unique appended row").unwrap();
    file.flush().unwrap();
    wait_runtime(&api, 2).await;
    assert_eq!(adapter.union_needs_refresh("union"), Some(true));

    adapter
        .arm_union_transient_test_failure("union", "injected transient resource failure")
        .unwrap();
    adapter
        .submit_union_candidate(raw_candidate(2), &|_| None)
        .unwrap();
    let failed = wait_union(&mut adapter, 2).unwrap();
    assert_eq!(
        failed.error.as_deref(),
        Some("injected transient resource failure")
    );
    assert_eq!(
        adapter.union_needs_refresh("union"),
        Some(false),
        "the retry deadline prevents an every-tick resubmit"
    );

    let started = std::time::Instant::now();
    loop {
        if adapter.union_needs_refresh("union") == Some(true) {
            break;
        }
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "the unchanged dependency must become retryable after backoff"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    adapter
        .submit_union_candidate(raw_candidate(3), &|_| None)
        .unwrap();
    assert_eq!(wait_union(&mut adapter, 3).unwrap().error, None);
    let rows = union_rows(&mut adapter);
    assert_eq!(rows.len(), 3);
    assert!(rows.iter().any(|row| row.id == original));
    assert!(
        rows.iter()
            .any(|row| row.text.contains("unique appended row"))
    );
    adapter.shutdown();
    manager.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_first_union_retries_after_inputs_publish_accepted_enrichments() {
    let api_rows = b"{\"request_key\":\"raw-api-other\",\"api_id\":9007199254740992,\"msg\":\"api-other\"}\n{\"request_key\":\"raw-api-selected\",\"api_id\":9007199254740993,\"msg\":\"api-selected\"}\n";
    let worker_rows = b"{\"request_key\":\"raw-worker-other\",\"worker_id\":9007199254740994,\"msg\":\"worker-other\"}\n{\"request_key\":\"raw-worker-selected\",\"worker_id\":9007199254740993,\"msg\":\"worker-selected\"}\n";
    let (root, manager, api, worker, mut adapter) =
        setup_raw_bytes_with_budget(8 * 1024 * 1024, api_rows, worker_rows).await;
    adapter
        .register_union_view("union", vec![api.source_id(), worker.source_id()])
        .unwrap();
    let exact = ExactFieldConstraint::new(
        "request_key",
        ExactScalar::UnsignedInteger(9_007_199_254_740_993),
    )
    .unwrap();
    let mut first = raw_candidate(1);
    first.filter.exact_key = Some(exact.clone());
    adapter.submit_union_candidate(first, &|_| None).unwrap();
    let error = wait_union(&mut adapter, 1)
        .unwrap()
        .error
        .expect("the temporary raw String column cannot satisfy a UInt64 key");
    assert!(
        error.contains("incompatible with unsigned integer"),
        "{error}"
    );

    apply_slash_enrichment(
        &mut adapter,
        "raw-a",
        "request_key = pl.col('api_id').cast(pl.UInt64)",
    );
    wait_applied(&mut adapter, 1).await;
    apply_slash_enrichment(
        &mut adapter,
        "raw-b",
        "request_key = pl.col('worker_id').cast(pl.UInt64)",
    );
    wait_applied(&mut adapter, 1).await;
    assert_eq!(
        adapter.union_needs_refresh("union"),
        Some(true),
        "accepted input publication invalidates the failed startup attempt"
    );

    let mut retry = raw_candidate(2);
    for input in &mut retry.inputs {
        input.accepted_revision = 1;
        input.applied_generation = 1;
    }
    retry.filter.exact_key = Some(exact);
    adapter.submit_union_candidate(retry, &|_| None).unwrap();
    assert_eq!(wait_union(&mut adapter, 2).unwrap().error, None);
    let rows = union_texts(&mut adapter);
    assert_eq!(rows.len(), 2, "only the exact UInt64 matches publish");
    assert!(rows.iter().any(|row| row.contains("api-selected")));
    assert!(rows.iter().any(|row| row.contains("worker-selected")));
    assert!(rows.iter().all(|row| !row.contains("other")));

    adapter.shutdown();
    manager.shutdown().await;
    drop(root);
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
    adapter.register_source(lvu_shared::AnySourceHandle::Local(third.clone())).unwrap();
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
async fn native_advanced_filters_post_dedup_and_preserve_last_good() {
    let (root, manager, api, _worker, mut adapter) = setup().await;
    adapter
        .register_view("view-loser", vec![api.source_id()])
        .unwrap();
    adapter
        .register_view("view-winner", vec![api.source_id()])
        .unwrap();
    apply_slash_enrichment(&mut adapter, "view-loser", r#"/svc\":\"(?P<choice>api)/"#);
    wait_applied(&mut adapter, 1).await;
    apply_slash_enrichment(&mut adapter, "view-winner", r#"/\"n\":(?P<choice>\d+)/"#);
    wait_applied(&mut adapter, 1).await;
    adapter
        .register_union_view("union", vec![api.source_id()])
        .unwrap();

    // Both inputs contain the same stable identities. First-input dedup keeps
    // choice="api"; filtering inputs separately would incorrectly retain the
    // second projection's choice="0" row.
    adapter
        .submit_union_candidate(
            duplicate_projection_candidate(
                1,
                UnionFilterSpec {
                    advanced_polars: Some("pl.col('choice') == '0'".into()),
                    ..UnionFilterSpec::default()
                },
            ),
            &|_| None,
        )
        .unwrap();
    assert_eq!(wait_union(&mut adapter, 1).unwrap().error, None);
    assert!(union_rows(&mut adapter).is_empty());

    adapter
        .submit_union_candidate(
            duplicate_projection_candidate(
                2,
                UnionFilterSpec {
                    advanced_polars: Some("pl.col('choice') == 'api'".into()),
                    ..UnionFilterSpec::default()
                },
            ),
            &|_| None,
        )
        .unwrap();
    assert_eq!(wait_union(&mut adapter, 2).unwrap().error, None);
    let accepted = union_rows(&mut adapter);
    assert_eq!(accepted.len(), 6);

    for (revision, invalid) in [
        (3, "pl.col('unknown_union_column') == 1"),
        (4, "pl.col('choice')"),
    ] {
        adapter
            .submit_union_candidate(
                duplicate_projection_candidate(
                    revision,
                    UnionFilterSpec {
                        advanced_polars: Some(invalid.into()),
                        ..UnionFilterSpec::default()
                    },
                ),
                &|_| None,
            )
            .unwrap();
        assert!(wait_union(&mut adapter, revision).unwrap().error.is_some());
        assert_eq!(union_rows(&mut adapter), accepted);
    }

    let exact = ExactFieldConstraint::new("choice", ExactScalar::string("api").unwrap()).unwrap();
    let combined = UnionFilterSpec {
        advanced_polars: Some("pl.col('n') >= 3".into()),
        exact_key: Some(exact),
        ..UnionFilterSpec::default()
    };
    adapter
        .submit_union_candidate(duplicate_projection_candidate(5, combined.clone()), &|_| {
            None
        })
        .unwrap();
    assert_eq!(wait_union(&mut adapter, 5).unwrap().error, None);
    assert_eq!(union_rows(&mut adapter).len(), 3);
    let compiled_before_refresh = adapter.compiler_calls();

    let mut file = OpenOptions::new()
        .append(true)
        .open(root.path().join("api.log"))
        .unwrap();
    writeln!(
        file,
        "{{\"ts\":\"2026-03-04T05:06:20Z\",\"svc\":\"api\",\"n\":6}}"
    )
    .unwrap();
    file.flush().unwrap();
    wait_runtime(&api, 7).await;
    let started = std::time::Instant::now();
    loop {
        adapter.drain_updates(64);
        let ready = ["view-loser", "view-winner"].into_iter().all(|view_id| {
            adapter.status(view_id).is_some_and(|status| {
                status
                    .high_watermarks
                    .iter()
                    .any(|(source, high)| *source == api.source_id() && *high == Some(6))
            })
        });
        if ready {
            break;
        }
        assert!(started.elapsed() < Duration::from_secs(20));
        std::thread::sleep(Duration::from_millis(5));
    }
    adapter
        .submit_union_candidate(duplicate_projection_candidate(6, combined), &|_| None)
        .unwrap();
    assert_eq!(wait_union(&mut adapter, 6).unwrap().error, None);
    assert_eq!(union_rows(&mut adapter).len(), 4);
    assert_eq!(
        adapter.compiler_calls(),
        compiled_before_refresh,
        "an unchanged published Advanced definition reuses its compiler cache"
    );

    // Configured grouping uses native flags over the winning typed column.
    // Every non-null value is an event start under Filter, whereas equal
    // `choice` values form one Run; a raw/legacy continuation evaluator could
    // not produce both distinct outcomes from the same records.
    let mut starts = duplicate_projection_candidate(7, UnionFilterSpec::default());
    starts.filter.grouping = Some(lvu::grouping::filter_rule("choice"));
    adapter.submit_union_candidate(starts, &|_| None).unwrap();
    assert_eq!(wait_union(&mut adapter, 7).unwrap().error, None);
    assert_eq!(union_rows(&mut adapter).len(), 7);

    let mut run = duplicate_projection_candidate(8, UnionFilterSpec::default());
    run.filter.grouping = Some(lvu::grouping::run_rule("choice"));
    adapter.submit_union_candidate(run, &|_| None).unwrap();
    assert_eq!(wait_union(&mut adapter, 8).unwrap().error, None);
    let grouped = union_rows(&mut adapter);
    assert_eq!(grouped.len(), 1);
    let constituent_ids = grouped[0]
        .details
        .iter()
        .filter(|(name, _)| name.starts_with("group_line_") && name != "group_line_count")
        .map(|(_, value)| {
            value
                .rsplit_once(" [")
                .and_then(|(_, id)| id.strip_suffix(']'))
                .expect("configured group details retain the stable ID")
                .to_owned()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        constituent_ids,
        (0..7)
            .map(|sequence| format!("{}:{sequence}", api.source_id().0))
            .collect::<Vec<_>>()
    );
    adapter.shutdown();
    manager.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn union_derived_inventory_preserves_winning_input_authority() {
    let (root, manager, api, _worker, mut adapter) = setup().await;
    let mut file = OpenOptions::new()
        .append(true)
        .open(root.path().join("api.log"))
        .unwrap();
    writeln!(
        file,
        "{{\"ts\":\"2026-03-04T05:06:20Z\",\"svc\":\"api\",\"choice\":\"raw-name\"}}"
    )
    .unwrap();
    file.flush().unwrap();
    wait_runtime(&api, 7).await;

    adapter
        .register_view("view-loser", vec![api.source_id()])
        .unwrap();
    adapter
        .register_view("view-winner", vec![api.source_id()])
        .unwrap();
    apply_slash_enrichment(&mut adapter, "view-loser", r#"/svc\":\"(?P<choice>api)/"#);
    wait_applied(&mut adapter, 1).await;
    apply(&mut adapter, "view-winner", 1);
    wait_applied(&mut adapter, 1).await;
    adapter
        .register_union_view("union", vec![api.source_id()])
        .unwrap();
    let mut candidate = duplicate_projection_candidate(1, UnionFilterSpec::default());
    candidate.color_rules = vec![lvu::ColorRule::column_rule(
        "choice".into(),
        "raw-name".into(),
        lvu::RuleColor::Red,
    )];
    adapter
        .submit_union_candidate(candidate, &|_| None)
        .unwrap();
    assert_eq!(wait_union(&mut adapter, 1).unwrap().error, None);

    let frozen = adapter
        .freeze_input("union", FrozenInputLimits::default())
        .unwrap();
    assert_eq!(frozen.summary().view_id, "union");
    assert_eq!(frozen.summary().applied_revision, 1);
    assert_eq!(frozen.summary().applied_generation, 1);
    assert_eq!(
        frozen.summary().accepted_enrichment_outputs,
        vec!["choice"],
        "an accepted winning-input output remains available to Fields"
    );
    let rows = std::thread::spawn(move || {
        let mut rows = Vec::new();
        frozen
            .visit_precise(&AtomicBool::new(false), |batch| {
                rows.extend(batch.rows);
                Ok(())
            })
            .unwrap();
        rows
    })
    .join()
    .unwrap();
    let overlapping_raw = rows
        .iter()
        .find(|row| {
            row.record.record_id.sequence == 6
                && String::from_utf8_lossy(&row.record.bytes).contains("\"choice\":\"raw-name\"")
        })
        .expect("the overlapping record retains its original raw namesake");
    assert_eq!(
        overlapping_raw.fields.get("choice"),
        Some(&serde_json::json!("api")),
        "the first-kept input's accepted value is authoritative for the winning identity"
    );
    assert_eq!(
        overlapping_raw
            .field_types
            .get("choice")
            .map(String::as_str),
        Some("String")
    );
    assert!(
        union_rows(&mut adapter).iter().all(|row| row
            .details
            .iter()
            .all(|(name, _)| name != lvu_view::COLOR_RULE_DETAIL)),
        "a raw namesake must not acquire union classifier authority"
    );

    adapter.shutdown();
    manager.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn heterogeneous_union_keeps_winning_row_derived_authority_only() {
    const KEY: u64 = 9_007_199_254_740_993;
    let (root, manager, api, worker, mut adapter) = setup().await;
    let mut worker_file = OpenOptions::new()
        .append(true)
        .open(root.path().join("worker.log"))
        .unwrap();
    writeln!(
        worker_file,
        "{{\"ts\":\"2026-03-04T05:06:13Z\",\"request_key\":\"{KEY}\",\"svc\":\"worker\"}}"
    )
    .unwrap();
    worker_file.flush().unwrap();
    wait_runtime(&worker, 7).await;
    apply_enrichment_after_filter(
        &mut adapter,
        "view-a",
        &format!("request_key = pl.lit({KEY}, dtype=pl.UInt64)"),
        2,
    );
    wait_applied(&mut adapter, 2).await;
    let started = std::time::Instant::now();
    loop {
        adapter.drain_updates(64);
        if adapter.status("view-b").is_some_and(|status| {
            status
                .high_watermarks
                .iter()
                .any(|(source, high)| *source == worker.source_id() && *high == Some(6))
        }) {
            break;
        }
        assert!(started.elapsed() < Duration::from_secs(20));
        std::thread::sleep(Duration::from_millis(5));
    }
    adapter
        .register_union_view("union", vec![api.source_id(), worker.source_id()])
        .unwrap();
    let mut union_candidate = candidate(1, &api, &worker, 2, 1);
    union_candidate.inputs[0].applied_generation = 2;
    union_candidate.color_rules = vec![lvu::ColorRule::column_rule(
        "request_key".into(),
        KEY.to_string(),
        lvu::RuleColor::Red,
    )];
    adapter
        .submit_union_candidate(union_candidate, &|_| None)
        .unwrap();
    assert_eq!(wait_union(&mut adapter, 1).unwrap().error, None);

    let rows = union_rows(&mut adapter);
    assert_eq!(rows.len(), 13);
    for row in &rows {
        let painted = row
            .details
            .iter()
            .any(|(name, value)| name == lvu_view::COLOR_RULE_DETAIL && value == "1");
        let ready = row
            .details
            .iter()
            .any(|(name, value)| name == "derived_ready.request_key" && value == &KEY.to_string());
        if row.id.source_id == api.source_id().0.to_string() {
            assert!(painted && ready);
            assert!(
                row.fields
                    .contains(&("request_key".into(), KEY.to_string()))
            );
        } else {
            assert!(!painted && !ready, "raw/missing B rows have no authority");
            assert!(row.fields.contains(&("request_key".into(), "null".into())));
        }
    }
    assert!(
        rows.iter()
            .any(|row| row.id.source_id == worker.source_id().0.to_string()
                && row.text.contains(&format!("\"request_key\":\"{KEY}\""))),
        "the raw namesake remains intact as original context"
    );

    let frozen = adapter
        .freeze_input("union", FrozenInputLimits::default())
        .unwrap();
    assert_eq!(
        frozen.summary().accepted_enrichment_outputs,
        vec!["request_key"]
    );
    let precise = std::thread::spawn(move || {
        let mut rows = Vec::new();
        frozen
            .visit_precise(&AtomicBool::new(false), |batch| {
                rows.extend(batch.rows);
                Ok(())
            })
            .unwrap();
        rows
    })
    .join()
    .unwrap();
    assert!(precise.iter().any(|row| {
        row.record.record_id.source_id == api.source_id()
            && row.fields.get("request_key")
                == Some(&serde_json::json!({"kind": "u64", "decimal": KEY.to_string()}))
            && row.field_types.get("request_key").map(String::as_str) == Some("UInt64")
    }));
    let raw_namesake = precise
        .iter()
        .find(|row| {
            row.record.record_id.source_id == worker.source_id()
                && String::from_utf8_lossy(&row.record.bytes)
                    .contains(&format!("\"request_key\":\"{KEY}\""))
        })
        .expect("raw B namesake survives in original bytes");
    assert!(!raw_namesake.fields.contains_key("request_key"));
    assert_eq!(
        raw_namesake
            .omitted_fields
            .get("request_key")
            .map(String::as_str),
        Some("accepted output unavailable for this union row")
    );

    let mut shared_key = candidate(2, &api, &worker, 2, 1);
    shared_key.inputs[0].applied_generation = 2;
    shared_key.filter.exact_key =
        Some(ExactFieldConstraint::new("request_key", ExactScalar::UnsignedInteger(KEY)).unwrap());
    adapter
        .submit_union_candidate(shared_key, &|_| None)
        .unwrap();
    assert_eq!(wait_union(&mut adapter, 2).unwrap().error, None);
    let selected = union_rows(&mut adapter);
    assert_eq!(selected.len(), 6);
    assert!(
        selected
            .iter()
            .all(|row| row.id.source_id == api.source_id().0.to_string()),
        "the B raw namesake cannot satisfy the derived shared-key predicate"
    );

    adapter.shutdown();
    manager.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn union_colour_rules_preserve_native_classifier_authority_and_rule_order() {
    let (_root, manager, api, _worker, mut adapter) = setup().await;
    adapter
        .register_view("view-loser", vec![api.source_id()])
        .unwrap();
    adapter
        .register_view("view-winner", vec![api.source_id()])
        .unwrap();
    apply_slash_enrichment(&mut adapter, "view-loser", r#"/svc\":\"(?P<choice>api)/"#);
    wait_applied(&mut adapter, 1).await;
    apply_slash_enrichment(&mut adapter, "view-winner", r#"/svc\":\"(?P<choice>api)/"#);
    wait_applied(&mut adapter, 1).await;
    adapter
        .register_union_view("union", vec![api.source_id()])
        .unwrap();

    let mut candidate = duplicate_projection_candidate(1, UnionFilterSpec::default());
    candidate.color_rules = vec![
        lvu::ColorRule {
            predicate: "05:06:00".into(),
            color: lvu::RuleColor::Red,
            column: None,
            value: None,
        },
        lvu::ColorRule::column_rule("choice".into(), "api".into(), lvu::RuleColor::Blue),
    ];
    adapter
        .submit_union_candidate(candidate, &|_| None)
        .unwrap();
    assert_eq!(wait_union(&mut adapter, 1).unwrap().error, None);

    let rows = union_rows(&mut adapter);
    assert_eq!(rows.len(), 6);
    for row in rows {
        let rule = row
            .details
            .iter()
            .find(|(name, _)| name == lvu_view::COLOR_RULE_DETAIL)
            .map(|(_, value)| value.as_str());
        if row.text.contains("05:06:00") {
            assert_eq!(rule, Some("1"), "the earlier legacy rule wins");
        } else {
            assert_eq!(
                rule,
                Some("2"),
                "the accepted slash-derived classifier paints through the union"
            );
        }
    }

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
