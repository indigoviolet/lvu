//! A filter must spend its time scanning, not waiting.
//!
//! The defect this guards against is not "slow on this machine": it is a scan
//! that blocks per record or per batch — a poll interval, a timer-paced
//! channel, a round trip per page — which shows up as a query that takes
//! seconds while the process is almost entirely idle. A wall-clock budget
//! cannot tell those apart on a shared or loaded host, so this asserts the two
//! things that are properties of the pipeline rather than of the box:
//!
//! * the scan consumes CPU while it runs, and
//! * it converts CPU into scanned records at a sane rate.
//!
//! Both floors are deliberately far below what this pipeline achieves, because
//! the failure they exist to catch is an order of magnitude, not a percentage.

use lvu::{
    QueryConstraints, QueryPurpose, QueryRequest, TextConstraint, terminal::QueryDispatcher,
};
use lvu_core::{Acquisition, SourceDefinition, SourceId};
use lvu_ingest::{RuntimeConfig, SourceManager};
use lvu_live::{LiveConfig, LiveRowProvider};
use lvu_view::{NativeViewAdapter, ViewConfig};
use std::{
    collections::BTreeMap,
    fs,
    io::{BufWriter, Write},
    sync::Arc,
    time::{Duration, Instant},
};
use tempfile::TempDir;

const RECORDS: usize = 150_000;
/// Every tenth record carries the literal, so the answer is known exactly and
/// the filter is neither trivially empty nor a copy of the whole source.
const MATCHING: usize = RECORDS / 10;
/// A scan that waits per batch reports a ratio near zero; a scan that computes
/// reports at least one, and more when Polars uses several threads. Half of one
/// core is far under anything the working pipeline produces and far over
/// anything a per-batch wait could.
const MINIMUM_CPU_PER_WALL: f64 = 0.5;
/// This pipeline measures around 500,000 records per CPU-second here, and the
/// scan it replaced managed 30,000–40,000. The floor sits between them with
/// room on both sides: a slower machine still passes, and a return to
/// per-record identity allocation or a page round trip per few hundred records
/// still fails.
const MINIMUM_RECORDS_PER_CPU_SECOND: f64 = 100_000.0;

/// Process CPU time, or `None` where the platform does not publish it.
///
/// `/proc/self/stat` covers every thread of this test binary, and the scan runs
/// on the view worker thread, so process-wide is the right scope. Each
/// integration test is its own process, and this file holds one test, so no
/// unrelated work is counted.
fn cpu_seconds() -> Option<f64> {
    let stat = fs::read_to_string("/proc/self/stat").ok()?;
    // The executable name can contain spaces and parentheses; fields are
    // positional only after the closing one.
    let fields: Vec<&str> = stat.rsplit_once(')')?.1.split_whitespace().collect();
    let utime: u64 = fields.get(11)?.parse().ok()?;
    let stime: u64 = fields.get(12)?.parse().ok()?;
    Some((utime + stime) as f64 / 100.0)
}

fn fixture(path: &std::path::Path) -> u64 {
    let mut file = BufWriter::new(fs::File::create(path).unwrap());
    for index in 0..RECORDS {
        writeln!(
            file,
            "{{\"level\":\"INFO\",\"service\":\"api\",\"seq\":{index},\"request_id\":\"req-{:05}\",\"message\":\"handled {}\"}}",
            index % 5000,
            if index % 10 == 0 { "RARE_MARKER" } else { "request" }
        )
        .unwrap();
    }
    file.flush().unwrap();
    drop(file);
    fs::metadata(path).unwrap().len()
}

