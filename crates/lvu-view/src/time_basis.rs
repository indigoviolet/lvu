//! Applying a user-declared event-time basis.
//!
//! `lvu` holds only the token, because `lvu-live` depends on it and cannot be a
//! dependency of it. This module is where the token is turned back into a
//! reading: raw fields row by row through `lvu_live::TimeFieldSelection::read`,
//! enriched columns through the expression `lvu_query::time_basis_expression`
//! compiles. Unreadable values become misses with a count, never dropped rows.

use lvu_live::time::{TimeFieldRef, TimeFieldSelection, TimeInterpretation, TimeOutcome};
use lvu_query::time_field::{
    ColumnTimeInterpretation, EpochUnit as ColumnEpochUnit, TIME_BASIS_COLUMN, TimeColumnSelection,
    ZoneAssumption, time_basis_expression,
};
use polars::prelude::*;

/// The output column the `Extracted` basis reads.
pub const EXTRACTED_COLUMN: &str = "timestamp_utc";

/// The reading the `Extracted` basis applies to [`EXTRACTED_COLUMN`].
///
/// `timestamp_utc` is declared to hold UTC RFC3339 text, so it is a designated
/// column like any other and the engine compiles its reading the same way. The
/// app used to parse these characters itself, one record at a time, which was a
/// second evaluator for a job `ColumnTimeInterpretation` already does.
pub fn extracted_selection() -> TimeColumnSelection {
    TimeColumnSelection {
        column: EXTRACTED_COLUMN.into(),
        interpretation: ColumnTimeInterpretation::Rfc3339,
        zone: ZoneAssumption::Reject,
    }
}

/// Reads `timestamp_utc` for a whole batch, in the order `values` is given.
///
/// Returns one entry per value: `Some` for an instant, `None` for a value the
/// reading could not use, so the caller counts its own misses in the terms its
/// diagnostics are written in. An error means the reading itself could not be
/// compiled, which no caller can recover from per record.
pub fn read_extracted(values: &[Option<&str>]) -> Result<Vec<Option<i64>>, String> {
    read_column(&extracted_selection(), values.len(), values)
}

/// Per-record instants under a declared basis, plus why the misses missed.
#[derive(Debug, Default)]
pub struct SelectedTimes {
    /// Record sequence to UTC nanoseconds. Absent means the record had no
    /// readable value.
    pub by_sequence: std::collections::HashMap<u64, i64>,
    pub invalid: usize,
    pub missing: usize,
    /// Set when the declaration itself could not be honoured, in which case no
    /// record has a time. The caller reports it rather than silently matching
    /// nothing under a basis it never applied.
    pub error: Option<String>,
}

/// The `TimeColumnSelection` a token describes, when it describes one.
///
/// `TimeInterpretation::Text` carries the chrono format the query layer
/// requires in `TimeFieldSelection::text_format`. It is inferred from a bounded
/// sample and shown with its match rate before it is accepted, so the format is
/// declared — the user saw it and could change it — rather than guessed.
pub fn column_selection(selection: &TimeFieldSelection) -> Result<TimeColumnSelection, String> {
    let TimeFieldRef::Column(name) = &selection.field else {
        return Err("not a column basis".into());
    };
    let column = match selection.interpretation {
        TimeInterpretation::Epoch(unit) => TimeColumnSelection {
            column: name.clone(),
            interpretation: ColumnTimeInterpretation::Epoch(match unit.token() {
                "s" => ColumnEpochUnit::Seconds,
                "ms" => ColumnEpochUnit::Milliseconds,
                "us" => ColumnEpochUnit::Microseconds,
                _ => ColumnEpochUnit::Nanoseconds,
            }),
            zone: lvu_query::time_field::ZoneAssumption::Reject,
        },
        TimeInterpretation::Text => {
            let Some(format) = selection.text_format.clone() else {
                return Err(format!(
                    "column {name:?} is read as text, which needs an explicit time format"
                ));
            };
            TimeColumnSelection {
                column: name.clone(),
                interpretation: ColumnTimeInterpretation::Text { format },
                zone: match selection.zone {
                    lvu_live::time::ZoneAssumption::Utc => {
                        lvu_query::time_field::ZoneAssumption::Utc
                    }
                    lvu_live::time::ZoneAssumption::FixedOffsetSeconds(offset) => {
                        lvu_query::time_field::ZoneAssumption::FixedOffsetSeconds(offset)
                    }
                    lvu_live::time::ZoneAssumption::Reject => {
                        lvu_query::time_field::ZoneAssumption::Reject
                    }
                },
            }
        }
        TimeInterpretation::Auto => {
            return Err(format!(
                "column {name:?} needs a declared epoch unit or an explicit time format"
            ));
        }
    };
    Ok(column)
}

