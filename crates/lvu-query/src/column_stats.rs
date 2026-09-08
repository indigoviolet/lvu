//! Whole-view statistics for one column, computed by the engine.
//!
//! The Fields dialog describes a field over a bounded sample of the view
//! (`docs/dialog-system.md` §8.12). This computes the same figures over the
//! whole membership instead, one batch at a time, so a scan of any size costs
//! bounded memory.
//!
//! Every figure here is a Polars aggregation. The engine counts; the app names
//! (`AGENTS.md`). That division is not stylistic: the app decides what a number
//! *is* from the record's own bytes, and if this module classified values too,
//! the two could disagree about the same field in the same dialog. So the
//! caller passes the predicate that expresses its own verdict and this counts
//! the rows satisfying it, rather than inferring anything.
//!
//! What the engine cannot express, and the caller must therefore hold: the
//! ordering that `minimum`/`maximum` mean. Polars will happily compare strings
//! lexically, which is the wrong answer for a numeric field spelled as text, so
//! the caller casts the column to the type it has already decided on and this
//! aggregates in that type.

use polars::prelude::*;

/// The expression that reads one field out of a batch.
///
/// A top-level field is a column. A nested one lives as JSON text inside its
/// top-level column, so it is addressed by the JSON path the caller supplies —
/// the same addressing the Fields dialog's own predicates use, so a field
/// counted here is the field a filter would select.
pub fn column_expr(column: &str, json_path: Option<&str>, alias: &str) -> Expr {
    match json_path {
        None => col(column).alias(alias),
        Some(path) => col(column)
            .str()
            .json_path_match(lit(path.to_owned()))
            .alias(alias),
    }
}

/// What the engine counted over the whole view.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ColumnAggregate {
    /// Rows the aggregation saw.
    pub rows: u64,
    /// Rows where the column had a value.
    pub present: u64,
    /// Present values satisfying the caller's predicate, when it passed one.
    pub matching: u64,
    /// Distinct present values. Exact unless `distinct_capped`.
    pub distinct: u64,
    /// `distinct` and `top` stopped being exact at the configured cap.
    pub distinct_capped: bool,
    /// Value and count, most frequent first.
    pub top: Vec<(String, u64)>,
    /// Smallest and largest present value, in the caller's ordering.
    pub minimum: Option<String>,
    pub maximum: Option<String>,
}

/// Accumulates one column's statistics across the batches of a scan.
///
/// Per-batch value counts are folded into a running frame keyed by value, so
/// memory follows the number of distinct values rather than the number of
/// records, and the cap bounds even that.
pub struct ColumnAggregator {
    column: String,
    predicate: Option<Expr>,
    top: usize,
    distinct_cap: usize,
    counts: Option<DataFrame>,
    aggregate: ColumnAggregate,
}

const VALUE: &str = "_lvu_value";
const COUNT: &str = "_lvu_count";

impl ColumnAggregator {
    pub fn new(
        column: impl Into<String>,
        predicate: Option<Expr>,
        top: usize,
        distinct_cap: usize,
    ) -> Self {
        Self {
            column: column.into(),
            predicate,
            top,
            distinct_cap,
            counts: None,
            aggregate: ColumnAggregate::default(),
        }
    }

