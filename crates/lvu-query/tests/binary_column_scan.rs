//! How fast does Polars scan one contiguous column of record text?
//!
//! Evidence for `docs/columnar-cache.md`. The question the note has to answer
//! is whether a derived column store that holds `raw` as a column would beat
//! reading and framing the journal per page, so this measures the engine ops a
//! scan over such a column would use — nothing hand-rolled — against the
//! 590 MB/s of journal the current path reaches.
//!
//! Ignored: it is a measurement, not an assertion about this machine.

use polars::prelude::*;
use std::time::Instant;

/// Roughly the soak's generated record.
fn record(index: usize) -> String {
    format!(
        "{{\"level\":\"INFO\",\"service\":\"api\",\"seq\":{index},\
         \"request_id\":\"req-{:05}\",\"duration_ms\":{},\"message\":\"handled {}\"}}",
        index % 5000,
        index % 4000,
        if index % 10_000 == 0 {
            "RARE_MARKER"
        } else {
            "request"
        }
    )
}

fn column(rows: usize) -> (DataFrame, usize) {
    let text: Vec<String> = (0..rows).map(record).collect();
    let bytes = text.iter().map(String::len).sum::<usize>();
    let borrowed: Vec<&str> = text.iter().map(String::as_str).collect();
    let frame = DataFrame::new(
        rows,
        vec![Column::from(Series::new("raw".into(), borrowed))],
    )
    .unwrap();
    (frame, bytes)
}

fn time(label: &str, bytes: usize, rows: usize, run: impl Fn() -> usize) {
    // One warm pass, then the measured one.
    let matched = run();
    let started = Instant::now();
    let again = run();
    let elapsed = started.elapsed().as_secs_f64();
    assert_eq!(matched, again);
    println!(
        "SCAN {label}: {rows} rows / {:.1} MB, {:.3}s, {:.0} MB/s, {:.0} rows/s, matched {matched}",
        bytes as f64 / 1e6,
        elapsed,
        bytes as f64 / 1e6 / elapsed,
        rows as f64 / elapsed,
    );
}

#[test]
#[ignore = "measurement for docs/columnar-cache.md; run with --ignored --nocapture"]
fn how_fast_polars_scans_one_column_of_record_text() {
    for rows in [620_000usize, 3_000_000] {
        let (frame, bytes) = column(rows);

        // What a literal search compiles to today, over a whole column instead
        // of a 4096-record page.
        time("case_insensitive_contains", bytes, rows, || {
            frame
                .clone()
                .lazy()
                .select([col("raw")
                    .str()
                    .to_lowercase()
                    .str()
                    .contains_literal(lit("rare_marker"))
                    .alias("hit")])
                .collect()
                .unwrap()
                .column("hit")
                .unwrap()
                .bool()
                .unwrap()
                .sum()
                .unwrap_or(0) as usize
        });

        // The same question without the lowercasing copy, for comparison: what
        // a case-sensitive search costs over the same buffer.
        time("case_sensitive_contains", bytes, rows, || {
            frame
                .clone()
                .lazy()
                .select([col("raw")
                    .str()
                    .contains_literal(lit("RARE_MARKER"))
                    .alias("hit")])
                .collect()
                .unwrap()
                .column("hit")
                .unwrap()
                .bool()
                .unwrap()
                .sum()
                .unwrap_or(0) as usize
        });

        // Case-insensitivity expressed as a regex instead of a lowercased copy.
        time("case_insensitive_regex", bytes, rows, || {
            frame
                .clone()
                .lazy()
                .select([col("raw")
                    .str()
                    .contains(lit("(?i)RARE_MARKER"), true)
                    .alias("hit")])
                .collect()
                .unwrap()
                .column("hit")
                .unwrap()
                .bool()
                .unwrap()
                .sum()
                .unwrap_or(0) as usize
        });
    }
}
