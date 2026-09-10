//! Display-role provenance through the public view adapter.
//!
//! Display roles resolve in the terminal against the `derived.{name}` marker
//! each served row carries: the marker proves the accepted chain evaluated
//! that output for the batch, so a same-named raw field with no marker stays
//! raw data and never feeds a role. These tests pin the worker side of that
//! contract — raw same-name fields before any enrichment, marker loss on
//! stage removal, applied markers surviving an invalid draft, and typed stage
//! failure surfacing explicit error text with its marker on live arrivals.

use lvu::{
    CaptureTimeRange, QueryConstraints, QueryPurpose, QueryRequest, RowProvider, TimeBasis,
    ViewportRequest, terminal::QueryDispatcher,
};
use lvu_core::{Acquisition, SourceDefinition, SourceId};
use lvu_ingest::{RuntimeConfig, SourceHandle, SourceManager};
use lvu_live::{LiveConfig, LiveRowProvider};
use lvu_query::CompilerHostConfig;
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
        name: "role provenance fixture".into(),
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
    live.cache_rows = 32;
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
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let current = progress.borrow().clone();
            if current.records >= records {
                break;
            }
            progress.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
}

async fn wait_completion(adapter: &mut NativeViewAdapter, revision: u64) -> lvu::QueryCompletion {
    tokio::time::timeout(Duration::from_secs(20), async {
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
    .unwrap()
}

async fn wait_page(adapter: &mut NativeViewAdapter, len: usize) -> Vec<lvu::DisplayRow> {
    let rows = adapter.rows();
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let page = rows.page("view", ViewportRequest { start: 0, len });
            adapter.drain_updates(64);
            if page.rows.len() == len {
                break page.rows;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap()
}

fn request(
    generation: u64,
    revision: u64,
    base_revision: u64,
    advanced: Option<&str>,
) -> QueryRequest {
    QueryRequest {
        view_id: "view".into(),
        generation,
        revision,
        base_revision,
        base_constraints: QueryConstraints::default(),
        purpose: if advanced.is_some() {
            QueryPurpose::Advanced
        } else {
            QueryPurpose::Search
        },
        constraints: QueryConstraints {
            text: None,
            advanced_polars: advanced.map(str::to_owned),
            enrichments: Vec::new(),
            ..QueryConstraints::default()
        },
    }
}

fn enrichment_step(id: &str, source: &str) -> lvu::EnrichmentDefinition {
    lvu::EnrichmentDefinition {
        id: lvu::EnrichmentStageId(id.into()),
        source: source.into(),
        command: None,
    }
}

async fn setup(
    root: &TempDir,
    contents: &str,
    follow: bool,
) -> (SourceManager, SourceHandle, NativeViewAdapter) {
    let input = root.path().join("input.log");
    fs::write(&input, contents).unwrap();
    let manager = SourceManager::new(root.path().join("capture"), runtime_config()).unwrap();
    let handle = manager
        .start(source(SourceId::new(), &input, follow))
        .await
        .unwrap();
    wait_runtime(
        &handle,
        contents.bytes().filter(|byte| *byte == b'\n').count() as u64,
    )
    .await;
    let (live_config, view_config) = configs(root);
    let raw = Arc::new(LiveRowProvider::new(live_config).unwrap());
    let adapter = NativeViewAdapter::new(raw, view_config).unwrap();
    adapter
        .register_source(lvu_shared::AnySourceHandle::Local(handle.clone()))
        .unwrap();
    adapter
        .register_view("view", vec![handle.source_id()])
        .unwrap();
    (manager, handle, adapter)
}

fn has_marker(row: &lvu::DisplayRow, name: &str) -> bool {
    let marker = format!("derived.{name}");
    row.details.iter().any(|(key, _)| key == &marker)
}

/// Structural readiness: the view evaluated the output for this row's batch
/// without a stage failure. Roles consume only ready cells; validity is key
/// presence here, never value text.
fn has_ready(row: &lvu::DisplayRow, name: &str) -> bool {
    let marker = format!("derived_ready.{name}");
    row.details.iter().any(|(key, _)| key == &marker)
}

/// Structural failure: the stage failed for this row's batch. The
/// compatibility `derived.{name}` marker still carries the error text for
/// Details, but the cell never feeds a display role.
fn has_error(row: &lvu::DisplayRow, name: &str) -> bool {
    let marker = format!("derived_error.{name}");
    row.details.iter().any(|(key, _)| key == &marker)
}

fn field(row: &lvu::DisplayRow, name: &str) -> Option<String> {
    row.fields
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.clone())
}

/// A raw same-name field with no enrichment carries no provenance marker,
/// which is what keeps it raw data once a role names it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn raw_same_name_field_has_no_derived_marker() {
    let root = TempDir::new().unwrap();
    let (manager, _handle, mut adapter) =
        setup(&root, "severity=low first\nseverity=high second\n", false).await;

    let applied = request(1, 1, 0, None);
    adapter.submit(applied).unwrap();
    assert!(wait_completion(&mut adapter, 1).await.result.is_ok());
    let rows = wait_page(&mut adapter, 2).await;
    for row in &rows {
        assert!(
            field(row, "severity").is_some(),
            "the raw field stays visible: {:?}",
            row.fields
        );
        assert!(
            !has_marker(row, "severity"),
            "no enrichment evaluated it: {:?}",
            row.details
        );
    }
    adapter.shutdown();
    manager.shutdown().await;
}

/// Removing the stage that produced a name drops its marker and returns the
/// raw field: the terminal's role read follows the marker, so nothing
/// silently keeps the removed output's meaning.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stage_removal_drops_the_marker_and_returns_the_raw_field() {
    let root = TempDir::new().unwrap();
    let (manager, _handle, mut adapter) =
        setup(&root, "severity=low first\nseverity=high second\n", false).await;

    let chain = vec![enrichment_step(
        "shout",
        "severity = pl.col(\"severity\").str.to_uppercase()",
    )];
    let mut applied = request(1, 1, 0, None);
    applied.purpose = QueryPurpose::Enrichment;
    applied.constraints.enrichments = chain.clone();
    adapter.submit(applied).unwrap();
    assert!(wait_completion(&mut adapter, 1).await.result.is_ok());
    let rows = wait_page(&mut adapter, 2).await;
    assert_eq!(field(&rows[0], "severity").as_deref(), Some("LOW"));
    assert!(
        rows.iter().all(|row| has_marker(row, "severity")),
        "accepted output marks every row"
    );

    let mut removed = request(2, 2, 1, None);
    removed.purpose = QueryPurpose::Enrichment;
    removed.base_constraints.enrichments = chain;
    removed.constraints.enrichments = Vec::new();
    adapter.submit(removed).unwrap();
    assert!(wait_completion(&mut adapter, 2).await.result.is_ok());
    let rows = wait_page(&mut adapter, 2).await;
    assert_eq!(field(&rows[0], "severity").as_deref(), Some("low"));
    assert!(
        rows.iter().all(|row| !has_marker(row, "severity")),
        "the removed output marks nothing: {:?}",
        rows[0].details
    );

    adapter.shutdown();
    manager.shutdown().await;
}

