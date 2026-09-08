//! The `Extracted` basis, read by the engine rather than parsed in the app.
//!
//! `timestamp_utc` is a designated column holding UTC RFC3339 text, so its
//! reading is a `ColumnTimeInterpretation` like any other column's — the same
//! job the engine already compiles for a *chosen* column (AGENTS.md: "the query
//! engine computes; the app names and presents"). What it replaced was
//! `lvu::parse_utc_nanos` called once per record inside `lvu-view`.
//!
//! These tests are the equivalence argument. The old parser is still in the
//! tree — the Time dialog parses what a user *types* into Start/End with it,
//! which is naming an input rather than evaluating a column — so it can be
//! called here and compared against directly.

use lvu_view::time_basis::read_extracted;

/// Every value the extracted-time fixtures actually carry, across
/// `live_view.rs`'s `extracted_time_follows_enrichment_…` and
/// `tests/pty/test_extracted_time_pty.py`.
const FIXTURE_VALUES: &[&str] = &[
    "2026-09-05T12:30:45Z",
    "2026-09-05T12:30:46Z",
    "bad",
    "2026-09-05T12:30:45.500000Z",
];

fn engine(values: &[&str]) -> Vec<Option<i64>> {
    let values: Vec<Option<&str>> = values.iter().map(|value| Some(*value)).collect();
    read_extracted(&values).expect("the reading compiles")
}

#[test]
fn the_engine_reads_the_fixtures_exactly_as_the_app_parser_did() {
    let read = engine(FIXTURE_VALUES);
    for (value, engine) in FIXTURE_VALUES.iter().zip(read) {
        assert_eq!(
            engine,
            lvu::parse_utc_nanos(value).ok(),
            "{value:?} changed meaning when the parse moved into the engine"
        );
    }
}

/// The spellings RFC3339 allows, beyond the ones the fixtures happen to use.
/// One chrono format cannot read both — `%:z` does not accept `Z`, and a
/// literal `Z` does not accept an offset — which is why the reading is the two
/// coalesced rather than a single `Text` format.
#[test]
fn both_rfc3339_spellings_and_any_fraction_read_as_the_app_parser_read_them() {
    let values = [
        "2026-09-05T12:30:45Z",
        "2026-09-05T12:30:45.5Z",
        "2026-09-05T12:30:45.123456789Z",
        "2026-09-05T14:30:45+02:00",
        "2026-09-05T07:00:45-05:30",
        "2026-09-05T12:30:45.500000+00:00",
    ];
    let read = engine(&values);
    for (value, engine) in values.iter().zip(read) {
        assert_eq!(
            engine,
            lvu::parse_utc_nanos(value).ok(),
            "{value:?} reads differently"
        );
        assert!(engine.is_some(), "{value:?} is valid RFC3339");
    }
    // The two spellings of one instant agree, which is the whole point of
    // coalescing them rather than declaring one and losing the other.
    let same = engine(&["2026-09-05T12:30:45Z", "2026-09-05T14:30:45+02:00"]);
    assert_eq!(same[0], same[1]);
}

#[test]
fn unreadable_values_are_misses_and_never_a_failed_batch() {
    let values = [
        "",
        "bad",
        "2026-09-05",
        "not a time at all",
        "2023-02-29T00:00:00Z",
    ];
    let read = engine(&values);
    for (value, engine) in values.iter().zip(read) {
        assert_eq!(engine, None, "{value:?} must not resolve");
        assert_eq!(engine, lvu::parse_utc_nanos(value).ok(), "{value:?}");
    }
}

#[test]
fn a_null_input_is_a_miss_rather_than_an_instant() {
    let read = read_extracted(&[None, Some("2026-09-05T12:30:45Z"), None]).unwrap();
    assert_eq!(read[0], None);
    assert!(read[1].is_some());
    assert_eq!(read[2], None);
}

/// Persisted user-authored enrichments are not constrained to the canonical
/// `%Y-%m-%dT%H:%M:%S%.6fZ` producer convention. Moving evaluation into Polars
/// must therefore preserve every spelling the old column evaluator accepted,
/// including its whitespace and `UTC` aliases, and its rejection of `:60`.
#[test]
fn historical_whitespace_utc_and_second_validation_are_preserved() {
    for value in [
        "2026-09-05T12:30:45 UTC",
        "2026-09-05T12:30:45UTC",
        "  2026-09-05T12:30:45Z  ",
    ] {
        assert_eq!(
            engine(&[value])[0],
            lvu::parse_utc_nanos(value).ok(),
            "{value:?} changed meaning"
        );
    }
    for value in [
        "2026-09-05T12:30:60Z",
        "2026-09-05T12:30:60UTC",
        "2026-09-05T14:30:60+02:00",
        "0000-01-01T00:00:00Z",
    ] {
        assert_eq!(lvu::parse_utc_nanos(value).ok(), None, "{value:?}");
        assert_eq!(engine(&[value])[0], None, "{value:?} must stay invalid");
    }
}

#[test]
fn historical_checked_nanosecond_range_is_preserved_exactly() {
    let values = [
        "0001-01-01T00:00:00Z",
        "9999-12-31T23:59:59.999999999Z",
        "1677-09-21T00:12:43.999999999Z",
        "1677-09-21T00:12:44Z",
        "1677-09-21T00:12:44.000000001Z",
        "2262-04-11T23:47:16.854775806Z",
        "2262-04-11T23:47:16.854775807Z",
        "2262-04-11T23:47:16.854775808Z",
        "1677-09-20T00:13:44-23:59",
        "1677-09-20T00:13:43.999999999-23:59",
        "2262-04-12T23:46:16.854775807+23:59",
        "2262-04-12T23:46:16.854775808+23:59",
        "  2262-04-11T23:47:16.854775807 UTC  ",
    ];
    let read = engine(&values);
    for (value, engine) in values.iter().zip(read) {
        assert_eq!(
            engine,
            lvu::parse_utc_nanos(value).ok(),
            "{value:?} changed at the historical checked-arithmetic boundary"
        );
    }
}

#[test]
fn every_historical_fraction_width_remains_exact() {
    let values = [
        "2026-09-05T12:30:45.1Z",
        "2026-09-05T12:30:45.12Z",
        "2026-09-05T12:30:45.123Z",
        "2026-09-05T12:30:45.1234Z",
        "2026-09-05T12:30:45.12345Z",
        "2026-09-05T12:30:45.123456Z",
        "2026-09-05T12:30:45.1234567Z",
        "2026-09-05T12:30:45.12345678Z",
        "2026-09-05T12:30:45.123456789Z",
    ];
    let read = engine(&values);
    for (value, engine) in values.iter().zip(read) {
        assert_eq!(engine, lvu::parse_utc_nanos(value).ok(), "{value:?}");
    }
}