fn file_source(id: SourceId, path: &std::path::Path) -> SourceDefinition {
    SourceDefinition {
        schema_version: 1,
        id,
        name: "throughput fixture".into(),
        acquisition: Acquisition::File {
            path: path.to_owned(),
            // A settled historical source: the subject is the cost of scanning
            // what is already captured, not of chasing arriving input.
            follow: false,
        },
        identity_hints: BTreeMap::new(),
        retention: None,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_historical_filter_spends_its_time_scanning() {
    let root = TempDir::new().unwrap();
    let path = root.path().join("throughput.log");
    let input_bytes = fixture(&path);
    let manager =
        SourceManager::new(root.path().join("capture"), RuntimeConfig::default()).unwrap();
    let handle = manager
        .start(file_source(SourceId::new(), &path))
        .await
        .unwrap();
    let mut progress = handle.subscribe();
    tokio::time::timeout(Duration::from_secs(600), async {
        while progress.borrow().records < RECORDS as u64 {
            progress.changed().await.unwrap();
        }
    })
    .await
    .expect("capture of the fixture");

    let mut live = LiveConfig::new(root.path().join("raw-index"));
    live.maximum_index_bytes_per_source = 64 * 1024 * 1024;
    live.maximum_total_index_bytes = 64 * 1024 * 1024;
    let mut view = ViewConfig::new(root.path().join("view-index"));
    view.maximum_index_bytes = 64 * 1024 * 1024;
    let raw = Arc::new(LiveRowProvider::new(live).unwrap());
    let mut adapter = NativeViewAdapter::new(raw, view).unwrap();
    adapter
        .register_source(lvu_shared::AnySourceHandle::Local(handle.clone()))
        .unwrap();
    adapter
        .register_view("view", vec![handle.source_id()])
        .unwrap();

    let cpu_before = cpu_seconds();
    let started = Instant::now();
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
                    literal: "RARE_MARKER".into(),
                    case_insensitive: true,
                }),
                ..QueryConstraints::default()
            },
        })
        .unwrap();
    let completion = tokio::time::timeout(Duration::from_secs(600), async {
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
    let wall = started.elapsed().as_secs_f64();
    let cpu = cpu_before
        .zip(cpu_seconds())
        .map(|(before, after)| after - before);

    assert!(completion.result.is_ok(), "{:?}", completion.result);
    let status = adapter.status("view").unwrap();
    // Not an exact count. Capture flushes a partial line as a record of its own
    // before its terminator arrives, so a line — and the marker in it — can be
    // split in two, and the number of records is not the number of lines
    // written. One short is capture doing its job, not the filter missing a
    // record. What this needs is that the filter really did select a tenth of
    // the source, so the throughput below is over a real scan.
    let matched = status.matched_records;
    assert!(
        matched * 100 >= MATCHING as u64 * 99 && matched <= MATCHING as u64 + 1,
        "the filter selected {matched} records where about {MATCHING} were marked"
    );
    // Throughput is measured over the records the scan actually reported, so a
    // split line changes the denominator rather than failing the test.
    let scanned = status.scanned_records;
    assert!(
        scanned >= RECORDS as u64,
        "the scan must reach the whole captured source, saw {scanned}"
    );

    match cpu {
        None => println!(
            "SKIPPED the CPU assertions: this platform does not publish process CPU time; \
             scanned {scanned} records over {input_bytes} bytes in {wall:.3}s"
        ),
        Some(cpu) => {
            println!(
                "scan: {scanned} records / {input_bytes} bytes, wall {wall:.3}s, cpu {cpu:.3}s, \
                 cpu/wall {:.2}, {:.0} records per cpu-second",
                cpu / wall,
                scanned as f64 / cpu.max(f64::EPSILON)
            );
            assert!(
                cpu / wall >= MINIMUM_CPU_PER_WALL,
                "the scan used {cpu:.3}s of CPU across {wall:.3}s of wall clock \
                 (ratio {:.2}, floor {MINIMUM_CPU_PER_WALL}): it is waiting, not scanning",
                cpu / wall
            );
            let rate = scanned as f64 / cpu.max(f64::EPSILON);
            assert!(
                rate >= MINIMUM_RECORDS_PER_CPU_SECOND,
                "the scan converted CPU into {rate:.0} records per second \
                 (floor {MINIMUM_RECORDS_PER_CPU_SECOND}): per-record or per-batch \
                 overhead has regressed"
            );
        }
    }

    adapter.shutdown();
    manager.shutdown().await;
}