/// A rejected enrichment draft leaves the applied markers exactly where they
/// were: the last good chain keeps serving proven values.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invalid_draft_preserves_applied_markers() {
    let root = TempDir::new().unwrap();
    let (manager, _handle, mut adapter) = setup(&root, "level=warn first\n", false).await;

    let chain = vec![enrichment_step(
        "shout",
        "severity = pl.col(\"level\").str.to_uppercase()",
    )];
    let mut applied = request(1, 1, 0, None);
    applied.purpose = QueryPurpose::Enrichment;
    applied.constraints.enrichments = chain.clone();
    adapter.submit(applied).unwrap();
    assert!(wait_completion(&mut adapter, 1).await.result.is_ok());
    assert_eq!(
        field(&wait_page(&mut adapter, 1).await[0], "severity").as_deref(),
        Some("WARN")
    );

    let mut invalid = request(2, 2, 1, None);
    invalid.purpose = QueryPurpose::Enrichment;
    invalid.base_constraints.enrichments = chain.clone();
    invalid.constraints.enrichments = vec![enrichment_step("broken", "broken = pl.col(")];
    adapter.submit(invalid).unwrap();
    assert!(wait_completion(&mut adapter, 2).await.result.is_err());
    let rows = wait_page(&mut adapter, 1).await;
    assert_eq!(field(&rows[0], "severity").as_deref(), Some("WARN"));
    assert!(has_marker(&rows[0], "severity"));

    adapter.shutdown();
    manager.shutdown().await;
}

