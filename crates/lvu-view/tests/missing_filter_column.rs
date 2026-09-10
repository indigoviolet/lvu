//! Missing-column filter keeps the applied chain, rows and live append.
//!
//! A filter naming a column absent from the current typed batch must fail the
//! candidate with an actionable per-batch diagnostic
//! (`not available in this batch`, code `unknown_field`) instead of Polars'
//! lowering implementation, while the last-good applied chain, its rows and
//! later arrivals stay live. Absence here never claims a global schema and
//! never asserts a race. Heterogeneous batches keep shared-schema nulls:
//! a base field seen in one batch stays known (null) in later batches without
//! the field, so the same predicate is valid there. A stage reading a later
//! stage names the ordering.

use lvu::{
    QueryConstraints, QueryPurpose, QueryRequest, RowProvider, ViewportRequest,
    terminal::QueryDispatcher,
};
use lvu_core::{Acquisition, SourceDefinition, SourceId};
use lvu_ingest::{RuntimeConfig, SourceHandle, SourceManager};
use lvu_live::{LiveConfig, LiveRowProvider};
use lvu_query::CompilerHostConfig;
use lvu_view::{NativeViewAdapter, ScanState, ViewConfig};
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
        name: "missing-column fixture".into(),
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

/// The applied enrichment chain, its rows and live arrivals survive a
/// missing-column filter candidate, which reports per batch.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn missing_filter_column_keeps_applied_chain_rows_and_live_append() {
    let root = TempDir::new().unwrap();
    let (manager, handle, mut adapter) =
        setup(&root, "level=ERROR first\nlevel=INFO second\n", true).await;

    let chain = vec![enrichment_step(
        "copy-level",
        "error_flag = pl.col(\"level\")",
    )];
    let applied_filter = "pl.col('error_flag') == 'ERROR'";
    let mut applied = request(1, 1, 0, Some(applied_filter));
    applied.purpose = QueryPurpose::Advanced;
    applied.constraints.enrichments = chain.clone();
    adapter.submit(applied).unwrap();
    assert!(wait_completion(&mut adapter, 1).await.result.is_ok());
    assert_eq!(adapter.status("view").unwrap().matched_records, 1);
    let rows = wait_page(&mut adapter, 1).await;
    assert!(rows[0].text.contains("first"));
    let applied_revision = adapter.revision("view");

    // Same applied base, but the candidate filter names a column absent from
    // this batch. No stage produces it here; the message must stay per batch.
    let mut candidate = request(2, 2, 1, Some("pl.col('never_produced_xyz') == 'ERROR'"));
    candidate.purpose = QueryPurpose::Advanced;
    candidate.base_constraints.advanced_polars = Some(applied_filter.into());
    candidate.base_constraints.enrichments = chain.clone();
    candidate.constraints.enrichments = chain.clone();
    adapter.submit(candidate).unwrap();
    let failure = wait_completion(&mut adapter, 2).await;
    let message = failure.result.unwrap_err().message;
    assert!(
        message.contains("never_produced_xyz") && message.contains("not available in this batch"),
        "actionable per-batch diagnostic, got: {message}"
    );
    assert!(
        !message.contains("cannot be lowered") && !message.contains("unable to find column"),
        "Polars implementation detail leaked: {message}"
    );

    // Full applied chain, rows and revision survive the rejected candidate.
    assert_eq!(adapter.revision("view"), applied_revision);
    assert_eq!(adapter.status("view").unwrap().matched_records, 1);
    assert_eq!(adapter.status("view").unwrap().state, ScanState::Ready);
    let rows = wait_page(&mut adapter, 1).await;
    assert!(rows[0].text.contains("first"));

    // Live arrivals still refresh through the applied chain.
    let mut file = OpenOptions::new()
        .append(true)
        .open(root.path().join("input.log"))
        .unwrap();
    writeln!(file, "level=ERROR late").unwrap();
    file.flush().unwrap();
    wait_runtime(&handle, 3).await;
    let rows = wait_page(&mut adapter, 2).await;
    assert!(rows[1].text.contains("late"));

    adapter.shutdown();
    manager.shutdown().await;
}

/// A base field seen in one batch stays known (null) in later batches without
/// it, so the same predicate is valid there. A truly absent column is per-batch
/// unknown, not a global claim.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn heterogeneous_base_field_uses_shared_schema_nulls() {
    // First lines carry `status`; later plain lines do not. With the view's
    // shared schema the later batches project nulls, so the predicate stays
    // valid and matches only the carrying records.
    let mut contents = String::new();
    for index in 0..6 {
        contents.push_str(&format!("status=500 id={index}\n"));
    }
    for index in 0..6 {
        contents.push_str(&format!("plain hello {index}\n"));
    }
    let root = TempDir::new().unwrap();
    let (manager, _handle, mut adapter) = setup(&root, &contents, false).await;

    let mut valid = request(1, 1, 0, Some("pl.col('status') == '500'"));
    valid.purpose = QueryPurpose::Advanced;
    adapter.submit(valid).unwrap();
    let done = wait_completion(&mut adapter, 1).await;
    assert!(
        done.result.is_ok(),
        "shared-schema nulls stay valid: {done:?}"
    );
    assert_eq!(adapter.status("view").unwrap().matched_records, 6);
    let rows = wait_page(&mut adapter, 6).await;
    assert!(rows.iter().all(|row| row.text.contains("status=500")));

    // Nothing in the capture produces this column in any batch seen here; the
    // candidate still fails per batch, preserving the applied valid filter.
    let mut missing = request(2, 2, 1, Some("pl.col('never_produced_xyz') == 'x'"));
    missing.purpose = QueryPurpose::Advanced;
    missing.base_constraints.advanced_polars = Some("pl.col('status') == '500'".into());
    missing.constraints.advanced_polars = Some("pl.col('never_produced_xyz') == 'x'".into());
    adapter.submit(missing).unwrap();
    let failure = wait_completion(&mut adapter, 2).await;
    let message = failure.result.unwrap_err().message;
    assert!(
        message.contains("never_produced_xyz") && message.contains("not available in this batch"),
        "{message}"
    );
    assert_eq!(adapter.status("view").unwrap().matched_records, 6);

    adapter.shutdown();
    manager.shutdown().await;
}

/// An early stage reading a later stage names the ordering per batch.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stage_reading_later_stage_reports_ordering() {
    let root = TempDir::new().unwrap();
    let (manager, _handle, mut adapter) = setup(&root, "level=ERROR\n", false).await;

    let mut base = request(1, 1, 0, None);
    base.purpose = QueryPurpose::Enrichment;
    base.constraints.enrichments = vec![enrichment_step("later", "later = pl.lit(\"ok\")")];
    adapter.submit(base).unwrap();
    assert!(wait_completion(&mut adapter, 1).await.result.is_ok());

    let mut forward = request(2, 2, 1, None);
    forward.purpose = QueryPurpose::Enrichment;
    forward.base_constraints.enrichments = vec![enrichment_step("later", "later = pl.lit(\"ok\")")];
    forward.constraints.enrichments = vec![
        enrichment_step("early", "early = pl.col(\"later\")"),
        enrichment_step("later", "later = pl.lit(\"ok\")"),
    ];
    adapter.submit(forward).unwrap();
    let failure = wait_completion(&mut adapter, 2).await;
    let message = failure.result.unwrap_err().message;
    assert!(
        message.contains("later stage") && message.contains("stages run in order"),
        "forward reference must name the ordering: {message}"
    );
    assert!(
        !message.contains("cannot be lowered"),
        "implementation detail leaked: {message}"
    );

    adapter.shutdown();
    manager.shutdown().await;
}
