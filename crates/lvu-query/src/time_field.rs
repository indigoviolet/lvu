//! Designating an arbitrary structured or enriched column as the event-time
//! basis.
//!
//! The accepted `timestamp_utc` enrichment keeps its own path; this module adds
//! the general case, where the user points at any column and declares how its
//! values are to be read. Nothing here infers a format, a unit or a timezone:
//! an inferred interpretation would be free to change with the next batch, and
//! the whole point of a declared basis is that it does not.
//!
//! Every basis is compiled to one elementwise Polars expression producing
//! `_lvu_selected_time_unix_nanos`, then validated against the real frame
//! schema with the same row-locality rules as any other lvu expression.
//! Unreadable values become nulls with a count, never dropped rows.

use crate::validate::{ExpressionKind, ValidationError, validate_expression_candidate};
use polars::prelude::*;
use thiserror::Error;

/// Output column of a compiled time basis. Matches the snapshot export column
/// so a validated basis and an exported basis cannot disagree.
pub const TIME_BASIS_COLUMN: &str = "_lvu_selected_time_unix_nanos";

/// Longest accepted chrono format string.
pub const MAX_TIME_FORMAT_BYTES: usize = 64;

/// Rows read when validating a basis against a frame.
pub const MAX_TIME_BASIS_SAMPLE_ROWS: usize = 4096;

/// Unit of a numeric epoch column. Tokens match `lvu_live::time::EpochUnit`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EpochUnit {
    Seconds,
    Milliseconds,
    Microseconds,
    Nanoseconds,
}

impl EpochUnit {
    pub fn token(self) -> &'static str {
        match self {
            EpochUnit::Seconds => "s",
            EpochUnit::Milliseconds => "ms",
            EpochUnit::Microseconds => "us",
            EpochUnit::Nanoseconds => "ns",
        }
    }

    pub fn parse_token(token: &str) -> Option<Self> {
        match token {
            "s" | "sec" | "secs" | "seconds" => Some(EpochUnit::Seconds),
            "ms" | "millis" | "milliseconds" => Some(EpochUnit::Milliseconds),
            "us" | "micros" | "microseconds" => Some(EpochUnit::Microseconds),
            "ns" | "nanos" | "nanoseconds" => Some(EpochUnit::Nanoseconds),
            _ => None,
        }
    }

    pub fn nanos_per_unit(self) -> i64 {
        match self {
            EpochUnit::Seconds => 1_000_000_000,
            EpochUnit::Milliseconds => 1_000_000,
            EpochUnit::Microseconds => 1_000,
            EpochUnit::Nanoseconds => 1,
        }
    }
}

/// What to do about values that carry no timezone.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ZoneAssumption {
    /// Refuse the basis. The default, so a zone is never invented.
    #[default]
    Reject,
    Utc,
    /// Seconds east of UTC.
    FixedOffsetSeconds(i32),
}

impl ZoneAssumption {
    fn offset_seconds(self) -> Option<i32> {
        match self {
            ZoneAssumption::Reject => None,
            ZoneAssumption::Utc => Some(0),
            ZoneAssumption::FixedOffsetSeconds(offset) => Some(offset),
        }
    }

    fn note(self) -> String {
        match self {
            ZoneAssumption::Reject => String::new(),
            ZoneAssumption::Utc => "values have no timezone; read as UTC".into(),
            ZoneAssumption::FixedOffsetSeconds(offset) => {
                let sign = if offset < 0 { '-' } else { '+' };
                let absolute = offset.unsigned_abs();
                format!(
                    "values have no timezone; read as UTC{sign}{:02}:{:02}",
                    absolute / 3600,
                    (absolute % 3600) / 60
                )
            }
        }
    }
}

/// How a designated column's values are read.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ColumnTimeInterpretation {
    /// The column is already a Polars `Datetime`.
    Native,
    /// Integer or float epoch in the declared unit.
    Epoch(EpochUnit),
    /// Text parsed with an explicit chrono format.
    Text { format: String },
}