/// A stage that fails on newly appended records marks those rows with
/// explicit error text: typed failure stays distinguishable from a value,
/// and earlier rows keep their proven values.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_stage_marks_new_rows_with_error_text() {
    let root = TempDir::new().unwrap();
    let (manager, handle, mut adapter) = setup(&root, "s=1 first\ns=2 second\n", true).await;

    let chain = vec![enrichment_step(
        "strict",
        "n = pl.col(\"s\").cast(pl.Int64, strict=True)",
    )];
    let mut applied = request(1, 1, 0, None);
    applied.purpose = QueryPurpose::Enrichment;
    applied.constraints.enrichments = chain;
    adapter.submit(applied).unwrap();
    assert!(wait_completion(&mut adapter, 1).await.result.is_ok());
    let rows = wait_page(&mut adapter, 2).await;
    assert_eq!(field(&rows[0], "n").as_deref(), Some("1"));

    let mut file = OpenOptions::new()
        .append(true)
        .open(root.path().join("input.log"))
        .unwrap();
    writeln!(file, "s=oops late").unwrap();
    file.flush().unwrap();
    wait_runtime(&handle, 3).await;
    let rows = wait_page(&mut adapter, 3).await;
    let late = rows
        .iter()
        .find(|row| row.text.contains("late"))
        .expect("the late arrival is served");
    let value = field(late, "n").expect("failed stage still projects a value");
    assert!(
        value.starts_with("error:"),
        "typed failure reads as explicit error text, not a value: {value:?}"
    );
    assert!(has_marker(late, "n"));
    assert_eq!(field(&rows[0], "n").as_deref(), Some("1"));

    adapter.shutdown();
    manager.shutdown().await;
}

/// A Selected time basis over an enrichment column filters one atomically
/// published membership: capture order differs from event-time order, and a
/// window keeps exactly the records inside it, in capture order. The terminal
/// reads the same basis for its gutter through the timestamp role, so the two
/// can never disagree once converged.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn selected_basis_window_filters_divergent_orders_atomically() {
    let root = TempDir::new().unwrap();
    let (manager, _handle, mut adapter) = setup(
        &root,
        "ts=2026-09-05T12:30:50.000000Z id=first\n\
         ts=2026-09-05T12:30:40.000000Z id=second\n\
         ts=2026-09-05T12:30:41.000000Z id=third\n",
        false,
    )
    .await;

    // The token grammar is the role helper's: the test proves the helper's
    // output is directly usable as an accepted Selected basis. Selected reads
    // enrichment outputs (never raw fields), so the windowed column is an
    // accepted enrichment stage, exactly as a timestamp role requires.
    let token = lvu::app::role_time_token("event_time");
    let mut windowed = request(1, 1, 0, None);
    windowed.constraints.enrichments =
        vec![enrichment_step("copy-time", "event_time = pl.col(\"ts\")")];
    windowed.constraints.time_basis = TimeBasis::Selected;
    windowed.constraints.time_field = Some(token);
    windowed.constraints.capture_time = Some(CaptureTimeRange {
        start_unix_nanos: 1_788_611_439_000_000_000,
        end_unix_nanos: 1_788_611_442_000_000_000,
    });
    adapter.submit(windowed).unwrap();
    assert!(wait_completion(&mut adapter, 1).await.result.is_ok());
    assert_eq!(adapter.status("view").unwrap().matched_records, 2);
    let rows = wait_page(&mut adapter, 2).await;
    let ids: Vec<u64> = rows.iter().map(|row| row.id.sequence).collect();
    // Journal sequences are 0-based: lines two and three carry the
    // in-window event times while capture order is unchanged.
    assert_eq!(ids, vec![1, 2], "capture order, event-time membership");

    adapter.shutdown();
    manager.shutdown().await;
}

