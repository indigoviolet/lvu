//! A refresh must cost what arrived, not what the view already holds.
//!
//! An applied view refreshes when its sources grow, and each refresh publishes
//! a new immutable membership. Rebuilding that membership from the previous one
//! is correct but scales with the view, so the same trickle of arriving records
//! costs more and more as capture proceeds — and because only one refresh is in
//! flight per view, the symptom is not a slower second but a longer refresh,
//! which a per-second measurement hides. So this measures per refresh, and
//! compares two view sizes rather than asserting a duration: the ratio is a
//! property of the pipeline, a duration is a property of the machine.

use lvu::{
    QueryConstraints, QueryPurpose, QueryRequest, TextConstraint, terminal::QueryDispatcher,
};
use lvu_core::{Acquisition, SourceDefinition, SourceId};
use lvu_ingest::{RuntimeConfig, SourceManager};
use lvu_live::{LiveConfig, LiveRowProvider};
use lvu_view::{NativeViewAdapter, ViewConfig};
use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::{BufWriter, Write},
    sync::Arc,
    time::Duration,
};
use tempfile::TempDir;

/// View sizes to compare. Eight times the records; a rebuild-per-refresh costs
/// about eight times as much, an append costs the same.
const SMALL: usize = 20_000;
const LARGE: usize = 160_000;
/// Records appended per burst, and bursts driven. The chatty command source the
/// soak uses emits one record every 50 ms; this is the same shape, faster.
const BURST: usize = 20;
const BURSTS: usize = 12;
/// A rebuild would be ~8x. Three leaves room for a noisy shared machine while
/// still failing an O(view) refresh outright.
const MAXIMUM_GROWTH: f64 = 3.0;

fn line(index: usize) -> String {
    format!(
        "{{\"level\":\"INFO\",\"service\":\"api\",\"seq\":{index},\"message\":\"handled request\"}}\n"
    )
}

fn source(id: SourceId, path: &std::path::Path) -> SourceDefinition {
    SourceDefinition {
        schema_version: 1,
        id,
        name: "refresh fixture".into(),
        acquisition: Acquisition::File {
            path: path.to_owned(),
            // Appends have to reach the view: that is what a refresh is.
            follow: true,
        },
        identity_hints: BTreeMap::new(),
        retention: None,
    }
}

/// Nanoseconds per incremental refresh over a view of `records` matched rows.
async fn nanos_per_refresh(records: usize) -> (u64, f64) {
    let root = TempDir::new().unwrap();
    let path = root.path().join("refresh.log");
    {
        let mut file = BufWriter::new(fs::File::create(&path).unwrap());
        for index in 0..records {
            file.write_all(line(index).as_bytes()).unwrap();
        }
        file.flush().unwrap();
    }
    let manager =
        SourceManager::new(root.path().join("capture"), RuntimeConfig::default()).unwrap();
    let handle = manager.start(source(SourceId::new(), &path)).await.unwrap();
    let mut progress = handle.subscribe();
    tokio::time::timeout(Duration::from_secs(900), async {
        while progress.borrow().records < records as u64 {
            progress.changed().await.unwrap();
        }
    })
    .await
    .expect("capture of the fixture");

    let mut live = LiveConfig::new(root.path().join("raw-index"));
    live.maximum_index_bytes_per_source = 128 * 1024 * 1024;
    live.maximum_total_index_bytes = 128 * 1024 * 1024;
    let mut view = ViewConfig::new(root.path().join("view-index"));
    view.maximum_index_bytes = 128 * 1024 * 1024;
    let raw = Arc::new(LiveRowProvider::new(live).unwrap());
    let mut adapter = NativeViewAdapter::new(raw, view).unwrap();
    adapter.register_source(lvu_shared::AnySourceHandle::Local(handle.clone())).unwrap();
    adapter
        .register_view("view", vec![handle.source_id()])
        .unwrap();
    // Matches every record, so the membership a refresh would rebuild is the
    // whole capture rather than a handful of rows.
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
                    literal: "handled".into(),
                    case_insensitive: true,
                }),
                ..QueryConstraints::default()
            },
        })
        .unwrap();
    let completion = tokio::time::timeout(Duration::from_secs(900), async {
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
    .expect("the filter completed");
    assert!(completion.result.is_ok(), "{:?}", completion.result);
    // Not an equality: the source is followed, so a partial line can be
    // captured as its own record and the count legitimately exceeds the lines
    // written. What matters here is that the membership really is the whole
    // view, which is what makes a rebuild expensive.
    // Not a count: the source is followed, so what is captured at this instant
    // depends on how busy the machine is, and a partial line is a record of its
    // own. All this needs is that the membership really is most of the view,
    // because that is what makes rebuilding it expensive.
    let matched = adapter.status("view").unwrap().matched_records;
    assert!(
        matched * 10 >= records as u64 * 9,
        "the filter matched {matched} of {records} records; the membership is \
         not the whole view and the measurement would mean nothing"
    );

    let (before_refreshes, before_nanos) = adapter.refresh_stats();
    let mut appended = records;
    let mut file = OpenOptions::new().append(true).open(&path).unwrap();
    for _ in 0..BURSTS {
        for _ in 0..BURST {
            file.write_all(line(appended).as_bytes()).unwrap();
            appended += 1;
        }
        file.flush().unwrap();
        // Let the burst be captured, then drained, then refreshed.
        let target = handle.progress().records + BURST as u64;
        let _ = tokio::time::timeout(Duration::from_secs(60), async {
            while handle.progress().records < target {
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
        .await;
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        let want = adapter.refresh_stats().0 + 1;
        while adapter.refresh_stats().0 < want && std::time::Instant::now() < deadline {
            adapter.drain_updates(256);
            let _ = adapter.poll();
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    }
    let (after_refreshes, after_nanos) = adapter.refresh_stats();
    adapter.shutdown();
    manager.shutdown().await;

    let refreshes = after_refreshes - before_refreshes;
    assert!(
        refreshes > 0,
        "no refresh ran over {records} records; the measurement has nothing to say"
    );
    (
        refreshes,
        (after_nanos - before_nanos) as f64 / refreshes as f64,
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_refresh_costs_what_arrived_not_what_the_view_holds() {
    let (small_count, small) = nanos_per_refresh(SMALL).await;
    let (large_count, large) = nanos_per_refresh(LARGE).await;
    let growth = large / small.max(1.0);
    println!(
        "refresh: {SMALL} rows {small:.0} ns over {small_count} refreshes, \
         {LARGE} rows {large:.0} ns over {large_count} refreshes, growth {growth:.2}x \
         for {}x the view",
        LARGE / SMALL
    );
    assert!(
        growth < MAXIMUM_GROWTH,
        "a refresh over {LARGE} rows cost {growth:.2}x one over {SMALL} rows \
         ({large:.0} ns against {small:.0} ns) for {}x the view: refresh is \
         scaling with the view, not with what arrived",
        LARGE / SMALL
    );
}
