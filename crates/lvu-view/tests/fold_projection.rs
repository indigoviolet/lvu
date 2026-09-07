//! Repeated-pattern folding through the real view seam.
//!
//! The engine itself is covered by `folding.rs`. These tests exercise the
//! integration: a realistic flood collapsing in a live view, expansion
//! reproducing the original events in their original order, counts that do not
//! move when the viewport does, and the guarantee that records, filtering and
//! identity are untouched by any of it.

use lvu::{
    FoldRequest, QueryConstraints, QueryPurpose, QueryRequest, RowId, RowProvider, TextConstraint,
    ViewportRequest, terminal::QueryDispatcher,
};
use lvu_core::{Acquisition, SourceDefinition, SourceId};
use lvu_ingest::{RuntimeConfig, RuntimeState, SourceHandle, SourceManager};
use lvu_live::{IndexState, LiveConfig, LiveRowProvider};
use lvu_view::{NativeViewAdapter, RawRowSource, ViewConfig};
use std::{collections::BTreeMap, fs, sync::Arc, time::Duration};
use tempfile::TempDir;

/// Two unique events, a flood of forty near-identical retries, then two more
/// unique events. The retries differ in the parts normalisation removes.
fn fixture() -> String {
    let mut lines = vec![
        "service started on port 8080".to_owned(),
        "loaded 12 rules".to_owned(),
    ];
    for attempt in 0..40 {
        lines.push(format!(
            "retry connect to 10.0.0.{} failed after {}ms",
            attempt % 7,
            120 + attempt
        ));
    }
    lines.push("connection established".to_owned());
    lines.push("ready to serve".to_owned());
    let mut text = lines.join("\n");
    text.push('\n');
    text
}

