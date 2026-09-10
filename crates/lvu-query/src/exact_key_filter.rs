//! Native exact-key predicates over an already evaluated typed column.
//!
//! This is the shared-key counterpart to the raw-correlation compatibility
//! projection. It deliberately does not read `_lvu_exact_correlation_value:*`:
//! the caller supplies a frame containing accepted enrichment values, and
//! Polars compares that actual column. A missing or all-null column produces
//! a false predicate for that frame; an incompatible present dtype rejects the
//! candidate so callers can preserve their last-good publication.

use polars::prelude::*;

use lvu_core::RecordId;

use crate::{ExactFieldConstraint, ExactScalar, SEQUENCE_COLUMN, SOURCE_ID_COLUMN};

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ExactKeyFilterError {
    #[error("invalid exact-key constraint: {0}")]
    InvalidConstraint(String),
    #[error("exact-key value is null; select a ready non-null enrichment value")]
    NullSelection,
    #[error("exact-key column {field:?} has type {dtype}, incompatible with {value_kind}")]
    IncompatibleType {
        field: String,
        dtype: String,
        value_kind: &'static str,
    },
    #[error("exact-key Float64 value cannot be represented exactly by Float32 column {field:?}")]
    InexactFloat32 { field: String },
    #[error("exact-key origin identity columns are unavailable: {0}")]
    Identity(String),
    #[error("the selected record is unavailable in the accepted view")]
    MissingOrigin,
    #[error("the selected record appears more than once in the accepted view")]
    DuplicateOrigin,
    #[error("accepted enrichment column {field:?} is unavailable for the selected record")]
    MissingColumn { field: String },
    #[error("accepted enrichment column {field:?} is null for the selected record")]
    NullValue { field: String },
    #[error("accepted enrichment column {field:?} has unsupported native type {dtype}")]
    UnsupportedValue { field: String, dtype: String },
}

fn value_kind(value: &ExactScalar) -> &'static str {
    match value {
        ExactScalar::Null => "null",
        ExactScalar::Bool(_) => "boolean",
        ExactScalar::SignedInteger(_) => "signed integer",
        ExactScalar::UnsignedInteger(_) => "unsigned integer",
        ExactScalar::FloatBits(_) => "float",
        ExactScalar::String(_) => "string",
    }
}

fn incompatible(constraint: &ExactFieldConstraint, dtype: &DataType) -> ExactKeyFilterError {
    ExactKeyFilterError::IncompatibleType {
        field: constraint.field().to_owned(),
        dtype: format!("{dtype:?}"),
        value_kind: value_kind(constraint.value()),
    }
}

fn signed_fits(value: i64, dtype: &DataType) -> bool {
    match dtype {
        DataType::Int8 => i8::try_from(value).is_ok(),
        DataType::Int16 => i16::try_from(value).is_ok(),
        DataType::Int32 => i32::try_from(value).is_ok(),
        DataType::Int64 => true,
        _ => false,
    }
}

fn unsigned_fits(value: u64, dtype: &DataType) -> bool {
    match dtype {
        DataType::UInt8 => u8::try_from(value).is_ok(),
        DataType::UInt16 => u16::try_from(value).is_ok(),
        DataType::UInt32 => u32::try_from(value).is_ok(),
        DataType::UInt64 => true,
        _ => false,
    }
}

/// Build native Polars equality for one exact enrichment key.
///
/// The literal is checked against and cast to the frame's physical dtype, so
/// integer width and Float32 representation are proven before evaluation.
/// Missing and `Null` columns return `lit(false)`: rows without the shared key
/// simply fail the ordinary predicate. Other type mismatches are definition
/// errors, not empty membership.
pub fn exact_column_expr(
    frame: &DataFrame,
    constraint: &ExactFieldConstraint,
) -> Result<Expr, ExactKeyFilterError> {
    constraint
        .validate()
        .map_err(|error| ExactKeyFilterError::InvalidConstraint(error.to_string()))?;
    if matches!(constraint.value(), ExactScalar::Null) {
        return Err(ExactKeyFilterError::NullSelection);
    }
    let Ok(column) = frame.column(constraint.field()) else {
        return Ok(lit(false));
    };
    let dtype = column.dtype();
    if dtype == &DataType::Null {
        return Ok(lit(false));
    }
    let literal = match constraint.value() {
        ExactScalar::Null => unreachable!("rejected above"),
        ExactScalar::Bool(value) if dtype == &DataType::Boolean => lit(*value),
        ExactScalar::String(value) if dtype == &DataType::String => lit(value.clone()),
        ExactScalar::SignedInteger(value) if signed_fits(*value, dtype) => {
            lit(*value).cast(dtype.clone())
        }
        ExactScalar::UnsignedInteger(value) if unsigned_fits(*value, dtype) => {
            lit(*value).cast(dtype.clone())
        }
        ExactScalar::FloatBits(bits) if dtype == &DataType::Float64 => lit(f64::from_bits(*bits)),
        ExactScalar::FloatBits(bits) if dtype == &DataType::Float32 => {
            let value = f64::from_bits(*bits);
            let narrowed = value as f32;
            if f64::from(narrowed).to_bits() != value.to_bits() {
                return Err(ExactKeyFilterError::InexactFloat32 {
                    field: constraint.field().to_owned(),
                });
            }
            lit(narrowed)
        }
        _ => return Err(incompatible(constraint, dtype)),
    };
    Ok(col(constraint.field()).eq(literal))
}

