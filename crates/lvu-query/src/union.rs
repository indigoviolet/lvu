//! Union of accepted typed frames in deterministic timestamp order.
//!
//! Ownership: union-views worktree (new file, exported from `lib.rs`).
//! Kept distinct from any grouping helper by construction: this merges whole
//! accepted frames across views (concat, identity dedup, total ordering),
//! grouping folds adjacent rows within one view and never does any of these.
//!
//! The query engine computes; the app names and presents (AGENTS.md):
//! diagonal concatenation, stable-identity deduplication and the total
//! timestamp ordering all execute here in Polars, and `lvu-view` holds only
//! the spec/contract layer on top.
//!
//! Pipeline, in order:
//! 1. Tag each frame with `input_column` (its input position). Frames already
//!    carrying that name are rejected: the tag is provenance, and a silent
//!    overwrite would corrupt tiebreaks.
//! 2. Diagonal concat in input order, collected once: every column is kept
//!    and absent fields arrive as null.
//! 3. `unique_stable` on the canonical identity columns with first keep: the
//!    same stable RecordId in several inputs is emitted once with the FIRST
//!    input's whole projection. Precedes the sort so a duplicate whose
//!    projections disagree on time still loses to the earlier input.
//! 4. One Polars sort by `(timestamp, input, sequence, source)`, ascending,
//!    nulls last. The key is unique per surviving row (identity is unique and
//!    every row carries exactly one input tag), so the order is total and
//!    deterministic across restarts without relying on sort stability.
//!
//! Dtype conflicts across inputs for one column are a Polars schema error,
//! surfaced as `Err`: the candidate is rejected, the prior union stands, and
//! normalization stays where it belongs — ordinary upstream enrichment.

use lvu_core::{ExactFieldConstraint, ExactScalar};
use polars::prelude::*;

/// Merge accepted typed frames into one timestamp-ordered frame.
///
/// `timestamp_column` carries `i64` unix nanos with null for missing;
/// `source_column`/`sequence_column` are the canonical stable-identity
/// columns; `input_column` names the provenance tag this function adds
/// (`u32` input position, retained in the output).
pub fn union_sorted_frames(
    frames: Vec<DataFrame>,
    timestamp_column: &str,
    source_column: &str,
    sequence_column: &str,
    input_column: &str,
) -> Result<DataFrame, String> {
    if frames.len() < 2 {
        return Err("a union needs at least two input frames".into());
    }
    for frame in &frames {
        if frame
            .get_column_names()
            .iter()
            .any(|name| name.as_str() == input_column)
        {
            return Err(format!(
                "input frame already carries the protected column '{input_column}'"
            ));
        }
    }
    let tagged: Vec<LazyFrame> = frames
        .into_iter()
        .enumerate()
        .map(|(position, frame)| {
            frame
                .lazy()
                .with_column(lit(position as u32).alias(input_column))
        })
        .collect();
    let stacked = concat(
        tagged,
        UnionArgs {
            diagonal: true,
            ..Default::default()
        },
    )
    .map_err(|error| error.to_string())?
    .collect()
    .map_err(|error| error.to_string())?;
    let identity = [source_column.to_owned(), sequence_column.to_owned()];
    let deduped = stacked
        .unique_stable(Some(&identity), UniqueKeepStrategy::First, None)
        .map_err(|error| error.to_string())?;
    deduped
        .sort(
            [
                timestamp_column,
                input_column,
                sequence_column,
                source_column,
            ],
            SortMultipleOptions::default().with_nulls_last(true),
        )
        .map_err(|error| error.to_string())
}

/// Apply one exact typed key over a merged union frame: native Polars
/// equality against the ACTUAL merged typed column, never the legacy
/// `_lvu_exact_correlation_value` token projection.
///
/// This is the single canonical exact-key predicate for unions (correlation
/// replacement): the worker applies it after concat/dedup alongside search,
/// and the correlation controller resolves the selected key value through
/// frozen precise replay before submitting. One DTO
/// (`lvu_core::ExactFieldConstraint`), one predicate, no second evaluator.
///
/// Semantics: a missing column matches nothing (same as the search path's
/// absent-column rule); null cells never match (Polars three-valued
/// equality); a carrier/dtype mismatch rejects the candidate instead of
/// silently coercing (float/int mixing, string/number mixing, bool/anything
/// mixing). Integer widening within one signedness is exact and allowed;
/// float comparison runs in binary64, which is exact for finite values.
pub fn exact_key_filter(
    frame: DataFrame,
    constraint: &ExactFieldConstraint,
) -> Result<DataFrame, String> {
    constraint.validate().map_err(|error| error.to_string())?;
    let field = constraint.field();
    let Ok(column) = frame.column(field) else {
        return Ok(frame.clear());
    };
    if matches!(constraint.value(), ExactScalar::Null) {
        return Ok(frame.clear());
    }
    let predicate = match (column.dtype(), constraint.value()) {
        (DataType::String, ExactScalar::String(value)) => col(field).eq(lit(value.clone())),
        (DataType::Boolean, ExactScalar::Bool(value)) => col(field).eq(lit(*value)),
        (
            DataType::Int8 | DataType::Int16 | DataType::Int32 | DataType::Int64,
            ExactScalar::SignedInteger(value),
        ) => col(field).eq(lit(*value)),
        (
            DataType::UInt8 | DataType::UInt16 | DataType::UInt32 | DataType::UInt64,
            ExactScalar::UnsignedInteger(value),
        ) => col(field).eq(lit(*value)),
        (DataType::Float32 | DataType::Float64, ExactScalar::FloatBits(bits)) => {
            col(field).eq(lit(f64::from_bits(*bits)))
        }
        (dtype, scalar) => {
            return Err(format!(
                "exact key on {field:?} needs a matching column type, found {dtype:?} for {scalar:?}"
            ));
        }
    };
    frame
        .lazy()
        .filter(predicate)
        .collect()
        .map_err(|error| error.to_string())
}