/// Rows served under a Selected basis carry its validated instants as
/// `basis_nanos` details: decimal UTC nanoseconds for readable records,
/// nothing for misses. The terminal formats those through the display-zone
/// formatter instead of parsing text, so this is what the timestamp gutter
/// reads. A row whose value the basis could not use carries no detail.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn selected_basis_projects_validated_nanos_details() {
    let root = TempDir::new().unwrap();
    let (manager, _handle, mut adapter) = setup(
        &root,
        "ts=2026-09-05T12:30:50.000000Z id=first\n\
         ts=oops id=second\n",
        false,
    )
    .await;

    let token = lvu::app::role_time_token("event_time");
    let mut selected = request(1, 1, 0, None);
    selected.constraints.enrichments =
        vec![enrichment_step("copy-time", "event_time = pl.col(\"ts\")")];
    selected.constraints.time_basis = TimeBasis::Selected;
    selected.constraints.time_field = Some(token);
    adapter.submit(selected).unwrap();
    assert!(wait_completion(&mut adapter, 1).await.result.is_ok());
    let rows = wait_page(&mut adapter, 2).await;
    let nanos = |row: &lvu::DisplayRow| {
        row.details
            .iter()
            .find(|(key, _)| key == "basis_nanos")
            .map(|(_, value)| value.clone())
    };
    assert_eq!(
        nanos(&rows[0]).as_deref(),
        Some("1788611450000000000"),
        "validated instants ride the row for display: {:?}",
        rows[0].details
    );
    assert_eq!(
        nanos(&rows[1]),
        None,
        "unreadable values leave no instant behind: {:?}",
        rows[1].details
    );

    adapter.shutdown();
    manager.shutdown().await;
}

/// Ready, null and failed cells are told apart structurally through the real
/// worker path: only successfully evaluated cells carry the ready marker —
/// including numbers, booleans and strings that collide with the null/error
/// display texts — while valid nulls and stage failures never do. Severity
/// additionally requires a canonical token at render time; the worker's half
/// is proving evaluation outcome, not token membership.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ready_null_and_failed_cells_carry_structural_markers() {
    let root = TempDir::new().unwrap();
    let (manager, _handle, mut adapter) = setup(
        &root,
        "level=warn s=oops id=first\n\
         level=info s=42 id=second\n",
        false,
    )
    .await;

    let chain = vec![
        enrichment_step("ok", "sev_ok = pl.col(\"level\").str.to_uppercase()"),
        enrichment_step(
            "null",
            "sev_null = pl.col(\"s\").cast(pl.Int64, strict=False)",
        ),
        enrichment_step("lower", "sev_low = pl.col(\"level\").str.to_lowercase()"),
        enrichment_step(
            "flag",
            "sev_flag = pl.col(\"level\").str.contains(\"warn\")",
        ),
        enrichment_step(
            "num",
            "sev_num = pl.col(\"s\").cast(pl.Int64, strict=False)",
        ),
        enrichment_step("nullstr", "sev_nullstr = pl.lit(\"null\")"),
        enrichment_step("errstr", "sev_errstr = pl.lit(\"error: boom\")"),
    ];
    let mut applied = request(1, 1, 0, None);
    applied.purpose = QueryPurpose::Enrichment;
    applied.constraints.enrichments = chain;
    adapter.submit(applied).unwrap();
    assert!(wait_completion(&mut adapter, 1).await.result.is_ok());
    let rows = wait_page(&mut adapter, 2).await;
    let first = rows
        .iter()
        .find(|row| row.text.contains("id=first"))
        .expect("first record served");
    let second = rows
        .iter()
        .find(|row| row.text.contains("id=second"))
        .expect("second record served");

    // Canonical ready strings: the only cells a severity role may consume.
    assert_eq!(field(first, "sev_ok").as_deref(), Some("WARN"));
    assert!(has_ready(first, "sev_ok"));
    assert_eq!(field(second, "sev_ok").as_deref(), Some("INFO"));
    assert!(has_ready(second, "sev_ok"));

    // Valid nulls carry no ready marker: strict=False leaves `s=oops`
    // uncastable, which is a typed null, not a failure.
    assert_eq!(field(first, "sev_null").as_deref(), Some("null"));
    assert!(
        !has_ready(first, "sev_null"),
        "a valid null is not a ready value: {:?}",
        first.details
    );
    assert!(!has_error(first, "sev_null"));
    // The same stage on a castable row is ready — outcome is per record.
    assert_eq!(field(second, "sev_null").as_deref(), Some("42"));
    assert!(has_ready(second, "sev_null"));
    assert_eq!(field(second, "sev_num").as_deref(), Some("42"));
    assert!(has_ready(second, "sev_num"));

    // Ready but not severity: lowercase words, booleans and numbers evaluate
    // successfully yet match no canonical token at render time.
    assert_eq!(field(first, "sev_low").as_deref(), Some("warn"));
    assert!(has_ready(first, "sev_low"));
    assert_eq!(field(first, "sev_flag").as_deref(), Some("true"));
    assert!(has_ready(first, "sev_flag"));
    assert_eq!(field(second, "sev_flag").as_deref(), Some("false"));
    assert!(has_ready(second, "sev_flag"));
    assert_eq!(field(first, "sev_num").as_deref(), Some("null"));
    assert!(!has_ready(first, "sev_num"));

    // Ready literals colliding with the null/error display texts are still
    // ready — decided structurally, not by sniffing the value — while the
    // severity token gate keeps them out of the level cell.
    assert_eq!(field(first, "sev_nullstr").as_deref(), Some("null"));
    assert!(has_ready(first, "sev_nullstr"));
    assert_eq!(field(first, "sev_errstr").as_deref(), Some("error: boom"));
    assert!(has_ready(first, "sev_errstr"));
    assert!(!has_error(first, "sev_errstr"));

    // No stage failed anywhere in this static pass.
    for row in &rows {
        for name in [
            "sev_ok",
            "sev_null",
            "sev_low",
            "sev_flag",
            "sev_num",
            "sev_nullstr",
            "sev_errstr",
        ] {
            assert!(
                !has_error(row, name),
                "no failure expected for {name}: {:?}",
                row.details
            );
        }
        // Raw bytes stay visible beside every derived cell.
        assert!(
            row.text.contains("level=") && row.text.contains("s="),
            "raw input remains: {:?}",
            row.text
        );
    }

    adapter.shutdown();
    manager.shutdown().await;
}