/// Resolve one selected record's exact key from an accepted typed frame.
///
/// Polars performs the stable-identity selection. The returned dtype string is
/// evidence retained by the controller until the origin fence is rechecked;
/// the persisted value remains only [`ExactFieldConstraint`].
pub fn exact_key_for_record(
    frame: &DataFrame,
    record_id: RecordId,
    field: &str,
) -> Result<(ExactFieldConstraint, String), ExactKeyFilterError> {
    frame
        .column(SOURCE_ID_COLUMN)
        .and_then(|column| column.str().map(|_| ()))
        .map_err(|error| ExactKeyFilterError::Identity(error.to_string()))?;
    frame
        .column(SEQUENCE_COLUMN)
        .and_then(|column| column.u64().map(|_| ()))
        .map_err(|error| ExactKeyFilterError::Identity(error.to_string()))?;
    let selected = frame
        .clone()
        .lazy()
        .filter(
            col(SOURCE_ID_COLUMN)
                .eq(lit(record_id.source_id.0.to_string()))
                .and(col(SEQUENCE_COLUMN).eq(lit(record_id.sequence))),
        )
        .collect()
        .map_err(|error| ExactKeyFilterError::Identity(error.to_string()))?;
    match selected.height() {
        0 => return Err(ExactKeyFilterError::MissingOrigin),
        1 => {}
        _ => return Err(ExactKeyFilterError::DuplicateOrigin),
    }
    let column = selected
        .column(field)
        .map_err(|_| ExactKeyFilterError::MissingColumn {
            field: field.to_owned(),
        })?;
    let dtype = format!("{:?}", column.dtype());
    let value = column
        .get(0)
        .map_err(|error| ExactKeyFilterError::Identity(error.to_string()))?;
    let scalar = match value {
        AnyValue::Null => {
            return Err(ExactKeyFilterError::NullValue {
                field: field.to_owned(),
            });
        }
        AnyValue::Boolean(value) => ExactScalar::Bool(value),
        AnyValue::Int8(value) => ExactScalar::SignedInteger(i64::from(value)),
        AnyValue::Int16(value) => ExactScalar::SignedInteger(i64::from(value)),
        AnyValue::Int32(value) => ExactScalar::SignedInteger(i64::from(value)),
        AnyValue::Int64(value) => ExactScalar::SignedInteger(value),
        AnyValue::UInt8(value) => ExactScalar::UnsignedInteger(u64::from(value)),
        AnyValue::UInt16(value) => ExactScalar::UnsignedInteger(u64::from(value)),
        AnyValue::UInt32(value) => ExactScalar::UnsignedInteger(u64::from(value)),
        AnyValue::UInt64(value) => ExactScalar::UnsignedInteger(value),
        AnyValue::Float32(value) => ExactScalar::finite_float(f64::from(value))
            .map_err(|error| ExactKeyFilterError::InvalidConstraint(error.to_string()))?,
        AnyValue::Float64(value) => ExactScalar::finite_float(value)
            .map_err(|error| ExactKeyFilterError::InvalidConstraint(error.to_string()))?,
        AnyValue::String(value) => ExactScalar::string(value)
            .map_err(|error| ExactKeyFilterError::InvalidConstraint(error.to_string()))?,
        AnyValue::StringOwned(value) => ExactScalar::string(value.as_str())
            .map_err(|error| ExactKeyFilterError::InvalidConstraint(error.to_string()))?,
        value => {
            return Err(ExactKeyFilterError::UnsupportedValue {
                field: field.to_owned(),
                dtype: format!("{:?}", value.dtype()),
            });
        }
    };
    let constraint = ExactFieldConstraint::new(field, scalar)
        .map_err(|error| ExactKeyFilterError::InvalidConstraint(error.to_string()))?;
    Ok((constraint, dtype))
}
