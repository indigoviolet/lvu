//! A text column as the event-time basis: the shapes that are inferred, and
//! the formats they propose measured by the parser that will actually run.

use lvu_live::time::{
    TimeFieldSelection, TimeInterpretation, ZoneAssumption, infer_text_time_formats,
};
use lvu_view::time_basis::{column_selection, validate_text_column};

/// Every shape the inference offers must be a format Polars can really read.
/// The matcher and the parser are different pieces of code; if they disagree
/// the user is shown a rate of 0% for a value that plainly is a timestamp.
#[test]
fn every_inferred_shape_parses_under_polars() {
    for (values, expected_label) in [
        (
            vec!["2026-09-07T12:00:01Z", "2026-09-07T12:00:02.5Z"],
            "RFC 3339",
        ),
        (
            vec!["2026-09-07T12:00:01+02:00", "2026-09-07T12:00:01+0200"],
            "RFC 3339",
        ),
        (
            vec!["2026-09-07 12:00:01+02:00"],
            "ISO with a space and an offset",
        ),
        (
            vec!["2026-09-07T12:00:01", "2026-09-07T12:00:02.25"],
            "ISO without a zone",
        ),
        (vec!["2026-09-07 12:00:01"], "date and time without a zone"),
        (
            vec!["10/Oct/2000:13:55:36 -0700"],
            "Apache / common log format",
        ),
    ] {
        let inferred = infer_text_time_formats(&values);
        let best = inferred
            .first()
            .unwrap_or_else(|| panic!("no shape inferred for {values:?}"));
        assert_eq!(best.label, expected_label, "for {values:?}");
        assert_eq!(best.matched, values.len(), "for {values:?}");
        assert_eq!(best.match_percent(), 100, "for {values:?}");

        // Zone-less shapes are only readable under a declared assumption; that
        // is the product rule, not an accident of the format string.
        assert!(best.readable, "for {values:?}");
        let zone = if lvu_query::time_field::format_reads_zone(&best.format) {
            ZoneAssumption::Reject
        } else {
            ZoneAssumption::Utc
        };
        let owned: Vec<Option<&str>> = values.iter().map(|value| Some(*value)).collect();
        let (coverage, parsed, _) = validate_text_column("t", &best.format, zone, &owned)
            .unwrap_or_else(|error| panic!("{} rejected for {values:?}: {error}", best.format));
        assert_eq!(
            parsed,
            values.len(),
            "{} read {parsed} of {} for {values:?}",
            best.format,
            values.len()
        );
        assert!((coverage - 1.0).abs() < f64::EPSILON, "for {values:?}");
    }
}

#[test]
fn a_value_of_another_shape_lowers_the_rate_it_is_offered_with() {
    let values = vec![
        "2026-09-07T12:00:01Z",
        "2026-09-07T12:00:02Z",
        "not a time at all",
        "",
    ];
    let best = infer_text_time_formats(&values).into_iter().next().unwrap();
    // The empty value is a gap in the data, not evidence against the format.
    assert_eq!(best.sampled, 3);
    assert_eq!(best.matched, 2);
    assert_eq!(best.match_percent(), 66);
    assert_eq!(best.sample.as_deref(), Some("2026-09-07T12:00:01Z"));

    let owned: Vec<Option<&str>> = values.iter().map(|value| Some(*value)).collect();
    let (_, parsed, _) =
        validate_text_column("t", &best.format, ZoneAssumption::Reject, &owned).unwrap();
    assert_eq!(parsed, 2);

    // Unreadable rows become nulls with a count. They are never dropped: the
    // validation answers for every row it was given, which is what keeps a row
    // the format cannot read in All events with no time rather than out of it.
    use polars::prelude::{Column, DataFrame, NamedFrom, Series};
    let frame = DataFrame::new(
        values.len(),
        vec![Column::from(Series::new("t".into(), values.to_vec()))],
    )
    .unwrap();
    let validation = lvu_query::time_field::validate_time_basis(
        &frame,
        &lvu_query::time_field::TimeColumnSelection::text("t", &best.format),
    )
    .unwrap();
    assert_eq!(validation.rows, values.len());
    assert_eq!(validation.parsed, 2);
    assert_eq!(validation.unreadable, 2, "the miss and the empty value");
    let nanos = lvu_query::time_field::time_basis_unix_nanos(
        &frame,
        &lvu_query::time_field::TimeColumnSelection::text("t", &best.format),
    )
    .unwrap();
    assert_eq!(nanos.len(), values.len(), "one answer per row, always");
    assert_eq!(nanos.iter().filter(|value| value.is_none()).count(), 2);
}

#[test]
fn a_text_column_round_trips_through_a_field_token() {
    let selection = TimeFieldSelection::column("started_at")
        .with_interpretation(TimeInterpretation::Text)
        .with_text_format(Some("%Y-%m-%dT%H:%M:%S%.f%:z".into()));
    let token = selection.to_token();
    let parsed = TimeFieldSelection::parse_token(&token).unwrap();
    assert_eq!(parsed, selection);
    let column = column_selection(&parsed).unwrap();
    assert_eq!(column.column, "started_at");
    assert_eq!(
        column.interpretation,
        lvu_query::time_field::ColumnTimeInterpretation::Text {
            format: "%Y-%m-%dT%H:%M:%S%.f%:z".into()
        }
    );
}

/// A four-part token predates the format and must keep parsing.
#[test]
fn an_older_token_without_a_format_still_parses_and_says_what_is_missing() {
    let parsed = TimeFieldSelection::parse_token("column:started_at|text|reject|-").unwrap();
    assert_eq!(parsed.text_format, None);
    let error = column_selection(&parsed).unwrap_err();
    assert!(error.contains("explicit time format"), "{error}");
}

/// Syslog has no year and Polars has no way to supply one, so it is recognised
/// and reported rather than offered as a format that would read nothing.
#[test]
fn a_syslog_column_is_named_and_marked_unreadable_rather_than_offered() {
    let inferred = infer_text_time_formats(&["Sep  7 12:00:01", "Sep  8 12:00:02"]);
    let syslog = inferred
        .iter()
        .find(|shape| shape.label == "syslog")
        .unwrap();
    assert_eq!(syslog.matched, 2);
    assert!(!syslog.readable);
    assert!(
        inferred.iter().all(|shape| !shape.readable),
        "no readable shape should claim syslog values: {inferred:?}"
    );

    // And the claim is true: the format really does read nothing under Polars.
    let values = [Some("Sep  7 12:00:01")];
    let (_, parsed, _) =
        validate_text_column("t", &syslog.format, ZoneAssumption::Utc, &values).unwrap();
    assert_eq!(parsed, 0);
}

#[test]
fn a_format_that_could_not_survive_a_token_is_refused_where_it_is_typed() {
    assert!(lvu_live::time::text_format_error("%Y-%m-%d").is_none());
    assert!(
        lvu_live::time::text_format_error("%Y|%m")
            .unwrap()
            .contains("'|'")
    );
    assert!(
        lvu_live::time::text_format_error("  ")
            .unwrap()
            .contains("empty")
    );
    assert!(
        lvu_live::time::text_format_error(&"%Y".repeat(40))
            .unwrap()
            .contains("at most")
    );
}
