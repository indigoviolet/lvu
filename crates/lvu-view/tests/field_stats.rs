//! Whole-view field statistics: a bounded, cancellable pass over the membership.

use lvu::{
    QueryConstraints, QueryPurpose, QueryRequest, TextConstraint, terminal::QueryDispatcher,
};
use lvu_core::{Acquisition, SourceDefinition, SourceId};
use lvu_ingest::{RuntimeConfig, SourceManager};
use lvu_live::{LiveConfig, LiveRowProvider};
use lvu_view::{FieldStats, FieldStatsRequest, NativeViewAdapter, StatsType, ViewConfig};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{BufWriter, Write},
    sync::Arc,
    time::Duration,
};
use tempfile::TempDir;

const RECORDS: usize = 4_000;

/// `status` takes one of four values, unevenly, so top-N ordering and distinct
/// counts have something to be right or wrong about. Every tenth record is an
/// error, and one in a hundred carries no `status` at all.
fn line(index: usize) -> String {
    let status = match index % 10 {
        0 => 500,
        1 | 2 => 404,
        3 => 301,
        _ => 200,
    };
    if index % 100 == 99 {
        format!("{{\"seq\":{index},\"service\":\"api\"}}\n")
    } else {
        format!("{{\"seq\":{index},\"service\":\"api\",\"status\":{status}}}\n")
    }
}

fn source(id: SourceId, path: &std::path::Path) -> SourceDefinition {
    SourceDefinition {
        schema_version: 1,
        id,
        name: "stats fixture".into(),
        acquisition: Acquisition::File {
            path: path.to_owned(),
            follow: false,
        },
        identity_hints: BTreeMap::new(),
        retention: None,
    }
}

struct Harness {
    _root: TempDir,
    manager: SourceManager,
    adapter: NativeViewAdapter,
}

