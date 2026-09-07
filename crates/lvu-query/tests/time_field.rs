//! Declared event-time bases over arbitrary structured or enriched columns.

use lvu_query::time_field::{
    ColumnTimeInterpretation, EpochUnit, TimeColumnSelection, TimeFieldError, ZoneAssumption,
    time_basis_unix_nanos, validate_time_basis,
};
use polars::prelude::*;

/// 2026-09-05T12:30:45Z
const NOON: i64 = 1_788_611_445_000_000_000;

fn frame() -> DataFrame {
    df!(
        "raw" => ["a", "b", "c", "d"],
        "epoch_ms" => [Some(1_788_611_445_123_i64), Some(1_788_611_446_000), None, Some(0)],
        "local" => [
            Some("2026-09-05 12:30:45"),
            Some("2026-09-05 12:30:46.500"),
            Some("not a time at all"),
            None,
        ],
        "offset" => [
            Some("2026-09-05T14:30:45+0200"),
            Some("2026-09-05T12:30:46Z"),
            Some("漢字 👩 wide but not a time"),
            None,
        ],
        "size" => [10_i64, 20, 30, 40],
    )
    .unwrap()
}

#[test]
fn a_declared_epoch_unit_is_authoritative_and_never_re_inferred() {
    let frame = frame();
    let millis = TimeColumnSelection::epoch("epoch_ms", EpochUnit::Milliseconds);
    assert_eq!(
        time_basis_unix_nanos(&frame, &millis).unwrap(),
        vec![
            Some(NOON + 123_000_000),
            Some(NOON + 1_000_000_000),
            None,
            Some(0)
        ]
    );
    // The same column read as seconds is not silently wrapped into a plausible
    // looking instant: it cannot be scaled, so it is null and counted.
    let seconds = TimeColumnSelection::epoch("epoch_ms", EpochUnit::Seconds);
    assert_eq!(time_basis_unix_nanos(&frame, &seconds).unwrap()[0], None);
    let mis_declared = validate_time_basis(&frame, &seconds).unwrap();
    assert_eq!(mis_declared.parsed, 1, "only the zero value still scales");
    assert_eq!(mis_declared.unreadable, 2);

    // A fractional epoch keeps its sub-second digits.
    let fractional = df!("at" => [1_788_611_445.5_f64, 1_788_611_446.25]).unwrap();
    assert_eq!(
        time_basis_unix_nanos(
            &fractional,
            &TimeColumnSelection::epoch("at", EpochUnit::Seconds)
        )
        .unwrap(),
        vec![Some(NOON + 500_000_000), Some(NOON + 1_250_000_000)]
    );

    let validation = validate_time_basis(&frame, &millis).unwrap();
    assert_eq!(validation.rows, 4);
    assert_eq!(validation.parsed, 3);
    assert_eq!(validation.null_inputs, 1);
    assert_eq!(validation.unreadable, 0);
    assert_eq!(validation.first_unix_nanos, Some(0));
    assert!(validation.assumptions.is_empty(), "epochs assume nothing");
}

#[test]
fn a_column_without_a_timezone_is_refused_until_one_is_declared() {
    let frame = frame();
    let naive = TimeColumnSelection::text("local", "%Y-%m-%d %H:%M:%S%.f");
    assert_eq!(
        validate_time_basis(&frame, &naive).unwrap_err(),
        TimeFieldError::ZoneRequired
    );

    let utc = naive.clone().with_zone(ZoneAssumption::Utc);
    assert_eq!(
        time_basis_unix_nanos(&frame, &utc).unwrap(),
        vec![Some(NOON), Some(NOON + 1_500_000_000), None, None]
    );
    let validation = validate_time_basis(&frame, &utc).unwrap();
    assert_eq!(validation.parsed, 2);
    assert_eq!(validation.null_inputs, 1);
    assert_eq!(validation.unreadable, 1, "a malformed value is counted");
    assert!(
        validation
            .assumptions
            .iter()
            .any(|note| note.contains("read as UTC")),
        "{:?}",
        validation.assumptions
    );

    // A different declared offset is a different instant, which is exactly why
    // the zone may not be assumed.
    let east = naive.with_zone(ZoneAssumption::FixedOffsetSeconds(2 * 3600));
    assert_eq!(
        time_basis_unix_nanos(&frame, &east).unwrap()[0],
        Some(NOON - 2 * 3_600_000_000_000)
    );
}

#[test]
fn a_format_that_already_reads_the_zone_refuses_a_second_assumption() {
    let frame = frame();
    let explicit = TimeColumnSelection::text("offset", "%Y-%m-%dT%H:%M:%S%#z");
    let nanos = time_basis_unix_nanos(&frame, &explicit).unwrap();
    assert_eq!(
        nanos,
        vec![Some(NOON), Some(NOON + 1_000_000_000), None, None]
    );
    assert!(
        validate_time_basis(&frame, &explicit)
            .unwrap()
            .assumptions
            .iter()
            .all(|note| !note.contains("no timezone")),
        "nothing is assumed when the value carries its own offset"
    );
    assert_eq!(
        validate_time_basis(&frame, &explicit.with_zone(ZoneAssumption::Utc)).unwrap_err(),
        TimeFieldError::ZoneAlreadyRead
    );
}

