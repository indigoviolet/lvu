//! A statistics pass must read one field, not rebuild every record.
//!
//! The pass walks a view's membership and counts one column. What it must not
//! do is pay for the canonical projection on the way — a column per field the
//! record carries, ten of metadata and three copies of the record's text — to
//! read one of them. That mistake is invisible in correctness and costs three
//! to four times the pass, so it needs a guard of its own.
//!
//! Like `scan_throughput`, this asserts records per CPU-second rather than a
//! duration: throughput per unit of CPU is a property of the pipeline, and how
//! long a machine takes to supply that CPU is not.

use lvu_core::{Acquisition, SourceDefinition, SourceId};
use lvu_ingest::{RuntimeConfig, SourceManager};
use lvu_live::{LiveConfig, LiveRowProvider};
use lvu_view::{FieldStatsRequest, NativeViewAdapter, StatsType, ViewConfig};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{BufWriter, Write},
    sync::Arc,
    time::{Duration, Instant},
};
use tempfile::TempDir;

const RECORDS: usize = 150_000;
/// Measured on this test, both ways: the targeted projection converts CPU into
/// about 320,000 records a second and the canonical one about 113,000. The
/// floor sits between them with roughly equal headroom on each side, so a
/// slower machine still passes and a return to projecting every column fails
/// outright rather than merely being slower.
const MINIMUM_RECORDS_PER_CPU_SECOND: f64 = 200_000.0;

/// Records carry several fields the pass does not want, which is the whole
/// point: projecting them is the cost being guarded against.
fn line(index: usize) -> String {
    format!(
        "{{\"seq\":{index},\"service\":\"api\",\"status\":{},\"request_id\":\"req-{:05}\",\
         \"duration_ms\":{},\"region\":\"eu-west\",\"message\":\"handled request\"}}\n",
        200 + (index % 4) * 100,
        index % 5000,
        index % 4000,
    )
}

fn cpu_seconds() -> Option<f64> {
    let stat = fs::read_to_string("/proc/self/stat").ok()?;
    let fields: Vec<&str> = stat.rsplit_once(')')?.1.split_whitespace().collect();
    let utime: u64 = fields.get(11)?.parse().ok()?;
    let stime: u64 = fields.get(12)?.parse().ok()?;
    Some((utime + stime) as f64 / 100.0)
}

fn source(id: SourceId, path: &std::path::Path) -> SourceDefinition {
    SourceDefinition {
        schema_version: 1,
        id,
        name: "stats throughput fixture".into(),
        acquisition: Acquisition::File {
            path: path.to_owned(),
            follow: false,
        },
        identity_hints: BTreeMap::new(),
        retention: None,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_statistics_pass_reads_one_field_not_every_column() {
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
    tokio::time::timeout(Duration::from_secs(600), async {
        while progress.borrow().records < RECORDS as u64 {
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
    adapter.register_source(handle.clone()).unwrap();
    adapter
        .register_view("view", vec![handle.source_id()])
        .unwrap();

    // Let the derived index finish first. It runs on its own thread for the
    // same source, and process CPU cannot tell its work from the pass's — a
    // measurement taken across it would be reporting the index's cost as the
    // pass's. A user opening Fields on a view they have been reading is in
    // this state anyway.
    let quiet = Instant::now();
    let mut previous = cpu_seconds().unwrap_or(0.0);
    while quiet.elapsed() < Duration::from_secs(120) {
        tokio::time::sleep(Duration::from_millis(200)).await;
        adapter.drain_updates(256);
        let now = cpu_seconds().unwrap_or(0.0);
        if now - previous < 0.01 {
            break;
        }
        previous = now;
    }

    let cpu_before = cpu_seconds();
    let started = Instant::now();
    adapter
        .submit_field_stats(FieldStatsRequest {
            generation: 1,
            view_id: "view".into(),
            column: "status".into(),
            json_path: None,
            label: "status".into(),
            kind: StatsType::Integer,
            top: 5,
            distinct_cap: 4096,
        })
        .unwrap();
    let stats = tokio::time::timeout(Duration::from_secs(600), async {
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
    .expect("the pass finished");
    let wall = started.elapsed().as_secs_f64();
    let cpu = cpu_before.zip(cpu_seconds()).map(|(a, b)| b - a);

    let aggregate = stats[0].result.as_ref().expect("statistics");
    assert_eq!(
        aggregate.distinct, 4,
        "the pass read the field it was asked for"
    );
    let scanned = stats[0].scanned;
    assert!(
        scanned >= RECORDS as u64,
        "the pass must reach every record, saw {scanned}"
    );

    match cpu {
        None => println!(
            "SKIPPED the CPU assertion: this platform does not publish process CPU time; \
             {scanned} records in {wall:.3}s"
        ),
        Some(cpu) => {
            let rate = scanned as f64 / cpu.max(f64::EPSILON);
            println!(
                "stats pass: {scanned} records, wall {wall:.3}s, cpu {cpu:.3}s, \
                 {rate:.0} records per cpu-second"
            );
            assert!(
                rate >= MINIMUM_RECORDS_PER_CPU_SECOND,
                "the pass converted CPU into {rate:.0} records per second \
                 (floor {MINIMUM_RECORDS_PER_CPU_SECOND}): it is projecting more \
                 than the column it was asked for"
            );
        }
    }

    adapter.shutdown();
    manager.shutdown().await;
}