fn definition(id: SourceId, path: &std::path::Path) -> SourceDefinition {
    SourceDefinition {
        schema_version: 1,
        id,
        name: "fold fixture".into(),
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

async fn setup(root: &TempDir, contents: &str) -> (SourceManager, NativeViewAdapter) {
    let input = root.path().join("input.log");
    fs::write(&input, contents).unwrap();
    // Defaults: an aggressive partial-flush interval publishes incomplete lines
    // as records, which is a capture concern rather than a folding one.
    let manager =
        SourceManager::new(root.path().join("capture"), RuntimeConfig::default()).unwrap();
    let handle = manager
        .start(definition(SourceId::new(), &input))
        .await
        .unwrap();
    let expected = contents.bytes().filter(|byte| *byte == b'\n').count() as u64;
    wait_runtime(&handle, expected).await;

    let mut live = LiveConfig::new(root.path().join("raw-index"));
    live.index_page_records = 32;
    live.index_page_bytes = 16_384;
    let raw: Arc<dyn RawRowSource> = Arc::new(LiveRowProvider::new(live).unwrap());
    let view = ViewConfig::new(root.path().join("view-index"));
    let mut adapter = NativeViewAdapter::with_raw_rows(Arc::clone(&raw), view).unwrap();
    adapter.register_source(handle.clone()).unwrap();
    adapter
        .register_view("view", vec![handle.source_id()])
        .unwrap();
    let source_id = handle.source_id();
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            adapter.drain_updates(64);
            if raw.source_status(source_id).is_some_and(|status| {
                status.index == IndexState::Ready && status.indexed_records >= expected
            }) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("index must settle");
    (manager, adapter)
}

/// Serve the whole stream repeatedly until every row is cached and folding has
/// consumed the stream, the way the terminal's redraw loop does.
async fn settle(adapter: &mut NativeViewAdapter, window: usize) -> Vec<lvu::DisplayRow> {
    let mut last = Vec::new();
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            adapter.drain_updates(64);
            let rows = adapter.rows();
            let page = rows.page(
                "view",
                ViewportRequest {
                    start: 0,
                    len: window,
                },
            );
            let complete = page.rows.len() == page.total.min(window);
            last = page.rows;
            let settled = rows
                .fold_summary("view")
                .is_none_or(|summary| summary.pending_rows == 0);
            if complete && settled {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("rows and folding must settle");
    last
}

fn enabled(minimum_run: usize) -> FoldRequest {
    FoldRequest {
        enabled: true,
        minimum_run,
        expanded: Vec::new(),
        ..FoldRequest::default()
    }
}

#[tokio::test]
async fn a_repeated_flood_collapses_and_expands_to_the_original_events() {
    let root = TempDir::new().unwrap();
    let contents = fixture();
    let (manager, mut adapter) = setup(&root, &contents).await;
    // Unfolded: every event is its own row, in file order.
    let unfolded = settle(&mut adapter, 64).await;
    let original: Vec<RowId> = unfolded.iter().map(|row| row.id.clone()).collect();
    assert_eq!(original.len(), 44);
    let rows = adapter.rows();

    rows.set_fold("view", &enabled(3));
    let folded = settle(&mut adapter, 64).await;
    // Two unique events, one collapsed run of forty, two unique events.
    assert_eq!(folded.len(), 5, "{folded:#?}");
    let summary = rows.fold_summary("view").unwrap();
    assert!(summary.enabled);
    assert_eq!(summary.folded_entries, 1);
    assert_eq!(summary.hidden_rows, 39);
    assert_eq!(summary.evicted_entries, 0);
    assert_eq!(summary.pending_rows, 0);
    assert_eq!(
        rows.page("view", ViewportRequest { start: 0, len: 0 })
            .total,
        5
    );

    let collapsed = &folded[2];
    assert!(collapsed.text.contains("[x40 repeated]"), "{collapsed:#?}");
    assert!(
        collapsed
            .details
            .iter()
            .any(|(key, value)| key == "fold_count" && value == "40")
    );
    assert!(
        collapsed
            .details
            .iter()
            .any(|(key, value)| key == "folding" && value.contains("physical records unchanged"))
    );
    // The collapsed line stands for a real record and keeps its identity.
    assert_eq!(collapsed.id, original[2]);

    // Every constituent stays individually addressable and unmodified.
    let members = rows.fold_members("view", &original[20]);
    assert_eq!(members.len(), 40);
    assert_eq!(members, original[2..42].to_vec());
    for id in &members {
        let row = rows.row_by_id("view", id).expect("record resolves");
        assert_eq!(&row.id, id);
        assert!(
            !row.text.contains("repeated"),
            "row_by_id must return the record, not the fold: {row:#?}"
        );
        assert!(row.details.iter().all(|(key, _)| key != "fold_count"));
    }

    // Expanding reproduces the original events, in the original order.
    rows.set_fold(
        "view",
        &FoldRequest {
            expanded: vec![original[2].clone()],
            ..enabled(3)
        },
    );
    let expanded = settle(&mut adapter, 64).await;
    let expanded_ids: Vec<RowId> = expanded.iter().map(|row| row.id.clone()).collect();
    assert_eq!(expanded_ids, original);
    assert!(
        expanded.iter().all(|row| !row.text.contains("repeated")),
        "an expanded run renders its own events"
    );

    drop(adapter);
    manager.shutdown().await;
}

#[tokio::test]
async fn counts_do_not_move_when_the_viewport_does() {
    let root = TempDir::new().unwrap();
    let (manager, mut adapter) = setup(&root, &fixture()).await;
    settle(&mut adapter, 64).await;
    let rows = adapter.rows();
    rows.set_fold("view", &enabled(3));
    settle(&mut adapter, 64).await;

    // Every window that contains the collapsed line reports the same count, and
    // the totals never move, because folding is a function of the stream and
    // the policy alone.
    let mut seen = Vec::new();
    for start in 0..5 {
        for len in 1..=5 {
            let page = rows.page("view", ViewportRequest { start, len });
            assert_eq!(page.total, 5, "start={start} len={len}");
            for row in page.rows {
                if let Some((_, count)) = row
                    .details
                    .iter()
                    .find(|(key, _)| key == "fold_count")
                    .map(|(key, value)| (key.clone(), value.clone()))
                {
                    seen.push(count);
                }
            }
        }
    }
    assert!(!seen.is_empty());
    assert!(
        seen.iter().all(|count| count == "40"),
        "a rendered count must not depend on the viewport: {seen:?}"
    );

    drop(adapter);
    manager.shutdown().await;
}

#[tokio::test]
async fn folding_changes_no_filter_result_and_no_record() {
    let root = TempDir::new().unwrap();
    let (manager, mut adapter) = setup(&root, &fixture()).await;

    let request = QueryRequest {
        view_id: "view".into(),
        generation: 1,
        revision: 1,
        base_revision: 0,
        base_constraints: QueryConstraints::default(),
        purpose: QueryPurpose::Search,
        constraints: QueryConstraints {
            text: Some(TextConstraint {
                literal: "retry connect".into(),
                case_insensitive: true,
            }),
            ..QueryConstraints::default()
        },
    };
    adapter.submit(request).unwrap();
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            adapter.drain_updates(64);
            if let Some(done) = adapter.poll() {
                assert!(done.result.is_ok(), "fixture query failed: {done:?}");
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("filter must publish");

    let matched = settle(&mut adapter, 64).await;
    assert_eq!(matched.len(), 40, "the filter matches the forty retries");
    let matched_ids: Vec<RowId> = matched.iter().map(|row| row.id.clone()).collect();
    let rows = adapter.rows();

    rows.set_fold("view", &enabled(3));
    settle(&mut adapter, 64).await;
    let folded_total = rows
        .page("view", ViewportRequest { start: 0, len: 0 })
        .total;
    assert_eq!(folded_total, 1, "the matched run collapses to one line");
    // What the filter matched is unchanged: the fold's constituents are exactly
    // the matched records, in the same order.
    let members = rows.fold_members("view", &matched_ids[0]);
    assert_eq!(members, matched_ids);

    // Turning folding off restores the individual rows with no requery.
    rows.set_fold("view", &FoldRequest::default());
    let restored = rows.page("view", ViewportRequest { start: 0, len: 64 });
    assert_eq!(restored.total, 40);
    assert_eq!(
        restored
            .rows
            .iter()
            .map(|row| row.id.clone())
            .collect::<Vec<_>>(),
        matched_ids
    );

    drop(adapter);
    manager.shutdown().await;
}

#[tokio::test]
async fn selection_positions_follow_the_folded_stream() {
    let root = TempDir::new().unwrap();
    let (manager, mut adapter) = setup(&root, &fixture()).await;
    let unfolded = settle(&mut adapter, 64).await;
    let ids: Vec<RowId> = unfolded.iter().map(|row| row.id.clone()).collect();
    let rows = adapter.rows();
    for (position, id) in ids.iter().enumerate() {
        assert_eq!(rows.index_of_id("view", id), Some(position));
    }

    rows.set_fold("view", &enabled(3));
    settle(&mut adapter, 64).await;
    // Head events keep their positions; every member of the collapsed run
    // answers with the position of the line that stands for it; the events
    // after it shift up by the rows the fold hides.
    assert_eq!(rows.index_of_id("view", &ids[0]), Some(0));
    assert_eq!(rows.index_of_id("view", &ids[1]), Some(1));
    for id in &ids[2..42] {
        assert_eq!(rows.index_of_id("view", id), Some(2));
    }
    assert_eq!(rows.index_of_id("view", &ids[42]), Some(3));
    assert_eq!(rows.index_of_id("view", &ids[43]), Some(4));

    rows.set_fold(
        "view",
        &FoldRequest {
            expanded: vec![ids[2].clone()],
            ..enabled(3)
        },
    );
    settle(&mut adapter, 64).await;
    for (position, id) in ids.iter().enumerate() {
        assert_eq!(
            rows.index_of_id("view", id),
            Some(position),
            "an expanded run restores every original position"
        );
    }

    drop(adapter);
    manager.shutdown().await;
}

/// Page through the whole stream in viewport-sized windows until folding has
/// consumed it. Serving hundreds of rows in one page is not something the
/// terminal ever asks for.
async fn feed_all(adapter: &mut NativeViewAdapter, total: usize) {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            adapter.drain_updates(64);
            let rows = adapter.rows();
            let mut start = 0;
            while start < total {
                rows.page("view", ViewportRequest { start, len: 64 });
                start += 64;
            }
            if rows
                .fold_summary("view")
                .is_none_or(|summary| summary.pending_rows == 0)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("folding must consume the stream");
}

#[tokio::test]
async fn a_stream_that_never_repeats_stays_bounded_and_unfolded() {
    let root = TempDir::new().unwrap();
    // Distinct shapes, not just distinct values: normalisation replaces numbers,
    // so a stream that only varies numerically is a repeated pattern.
    const WORDS: [&str; 23] = [
        "alpha", "bravo", "charlie", "delta", "echo", "foxtrot", "golf", "hotel", "india",
        "juliet", "kilo", "lima", "mike", "november", "oscar", "papa", "quebec", "romeo", "sierra",
        "tango", "uniform", "victor", "whiskey",
    ];
    let mut text = String::new();
    for index in 0..240usize {
        let line: Vec<&str> = (0..=(index % 7))
            .map(|offset| WORDS[(index * 3 + offset) % WORDS.len()])
            .collect();
        text.push_str(&line.join(" "));
        text.push('\n');
    }
    let (manager, mut adapter) = setup(&root, &text).await;
    feed_all(&mut adapter, 240).await;
    let rows = adapter.rows();
    rows.set_fold("view", &enabled(3));
    feed_all(&mut adapter, 240).await;

    let summary = rows.fold_summary("view").unwrap();
    assert_eq!(summary.folded_entries, 0, "nothing repeats, nothing folds");
    assert_eq!(summary.hidden_rows, 0);
    assert_eq!(summary.entries, 240);
    assert_eq!(
        rows.page("view", ViewportRequest { start: 0, len: 0 })
            .total,
        240,
        "a high-cardinality stream is served exactly as it is"
    );

    drop(adapter);
    manager.shutdown().await;
}

#[tokio::test]
async fn a_run_folds_identically_however_the_stream_is_batched() {
    // Feeding is incremental and bounded, so a long flood arrives in several
    // batches. Batch boundaries must not split a run: the count a user reads is
    // the run's real length, not the length of the batch it happened to land in.
    let root = TempDir::new().unwrap();
    let mut text = String::new();
    for attempt in 0..600 {
        text.push_str(&format!("upstream timeout after {}ms, retrying\n", attempt));
    }
    let (manager, mut adapter) = setup(&root, &text).await;
    feed_all(&mut adapter, 600).await;
    let rows = adapter.rows();
    rows.set_fold("view", &enabled(3));
    feed_all(&mut adapter, 600).await;

    let summary = rows.fold_summary("view").unwrap();
    assert_eq!(summary.entries, 1, "one shape, one entry: {summary:?}");
    assert_eq!(summary.folded_entries, 1);
    assert_eq!(summary.hidden_rows, 599);
    assert_eq!(
        rows.page("view", ViewportRequest { start: 0, len: 0 })
            .total,
        1
    );
    let page = rows.page("view", ViewportRequest { start: 0, len: 4 });
    assert_eq!(page.rows.len(), 1);
    assert!(
        page.rows[0].text.contains("[x600 repeated]"),
        "{:#?}",
        page.rows[0]
    );

    drop(adapter);
    manager.shutdown().await;
}

/// Every record identity the frozen input replays, in replay order. This is the
/// seam the assistance sampler and snapshot export both read.
fn replayed_ids(input: &lvu_view::FrozenInput) -> Vec<String> {
    tokio::task::block_in_place(|| replay_ids_blocking(input))
}

fn replay_ids_blocking(input: &lvu_view::FrozenInput) -> Vec<String> {
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let mut ids = Vec::new();
    input
        .visit(&cancel, |batch| {
            for row in batch.rows {
                ids.push(format!(
                    "{}:{}",
                    row.record.record_id.source_id.0, row.record.record_id.sequence
                ));
            }
            Ok(())
        })
        .expect("frozen input replays");
    ids
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn assistance_sampling_and_export_cannot_observe_folding() {
    // Folding is a projection over the display stream. The sampler and export
    // read the accepted view, not that projection, so turning folding on must
    // not change one row of what a 🧠 prompt is shown or what an investigation
    // exports. This is the invariant whose absence let a display option change
    // sampled data.
    let root = TempDir::new().unwrap();
    let (manager, mut adapter) = setup(&root, &fixture()).await;
    settle(&mut adapter, 64).await;
    let limits = lvu_view::FrozenInputLimits::default();

    let unfolded_input = adapter.freeze_input("view", limits).unwrap();
    let unfolded_summary = unfolded_input.summary().clone();
    let unfolded_ids = replayed_ids(&unfolded_input);
    drop(unfolded_input);
    assert_eq!(unfolded_ids.len(), 44);

    adapter.rows().set_fold("view", &enabled(3));
    settle(&mut adapter, 64).await;
    let summary = adapter.rows().fold_summary("view").unwrap();
    assert_eq!(
        summary.folded_entries, 1,
        "the fixture must actually be folded for this to prove anything"
    );
    assert_eq!(summary.hidden_rows, 39);
    assert_eq!(
        adapter
            .rows()
            .page("view", ViewportRequest { start: 0, len: 0 })
            .total,
        5,
        "the display stream really is collapsed while this is measured"
    );

    let folded_input = adapter.freeze_input("view", limits).unwrap();
    assert_eq!(
        folded_input.summary().sources,
        unfolded_summary.sources,
        "folding must not change the frozen input's sources"
    );
    assert_eq!(
        folded_input.summary().selected_records,
        unfolded_summary.selected_records
    );
    assert_eq!(
        replayed_ids(&folded_input),
        unfolded_ids,
        "the sampler and export must replay exactly the same rows, in the same \
         order, whether or not the view is folded"
    );
    drop(folded_input);

    // Expanding a run is also presentation and changes nothing here either.
    let head = unfolded_ids[2].clone();
    let expanded_head = RowId::new(
        head.rsplit_once(':').unwrap().0.to_owned(),
        head.rsplit_once(':').unwrap().1.parse().unwrap(),
    );
    adapter.rows().set_fold(
        "view",
        &FoldRequest {
            expanded: vec![expanded_head],
            ..enabled(3)
        },
    );
    settle(&mut adapter, 64).await;
    let expanded_input = adapter.freeze_input("view", limits).unwrap();
    assert_eq!(replayed_ids(&expanded_input), unfolded_ids);
    drop(expanded_input);

    drop(adapter);
    manager.shutdown().await;
}

#[tokio::test]
async fn snapshot_export_sees_the_unfolded_stream() {
    // Export freezes the view's membership, not its presentation. Folding must
    // not change what an investigation or a snapshot would contain.
    let root = TempDir::new().unwrap();
    let (manager, mut adapter) = setup(&root, &fixture()).await;
    settle(&mut adapter, 64).await;

    let limits = lvu_view::FrozenInputLimits::default();
    let before = adapter.freeze_input("view", limits).unwrap();
    let before_sources = before.summary().sources.clone();
    let before_selected = before.summary().selected_records;
    drop(before);

    adapter.rows().set_fold("view", &enabled(3));
    settle(&mut adapter, 64).await;
    assert_eq!(
        adapter
            .rows()
            .fold_summary("view")
            .map(|summary| summary.folded_entries),
        Some(1),
        "the fixture must actually be folded for this to mean anything"
    );

    let after = adapter.freeze_input("view", limits).unwrap();
    assert_eq!(
        after.summary().sources,
        before_sources,
        "folding is presentation; a snapshot still covers every record"
    );
    assert_eq!(after.summary().selected_records, before_selected);
    drop(after);

    drop(adapter);
    manager.shutdown().await;
}

#[tokio::test]
async fn the_unfolded_page_ignores_the_fold_whatever_the_policy_is() {
    // Every sampling consumer — assistance context, recipe suggestions, editor
    // completion, the enrichment preview — reads this, so it must be identical
    // whether the view is folded, expanded or unfolded.
    let root = TempDir::new().unwrap();
    let (manager, mut adapter) = setup(&root, &fixture()).await;
    settle(&mut adapter, 64).await;
    let rows = adapter.rows();
    let request = ViewportRequest { start: 0, len: 64 };
    let baseline = rows.unfolded_page("view", request);
    assert_eq!(baseline.total, 44);
    let baseline_ids: Vec<RowId> = baseline.rows.iter().map(|row| row.id.clone()).collect();
    let baseline_text: Vec<String> = baseline.rows.iter().map(|row| row.text.clone()).collect();

    for policy in [
        enabled(3),
        FoldRequest {
            expanded: vec![baseline_ids[2].clone()],
            ..enabled(3)
        },
        FoldRequest::default(),
    ] {
        rows.set_fold("view", &policy);
        settle(&mut adapter, 64).await;
        let sampled = rows.unfolded_page("view", request);
        assert_eq!(sampled.total, baseline.total, "policy {policy:?}");
        assert_eq!(
            sampled
                .rows
                .iter()
                .map(|row| row.id.clone())
                .collect::<Vec<_>>(),
            baseline_ids,
            "policy {policy:?} changed which rows are sampled"
        );
        assert_eq!(
            sampled
                .rows
                .iter()
                .map(|row| row.text.clone())
                .collect::<Vec<_>>(),
            baseline_text,
            "policy {policy:?} decorated a sampled row"
        );
    }

    drop(adapter);
    manager.shutdown().await;
}

/// A stream where the text of every row is different but two services repeat.
/// Nothing in it folds on the derived pattern column.
fn service_fixture() -> String {
    let mut lines = Vec::new();
    for sequence in 0..12 {
        let service = if sequence % 2 == 0 {
            "shipper"
        } else {
            "indexer"
        };
        lines.push(format!(
            "svc={service} step {sequence} did something quite unlike the others {}",
            "abcdefghijkl".chars().nth(sequence).unwrap()
        ));
    }
    let mut text = lines.join("\n");
    text.push('\n');
    text
}

/// The product claim end to end: an enrichment column becomes the fold key, and
/// its value is used as it stands.
///
/// The enrichment is the slash form, so this exercises the real chain without
/// needing the Python compiler host.
#[tokio::test]
async fn an_enrichment_column_becomes_the_fold_key() {
    let root = TempDir::new().unwrap();
    let (manager, mut adapter) = setup(&root, &service_fixture()).await;

    adapter
        .submit(QueryRequest {
            view_id: "view".into(),
            generation: 1,
            revision: 1,
            base_revision: 0,
            base_constraints: QueryConstraints::default(),
            purpose: QueryPurpose::Enrichment,
            constraints: QueryConstraints {
                enrichments: vec![lvu::EnrichmentDefinition {
                    id: lvu::EnrichmentStageId("stage-1".into()),
                    source: r"/svc=(?P<service>\w+)/".into(),
                }],
                ..QueryConstraints::default()
            },
        })
        .unwrap();
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            adapter.drain_updates(64);
            if let Some(done) = adapter.poll() {
                assert!(done.result.is_ok(), "enrichment failed: {done:?}");
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the enrichment must publish");

    let enriched = settle(&mut adapter, 64).await;
    assert_eq!(enriched.len(), 12);
    assert!(
        enriched
            .iter()
            .all(|row| row.fields.iter().any(|(name, _)| name == "service")),
        "the enrichment column must reach the display row"
    );
    let rows = adapter.rows();

    // The default key folds nothing here: every row's text is different.
    rows.set_fold("view", &enabled(2));
    settle(&mut adapter, 64).await;
    assert_eq!(
        rows.page("view", ViewportRequest { start: 0, len: 0 })
            .total,
        12,
        "the derived pattern column has nothing to collapse"
    );

    // Keyed on the enrichment column with a lookback wide enough to see past
    // the interleaving, the two services collapse to two counted lines.
    let by_service = FoldRequest {
        key_column: Some("service".into()),
        scope: lvu::FoldScopeRequest::Lookback(4),
        ..enabled(2)
    };
    rows.set_fold("view", &by_service);
    settle(&mut adapter, 64).await;
    let summary = rows.fold_summary("view").expect("folding is on");
    assert_eq!(summary.folded_entries, 2, "one line per service");
    assert_eq!(summary.hidden_rows, 10);

    // Presentation only: every record is still individually addressable, and
    // the members of a run are exactly the rows that carried its value.
    let members = rows.fold_members("view", &enriched[0].id);
    assert_eq!(members.len(), 6);
    assert!(
        members
            .iter()
            .all(|id| enriched.iter().any(|row| &row.id == id)),
        "a fold never invents a record"
    );
    // Every row still resolves to a display position inside the folded stream.
    // A lookback window interleaves the runs, and a position that fell outside
    // the stream would put the selection somewhere the user cannot see.
    let folded_total = rows
        .page("view", ViewportRequest { start: 0, len: 0 })
        .total;
    for row in &enriched {
        let index = rows
            .index_of_id("view", &row.id)
            .unwrap_or_else(|| panic!("{} has no display position", row.id));
        assert!(
            index < folded_total,
            "{} resolved to {index} of {folded_total}",
            row.id
        );
    }

    // Sampling still sees the unfolded stream, whatever the key is.
    assert_eq!(
        rows.unfolded_page("view", ViewportRequest { start: 0, len: 64 })
            .total,
        12
    );

    // A key column no row carries folds nothing rather than folding everything.
    rows.set_fold(
        "view",
        &FoldRequest {
            key_column: Some("absent".into()),
            ..enabled(2)
        },
    );
    settle(&mut adapter, 64).await;
    assert_eq!(
        rows.page("view", ViewportRequest { start: 0, len: 0 })
            .total,
        12,
        "a column the rows do not carry must not collapse them together"
    );

    drop(adapter);
    manager.shutdown().await;
}