    /// Fold one batch in. The column must be present and already cast to the
    /// type the caller decided the field has.
    pub fn push(&mut self, frame: &DataFrame) -> Result<(), String> {
        let column = frame.column(&self.column).map_err(|e| e.to_string())?;
        self.aggregate.rows = self.aggregate.rows.saturating_add(frame.height() as u64);
        let present = frame.height().saturating_sub(column.null_count());
        self.aggregate.present = self.aggregate.present.saturating_add(present as u64);
        if let Some(predicate) = &self.predicate {
            let matched = frame
                .clone()
                .lazy()
                .select([predicate.clone().alias("_lvu_matching").sum()])
                .collect()
                .map_err(|e| e.to_string())?;
            let matched = matched
                .column("_lvu_matching")
                .map_err(|e| e.to_string())?
                .get(0)
                .map_err(|e| e.to_string())?
                .try_extract::<u64>()
                .unwrap_or(0);
            self.aggregate.matching = self.aggregate.matching.saturating_add(matched);
        }
        if present == 0 {
            return Ok(());
        }
        // Values as text, because that is what the dialog shows and what the
        // record spelled. Ordering stays in the caller's type, below.
        let batch = frame
            .clone()
            .lazy()
            .select([col(&self.column)
                .drop_nulls()
                .cast(DataType::String)
                .alias(VALUE)])
            .group_by([col(VALUE)])
            .agg([len().alias(COUNT)])
            .collect()
            .map_err(|e| e.to_string())?;
        self.counts = Some(match self.counts.take() {
            None => batch,
            Some(running) => concat([running.lazy(), batch.lazy()], UnionArgs::default())
                .map_err(|e| e.to_string())?
                .group_by([col(VALUE)])
                .agg([col(COUNT).sum()])
                .collect()
                .map_err(|e| e.to_string())?,
        });
        if let Some(running) = &self.counts
            && running.height() > self.distinct_cap
        {
            // Past the cap the exact figures stop being affordable, so keep the
            // most frequent values seen so far and say the rest is a floor.
            self.aggregate.distinct_capped = true;
            let kept = self
                .counts
                .take()
                .expect("checked")
                .lazy()
                .sort(
                    [COUNT],
                    SortMultipleOptions::default().with_order_descending(true),
                )
                .limit(self.distinct_cap as u32)
                .collect()
                .map_err(|e| e.to_string())?;
            self.counts = Some(kept);
        }
        // Extremes in the caller's own ordering: the column arrives already
        // cast, so `min`/`max` compare as that type and the text below is only
        // how the answer is spelled.
        let extremes = frame
            .clone()
            .lazy()
            .select([
                col(&self.column)
                    .min()
                    .cast(DataType::String)
                    .alias("_lvu_min"),
                col(&self.column)
                    .max()
                    .cast(DataType::String)
                    .alias("_lvu_max"),
            ])
            .collect()
            .map_err(|e| e.to_string())?;
        let batch_min = text_at(&extremes, "_lvu_min");
        let batch_max = text_at(&extremes, "_lvu_max");
        merge_extreme(&mut self.aggregate.minimum, batch_min, true);
        merge_extreme(&mut self.aggregate.maximum, batch_max, false);
        Ok(())
    }

    /// Count rows that carry no such column at all.
    ///
    /// A batch whose records predate the field is not an error and not a batch
    /// of nulls to be aggregated: the rows existed and the value was absent,
    /// which is exactly what `rows` minus `present` means.
    pub fn push_absent(&mut self, rows: usize) -> Result<(), String> {
        self.aggregate.rows = self.aggregate.rows.saturating_add(rows as u64);
        Ok(())
    }

    pub fn finish(mut self) -> Result<ColumnAggregate, String> {
        if let Some(counts) = self.counts.take() {
            self.aggregate.distinct = counts.height() as u64;
            let ordered = counts
                .lazy()
                .sort(
                    [COUNT],
                    SortMultipleOptions::default().with_order_descending(true),
                )
                .limit(self.top as u32)
                .collect()
                .map_err(|e| e.to_string())?;
            let values = ordered.column(VALUE).map_err(|e| e.to_string())?;
            let counts = ordered.column(COUNT).map_err(|e| e.to_string())?;
            let mut top = Vec::with_capacity(ordered.height());
            for index in 0..ordered.height() {
                let value = values
                    .get(index)
                    .map_err(|e| e.to_string())?
                    .str_value()
                    .to_string();
                let count = counts
                    .get(index)
                    .map_err(|e| e.to_string())?
                    .try_extract::<u64>()
                    .unwrap_or(0);
                top.push((value, count));
            }
            self.aggregate.top = top;
        }
        Ok(self.aggregate)
    }
}

fn text_at(frame: &DataFrame, name: &str) -> Option<String> {
    let value = frame.column(name).ok()?.get(0).ok()?;
    match value {
        AnyValue::Null => None,
        other => Some(other.str_value().to_string()),
    }
}

/// Extremes merge across batches by the same comparison the batch used, which
/// for a cast column is that column's type. Comparing the rendered text here
/// would reintroduce the lexical ordering the cast exists to avoid, so the
/// caller is expected to have cast; what is compared here is only used to pick
/// between two already-typed answers of the same kind.
fn merge_extreme(held: &mut Option<String>, candidate: Option<String>, keep_smaller: bool) {
    let Some(candidate) = candidate else {
        return;
    };
    match held {
        None => *held = Some(candidate),
        Some(current) => {
            let take = match (current.parse::<f64>(), candidate.parse::<f64>()) {
                (Ok(current_number), Ok(candidate_number)) => {
                    if keep_smaller {
                        candidate_number < current_number
                    } else {
                        candidate_number > current_number
                    }
                }
                _ => {
                    if keep_smaller {
                        candidate.as_str() < current.as_str()
                    } else {
                        candidate.as_str() > current.as_str()
                    }
                }
            };
            if take {
                *current = candidate;
            }
        }
    }
}
