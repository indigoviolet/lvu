//! Column-based colour classification through the public view adapter.
//!
//! Colour rules classify accepted enrichment outputs by exact ready-cell
//! equality: patterns and keys belong in ordinary enrichment definitions,
//! so a rule never evaluates an independent pattern of its own. These tests
//! pin the worker side of that contract — exact (case-sensitive) matches on
//! ready cells, no match for null/failed/missing cells or removed outputs,
//! first-match-wins against legacy predicate rules, and unchanged membership.

use lvu::{
    QueryConstraints, QueryPurpose, QueryRequest, RowProvider, ViewportRequest,
    terminal::QueryDispatcher,
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
        name: "colour classification fixture".into(),
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

fn request(generation: u64, revision: u64, base_revision: u64) -> QueryRequest {
    QueryRequest {
        view_id: "view".into(),
        generation,
        revision,
        base_revision,
        base_constraints: QueryConstraints::default(),
        purpose: QueryPurpose::Search,
        constraints: QueryConstraints {
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
    adapter.register_source(lvu_shared::AnySourceHandle::Local(handle.clone())).unwrap();
    adapter
        .register_view("view", vec![handle.source_id()])
        .unwrap();
    (manager, handle, adapter)
}

fn rule_of(row: &lvu::DisplayRow) -> Option<String> {
    row.details
        .iter()
        .find(|(key, _)| key == "color_rule")
        .map(|(_, value)| value.clone())
}

/// A column rule paints exactly the rows whose ready cell equals its value —
/// case-sensitively, and never null, failed or missing cells — without
/// narrowing the view. A legacy predicate rule later in the list still paints
/// what it matches, in order.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn column_rules_match_exact_ready_cells_in_order() {
    let root = TempDir::new().unwrap();
    let (manager, _handle, mut adapter) = setup(
        &root,
        "level=warn s=oops id=first\n\
         level=info s=42 id=second\n\
         level=error s=7 id=third\n",
        false,
    )
    .await;

    let mut painted = request(1, 1, 0);
    painted.purpose = QueryPurpose::Enrichment;
    painted.constraints.enrichments = vec![
        enrichment_step("upper", "severity = pl.col(\"level\").str.to_uppercase()"),
        enrichment_step("num", "n = pl.col(\"s\").cast(pl.Int64, strict=False)"),
    ];
    // Column rule first: exact `ERROR` wins for the third row even though the
    // legacy literal below also matches it. The lowercase `warn` value rule
    // matches nothing: classification is case-sensitive, unlike search.
    painted.constraints.color_rules = vec![
        lvu::ColorRule::column_rule("severity".into(), "ERROR".into(), lvu::RuleColor::Red),
        lvu::ColorRule::column_rule("severity".into(), "warn".into(), lvu::RuleColor::Blue),
        lvu::ColorRule {
            predicate: "id=second".into(),
            color: lvu::RuleColor::Green,
            column: None,
            value: None,
        },
    ];
    adapter.submit(painted).unwrap();
    assert!(wait_completion(&mut adapter, 1).await.result.is_ok());
    let rows = wait_page(&mut adapter, 3).await;
    assert_eq!(rows.len(), 3, "painting does not narrow the view");
    let first = rows
        .iter()
        .find(|row| row.text.contains("id=first"))
        .expect("first record served");
    let second = rows
        .iter()
        .find(|row| row.text.contains("id=second"))
        .expect("second record served");
    let third = rows
        .iter()
        .find(|row| row.text.contains("id=third"))
        .expect("third record served");
    // `severity` is WARN on the first row: no exact rule matches, and the
    // legacy literal names another row.
    assert_eq!(rule_of(first), None);
    // The second row's legacy literal matches (its `n` cell is ready `42`,
    // which no column rule names).
    assert_eq!(rule_of(second).as_deref(), Some("3"));
    // Exact, case-sensitive, first-wins.
    assert_eq!(rule_of(third).as_deref(), Some("1"));

    adapter.shutdown();
    manager.shutdown().await;
}

/// Null cells never match a column rule — not even a rule naming their
/// display text — while ready siblings on the same rows do.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn column_rules_skip_null_cells() {
    let root = TempDir::new().unwrap();
    let (manager, _handle, mut adapter) = setup(
        &root,
        "level=warn s=oops first\nlevel=info s=42 second\n",
        false,
    )
    .await;

    let mut painted = request(1, 1, 0);
    painted.purpose = QueryPurpose::Enrichment;
    painted.constraints.enrichments = vec![
        enrichment_step("ok", "severity = pl.col(\"level\").str.to_uppercase()"),
        enrichment_step("soft", "n = pl.col(\"s\").cast(pl.Int64, strict=False)"),
    ];
    painted.constraints.color_rules = vec![
        lvu::ColorRule::column_rule("severity".into(), "WARN".into(), lvu::RuleColor::Red),
        // Names the null display text: must still match nothing.
        lvu::ColorRule::column_rule("n".into(), "null".into(), lvu::RuleColor::Blue),
        lvu::ColorRule::column_rule("n".into(), "42".into(), lvu::RuleColor::Green),
    ];
    adapter.submit(painted).unwrap();
    assert!(wait_completion(&mut adapter, 1).await.result.is_ok());
    let rows = wait_page(&mut adapter, 2).await;
    let first = rows
        .iter()
        .find(|row| row.text.contains("first"))
        .expect("first record served");
    // Ready WARN wins (rule 1) over the null-`n` rule naming "null".
    assert_eq!(rule_of(first).as_deref(), Some("1"));
    let second = rows
        .iter()
        .find(|row| row.text.contains("second"))
        .expect("second record served");
    // Ready INFO is unnamed; ready `42` paints green.
    assert_eq!(rule_of(second).as_deref(), Some("3"));

    adapter.shutdown();
    manager.shutdown().await;
}

/// Failed cells never match a column rule — not even one naming their error
/// text — while the ready sibling cell on the same row still classifies.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn column_rules_skip_failed_cells() {
    let root = TempDir::new().unwrap();
    let (manager, handle, mut adapter) =
        setup(&root, "level=warn s=1 first\nlevel=info s=2 second\n", true).await;

    let mut painted = request(1, 1, 0);
    painted.purpose = QueryPurpose::Enrichment;
    painted.constraints.enrichments = vec![
        enrichment_step("ok", "severity = pl.col(\"level\").str.to_uppercase()"),
        enrichment_step("strict", "n = pl.col(\"s\").cast(pl.Int64, strict=True)"),
    ];
    painted.constraints.color_rules = vec![
        lvu::ColorRule::column_rule("severity".into(), "ERROR".into(), lvu::RuleColor::Red),
        lvu::ColorRule::column_rule("n".into(), "oops".into(), lvu::RuleColor::Blue),
        lvu::ColorRule::column_rule("n".into(), "2".into(), lvu::RuleColor::Green),
    ];
    adapter.submit(painted).unwrap();
    assert!(wait_completion(&mut adapter, 1).await.result.is_ok());
    let rows = wait_page(&mut adapter, 2).await;
    let second = rows
        .iter()
        .find(|row| row.text.contains("second"))
        .expect("second record served");
    assert_eq!(rule_of(second).as_deref(), Some("3"));

    // A late arrival the strict stage cannot cast fails that cell: the
    // failure carries error text no rule may match, while the ready
    // `severity` cell on the same row still classifies first.
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
    assert_eq!(
        rule_of(late).as_deref(),
        Some("1"),
        "ready ERROR classifies while failed `n` matches nothing: {:?}",
        late.details
    );

    adapter.shutdown();
    manager.shutdown().await;
}