async fn harness(filter: Option<&str>) -> Harness {
    let root = TempDir::new().unwrap();
    let path = root.path().join("stats.log");
    {
        let mut file = BufWriter::new(File::create(&path).unwrap());
        for index in 0..RECORDS {
            file.write_all(line(index).as_bytes()).unwrap();
        }
        file.flush().unwrap();
    }
    let manager =
        SourceManager::new(root.path().join("capture"), RuntimeConfig::default()).unwrap();
    let handle = manager.start(source(SourceId::new(), &path)).await.unwrap();
    let mut progress = handle.subscribe();
    tokio::time::timeout(Duration::from_secs(120), async {
        while progress.borrow().records < RECORDS as u64 {
            progress.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    let live = LiveConfig::new(root.path().join("raw-index"));
    let view = ViewConfig::new(root.path().join("view-index"));
    let raw = Arc::new(LiveRowProvider::new(live).unwrap());
    let mut adapter = NativeViewAdapter::new(raw, view).unwrap();
    adapter.register_source(handle.clone()).unwrap();
    adapter
        .register_view("view", vec![handle.source_id()])
        .unwrap();
    if let Some(literal) = filter {
        adapter
            .submit(QueryRequest {
                view_id: "view".into(),
                generation: 1,
                revision: 1,
                base_revision: 0,
                base_constraints: QueryConstraints::default(),
                purpose: QueryPurpose::Search,
                constraints: QueryConstraints {
                    text: Some(TextConstraint {
                        literal: literal.into(),
                        case_insensitive: true,
                    }),
                    ..QueryConstraints::default()
                },
            })
            .unwrap();
        let completion = tokio::time::timeout(Duration::from_secs(120), async {
            loop {
                adapter.drain_updates(256);
                if let Some(done) = adapter.poll()
                    && done.revision == 1
                {
                    break done;
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap();
        assert!(completion.result.is_ok(), "{:?}", completion.result);
    }
    Harness {
        _root: root,
        manager,
        adapter,
    }
}

async fn settle(adapter: &mut NativeViewAdapter) -> Vec<FieldStats> {
    tokio::time::timeout(Duration::from_secs(120), async {
        loop {
            adapter.drain_updates(256);
            let taken = adapter.take_field_stats();
            if !taken.is_empty() {
                break taken;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .expect("a field statistics pass finished")
}

fn request(generation: u64, kind: StatsType) -> FieldStatsRequest {
    FieldStatsRequest {
        generation,
        view_id: "view".into(),
        column: "status".into(),
        kind,
        top: 5,
        distinct_cap: 4096,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn statistics_cover_every_record_of_a_raw_view() {
    let mut harness = harness(None).await;
    harness
        .adapter
        .submit_field_stats(request(1, StatsType::Integer))
        .unwrap();
    let stats = settle(&mut harness.adapter).await;
    let stats = &stats[0];
    let aggregate = stats.result.as_ref().expect("statistics");
    assert_eq!(stats.column, "status");
    // One record in a hundred carries no `status`; the rest do.
    assert_eq!(aggregate.rows, RECORDS as u64);
    assert_eq!(aggregate.present, (RECORDS - RECORDS / 100) as u64);
    assert_eq!(aggregate.distinct, 4);
    assert!(!aggregate.distinct_capped);
    // 200 is six in ten, 404 two, 500 and 301 one each.
    assert_eq!(aggregate.top[0].0, "200");
    assert!(
        aggregate.top[0].1 > aggregate.top[1].1,
        "top values come most frequent first: {:?}",
        aggregate.top
    );
    // Integer ordering, not lexical: "301" < "500" either way, but "1000"
    // would not be. The cast is what makes this an integer comparison.
    assert_eq!(aggregate.minimum.as_deref(), Some("200"));
    assert_eq!(aggregate.maximum.as_deref(), Some("500"));
    assert_eq!(
        aggregate.matching, aggregate.present,
        "every present status parses as the integer the app said it was"
    );
    harness.adapter.shutdown();
    harness.manager.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn statistics_cover_the_membership_of_a_filtered_view_and_nothing_else() {
    // Only records spelling 500 survive the filter, so the whole-view figures
    // must describe those and not the source they came from.
    let mut harness = harness(Some("\"status\":500")).await;
    let matched = harness.adapter.status("view").unwrap().matched_records;
    assert!(matched > 0 && matched < RECORDS as u64, "matched {matched}");
    harness
        .adapter
        .submit_field_stats(request(1, StatsType::Integer))
        .unwrap();
    let stats = settle(&mut harness.adapter).await;
    let aggregate = stats[0].result.as_ref().expect("statistics");
    assert_eq!(aggregate.rows, matched, "the pass read the membership");
    assert_eq!(aggregate.present, matched);
    assert_eq!(aggregate.distinct, 1);
    assert_eq!(aggregate.top, vec![("500".to_owned(), matched)]);
    assert_eq!(aggregate.minimum.as_deref(), Some("500"));
    assert_eq!(aggregate.maximum.as_deref(), Some("500"));
    harness.adapter.shutdown();
    harness.manager.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_superseded_pass_never_reports() {
    let mut harness = harness(None).await;
    harness
        .adapter
        .submit_field_stats(request(1, StatsType::Integer))
        .unwrap();
    // The selection moved: a second request supersedes the first, and the
    // first must not deliver an answer about a field nobody is looking at.
    harness
        .adapter
        .submit_field_stats(request(2, StatsType::Integer))
        .unwrap();
    let stats = settle(&mut harness.adapter).await;
    assert_eq!(stats.len(), 1, "one answer, for the live question");
    assert_eq!(stats[0].generation, 2);
    // Nothing late arrives from the superseded pass either.
    for _ in 0..40 {
        harness.adapter.drain_updates(256);
        assert!(
            harness.adapter.take_field_stats().is_empty(),
            "a superseded pass reported after the fact"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    harness.adapter.shutdown();
    harness.manager.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancelling_leaves_nothing_behind() {
    let mut harness = harness(None).await;
    harness
        .adapter
        .submit_field_stats(request(1, StatsType::Integer))
        .unwrap();
    harness.adapter.cancel_field_stats();
    for _ in 0..40 {
        harness.adapter.drain_updates(256);
        assert!(
            harness.adapter.take_field_stats().is_empty(),
            "a cancelled pass published a result"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    harness.adapter.shutdown();
    harness.manager.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_field_absent_from_a_record_is_absent_not_an_error() {
    // `service` is on every record; `missing` is on none. Asking about a field
    // no record carries has to answer, not fail.
    let mut harness = harness(None).await;
    let mut ask = request(1, StatsType::Text);
    ask.column = "missing".into();
    harness.adapter.submit_field_stats(ask).unwrap();
    let stats = settle(&mut harness.adapter).await;
    let aggregate = stats[0].result.as_ref().expect("statistics, not an error");
    assert_eq!(aggregate.rows, RECORDS as u64);
    assert_eq!(aggregate.present, 0);
    assert_eq!(aggregate.distinct, 0);
    assert!(aggregate.top.is_empty());
    harness.adapter.shutdown();
    harness.manager.shutdown().await;
}

/// How long a whole-view pass takes at scale. Reported, not asserted: the
/// budget that matters is that it never blocks the sample the pane already
/// shows, which the supersession and cancellation tests above cover.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "latency measurement; run with --ignored --nocapture"]
async fn measure_whole_view_statistics_latency() {
    for records in [620_000usize, 3_000_000] {
        let root = TempDir::new().unwrap();
        let path = root.path().join("stats.log");
        {
            let mut file = BufWriter::new(File::create(&path).unwrap());
            for index in 0..records {
                file.write_all(line(index).as_bytes()).unwrap();
            }
            file.flush().unwrap();
        }
        let bytes = fs::metadata(&path).unwrap().len();
        let manager =
            SourceManager::new(root.path().join("capture"), RuntimeConfig::default()).unwrap();
        let handle = manager.start(source(SourceId::new(), &path)).await.unwrap();
        let mut progress = handle.subscribe();
        tokio::time::timeout(Duration::from_secs(3600), async {
            while progress.borrow().records < records as u64 {
                progress.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
        let mut live = LiveConfig::new(root.path().join("raw-index"));
        live.maximum_index_bytes_per_source = 256 * 1024 * 1024;
        live.maximum_total_index_bytes = 256 * 1024 * 1024;
        let mut view = ViewConfig::new(root.path().join("view-index"));
        view.maximum_index_bytes = 256 * 1024 * 1024;
        let raw = Arc::new(LiveRowProvider::new(live).unwrap());
        let mut adapter = NativeViewAdapter::new(raw, view).unwrap();
        adapter.register_source(handle.clone()).unwrap();
        adapter
            .register_view("view", vec![handle.source_id()])
            .unwrap();
        let started = std::time::Instant::now();
        adapter
            .submit_field_stats(request(1, StatsType::Integer))
            .unwrap();
        let stats = settle(&mut adapter).await;
        let elapsed = started.elapsed().as_secs_f64();
        let aggregate = stats[0].result.as_ref().expect("statistics");
        println!(
            "STATS n={records} bytes={bytes} scanned={} elapsed={elapsed:.3}s \
             {:.0} records/s distinct={} present={}",
            stats[0].scanned,
            stats[0].scanned as f64 / elapsed,
            aggregate.distinct,
            aggregate.present,
        );
        adapter.shutdown();
        manager.shutdown().await;
    }
}