/// A declared event-time basis over one column.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TimeColumnSelection {
    pub column: String,
    pub interpretation: ColumnTimeInterpretation,
    pub zone: ZoneAssumption,
}

impl TimeColumnSelection {
    pub fn epoch(column: impl Into<String>, unit: EpochUnit) -> Self {
        Self {
            column: column.into(),
            interpretation: ColumnTimeInterpretation::Epoch(unit),
            zone: ZoneAssumption::Reject,
        }
    }

    pub fn text(column: impl Into<String>, format: impl Into<String>) -> Self {
        Self {
            column: column.into(),
            interpretation: ColumnTimeInterpretation::Text {
                format: format.into(),
            },
            zone: ZoneAssumption::Reject,
        }
    }

    pub fn native(column: impl Into<String>) -> Self {
        Self {
            column: column.into(),
            interpretation: ColumnTimeInterpretation::Native,
            zone: ZoneAssumption::Reject,
        }
    }

    pub fn with_zone(mut self, zone: ZoneAssumption) -> Self {
        self.zone = zone;
        self
    }
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum TimeFieldError {
    #[error("column {0:?} is not in the current schema")]
    MissingColumn(String),
    #[error("column {0:?} is reserved by lvu and cannot be an event-time basis")]
    ProtectedColumn(String),
    #[error("column {column:?} has type {dtype} and cannot be read as {expected}")]
    IncompatibleType {
        column: String,
        dtype: String,
        expected: String,
    },
    #[error("values carry no timezone; declare UTC or a fixed offset before accepting this basis")]
    ZoneRequired,
    #[error("the declared format already reads a timezone; no assumption can be applied")]
    ZoneAlreadyRead,
    #[error("epoch values are absolute instants; a timezone assumption cannot apply")]
    ZoneNotApplicable,
    #[error("a time format must be declared explicitly; inferred formats vary by batch")]
    FormatRequired,
    #[error("time format exceeds {MAX_TIME_FORMAT_BYTES} bytes")]
    FormatTooLong,
    #[error("{0}")]
    Rejected(String),
    #[error("{0}")]
    Invalid(#[from] ValidationError),
}

/// What a declared basis actually produced against a real frame.
#[derive(Clone, Debug, PartialEq)]
pub struct TimeBasisValidation {
    pub column: String,
    pub rows: usize,
    /// Rows that produced an instant.
    pub parsed: usize,
    /// Rows whose input was already null.
    pub null_inputs: usize,
    /// Rows with a value the declared interpretation could not read.
    pub unreadable: usize,
    pub coverage: f64,
    pub first_unix_nanos: Option<i64>,
    pub last_unix_nanos: Option<i64>,
    /// Assumptions this basis applies, for display before it is accepted.
    pub assumptions: Vec<String>,
}

fn protected(column: &str) -> bool {
    column.starts_with("_lvu_")
}

/// True when a chrono format reads the timezone from the value itself.
///
/// Public because the caller offering a format has to know whether a zone
/// assumption applies to it before it asks for one: declaring UTC for a format
/// that already reads an offset is `ZoneAlreadyRead`, not a refinement.
#[must_use]
pub fn format_reads_zone(format: &str) -> bool {
    ["%z", "%:z", "%#z", "%Z", "%+"]
        .iter()
        .any(|directive| format.contains(directive))
}

/// Compiles a declared basis into one elementwise expression yielding UTC
/// nanoseconds, or explains why the declaration cannot be accepted.
///
/// This is schema-independent; [`validate_time_basis`] checks it against the
/// frame it will actually run on.
pub fn time_basis_expression(selection: &TimeColumnSelection) -> Result<Expr, TimeFieldError> {
    if selection.column.is_empty() {
        return Err(TimeFieldError::Rejected("no column was designated".into()));
    }
    if protected(&selection.column) {
        return Err(TimeFieldError::ProtectedColumn(selection.column.clone()));
    }
    let column = col(selection.column.as_str());
    let expression = match &selection.interpretation {
        ColumnTimeInterpretation::Epoch(unit) => {
            if selection.zone != ZoneAssumption::Reject {
                return Err(TimeFieldError::ZoneNotApplicable);
            }
            epoch_nanos(column, unit.nanos_per_unit())
        }
        ColumnTimeInterpretation::Native => {
            let offset = selection.zone.offset_seconds();
            shift(
                column.cast(DataType::Datetime(TimeUnit::Nanoseconds, None)),
                offset,
            )?
        }
        ColumnTimeInterpretation::Text { format } => {
            if format.trim().is_empty() {
                return Err(TimeFieldError::FormatRequired);
            }
            if format.len() > MAX_TIME_FORMAT_BYTES {
                return Err(TimeFieldError::FormatTooLong);
            }
            let reads_zone = format_reads_zone(format);
            if reads_zone && selection.zone != ZoneAssumption::Reject {
                return Err(TimeFieldError::ZoneAlreadyRead);
            }
            let dtype = if reads_zone {
                DataType::Datetime(TimeUnit::Nanoseconds, Some(TimeZone::UTC))
            } else {
                DataType::Datetime(TimeUnit::Nanoseconds, None)
            };
            let options = StrptimeOptions {
                format: Some(format.as_str().into()),
                // Unreadable values become nulls with a count, not a failed batch.
                strict: false,
                exact: true,
                cache: false,
            };
            let parsed = column
                .cast(DataType::String)
                .str()
                .strptime(dtype, options, lit("raise"));
            let offset = if reads_zone {
                Some(0)
            } else {
                selection.zone.offset_seconds()
            };
            shift(parsed, offset)?
        }
    };
    validate_expression_candidate(&expression, ExpressionKind::Enrichment)?;
    Ok(expression)
}

/// Scales a numeric epoch to nanoseconds.
///
/// The whole part is scaled in `Int64` so nanosecond epochs stay exact, and any
/// fractional part is added separately so a float seconds column does not lose
/// its sub-second digits. Values that cannot be scaled without overflowing the
/// nanosecond range become nulls, which the validation counts, rather than
/// wrapping into a plausible-looking wrong instant.
fn epoch_nanos(column: Expr, scale: i64) -> Expr {
    let whole = column.clone().cast(DataType::Int64);
    let fraction = (column.cast(DataType::Float64) - whole.clone().cast(DataType::Float64))
        * lit(scale as f64);
    let limit = i64::MAX / scale;
    when(
        whole
            .clone()
            .gt_eq(lit(-limit))
            .and(whole.clone().lt_eq(lit(limit))),
    )
    .then(whole * lit(scale) + fraction.cast(DataType::Int64))
    .otherwise(lit(NULL).cast(DataType::Int64))
}

/// Converts a parsed date-time to UTC nanoseconds, applying the declared offset
/// for values that carried no zone of their own.
fn shift(parsed: Expr, offset_seconds: Option<i32>) -> Result<Expr, TimeFieldError> {
    let offset = offset_seconds.ok_or(TimeFieldError::ZoneRequired)?;
    let nanos = parsed.cast(DataType::Int64);
    Ok(if offset == 0 {
        nanos
    } else {
        nanos - lit(i64::from(offset) * 1_000_000_000)
    })
}

fn expected_types(interpretation: &ColumnTimeInterpretation) -> &'static str {
    match interpretation {
        ColumnTimeInterpretation::Epoch(_) => "a numeric epoch",
        ColumnTimeInterpretation::Native => "a datetime",
        ColumnTimeInterpretation::Text { .. } => "text",
    }
}

fn compatible(dtype: &DataType, interpretation: &ColumnTimeInterpretation) -> bool {
    match interpretation {
        ColumnTimeInterpretation::Epoch(_) => dtype.is_primitive_numeric(),
        ColumnTimeInterpretation::Native => matches!(dtype, DataType::Datetime(_, _)),
        ColumnTimeInterpretation::Text { .. } => matches!(dtype, DataType::String),
    }
}

/// Checks a declared basis against the frame it will run on and reports what it
/// actually produced. A basis is not accepted on schema agreement alone.
pub fn validate_time_basis(
    frame: &DataFrame,
    selection: &TimeColumnSelection,
) -> Result<TimeBasisValidation, TimeFieldError> {
    let expression = time_basis_expression(selection)?;
    let column = frame
        .column(selection.column.as_str())
        .map_err(|_| TimeFieldError::MissingColumn(selection.column.clone()))?;
    let dtype = column.dtype();
    if !compatible(dtype, &selection.interpretation) {
        return Err(TimeFieldError::IncompatibleType {
            column: selection.column.clone(),
            dtype: dtype.to_string(),
            expected: expected_types(&selection.interpretation).into(),
        });
    }
    let sample = frame
        .head(Some(MAX_TIME_BASIS_SAMPLE_ROWS.min(frame.height())))
        .select([selection.column.as_str()])
        .map_err(|error| TimeFieldError::Rejected(error.to_string()))?;
    crate::validate::validate_expression_for_frame(&sample, expression.clone())?;
    let nanos = evaluate(&sample, expression)?;
    let null_inputs = sample
        .column(selection.column.as_str())
        .map(|column| column.null_count())
        .unwrap_or_default();
    let parsed = nanos.iter().filter(|value| value.is_some()).count();
    let rows = nanos.len();
    let mut assumptions = Vec::new();
    if selection.zone != ZoneAssumption::Reject {
        assumptions.push(selection.zone.note());
    }
    if let ColumnTimeInterpretation::Text { format } = &selection.interpretation {
        assumptions.push(format!(
            "values are read with the declared format {format:?}"
        ));
    }
    Ok(TimeBasisValidation {
        column: selection.column.clone(),
        rows,
        parsed,
        null_inputs,
        unreadable: rows.saturating_sub(parsed + null_inputs),
        coverage: if rows == 0 {
            0.0
        } else {
            parsed as f64 / rows as f64
        },
        first_unix_nanos: nanos.iter().flatten().copied().min(),
        last_unix_nanos: nanos.iter().flatten().copied().max(),
        assumptions,
    })
}

/// Applies a declared basis to a frame, one row at a time.
///
/// The expression is elementwise, so a frame split into batches yields the same
/// values as the whole frame.
pub fn time_basis_unix_nanos(
    frame: &DataFrame,
    selection: &TimeColumnSelection,
) -> Result<Vec<Option<i64>>, TimeFieldError> {
    let expression = time_basis_expression(selection)?;
    let dtype = frame
        .column(selection.column.as_str())
        .map_err(|_| TimeFieldError::MissingColumn(selection.column.clone()))?
        .dtype();
    if !compatible(dtype, &selection.interpretation) {
        return Err(TimeFieldError::IncompatibleType {
            column: selection.column.clone(),
            dtype: dtype.to_string(),
            expected: expected_types(&selection.interpretation).into(),
        });
    }
    evaluate(frame, expression)
}

fn evaluate(frame: &DataFrame, expression: Expr) -> Result<Vec<Option<i64>>, TimeFieldError> {
    let projected = frame
        .clone()
        .lazy()
        .select([expression.alias(TIME_BASIS_COLUMN)])
        .collect()
        .map_err(|error| TimeFieldError::Rejected(format!("time basis failed: {error}")))?;
    let column = projected
        .column(TIME_BASIS_COLUMN)
        .and_then(|column| column.cast(&DataType::Int64))
        .map_err(|error| TimeFieldError::Rejected(format!("time basis failed: {error}")))?;
    let values = column
        .i64()
        .map_err(|error| TimeFieldError::Rejected(format!("time basis failed: {error}")))?;
    Ok(values.iter().collect())
}