/// Reads every record under `token`.
///
/// `column_value` supplies an enriched column's value for a record, so this
/// module stays independent of how the caller stores derived values.
pub fn read_records<'a, F>(
    token: &str,
    records: &'a [lvu_core::RawRecord],
    mut column_value: F,
) -> SelectedTimes
where
    F: FnMut(&lvu_core::RawRecord) -> Option<&'a str>,
{
    let mut times = SelectedTimes::default();
    let selection = match TimeFieldSelection::parse_token(token) {
        Ok(selection) => selection,
        Err(error) => {
            times.error = Some(format!("event-time basis could not be read: {error}"));
            return times;
        }
    };
    if let TimeFieldRef::Column(_) = &selection.field {
        let column = match column_selection(&selection) {
            Ok(column) => column,
            Err(error) => {
                times.error = Some(error);
                return times;
            }
        };
        let values: Vec<Option<&str>> = records.iter().map(&mut column_value).collect();
        for (record, value) in records.iter().zip(values.iter()) {
            let _ = record;
            if value.is_none() {
                times.missing += 1;
            }
        }
        match read_column(&column, records.len(), &values) {
            Ok(resolved) => {
                for (record, nanos) in records.iter().zip(resolved) {
                    match nanos {
                        Some(nanos) => {
                            times.by_sequence.insert(record.record_id.sequence, nanos);
                        }
                        None => times.invalid += 1,
                    }
                }
                // Missing inputs were already counted; do not count them twice.
                times.invalid = times.invalid.saturating_sub(times.missing);
            }
            Err(error) => times.error = Some(error),
        }
        return times;
    }
    for record in records {
        match selection.read(&record.bytes) {
            TimeOutcome::Valid(reading) => {
                times
                    .by_sequence
                    .insert(record.record_id.sequence, reading.unix_nanos);
            }
            TimeOutcome::Missing => times.missing += 1,
            _ => times.invalid += 1,
        }
    }
    times
}

/// Evaluates a compiled column basis over the batch's values.
fn read_column(
    column: &TimeColumnSelection,
    height: usize,
    values: &[Option<&str>],
) -> Result<Vec<Option<i64>>, String> {
    let expression = time_basis_expression(column).map_err(|error| error.to_string())?;
    // An enriched column reaches this crate as text. The declared reading says
    // what those characters are; an epoch basis is numeric, so it is parsed
    // here rather than compared as a string.
    let series = match &column.interpretation {
        ColumnTimeInterpretation::Epoch(_) => Series::new(
            column.column.as_str().into(),
            values
                .iter()
                .map(|value| value.and_then(|value| value.trim().parse::<i64>().ok()))
                .collect::<Vec<_>>(),
        ),
        _ => Series::new(column.column.as_str().into(), values.to_vec()),
    };
    let frame =
        DataFrame::new(height, vec![Column::from(series)]).map_err(|error| error.to_string())?;
    let resolved = frame
        .lazy()
        .select([expression.alias(TIME_BASIS_COLUMN)])
        .collect()
        .map_err(|error| error.to_string())?;
    let series = resolved
        .column(TIME_BASIS_COLUMN)
        .map_err(|error| error.to_string())?
        .i64()
        .map_err(|error| error.to_string())?
        .iter()
        .collect::<Vec<_>>();
    Ok(series)
}

/// Validates one epoch reading of a column against sampled values, so a caller
/// without a Polars frame can still show what the query layer actually
/// resolved. Returns the coverage and the assumptions the reading applies.
pub fn validate_epoch_column(
    name: &str,
    unit: ColumnEpochUnit,
    values: &[Option<i64>],
) -> Result<(f64, usize, Vec<String>), String> {
    let frame = DataFrame::new(
        values.len(),
        vec![Column::from(Series::new(name.into(), values.to_vec()))],
    )
    .map_err(|error| error.to_string())?;
    let validation =
        lvu_query::time_field::validate_time_basis(&frame, &TimeColumnSelection::epoch(name, unit))
            .map_err(|error| error.to_string())?;
    Ok((
        validation.coverage,
        validation.parsed,
        validation.assumptions,
    ))
}

/// Validates one text reading of a column against sampled values, the way
/// [`validate_epoch_column`] does for an epoch reading.
///
/// The share it reports is measured by the parser the basis will actually run
/// under, so a format that the shape matcher liked but Polars cannot read is
/// reported as reading nothing rather than silently accepted.
pub fn validate_text_column(
    name: &str,
    format: &str,
    zone: lvu_live::time::ZoneAssumption,
    values: &[Option<&str>],
) -> Result<(f64, usize, Vec<String>), String> {
    if let Some(error) = lvu_live::time::text_format_error(format) {
        return Err(error);
    }
    let frame = DataFrame::new(
        values.len(),
        vec![Column::from(Series::new(name.into(), values.to_vec()))],
    )
    .map_err(|error| error.to_string())?;
    let mut selection = TimeColumnSelection::text(name, format);
    selection.zone = match zone {
        lvu_live::time::ZoneAssumption::Utc => lvu_query::time_field::ZoneAssumption::Utc,
        lvu_live::time::ZoneAssumption::FixedOffsetSeconds(offset) => {
            lvu_query::time_field::ZoneAssumption::FixedOffsetSeconds(offset)
        }
        lvu_live::time::ZoneAssumption::Reject => lvu_query::time_field::ZoneAssumption::Reject,
    };
    let validation = lvu_query::time_field::validate_time_basis(&frame, &selection)
        .map_err(|error| error.to_string())?;
    Ok((
        validation.coverage,
        validation.parsed,
        validation.assumptions,
    ))
}