/// A supported slash-shorthand named capture publishes the same structural
/// markers as assignment-form outputs: the named group is a first-class
/// derived cell, so Fields assignment and role rendering — which resolve
/// through markers, never by parsing definition source — treat both syntaxes
/// alike.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn slash_named_capture_publishes_ready_markers() {
    let root = TempDir::new().unwrap();
    let (manager, _handle, mut adapter) =
        setup(&root, "level=WARN first\nlevel=INFO second\n", false).await;

    let chain = vec![enrichment_step(
        "shorthand",
        "/level=(?P<severity>WARN|ERROR)/",
    )];
    let mut applied = request(1, 1, 0, None);
    applied.purpose = QueryPurpose::Enrichment;
    applied.constraints.enrichments = chain;
    adapter.submit(applied).unwrap();
    assert!(wait_completion(&mut adapter, 1).await.result.is_ok());
    let rows = wait_page(&mut adapter, 2).await;
    let first = rows
        .iter()
        .find(|row| row.text.contains("first"))
        .expect("first record served");
    // The named capture evaluated: ready marker plus canonical value, with
    // raw bytes untouched beside it.
    assert_eq!(field(first, "severity").as_deref(), Some("WARN"));
    assert!(has_marker(first, "severity"));
    assert!(
        has_ready(first, "severity"),
        "slash outputs are ready cells: {:?}",
        first.details
    );
    assert!(!has_error(first, "severity"));
    assert!(
        first.text.contains("level=WARN"),
        "raw input remains: {:?}",
        first.text
    );
    // The non-matching row has no capture: no ready cell, so no role.
    let second = rows
        .iter()
        .find(|row| row.text.contains("second"))
        .expect("second record served");
    assert!(
        !has_ready(second, "severity"),
        "an unmatched capture is not ready: {:?}",
        second.details
    );

    adapter.shutdown();
    manager.shutdown().await;
}

