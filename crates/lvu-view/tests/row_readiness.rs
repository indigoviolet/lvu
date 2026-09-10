//! Deterministic reproduction of blank restored filtered views.
//!
//! A view's matched-ID membership and its raw-row cache are separate paths. The
//! query can publish "N records matched" long before any of those rows can be
//! served, and a row request can be dropped at the live provider's bounded queue
//! and never retried. These tests inject that delay, starvation, index progress,
//! lookup failure and supersession directly at the row-request seam so the blank
//! pane is produced on purpose rather than waited for.

use lvu::{
    QueryConstraints, QueryPurpose, QueryRequest, RowId, RowPage, RowProvider, TextConstraint,
    ViewportRequest, terminal::QueryDispatcher,
};
use lvu_core::{Acquisition, SourceDefinition, SourceId};
use lvu_ingest::{RuntimeConfig, RuntimeState, SourceHandle, SourceManager};
use lvu_live::{AdapterStats, IndexState, LiveConfig, LiveRowProvider, SourceViewStatus};
use lvu_view::{
    MAX_ROW_FETCH_RETRIES, MAX_ROW_REQUESTS_PER_PAGE, NativeViewAdapter, RawRowSource,
    RowReadiness, ScanState, ViewConfig,
};
use std::{
    collections::{BTreeMap, HashSet},
    fs,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tempfile::TempDir;

/// A `LiveRowProvider` whose row-request seam can be starved on demand. Every
/// other operation is the real one, so membership, indexing and view identity
/// behave exactly as in the application.
struct GatedRows {
    inner: Arc<LiveRowProvider>,
    /// Withhold every row, as after a restart when nothing is cached yet.
    withhold_all: AtomicBool,
    /// Withhold specific record sequences, leaving holes in an otherwise
    /// servable range.
    withheld: Mutex<HashSet<u64>>,
    /// Row lookups observed, used to prove per-frame request bounds.
    lookups: AtomicUsize,
    /// Injected raw-lookup failure reason, as the live provider would retain it.
    failure: Mutex<Option<String>>,
    /// Injected index progress: (state, indexed_records, reported_records).
    index: Mutex<Option<(IndexState, u64, u64)>>,
}

impl GatedRows {
    fn new(inner: Arc<LiveRowProvider>) -> Arc<Self> {
        Arc::new(Self {
            inner,
            withhold_all: AtomicBool::new(false),
            withheld: Mutex::new(HashSet::new()),
            lookups: AtomicUsize::new(0),
            failure: Mutex::new(None),
            index: Mutex::new(None),
        })
    }

    fn withhold_all(&self, value: bool) {
        self.withhold_all.store(value, Ordering::Release);
    }

    fn withhold_sequences(&self, sequences: impl IntoIterator<Item = u64>) {
        *self.withheld.lock().unwrap() = sequences.into_iter().collect();
    }

    fn fail_lookups_with(&self, reason: Option<&str>) {
        *self.failure.lock().unwrap() = reason.map(str::to_owned);
    }

    fn indexing(&self, value: Option<(IndexState, u64, u64)>) {
        *self.index.lock().unwrap() = value;
    }

    fn take_lookups(&self) -> usize {
        self.lookups.swap(0, Ordering::AcqRel)
    }
}

impl RawRowSource for GatedRows {
    fn page(&self, view_id: &str, request: ViewportRequest) -> RowPage {
        let page = RowProvider::page(&*self.inner, view_id, request);
        if self.withhold_all.load(Ordering::Acquire) {
            return RowPage {
                total: page.total,
                rows: Vec::new(),
            };
        }
        page
    }

    fn row_by_id(&self, view_id: &str, id: &RowId) -> Option<lvu::DisplayRow> {
        self.lookups.fetch_add(1, Ordering::AcqRel);
        if self.withhold_all.load(Ordering::Acquire)
            || self.withheld.lock().unwrap().contains(&id.sequence)
        {
            return None;
        }
        RowProvider::row_by_id(&*self.inner, view_id, id)
    }

    fn index_of_id(&self, view_id: &str, id: &RowId) -> Option<usize> {
        RowProvider::index_of_id(&*self.inner, view_id, id)
    }

    fn context_page(
        &self,
        view_id: &str,
        anchor: &RowId,
        offset: isize,
        len: usize,
    ) -> lvu::ContextPage {
        RowProvider::context_page(&*self.inner, view_id, anchor, offset, len)
    }

    fn revision(&self, view_id: &str) -> u64 {
        RowProvider::revision(&*self.inner, view_id)
    }

    fn register_source(&self, handle: lvu_shared::AnySourceHandle) -> Result<(), String> {
        self.inner
            .register_source(handle)
            .map_err(|error| error.to_string())
    }

    fn register_raw_view(&self, view_id: &str, sources: Vec<SourceId>) -> Result<(), String> {
        self.inner
            .register_raw_view(view_id, sources)
            .map_err(|error| error.to_string())
    }

    fn drain_ready_updates(&self, maximum: usize) -> usize {
        self.inner.drain_ready_updates(maximum)
    }

    fn source_status(&self, source_id: SourceId) -> Option<SourceViewStatus> {
        let mut status = self.inner.source_status(source_id)?;
        if let Some(reason) = self.failure.lock().unwrap().clone() {
            status.last_error = Some(reason);
        }
        if let Some((state, indexed, reported)) = *self.index.lock().unwrap() {
            status.index = state;
            status.indexed_records = indexed;
            status.reported_records = reported;
        }
        Some(status)
    }

    fn stats(&self) -> AdapterStats {
        self.inner.stats()
    }
}

fn definition(id: SourceId, path: &std::path::Path) -> SourceDefinition {
    SourceDefinition {
        schema_version: 1,
        id,
        name: "readiness fixture".into(),
        acquisition: Acquisition::File {
            path: path.into(),
            follow: false,
        },
        identity_hints: BTreeMap::new(),
        retention: None,
    }
}

async fn wait_runtime(handle: &SourceHandle, records: u64) {
    let mut progress = handle.subscribe();
    tokio::time::timeout(Duration::from_secs(8), async {
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

/// Builds a real capture, index and view, then hands back the interposed row
/// seam so a test can starve it.
async fn setup(
    root: &TempDir,
    contents: &str,
) -> (SourceManager, Arc<GatedRows>, NativeViewAdapter) {
    let input = root.path().join("input.log");
    fs::write(&input, contents).unwrap();
    let mut runtime = RuntimeConfig::default();
    runtime.acquisition.read_chunk_bytes = 64;
    runtime.acquisition.partial_flush_interval = Duration::from_millis(10);
    runtime.batch_records = 8;
    let manager = SourceManager::new(root.path().join("capture"), runtime).unwrap();
    let handle = manager
        .start(definition(SourceId::new(), &input))
        .await
        .unwrap();
    wait_runtime(
        &handle,
        contents.bytes().filter(|byte| *byte == b'\n').count() as u64,
    )
    .await;

    let mut live = LiveConfig::new(root.path().join("raw-index"));
    live.index_page_records = 8;
    live.index_page_bytes = 4096;
    let raw = GatedRows::new(Arc::new(LiveRowProvider::new(live).unwrap()));
    let view = ViewConfig::new(root.path().join("view-index"));
    let adapter =
        NativeViewAdapter::with_raw_rows(Arc::clone(&raw) as Arc<dyn RawRowSource>, view).unwrap();
    adapter
        .register_source(lvu_shared::AnySourceHandle::Local(handle.clone()))
        .unwrap();
    adapter
        .register_view("view", vec![handle.source_id()])
        .unwrap();
    // Start every test from a settled index so the starvation each one injects
    // is the only reason a row can be missing.
    let expected = contents.bytes().filter(|byte| *byte == b'\n').count() as u64;
    let source_id = handle.source_id();
    let settled = Arc::clone(&raw);
    let mut adapter = adapter;
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            adapter.drain_updates(64);
            if settled.source_status(source_id).is_some_and(|status| {
                status.index == IndexState::Ready && status.indexed_records >= expected
            }) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("fixture index must settle before starvation is injected");
    (manager, raw, adapter)
}

fn search(revision: u64, literal: &str) -> QueryRequest {
    QueryRequest {
        view_id: "view".into(),
        generation: 1,
        revision,
        base_revision: revision.saturating_sub(1),
        base_constraints: QueryConstraints::default(),
        purpose: QueryPurpose::Search,
        constraints: QueryConstraints {
            text: Some(TextConstraint {
                literal: literal.into(),
                case_insensitive: true,
            }),
            ..QueryConstraints::default()
        },
    }
}

async fn apply(adapter: &mut NativeViewAdapter, request: QueryRequest) {
    let revision = request.revision;
    adapter.submit(request).unwrap();
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            adapter.drain_updates(64);
            if let Some(done) = adapter.poll()
                && done.revision == revision
            {
                assert!(done.result.is_ok(), "fixture query failed: {done:?}");
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}

fn lines(count: usize) -> String {
    (0..count)
        .map(|index| format!("keep record {index}\n"))
        .collect()
}

/// The reported symptom: status says the query is ready and matched a record,
/// and the pane is empty. Before the fix `page` gave the UI no way to tell that
/// apart from a genuinely empty view.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn restored_filter_with_uncached_rows_explains_the_empty_pane() {
    let root = TempDir::new().unwrap();
    let (manager, raw, mut adapter) = setup(&root, "keep me\ndrop me\n").await;
    apply(&mut adapter, search(1, "keep")).await;
    assert_eq!(adapter.status("view").unwrap().state, ScanState::Ready);
    assert_eq!(adapter.status("view").unwrap().matched_records, 1);

    raw.withhold_all(true);
    let rows = adapter.rows();
    let page = rows.page("view", ViewportRequest { start: 0, len: 20 });
    assert_eq!(page.total, 1, "membership is satisfied");
    assert!(page.rows.is_empty(), "no row can be served yet");
    assert_eq!(
        rows.readiness("view"),
        RowReadiness::RowsPending {
            pending: 1,
            requested: 1
        }
    );
    assert!(
        rows.readiness("view").describe().is_some(),
        "a satisfied membership with no rows must carry an explanation"
    );

    raw.withhold_all(false);
    let served = tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            adapter.drain_updates(64);
            let page = rows.page("view", ViewportRequest { start: 0, len: 20 });
            if !page.rows.is_empty() {
                break page;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("a satisfied membership must converge to displayed rows");
    assert_eq!(served.rows.len(), 1);
    assert_eq!(rows.readiness("view"), RowReadiness::Ready);
    assert!(rows.readiness("view").describe().is_none());
    drop(adapter);
    drop(manager);
}

/// Row requests are individually queued into a bounded channel. Asking for a
/// whole tall viewport of missing rows overflows it and every request is
/// dropped, which is how a blank pane becomes permanent instead of transient.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_tall_uncached_viewport_bounds_its_requests_and_still_converges() {
    let root = TempDir::new().unwrap();
    let (manager, raw, mut adapter) = setup(&root, &lines(48)).await;
    apply(&mut adapter, search(1, "keep")).await;
    assert_eq!(adapter.status("view").unwrap().matched_records, 48);

    raw.withhold_all(true);
    let rows = adapter.rows();
    raw.take_lookups();
    let page = rows.page("view", ViewportRequest { start: 0, len: 48 });
    assert!(page.rows.is_empty());
    assert_eq!(page.total, 48);
    assert!(
        raw.take_lookups() <= MAX_ROW_REQUESTS_PER_PAGE,
        "one frame must not flood the bounded raw request queue"
    );
    // Depending on how far the real index has progressed this is either an
    // outstanding row fetch or reported index progress. It is never `Ready`, and
    // it always carries an explanation.
    let readiness = rows.readiness("view");
    assert!(readiness.is_pending(), "{readiness:?}");
    assert!(readiness.describe().is_some(), "{readiness:?}");

    // Nothing external will wake the terminal while requests are outstanding, so
    // the view must advance its own revision to earn another frame.
    let before = rows.revision("view");
    rows.page("view", ViewportRequest { start: 0, len: 48 });
    assert_ne!(
        rows.revision("view"),
        before,
        "outstanding rows must keep asking for a redraw"
    );

    raw.withhold_all(false);
    let served = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            adapter.drain_updates(64);
            let page = rows.page("view", ViewportRequest { start: 0, len: 48 });
            if page.rows.len() == 48 {
                break page;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("bounded per-frame requests must still fill the viewport");
    assert_eq!(served.rows.len(), 48);
    assert_eq!(rows.readiness("view"), RowReadiness::Ready);
    drop(adapter);
    drop(manager);
}

/// A hole at the top of the range used to stop the loop outright, so only the
/// first missing row was ever requested and the pane refilled one row per frame
/// at best. The rest of the visible range must be requested too.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_hole_at_the_top_still_requests_the_rest_of_the_range() {
    let root = TempDir::new().unwrap();
    let (manager, raw, mut adapter) = setup(&root, &lines(12)).await;
    apply(&mut adapter, search(1, "keep")).await;

    let rows = adapter.rows();
    let first = rows
        .page("view", ViewportRequest { start: 0, len: 12 })
        .rows
        .first()
        .map(|row| row.id.sequence)
        .unwrap_or(0);
    raw.withhold_sequences([first]);
    raw.take_lookups();
    let page = rows.page("view", ViewportRequest { start: 0, len: 12 });
    assert!(page.rows.is_empty(), "the leading row cannot be placed");
    assert!(
        raw.take_lookups() > 1,
        "the whole visible range must be requested, not just its first hole"
    );
    assert!(!rows.readiness("view").is_ready());
    assert!(rows.readiness("view").describe().is_some());
    drop(adapter);
    drop(manager);
}

/// Index construction is a different reason from "the row cache is cold", and
/// the UI is required to be able to tell them apart and show progress.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn index_progress_is_reported_instead_of_a_bare_pending_row_fetch() {
    let root = TempDir::new().unwrap();
    let (manager, raw, mut adapter) = setup(&root, &lines(6)).await;
    apply(&mut adapter, search(1, "keep")).await;

    raw.withhold_all(true);
    raw.indexing(Some((IndexState::Indexing, 2, 6)));
    let rows = adapter.rows();
    let page = rows.page("view", ViewportRequest { start: 0, len: 6 });
    assert!(page.rows.is_empty());
    assert_eq!(
        rows.readiness("view"),
        RowReadiness::Indexing {
            indexed_records: 2,
            reported_records: 6
        }
    );
    let described = rows.readiness("view").describe().unwrap();
    assert!(
        described.contains('2') && described.contains('6'),
        "{described}"
    );

    raw.indexing(None);
    assert!(matches!(
        rows.readiness("view"),
        RowReadiness::RowsPending { .. }
    ));
    drop(adapter);
    drop(manager);
}

/// A held derived index is a wait, not a failure, and it must be readable as
/// such: the worker recovers on its own, so the pane has to say what it is
/// waiting for and then stop saying it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_contended_index_is_reported_as_a_wait_and_clears_on_recovery() {
    let root = TempDir::new().unwrap();
    let (manager, raw, mut adapter) = setup(&root, &lines(4)).await;
    apply(&mut adapter, search(1, "keep")).await;

    raw.withhold_all(true);
    // The live worker reports the conflict through `last_error` while it waits,
    // so contention has to outrank the failure reading of that same field.
    raw.fail_lookups_with(Some(
        "derived index is already owned: Resource temporarily unavailable",
    ));
    raw.indexing(Some((IndexState::Contended, 0, 4)));
    let rows = adapter.rows();
    let page = rows.page("view", ViewportRequest { start: 0, len: 4 });
    assert!(page.rows.is_empty());
    assert_eq!(
        rows.readiness("view"),
        RowReadiness::IndexContended { pending: 4 },
        "a held index must not read as an ordinary lookup failure"
    );
    let described = rows.readiness("view").describe().unwrap();
    assert!(
        described.contains("in use") && described.contains('4'),
        "{described}"
    );
    assert!(
        rows.readiness("view").is_pending(),
        "waiting for a lock is a state rows can still arrive from"
    );

    // Recovery: the worker opened the index, so the reason and the wait both go.
    raw.indexing(None);
    raw.fail_lookups_with(None);
    raw.withhold_all(false);
    let served = tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            adapter.drain_updates(64);
            let page = rows.page("view", ViewportRequest { start: 0, len: 4 });
            if page.rows.len() == 4 {
                break page;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("a contended view must recover once the index is released");
    assert_eq!(served.rows.len(), 4);
    assert_eq!(rows.readiness("view"), RowReadiness::Ready);
    assert!(
        rows.readiness("view").describe().is_none(),
        "the explanation must clear rather than stick"
    );
    drop(adapter);
    drop(manager);
}

/// A failed raw lookup must never reach the UI as an ordinary empty result.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_row_lookup_is_reported_with_its_reason() {
    let root = TempDir::new().unwrap();
    let (manager, raw, mut adapter) = setup(&root, &lines(4)).await;
    apply(&mut adapter, search(1, "keep")).await;

    raw.withhold_all(true);
    raw.fail_lookups_with(Some("journal segment read failed: Input/output error"));
    let rows = adapter.rows();
    let page = rows.page("view", ViewportRequest { start: 0, len: 4 });
    assert!(page.rows.is_empty());
    assert_eq!(page.total, 4);
    match rows.readiness("view") {
        RowReadiness::LookupFailed { reason, pending } => {
            assert_eq!(pending, 4);
            assert!(reason.contains("Input/output error"), "{reason}");
        }
        other => panic!("expected an explained lookup failure, got {other:?}"),
    }
    assert!(rows.readiness("view").describe().is_some());
    drop(adapter);
    drop(manager);
}

/// Retrying is bounded. Once the budget is spent the view stops asking for
/// redraws and reports a stalled fetch, which is still not a blank pane.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exhausted_row_retries_report_a_stall_rather_than_spinning() {
    let root = TempDir::new().unwrap();
    let (manager, raw, mut adapter) = setup(&root, &lines(3)).await;
    apply(&mut adapter, search(1, "keep")).await;

    raw.withhold_all(true);
    let rows = adapter.rows();
    for _ in 0..MAX_ROW_FETCH_RETRIES + 2 {
        rows.page("view", ViewportRequest { start: 0, len: 3 });
    }
    assert_eq!(
        rows.readiness("view"),
        RowReadiness::Stalled {
            pending: 3,
            requested: 3
        }
    );
    assert!(rows.readiness("view").describe().is_some());

    let settled = rows.revision("view");
    for _ in 0..8 {
        rows.page("view", ViewportRequest { start: 0, len: 3 });
    }
    assert_eq!(
        rows.revision("view"),
        settled,
        "an exhausted retry budget must stop requesting redraws"
    );

    // Recovery is still automatic once rows can be served again.
    raw.withhold_all(false);
    let served = tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            adapter.drain_updates(64);
            let page = rows.page("view", ViewportRequest { start: 0, len: 3 });
            if page.rows.len() == 3 {
                break page;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("a stalled view must recover when rows become available");
    assert_eq!(served.rows.len(), 3);
    assert_eq!(rows.readiness("view"), RowReadiness::Ready);
    drop(adapter);
    drop(manager);
}

/// The state that must stay distinguishable from every pending state above.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn genuinely_zero_matches_is_not_confused_with_pending_rows() {
    let root = TempDir::new().unwrap();
    let (manager, _raw, mut adapter) = setup(&root, &lines(5)).await;
    apply(&mut adapter, search(1, "absent-literal")).await;

    let rows = adapter.rows();
    let page = rows.page("view", ViewportRequest { start: 0, len: 20 });
    assert_eq!(page.total, 0);
    assert!(page.rows.is_empty());
    assert_eq!(rows.readiness("view"), RowReadiness::NoMatches);
    assert_eq!(
        rows.readiness("view").describe().as_deref(),
        Some("No records match this view.")
    );
    drop(adapter);
    drop(manager);
}

/// A later revision supersedes the earlier one. Row bookkeeping from the wider
/// membership must not survive into the narrower one and must not let the newer
/// view claim it is complete while its own row is still missing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_superseded_membership_cannot_leave_the_newer_view_silently_blank() {
    let root = TempDir::new().unwrap();
    let (manager, raw, mut adapter) = setup(&root, "keep alpha\nkeep beta\nkeep gamma\n").await;
    apply(&mut adapter, search(1, "keep")).await;
    assert_eq!(adapter.status("view").unwrap().matched_records, 3);

    raw.withhold_all(true);
    let rows = adapter.rows();
    rows.page("view", ViewportRequest { start: 0, len: 3 });
    assert_eq!(
        rows.readiness("view"),
        RowReadiness::RowsPending {
            pending: 3,
            requested: 3
        }
    );

    let mut narrowed = search(2, "gamma");
    narrowed.base_constraints = search(1, "keep").constraints;
    apply(&mut adapter, narrowed).await;
    assert_eq!(adapter.status("view").unwrap().matched_records, 1);
    let page = rows.page("view", ViewportRequest { start: 0, len: 20 });
    assert_eq!(page.total, 1);
    assert!(page.rows.is_empty());
    assert_eq!(
        rows.readiness("view"),
        RowReadiness::RowsPending {
            pending: 1,
            requested: 1
        },
        "readiness must describe the published membership, not the superseded one"
    );

    raw.withhold_all(false);
    let served = tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            adapter.drain_updates(64);
            let page = rows.page("view", ViewportRequest { start: 0, len: 20 });
            if !page.rows.is_empty() {
                break page;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the newer membership must converge too");
    assert_eq!(served.rows.len(), 1);
    assert!(served.rows[0].text.contains("gamma"));
    assert_eq!(rows.readiness("view"), RowReadiness::Ready);
    drop(adapter);
    drop(manager);
}

/// The product invariant behind both checklist rows: whenever rows are missing
/// from a non-empty view, the UI is handed a reason. `Ready` is reachable only
/// when the requested range is genuinely complete.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_satisfied_membership_never_ends_in_a_silent_blank_pane() {
    let root = TempDir::new().unwrap();
    let (manager, raw, mut adapter) = setup(&root, &lines(20)).await;
    apply(&mut adapter, search(1, "keep")).await;
    let rows = adapter.rows();

    type Arrange = Box<dyn Fn()>;
    let conditions: Vec<(&str, Arrange)> = vec![
        ("cold row cache", {
            let raw = Arc::clone(&raw);
            Box::new(move || {
                raw.withhold_all(true);
                raw.fail_lookups_with(None);
                raw.indexing(None);
            })
        }),
        ("index still building", {
            let raw = Arc::clone(&raw);
            Box::new(move || {
                raw.withhold_all(true);
                raw.fail_lookups_with(None);
                raw.indexing(Some((IndexState::Rebuilding, 4, 20)));
            })
        }),
        ("row lookup failure", {
            let raw = Arc::clone(&raw);
            Box::new(move || {
                raw.withhold_all(true);
                raw.indexing(None);
                raw.fail_lookups_with(Some("permission denied"));
            })
        }),
        ("holes inside the range", {
            let raw = Arc::clone(&raw);
            Box::new(move || {
                raw.withhold_all(false);
                raw.indexing(None);
                raw.fail_lookups_with(None);
                raw.withhold_sequences([0, 3, 7]);
            })
        }),
    ];

    for (name, arrange) in conditions {
        arrange();
        let page = rows.page("view", ViewportRequest { start: 0, len: 20 });
        assert!(page.total > 0, "{name}: membership stays satisfied");
        if page.rows.len() < page.total {
            let readiness = rows.readiness("view");
            assert!(
                !readiness.is_ready(),
                "{name}: reported complete while short"
            );
            assert!(
                readiness.describe().is_some(),
                "{name}: an incomplete pane must carry an explanation"
            );
        }
    }

    raw.withhold_all(false);
    raw.withhold_sequences([]);
    raw.fail_lookups_with(None);
    raw.indexing(None);
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            adapter.drain_updates(64);
            if rows
                .page("view", ViewportRequest { start: 0, len: 20 })
                .rows
                .len()
                == 20
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("every injected condition must still converge once it clears");
    assert_eq!(rows.readiness("view"), RowReadiness::Ready);
    drop(adapter);
    drop(manager);
}
