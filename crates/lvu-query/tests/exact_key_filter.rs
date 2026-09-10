use lvu_core::{RecordId, SourceId};
use lvu_query::{ExactFieldConstraint, ExactScalar};
pub use lvu_query::{SEQUENCE_COLUMN, SOURCE_ID_COLUMN};
use polars::df;
use polars::prelude::*;
use uuid::Uuid;

#[path = "../src/exact_key_filter.rs"]
mod exact_key_filter;

use exact_key_filter::{ExactKeyFilterError, exact_column_expr, exact_key_for_record};

fn matching(frame: &DataFrame, constraint: ExactFieldConstraint) -> Vec<u64> {
    let predicate = exact_column_expr(frame, &constraint).unwrap();
    frame
        .clone()
        .lazy()
        .filter(predicate)
        .collect()
        .unwrap()
        .column("row")
        .unwrap()
        .u64()
        .unwrap()
        .into_no_null_iter()
        .collect()
}

#[test]
fn native_equality_excludes_null_and_nonmatching_rows() {
    let frame = df!(
        "row" => &[1_u64, 2, 3, 4],
        "request_key" => &[Some(41_i64), None, Some(42), Some(43)],
    )
    .unwrap();
    let constraint =
        ExactFieldConstraint::new("request_key", ExactScalar::SignedInteger(42)).unwrap();
    assert_eq!(matching(&frame, constraint), vec![3]);
}

#[test]
fn missing_and_typed_null_columns_match_nothing() {
    let frame = df!("row" => &[1_u64, 2]).unwrap();
    let constraint = ExactFieldConstraint::new("request_key", ExactScalar::Bool(false)).unwrap();
    assert!(matching(&frame, constraint.clone()).is_empty());

    let mut typed_null = frame;
    typed_null
        .with_column(
            Series::full_null("request_key".into(), typed_null.height(), &DataType::Null).into(),
        )
        .unwrap();
    assert!(matching(&typed_null, constraint).is_empty());
}

#[test]
fn value_type_and_width_are_not_coerced_lossily() {
    let frame = df!(
        "row" => &[1_u64, 2],
        "small" => &[1_i8, 2],
        "float32" => &[1.25_f32, 2.5],
    )
    .unwrap();
    let too_wide = ExactFieldConstraint::new("small", ExactScalar::SignedInteger(128)).unwrap();
    assert!(matches!(
        exact_column_expr(&frame, &too_wide),
        Err(ExactKeyFilterError::IncompatibleType { .. })
    ));

    let inexact =
        ExactFieldConstraint::new("float32", ExactScalar::finite_float(1.1_f64).unwrap()).unwrap();
    assert!(matches!(
        exact_column_expr(&frame, &inexact),
        Err(ExactKeyFilterError::InexactFloat32 { .. })
    ));

    let exact = ExactFieldConstraint::new(
        "float32",
        ExactScalar::finite_float(f64::from(1.25_f32)).unwrap(),
    )
    .unwrap();
    assert_eq!(matching(&frame, exact), vec![1]);
}

#[test]
fn null_origin_and_incompatible_present_dtype_reject() {
    let frame = df!("row" => &[1_u64], "request_key" => &["42"]).unwrap();
    let null = ExactFieldConstraint::new("request_key", ExactScalar::Null).unwrap();
    assert_eq!(
        exact_column_expr(&frame, &null),
        Err(ExactKeyFilterError::NullSelection)
    );

    let numeric =
        ExactFieldConstraint::new("request_key", ExactScalar::UnsignedInteger(42)).unwrap();
    assert!(matches!(
        exact_column_expr(&frame, &numeric),
        Err(ExactKeyFilterError::IncompatibleType { .. })
    ));
}

#[test]
fn origin_resolution_uses_stable_identity_and_native_value() {
    let source = SourceId(Uuid::from_u128(7));
    let source_text = source.0.to_string();
    let frame = df!(
        SOURCE_ID_COLUMN => &[source_text.as_str(), source_text.as_str()],
        SEQUENCE_COLUMN => &[8_u64, 9],
        "request_key" => &[41_u32, 42],
    )
    .unwrap();
    let (constraint, dtype) = exact_key_for_record(
        &frame,
        RecordId {
            source_id: source,
            sequence: 9,
        },
        "request_key",
    )
    .unwrap();
    assert_eq!(constraint.value(), &ExactScalar::UnsignedInteger(42));
    assert_eq!(constraint.field(), "request_key");
    assert_eq!(dtype, "UInt32");
}

#[test]
fn missing_null_and_unsupported_origin_values_fail_closed() {
    let source = SourceId(Uuid::from_u128(7));
    let source_text = source.0.to_string();
    let id = RecordId {
        source_id: source,
        sequence: 1,
    };
    let frame = df!(
        SOURCE_ID_COLUMN => &[source_text.as_str()],
        SEQUENCE_COLUMN => &[1_u64],
        "request_key" => &[None::<i64>],
    )
    .unwrap();
    assert!(matches!(
        exact_key_for_record(&frame, id, "request_key"),
        Err(ExactKeyFilterError::NullValue { .. })
    ));
    assert!(matches!(
        exact_key_for_record(&frame, id, "other"),
        Err(ExactKeyFilterError::MissingColumn { .. })
    ));

    let mut unsupported = df!(
        SOURCE_ID_COLUMN => &[source_text.as_str()],
        SEQUENCE_COLUMN => &[1_u64],
    )
    .unwrap();
    unsupported
        .with_column(
            Series::new("request_key".into(), &[0_i32])
                .cast(&DataType::Date)
                .unwrap()
                .into(),
        )
        .unwrap();
    assert!(matches!(
        exact_key_for_record(&unsupported, id, "request_key"),
        Err(ExactKeyFilterError::UnsupportedValue { .. })
    ));
}
