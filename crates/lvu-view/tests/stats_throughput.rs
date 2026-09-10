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

use lvu_core::{Acquisition, ProcessCpu, SourceDefinition, SourceId};
use lvu_ingest::{RuntimeConfig, RuntimeState, SourceManager};
use lvu_live::{IndexState, LiveConfig, LiveRowProvider};
use lvu_view::{FieldStatsRequest, NativeViewAdapter, StatsType, ViewConfig};
use std::{
    collections::BTreeMap,
    fs::File,
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
        loop {
            if progress.borrow_and_update().state.is_terminal() {
                break;
            }
            progress.changed().await.expect("capture progress channel");
        }
    })
    .await
    .expect("capture reached a terminal state");
    let captured = handle.progress();
    assert_eq!(
        captured.state,
        RuntimeState::Stopped,
        "historical fixture capture did not finish cleanly: {:?}",
        captured.last_error
    );
    assert_eq!(
        captured.synced_records, captured.records,
        "terminal capture must make every record durable"
    );
    assert!(
        captured.records >= RECORDS as u64,
        "captured {} records for {RECORDS} fixture lines",
        captured.records
    );
    let captured_high_watermark = captured
        .high_watermark
        .expect("non-empty capture has a high watermark")
        .sequence;

    let mut live = LiveConfig::new(root.path().join("raw-index"));
    live.maximum_index_bytes_per_source = 128 * 1024 * 1024;
    live.maximum_total_index_bytes = 128 * 1024 * 1024;
    let mut view = ViewConfig::new(root.path().join("view-index"));
    view.maximum_index_bytes = 128 * 1024 * 1024;
    let raw = Arc::new(LiveRowProvider::new(live).unwrap());
    let mut adapter = NativeViewAdapter::new(Arc::clone(&raw), view).unwrap();
    adapter
        .register_source(lvu_shared::AnySourceHandle::Local(handle.clone()))
        .unwrap();
    adapter
        .register_view("view", vec![handle.source_id()])
        .unwrap();

    // Process CPU includes capture, indexing and query threads, so timing a
    // quiet interval cannot establish what consumed it. Wait for the index to
    // report the exact durable capture cursor, then join its worker. Field
    // statistics read the journal through the registered SourceHandle and do
    // not depend on the display index after registration.
    let source_id = handle.source_id();
    tokio::time::timeout(Duration::from_secs(120), async {
        loop {
            adapter.drain_updates(256);
            if let Some(status) = raw.source_status(source_id) {
                if matches!(
                    status.index,
                    IndexState::Limited | IndexState::Error | IndexState::Shutdown
                ) {
                    panic!("fixture index cannot settle: {status:?}");
                }
                if status.acquisition == RuntimeState::Stopped
                    && status.reported_records == captured.records
                    && status.index == IndexState::Ready
                    && status.indexed_records == captured.records
                    && status.high_watermark == Some(captured_high_watermark)
                {
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("derived index reached the durable capture cursor");
    raw.shutdown().await;

    let cpu_before = ProcessCpu::start();
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
    let cpu = cpu_before.and_then(|started| {
        started
            .elapsed_nanos()
            .map(|nanos| Duration::from_nanos(nanos).as_secs_f64())
    });

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
        Err(error) if error.kind() == std::io::ErrorKind::Unsupported => {
            println!("SKIPPED the CPU assertion: {error}; {scanned} records in {wall:.3}s")
        }
        Err(error) => panic!("process CPU accounting failed: {error}"),
        Ok(cpu) => {
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