/// Malformed column shapes reject the candidate with an indexed diagnostic
/// while the last good view stays applied; a valid empty-string value
/// applies and matches only literal empty-string ready cells.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn malformed_column_shapes_reject_the_candidate() {
    let root = TempDir::new().unwrap();
    let (manager, _handle, mut adapter) = setup(&root, "level=warn first\n", false).await;

    // A column binding with no value is malformed: restore bypasses the
    // dialog validator, so execution must refuse loudly instead of keeping
    // a rule that paints nothing.
    let mut missing = request(1, 1, 0);
    missing.purpose = QueryPurpose::Advanced;
    missing.constraints.color_rules = vec![lvu::ColorRule {
        predicate: String::new(),
        color: lvu::RuleColor::Red,
        column: Some("severity".into()),
        value: None,
    }];
    adapter.submit(missing).unwrap();
    let failure = wait_completion(&mut adapter, 1).await.result.unwrap_err();
    assert!(
        failure.message.contains("colour rule 1") && failure.message.contains("no value"),
        "indexed refusal, not a silent no-op: {failure:?}"
    );

    // A blank column is malformed for the same reason.
    let mut blank = request(2, 2, 0);
    blank.purpose = QueryPurpose::Advanced;
    blank.constraints.color_rules = vec![lvu::ColorRule {
        predicate: String::new(),
        color: lvu::RuleColor::Red,
        column: Some("   ".into()),
        value: Some("x".into()),
    }];
    adapter.submit(blank).unwrap();
    let failure = wait_completion(&mut adapter, 2).await.result.unwrap_err();
    assert!(
        failure.message.contains("colour rule 1") && failure.message.contains("no column"),
        "indexed refusal, not a silent no-op: {failure:?}"
    );

    // An empty-string value is valid end to end: it matches only literal
    // empty-string ready cells, distinctly from the missing value above.
    let mut empty = request(3, 3, 0);
    empty.purpose = QueryPurpose::Enrichment;
    empty.constraints.enrichments = vec![enrichment_step("blank", "e = pl.lit(\"\")")];
    empty.constraints.color_rules = vec![lvu::ColorRule::column_rule(
        "e".into(),
        String::new(),
        lvu::RuleColor::Green,
    )];
    adapter.submit(empty).unwrap();
    assert!(wait_completion(&mut adapter, 3).await.result.is_ok());
    let rows = wait_page(&mut adapter, 1).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rule_of(&rows[0]).as_deref(),
        Some("1"),
        "empty wants match empty ready cells: {:?}",
        rows[0].details
    );

    adapter.shutdown();
    manager.shutdown().await;
}

