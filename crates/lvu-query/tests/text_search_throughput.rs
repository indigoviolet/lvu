//! The dominant ASCII literal path must not return to lowercasing the column.

use lvu_core::{ChunkPosition, RawRecord, RecordId, SourceId, StreamKind};
use lvu_query::{BatchQuery, TextSearch, execute_batch, records_to_batch};
use std::{fs, time::Instant};
use uuid::Uuid;

const RECORDS: usize = 620_000;
const REPETITIONS: usize = 8;
/// Eight identical executions over the already-built fixture measured 4.96M
/// records / 0.760 CPU-seconds (6.526M records/CPU-second) with the regex path,
/// and 4.96M / 1.020 (4.863M/second) after disabling it. One 10 ms `/proc`
/// clock tick moves those rates to 6.442M and 4.911M respectively. A clean
/// full-workspace release run measured 5.573M under host contention, so 5.4M
/// retains roughly 10% separation from the known lowercasing regression
/// without treating a sub-percent scheduling shift as a product failure.
const MINIMUM_RECORDS_PER_CPU_SECOND: f64 = 5_400_000.0;

fn cpu_seconds() -> Option<f64> {
    let stat = fs::read_to_string("/proc/self/stat").ok()?;
    let fields: Vec<&str> = stat.rsplit_once(')')?.1.split_whitespace().collect();
    let utime: u64 = fields.get(11)?.parse().ok()?;
    let stime: u64 = fields.get(12)?.parse().ok()?;
    Some((utime + stime) as f64 / 100.0)
}

#[test]
fn ascii_literal_search_uses_the_regex_kernel() {
    let source = SourceId::new();
    let records = (0..RECORDS)
        .map(|index| RawRecord {
            record_id: RecordId {
                source_id: source,
                sequence: index as u64,
            },
            captured_at_unix_nanos: index as i64,
            stream: StreamKind::File,
            bytes: format!(
                "INFO service=api seq={index} request_id=req-{:05} message=handled {}",
                index % 5000,
                if index.is_multiple_of(10_000) {
                    "RARE_MARKER"
                } else {
                    "request"
                }
            )
            .into_bytes()
            .into(),
            delimiter: b"\n".into(),
            chunk: ChunkPosition::Complete,
            acquisition_id: Uuid::nil(),
        })
        .collect::<Vec<_>>();
    let frame = records_to_batch(&records).unwrap().frame;
    let search = TextSearch::new("rare_marker").unwrap();
    let run = || {
        execute_batch(
            &frame,
            BatchQuery {
                generation: 1,
                definition_generation: 1,
                stages: &[],
                filter: None,
                text_search: Some(&search),
                colors: &[],
                column_colors: &[],
            },
        )
    };
    assert_eq!(run().matched_ids.len(), RECORDS / 10_000);
    let cpu_before = cpu_seconds();
    let started = Instant::now();
    for _ in 0..REPETITIONS {
        let result = run();
        assert_eq!(result.matched_ids.len(), RECORDS / 10_000);
    }
    let wall = started.elapsed().as_secs_f64();
    if let Some(cpu) = cpu_before.zip(cpu_seconds()).map(|(a, b)| b - a) {
        let total_records = RECORDS * REPETITIONS;
        let throughput = total_records as f64 / cpu.max(f64::EPSILON);
        println!(
            "text search: {total_records} records ({REPETITIONS} repetitions), \
             wall {wall:.3}s, cpu {cpu:.3}s, \
             {throughput:.0} records per cpu-second"
        );
        assert!(
            throughput >= MINIMUM_RECORDS_PER_CPU_SECOND,
            "text search converted CPU into {throughput:.0} records per second \
             (floor {MINIMUM_RECORDS_PER_CPU_SECOND}); the column may be lowercased again"
        );
    }
}