/// A stage that fails on newly appended records marks those rows with the
/// structural error marker and no ready marker; earlier rows keep their
/// ready markers. The compatibility marker still carries explicit error
/// text for Details.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_severity_cells_carry_error_without_ready() {
    let root = TempDir::new().unwrap();
    let (manager, handle, mut adapter) =
        setup(&root, "level=warn s=1 first\nlevel=info s=2 second\n", true).await;

    let chain = vec![
        enrichment_step("ok", "sev_ok = pl.col(\"level\").str.to_uppercase()"),
        enrichment_step(
            "strict",
            "sev_n = pl.col(\"s\").cast(pl.Int64, strict=True)",
        ),
    ];
    let mut applied = request(1, 1, 0, None);
    applied.purpose = QueryPurpose::Enrichment;
    applied.constraints.enrichments = chain;
    adapter.submit(applied).unwrap();
    assert!(wait_completion(&mut adapter, 1).await.result.is_ok());
    let rows = wait_page(&mut adapter, 2).await;
    assert_eq!(field(&rows[0], "sev_n").as_deref(), Some("1"));
    assert!(has_ready(&rows[0], "sev_n"));
    assert!(!has_error(&rows[0], "sev_n"));

    let mut file = OpenOptions::new()
        .append(true)
        .open(root.path().join("input.log"))
        .unwrap();
    writeln!(file, "level=error s=oops late").unwrap();
    file.flush().unwrap();
    wait_runtime(&handle, 3).await;
    let rows = wait_page(&mut adapter, 3).await;
    let late = rows
        .iter()
        .find(|row| row.text.contains("late"))
        .expect("the late arrival is served");
    let value = field(late, "sev_n").expect("failed stage still projects a value");
    assert!(
        value.starts_with("error:"),
        "typed failure reads as explicit error text, not a value: {value:?}"
    );
    assert!(has_marker(late, "sev_n"));
    assert!(
        has_error(late, "sev_n"),
        "failures are structural: {:?}",
        late.details
    );
    assert!(
        !has_ready(late, "sev_n"),
        "a failed cell is never ready: {:?}",
        late.details
    );
    // The canonical sibling stage is unaffected on the same row.
    assert_eq!(field(late, "sev_ok").as_deref(), Some("ERROR"));
    assert!(has_ready(late, "sev_ok"));

    adapter.shutdown();
    manager.shutdown().await;
}

/// Removing the derived stage behind a timestamp role while a Selected basis
/// still names the column: served rows keep a readable instant for the
/// same-named raw column but lose the ready marker. The terminal resolves
/// the lingering role name against accepted outputs (no match) and must not
/// render the raw instant as the role — the worker half proven here is that
/// readability (`basis_nanos`) and role provenance (ready marker) diverge.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn removed_timestamp_stage_leaves_raw_instant_without_ready_marker() {
    let root = TempDir::new().unwrap();
    let (manager, _handle, mut adapter) = setup(
        &root,
        "event_time=2026-09-05T12:30:50.000000Z id=first\n\
         event_time=2026-09-05T12:30:40.000000Z id=second\n",
        false,
    )
    .await;

    let token = lvu::app::role_time_token("event_time");
    let chain = vec![enrichment_step(
        "copy-time",
        "event_time = pl.col(\"event_time\")",
    )];
    let mut applied = request(1, 1, 0, None);
    applied.purpose = QueryPurpose::Enrichment;
    applied.constraints.enrichments = chain.clone();
    applied.constraints.time_basis = TimeBasis::Selected;
    applied.constraints.time_field = Some(token.clone());
    adapter.submit(applied).unwrap();
    assert!(wait_completion(&mut adapter, 1).await.result.is_ok());
    let rows = wait_page(&mut adapter, 2).await;
    assert!(
        rows.iter().all(|row| has_ready(row, "event_time")),
        "the derived stage marks every row ready"
    );
    assert!(
        rows.iter()
            .all(|row| { row.details.iter().any(|(key, _)| key == "basis_nanos") }),
        "readable instants ride the rows"
    );

    // Accept removal of the derived stage while the Selected basis still
    // names the same column, now reading the raw field.
    let mut removed = request(2, 2, 1, None);
    removed.purpose = QueryPurpose::Enrichment;
    removed.base_constraints.enrichments = chain;
    removed.base_constraints.time_basis = TimeBasis::Selected;
    removed.base_constraints.time_field = Some(token.clone());
    removed.constraints.time_basis = TimeBasis::Selected;
    removed.constraints.time_field = Some(token);
    adapter.submit(removed).unwrap();
    assert!(wait_completion(&mut adapter, 2).await.result.is_ok());
    let rows = wait_page(&mut adapter, 2).await;
    assert!(
        rows.iter().all(|row| !has_ready(row, "event_time")),
        "no accepted output means no ready cell: {:?}",
        rows[0].details
    );
    assert!(
        rows.iter().all(|row| !has_marker(row, "event_time")),
        "removal drops the declared marker too: {:?}",
        rows[0].details
    );
    assert!(
        rows.iter().all(|row| field(row, "event_time").is_some()),
        "the raw same-name field stays visible: {:?}",
        rows[0].fields
    );

    adapter.shutdown();
    manager.shutdown().await;
}