/// Removing only the classified stage stops painting even with a raw
/// same-name field present: the lingering rule names no compiled accepted
/// output, so the raw projection must not satisfy it — while an unrelated
/// retained stage keeps evaluating and membership is untouched.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn removed_output_unpaints_without_narrowing() {
    let root = TempDir::new().unwrap();
    let (manager, _handle, mut adapter) =
        setup(&root, "severity=low first\nseverity=high second\n", false).await;

    let shout = enrichment_step(
        "shout",
        "severity = pl.col(\"severity\").str.to_uppercase()",
    );
    let other = enrichment_step("other", "marker = pl.lit(\"kept\")");
    let rule = lvu::ColorRule::column_rule("severity".into(), "LOW".into(), lvu::RuleColor::Red);
    let mut applied = request(1, 1, 0);
    applied.purpose = QueryPurpose::Enrichment;
    applied.constraints.enrichments = vec![shout.clone(), other.clone()];
    applied.constraints.color_rules = vec![rule.clone()];
    adapter.submit(applied).unwrap();
    assert!(wait_completion(&mut adapter, 1).await.result.is_ok());
    let rows = wait_page(&mut adapter, 2).await;
    assert_eq!(rule_of(&rows[0]).as_deref(), Some("1"));

    // Remove only the classified stage: the unrelated stage stays accepted
    // and the raw `severity=low` field stays visible, but nothing paints.
    let mut removed = request(2, 2, 1);
    removed.purpose = QueryPurpose::Enrichment;
    removed.base_constraints.enrichments = vec![shout, other.clone()];
    removed.base_constraints.color_rules = vec![rule];
    removed.constraints.enrichments = vec![other];
    removed.constraints.color_rules = vec![lvu::ColorRule::column_rule(
        "severity".into(),
        "LOW".into(),
        lvu::RuleColor::Red,
    )];
    adapter.submit(removed).unwrap();
    assert!(wait_completion(&mut adapter, 2).await.result.is_ok());
    let rows = wait_page(&mut adapter, 2).await;
    assert_eq!(rows.len(), 2, "unpainting does not narrow the view");
    assert!(
        rows.iter().all(|row| rule_of(row).is_none()),
        "the raw same-name field feeds no rule: {:?}",
        rows[0].details
    );
    assert!(
        rows.iter().all(|row| {
            row.fields
                .iter()
                .any(|(field, value)| field == "marker" && value == "kept")
        }),
        "the retained stage keeps evaluating: {:?}",
        rows[0].fields
    );

    adapter.shutdown();
    manager.shutdown().await;
}