#[test]
fn unreadable_and_wide_values_become_nulls_with_a_count_never_dropped_rows() {
    let frame = frame();
    let selection = TimeColumnSelection::text("offset", "%Y-%m-%dT%H:%M:%S%#z");
    let nanos = time_basis_unix_nanos(&frame, &selection).unwrap();
    assert_eq!(nanos.len(), frame.height(), "every row keeps its slot");
    assert_eq!(nanos[2], None, "wide non-time text is null, not an error");
    let validation = validate_time_basis(&frame, &selection).unwrap();
    assert_eq!(validation.unreadable, 1);
    assert!((validation.coverage - 0.5).abs() < 1e-9);
}

#[test]
fn a_basis_is_refused_before_acceptance_when_the_declaration_cannot_hold() {
    let frame = frame();
    assert_eq!(
        validate_time_basis(
            &frame,
            &TimeColumnSelection::epoch("absent", EpochUnit::Seconds)
        )
        .unwrap_err(),
        TimeFieldError::MissingColumn("absent".into())
    );
    assert!(matches!(
        validate_time_basis(
            &frame,
            &TimeColumnSelection::epoch("_lvu_raw", EpochUnit::Seconds)
        )
        .unwrap_err(),
        TimeFieldError::ProtectedColumn(_)
    ));
    assert!(matches!(
        validate_time_basis(
            &frame,
            &TimeColumnSelection::epoch("local", EpochUnit::Seconds)
        )
        .unwrap_err(),
        TimeFieldError::IncompatibleType { .. }
    ));
    assert!(matches!(
        validate_time_basis(
            &frame,
            &TimeColumnSelection::text("size", "%Y-%m-%d %H:%M:%S").with_zone(ZoneAssumption::Utc)
        )
        .unwrap_err(),
        TimeFieldError::IncompatibleType { .. }
    ));
    assert_eq!(
        validate_time_basis(
            &frame,
            &TimeColumnSelection::epoch("epoch_ms", EpochUnit::Seconds)
                .with_zone(ZoneAssumption::Utc)
        )
        .unwrap_err(),
        TimeFieldError::ZoneNotApplicable
    );
    assert_eq!(
        validate_time_basis(&frame, &TimeColumnSelection::text("local", "  ")).unwrap_err(),
        TimeFieldError::FormatRequired
    );
    assert_eq!(
        validate_time_basis(
            &frame,
            &TimeColumnSelection::text("local", "%Y".repeat(40)).with_zone(ZoneAssumption::Utc)
        )
        .unwrap_err(),
        TimeFieldError::FormatTooLong
    );
    assert!(matches!(
        validate_time_basis(
            &frame,
            &TimeColumnSelection {
                column: "size".into(),
                interpretation: ColumnTimeInterpretation::Native,
                zone: ZoneAssumption::Utc,
            }
        )
        .unwrap_err(),
        TimeFieldError::IncompatibleType { .. }
    ));
}

#[test]
fn a_declared_basis_is_independent_of_how_rows_are_batched() {
    let rows = 200usize;
    let values = (0..rows as i64)
        .map(|index| Some(1_788_611_445_000 + index * 1_000))
        .collect::<Vec<_>>();
    let text = (0..rows)
        .map(|index| Some(format!("2026-09-05 12:30:{:02}", index % 60)))
        .collect::<Vec<_>>();
    let frame = df!("epoch_ms" => values, "local" => text).unwrap();
    for selection in [
        TimeColumnSelection::epoch("epoch_ms", EpochUnit::Milliseconds),
        TimeColumnSelection::text("local", "%Y-%m-%d %H:%M:%S").with_zone(ZoneAssumption::Utc),
    ] {
        let whole = time_basis_unix_nanos(&frame, &selection).unwrap();
        for size in [1usize, 7, 64, 1000] {
            let mut batched = Vec::new();
            let mut offset = 0usize;
            while offset < frame.height() {
                let length = size.min(frame.height() - offset);
                let batch = frame.slice(offset as i64, length);
                batched.extend(time_basis_unix_nanos(&batch, &selection).unwrap());
                offset += length;
            }
            assert_eq!(batched, whole, "batch size {size}");
        }
    }
}

#[test]
fn a_native_datetime_column_keeps_its_own_zone_and_needs_one_when_naive() {
    let naive = df!("when" => [Some(NOON / 1_000), None])
        .unwrap()
        .lazy()
        .select([col("when").cast(DataType::Datetime(TimeUnit::Microseconds, None))])
        .collect()
        .unwrap();
    let selection = TimeColumnSelection::native("when");
    assert_eq!(
        validate_time_basis(&naive, &selection).unwrap_err(),
        TimeFieldError::ZoneRequired
    );
    let utc = selection.with_zone(ZoneAssumption::Utc);
    assert_eq!(
        time_basis_unix_nanos(&naive, &utc).unwrap(),
        vec![Some(NOON), None]
    );
    let validation = validate_time_basis(&naive, &utc).unwrap();
    assert_eq!(validation.parsed, 1);
    assert_eq!(validation.null_inputs, 1);
    assert_eq!(validation.unreadable, 0);
}
