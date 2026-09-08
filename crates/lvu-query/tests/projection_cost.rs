//! Where a statistics pass actually spends its time.
//!
//! The canonical projection builds a column per field plus ten of metadata and
//! three copies of the record's text; the targeted one builds a single column.
//! Both parse every record the same way. This separates the two costs so a
//! change aimed at the wrong one is visible as such.

use lvu_core::{ChunkPosition, RawRecord, RecordId, SourceId, StreamKind};
use lvu_query::{
    SchemaContext, records_to_batch_with_context_and_exact_field, records_to_field_column,
};
use std::time::Instant;

fn records(count: usize, fields: usize) -> Vec<RawRecord> {
    let source_id = SourceId::new();
    (0..count)
        .map(|index| {
            let mut text = format!("{{\"seq\":{index},\"status\":{}", 200 + (index % 4) * 100);
            for extra in 0..fields {
                text.push_str(&format!(",\"f{extra}\":\"value {extra} of {index}\""));
            }
            text.push('}');
            RawRecord {
                record_id: RecordId {
                    source_id,
                    sequence: index as u64,
                },
                captured_at_unix_nanos: 0,
                stream: StreamKind::File,
                bytes: text.into_bytes().into(),
                delimiter: b"\n".to_vec().into(),
                acquisition_id: uuid::Uuid::nil(),
                chunk: ChunkPosition::Complete,
            }
        })
        .collect()
}

fn time(label: &str, count: usize, run: impl Fn()) {
    run();
    let started = Instant::now();
    run();
    let elapsed = started.elapsed().as_secs_f64();
    println!(
        "PROJECT {label}: {count} records in {elapsed:.3}s, {:.2} us/record",
        elapsed * 1e6 / count as f64
    );
}

#[test]
#[ignore = "measurement; run with --ignored --nocapture"]
fn what_a_projection_costs() {
    const COUNT: usize = 200_000;
    for fields in [0usize, 8] {
        let batch = records(COUNT, fields);
        println!("--- {fields} extra fields per record");
        time("canonical (every column)", COUNT, || {
            let mut schema = SchemaContext::default();
            for chunk in batch.chunks(4096) {
                records_to_batch_with_context_and_exact_field(chunk, &mut schema, None).unwrap();
            }
        });
        time("targeted (one column)", COUNT, || {
            let mut schema = SchemaContext::default();
            for chunk in batch.chunks(4096) {
                records_to_field_column(chunk, &mut schema, "status").unwrap();
            }
        });
        // The ceiling if the engine read the field out of the text instead:
        // one string column and a JSON path, no value tree built here at all.
        // It only answers for records that are JSON, which is why it cannot
        // simply replace the parse.
        time("engine json_path_match", COUNT, || {
            use polars::prelude::*;
            for chunk in batch.chunks(4096) {
                let text: Vec<&str> = chunk
                    .iter()
                    .map(|record| std::str::from_utf8(&record.bytes).unwrap())
                    .collect();
                let frame = DataFrame::new(
                    chunk.len(),
                    vec![Column::from(Series::new("raw".into(), text))],
                )
                .unwrap();
                let out = frame
                    .lazy()
                    .select([col("raw")
                        .str()
                        .json_path_match(lit("$.status"))
                        .alias("status")])
                    .collect()
                    .unwrap();
                std::hint::black_box(out);
            }
        });
        // The parse alone, with no column built at all: the floor both share.
        time("parse only", COUNT, || {
            for record in &batch {
                let text = String::from_utf8_lossy(&record.bytes);
                let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
                std::hint::black_box(parsed);
            }
        });
    }
}
