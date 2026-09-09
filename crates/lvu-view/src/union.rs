//! Live union view over accepted input views.
//!
//! A union derived source is `view1 + view2` across two (or more) sources,
//! merged in timestamp order, with ordinary downstream filters, grouping and
//! folding applying on top of the merged stream.
//!
//! Ownership: Muse union-views assignment. This file is new and owned here;
//! the hooks that register it with the adapter live in primary-owned files
//! and are proposed verbatim at the bottom of this module (`PROPOSED HOOKS`).
//! Until those land this module is exercised through
//! `crates/lvu-view/tests/union_views.rs`, which includes it by path, so no
//! shared manifest or existing module is touched.
//!
//! Semantics (user request, preserved here):
//!
//! * A union is a LIVE VIEW referencing accepted input views. It never
//!   recaptures bytes and never assigns new source/record IDs: every merged
//!   row keeps its stable `(source_id, sequence)` identity, which is also what
//!   raw context, bookmarks and correlation keep addressing.
//! * Enrichment is the sole mechanism for pattern/key/time extraction. The
//!   union consumes the inputs' accepted typed columns (in particular the
//!   timestamp column the inputs' time basis resolved) and performs no parsing
//!   of its own. Source-specific schema differences are normalized by ordinary
//!   upstream enrichment; fields absent on one side arrive as null.
//! * The union reads each input's UNFOLDED physical membership. Presentation
//!   collapse (folding/grouping) of an input is never copied into the union;
//!   the union has its own folding/grouping downstream like any other view.
//! * Accepted revisions and editor drafts are distinct per input. A union
//!   candidate fences on the accepted revision of every input; an input that
//!   advances underneath a pending candidate makes it stale, and a failed
//!   candidate preserves the prior accepted union untouched. This module never
//!   mutates published state: every function here is pure, and publication
//!   stays with the adapter's existing revision-fenced worker path.
//! * Missing timestamps sort last. Equal timestamps break by input order, then
//!   by original sequence (with a final source-identity tiebreak so the order
//!   is total and deterministic across restarts). The same stable RecordId
//!   appearing in two inputs is emitted once, with first-input precedence for
//!   differing derived projections.
//! * No cycle or self-reference: a union may not (transitively) contain
//!   itself. Input count, row count, scan work and snapshot work are bounded.
//!
//! Engine/app division (AGENTS.md): Polars owns the union/sort/filter/equality
//! compute over typed values — diagonal concatenation, the timestamp ordering
//! with nulls last, and the value comparisons the tests assert through real
//! execution. The one hand-rolled pass is stable-identity deduplication and
//! the final tiebreak arrangement, which the engine cannot express as an
//! invariant: stable RecordIds are the raw-bytes/stable-identity invariant,
//! and first-input precedence is a provenance rule, not a value. This is the
//! same exemption `merge_order` in `lib.rs` already operates under: the engine
//! produces keys, this layer arranges rows already in hand.
//!
//! PROPOSED HOOKS (primary-owned; do not apply without assignment):
//!
//! 1. `crates/lvu-view/src/lib.rs`, next to the other `pub mod` lines:
//!      `pub mod union;`
//!    plus a re-export of the validated surface:
//!      `pub use union::{UnionError, UnionInputSnapshot, UnionInputRow,`
//!      `  MergedUnionRow, UnionRecordId, StoredUnionShape, MAX_UNION_INPUTS,`
//!      `  MAX_UNION_ROWS, INPUT_COLUMN, validate_union_spec,`
//!      `  detect_union_cycle, merge_union_rows, union_typed_frames};`
//! 2. Adapter registration (primary designs with the grouping/source owners):
//!    a `UnionRegistration { inputs: Vec<(ViewId, accepted_revision)> }`
//!    beside `ViewRegistration`, a `register_union_view` sibling of
//!    `register_view`, and a worker path that reuses the existing
//!    revision/base-constraint fencing with a union purpose. The worker reads
//!    inputs through `rows().unfolded_page`, never through folded pages.
//! 3. `crates/lvu/src/app.rs`: a `QueryPurpose::Union` variant (touches the
//!    terminal dispatch and the fork logic; coordinate with W22 grouping and
//!    the source-assistance owner), and union input state on the derived view.
//! 4. Persistence: `StoredUnionShape` below is shaped for an additive
//!    `presentation_json.union` key with `#[serde(default)]`, following the
//!    `color_rules` precedent in `contracts.md` — no `DB_SCHEMA_VERSION` bump.
//!    An older binary drops the key on save; that trade is documented, not
//!    hidden. Remembered union inputs must never launch commands on restore.
//! 5. Export/archive (`export.rs`, primary-owned): a union snapshot freezes
//!    input view IDs, their accepted revisions and each input's capture
//!    high-watermarks, and replays input batch boundaries the way accepted
//!    enrichment boundaries are replayed today, so part sizing cannot change
//!    typed interpretation when inputs advance independently.

use std::collections::{HashMap, HashSet};

use polars::prelude::*;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// How many accepted input views one union may reference.
///
/// Eight inputs of bounded memberships keep the re-merge, the dependency walk
/// and the snapshot fan-out affordable; a wider union is a second union over
/// unions, which the cycle check already keeps acyclic.
pub const MAX_UNION_INPUTS: usize = 8;

/// Upper bound on the merged row count one union publication may hold.
///
/// Inputs are already bounded memberships; this caps their sum so a union over
/// very large views fails as a rejected candidate (prior union preserved)
/// instead of as an unbounded allocation.
pub const MAX_UNION_ROWS: usize = 1_000_000;

/// Longest view identity a union spec accepts, in bytes.
///
/// View IDs are UUID strings in practice; this only stops a corrupt draft from
/// becoming an unbounded allocation.
pub const MAX_UNION_VIEW_ID_BYTES: usize = 256;

/// Provenance tag recording which input position a merged row came from.
///
/// A protected-style `_lvu_*` name so it can never collide with a user field:
/// inputs carrying this column are rejected as candidates rather than merged
/// with a silently overwritten tag. Retained in the output as provenance for
/// raw context and tiebreak audits; bounded to one `u32` per row.
pub const INPUT_COLUMN: &str = "_lvu_union_input";

/// Stable record identity shared with the rest of the product.
///
/// Maps 1:1 onto `lvu::RowId` and `lvu_core::RecordId` (source identity plus
/// monotonic sequence); redeclared here so this module stays dependency-light
/// and path-includable in tests without touching the crate graph.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct UnionRecordId {
    pub source_id: String,
    pub sequence: u64,
}

impl UnionRecordId {
    pub fn new(source_id: impl Into<String>, sequence: u64) -> Self {
        Self {
            source_id: source_id.into(),
            sequence,
        }
    }
}

/// One member of an input's unfolded physical membership, with its accepted
/// timestamp already extracted.
///
/// `timestamp_nanos` is the input's accepted typed time for this record in its
/// union basis (event/extracted/selected time as `i64` unix nanos, or capture
/// nanos under the capture basis). `None` is a missing/unreadable value in
/// that basis — never an error, never a reason to drop the record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnionInputRow {
    pub id: UnionRecordId,
    pub timestamp_nanos: Option<i64>,
}

/// The accepted state of one input view that a union candidate fences on.
///
/// `rows` is the input's UNFOLDED membership (see
/// `RowProvider::unfolded_page`): presentation folding of the input is never
/// an input to the union. `accepted_revision` is the input's applied query
/// revision the rows were published under; the candidate is stale once the
/// input's applied revision moves past it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnionInputSnapshot {
    pub view_id: String,
    pub accepted_revision: u64,
    pub rows: Vec<UnionInputRow>,
}

/// One merged row: its preserved identity, the winning input position, and the
/// timestamp it sorted under.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MergedUnionRow {
    pub id: UnionRecordId,
    pub input: usize,
    pub timestamp_nanos: Option<i64>,
}

/// Additive persistence shape for a union's inputs.
///
/// Intended as a `presentation_json.union` key with `#[serde(default)]` on the
/// reading side (the `color_rules` precedent): no schema bump, unknown fields
/// ignored, an older binary reading a newer row sees no union. Only view IDs
/// and accepted revisions persist — never commands, which must not relaunch on
/// restore.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct StoredUnionShape {
    #[serde(default)]
    pub inputs: Vec<StoredUnionInput>,
}

/// One persisted union input reference.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct StoredUnionInput {
    #[serde(default)]
    pub view_id: String,
    #[serde(default)]
    pub accepted_revision: u64,
}

/// Every way a union candidate can be rejected.
///
/// Rejection is always total: the caller keeps its prior accepted union, live
/// refresh continues on it, and the rejected draft stays editable. Pending is
/// never a failed predicate.
#[derive(Debug, Error, Eq, PartialEq)]
pub enum UnionError {
    #[error("a union needs at least two input views")]
    EmptyInputs,
    #[error("a union takes at most {maximum} input views, got {count}")]
    TooManyInputs { count: usize, maximum: usize },
    #[error("input view '{view_id}' is listed twice")]
    DuplicateInput { view_id: String },
    #[error("a union cannot contain itself ('{view_id}')")]
    SelfReference { view_id: String },
    #[error("union inputs contain a cycle: {path}")]
    UnionCycle { path: String },
    #[error("union would hold {rows} rows, above the {maximum} bound")]
    RowLimit { rows: usize, maximum: usize },
    #[error("view identity exceeds {maximum} bytes")]
    ViewIdTooLong { maximum: usize },
    #[error("input frame already carries the protected column '{name}'")]
    ProtectedColumn { name: String },
    #[error("timestamp column '{name}' is missing or unreadable: {reason}")]
    BadTimestampColumn { name: String, reason: String },
    #[error("identity column '{name}' is missing or unreadable: {reason}")]
    BadIdentityColumn { name: String, reason: String },
    #[error("union engine: {reason}")]
    Engine { reason: String },
}

/// Structural validation of a union spec: count, duplicates, self-reference
/// and identity bounds. Transitive cycles need the dependency graph; see
/// [`detect_union_cycle`].
pub fn validate_union_spec(union_view_id: &str, input_view_ids: &[&str]) -> Result<(), UnionError> {
    if input_view_ids.len() < 2 {
        return Err(UnionError::EmptyInputs);
    }
    if input_view_ids.len() > MAX_UNION_INPUTS {
        return Err(UnionError::TooManyInputs {
            count: input_view_ids.len(),
            maximum: MAX_UNION_INPUTS,
        });
    }
    let mut seen = HashSet::with_capacity(input_view_ids.len());
    for view_id in input_view_ids {
        if view_id.len() > MAX_UNION_VIEW_ID_BYTES || union_view_id.len() > MAX_UNION_VIEW_ID_BYTES
        {
            return Err(UnionError::ViewIdTooLong {
                maximum: MAX_UNION_VIEW_ID_BYTES,
            });
        }
        if *view_id == union_view_id {
            return Err(UnionError::SelfReference {
                view_id: (*view_id).to_owned(),
            });
        }
        if !seen.insert(*view_id) {
            return Err(UnionError::DuplicateInput {
                view_id: (*view_id).to_owned(),
            });
        }
    }
    Ok(())
}

/// Reject a union whose inputs reach back to itself through other unions.
///
/// `resolve` maps a union view ID to its own input view IDs and returns `None`
/// for ordinary (non-union) views, which are leaves. The walk is bounded: at
/// most `MAX_UNION_INPUTS` inputs per union and a visited cap, so a hostile or
/// corrupt graph cannot make validation itself unbounded. Direct
/// self-reference is reported by [`validate_union_spec`]; anything found here
/// is a transitive cycle and is reported with the path.
pub fn detect_union_cycle(
    union_view_id: &str,
    input_view_ids: &[&str],
    resolve: impl Fn(&str) -> Option<Vec<String>>,
) -> Result<(), UnionError> {
    // Depth-first walk from the inputs; the first time the union's own ID is
    // reached, the stack is the cycle path. `visited` bounds the walk when the
    // stored graph is wider than current validation bounds allow.
    const MAX_VISITED: usize = 64;
    let mut visited: HashSet<String> = HashSet::new();
    let mut stack: Vec<String> = Vec::new();
    let mut work: Vec<String> = input_view_ids.iter().map(|id| (*id).to_owned()).collect();
    while let Some(next) = work.pop() {
        if next == union_view_id {
            stack.push(next.clone());
            return Err(UnionError::UnionCycle {
                path: stack.join(" -> "),
            });
        }
        if !visited.insert(next.clone()) {
            continue;
        }
        if visited.len() > MAX_VISITED {
            return Err(UnionError::UnionCycle {
                path: format!("dependency graph exceeds {MAX_VISITED} views"),
            });
        }
        if let Some(inputs) = resolve(&next) {
            stack.push(next.clone());
            // Bound the fan-out the same way the spec bounds fresh input.
            for input in inputs.into_iter().take(MAX_UNION_INPUTS + 1) {
                work.push(input);
            }
        }
    }
    Ok(())
}

/// Merge unfolded input memberships into one deterministic timestamp order.
///
/// * The same stable RecordId in several inputs is emitted once, keeping the
///   FIRST input's row (timestamp and, downstream, its derived projection).
/// * Survivors sort by timestamp ascending with missing (`None`) last, then
///   by input position, then by original sequence, with source identity as the
///   final tiebreak so the order is total: the same snapshots always publish
///   the same order, including across restarts.
/// * The total row count is fenced by [`MAX_UNION_ROWS`] before any work.
///
/// Live appends re-run this merge over the inputs' current unfolded
/// memberships: inputs only grow by append and carry their own checkpoints, so
/// a refresh costs the bounded re-merge rather than a recapture. A prefix
/// extension without a full rebuild is future work — it must reuse the
/// strict-greater-than-maximum rule `merge_order`'s `extends` documents, since
/// a tie with the published maximum re-breaks by input position.
pub fn merge_union_rows(inputs: &[UnionInputSnapshot]) -> Result<Vec<MergedUnionRow>, UnionError> {
    let total: usize = inputs.iter().map(|input| input.rows.len()).sum();
    if total > MAX_UNION_ROWS {
        return Err(UnionError::RowLimit {
            rows: total,
            maximum: MAX_UNION_ROWS,
        });
    }
    // First-input precedence: the earliest input position wins each identity,
    // keeping its own timestamp for the sort below.
    let mut winners: HashMap<UnionRecordId, (usize, Option<i64>)> =
        HashMap::with_capacity(total.min(1 << 20));
    for (position, input) in inputs.iter().enumerate() {
        for row in &input.rows {
            winners
                .entry(row.id.clone())
                .or_insert((position, row.timestamp_nanos));
        }
    }
    let mut merged: Vec<MergedUnionRow> = winners
        .into_iter()
        .map(|(id, (input, timestamp_nanos))| MergedUnionRow {
            id,
            input,
            timestamp_nanos,
        })
        .collect();
    // Total deterministic order: missing times last, then timestamp, then the
    // input position the user listed, then original sequence, then source
    // identity (sequences alone are not total across sources).
    merged.sort_by(|left, right| {
        (left.timestamp_nanos.is_none(), left.timestamp_nanos)
            .cmp(&(right.timestamp_nanos.is_none(), right.timestamp_nanos))
            .then(left.input.cmp(&right.input))
            .then(left.id.sequence.cmp(&right.id.sequence))
            .then(left.id.source_id.cmp(&right.id.source_id))
    });
    Ok(merged)
}

/// Whether a union candidate fenced on `fenced_revision` is stale against the
/// input's current accepted revision.
pub fn union_input_stale(fenced_revision: u64, current_accepted_revision: u64) -> bool {
    fenced_revision != current_accepted_revision
}

/// Union accepted typed frames in timestamp order with Polars doing the value
/// compute.
///
/// Each frame is one input view's accepted typed output: its ordinary columns
/// plus `source_column` (string source identity), `sequence_column` (`u64`
/// physical sequence) and the accepted `timestamp_column` (`i64` unix nanos,
/// null where the basis had no value). Frames may have different column sets:
/// diagonal concatenation keeps every column and fills absent fields with
/// null, which is how "retain raw missing fields as null where semantically
/// known" is executed rather than asserted. A column present with conflicting
/// dtypes across inputs is a Polars schema error, surfaced here as
/// [`UnionError::Engine`]: the candidate is rejected, the prior union stands,
/// and normalization stays where it belongs — ordinary upstream enrichment.
///
/// Pipeline, in order:
/// 1. Tag each frame with [`INPUT_COLUMN`] (its input position). Frames
///    already carrying that name are rejected: the tag is provenance, and a
///    silent overwrite would corrupt tiebreaks.
/// 2. Diagonal concat in input order, collected once.
/// 3. Stable-identity dedup over the collected frame (the named exemption
///    above): the first input position wins each `(source, sequence)` pair,
///    keeping its whole derived projection. Precedes the sort so a duplicate
///    whose projections disagree on time still loses to the earlier input.
/// 4. Polars sort by `(timestamp, input, sequence)`, ascending, nulls last.
///
/// The output retains [`INPUT_COLUMN`] as per-row provenance.
pub fn union_typed_frames(
    frames: Vec<DataFrame>,
    timestamp_column: &str,
    source_column: &str,
    sequence_column: &str,
) -> Result<DataFrame, UnionError> {
    if frames.len() < 2 {
        return Err(UnionError::EmptyInputs);
    }
    if frames.len() > MAX_UNION_INPUTS {
        return Err(UnionError::TooManyInputs {
            count: frames.len(),
            maximum: MAX_UNION_INPUTS,
        });
    }
    for frame in &frames {
        if frame
            .get_column_names()
            .iter()
            .any(|name| name.as_str() == INPUT_COLUMN)
        {
            return Err(UnionError::ProtectedColumn {
                name: INPUT_COLUMN.to_owned(),
            });
        }
    }
    let total: usize = frames.iter().map(DataFrame::height).sum();
    if total > MAX_UNION_ROWS {
        return Err(UnionError::RowLimit {
            rows: total,
            maximum: MAX_UNION_ROWS,
        });
    }
    let tagged: Vec<LazyFrame> = frames
        .into_iter()
        .enumerate()
        .map(|(position, frame)| {
            frame
                .lazy()
                .with_column(lit(position as u32).alias(INPUT_COLUMN))
        })
        .collect();
    let stacked = concat(
        tagged,
        UnionArgs {
            diagonal: true,
            ..Default::default()
        },
    )
    .map_err(|error| UnionError::Engine {
        reason: error.to_string(),
    })?
    .collect()
    .map_err(|error| UnionError::Engine {
        reason: error.to_string(),
    })?;
    let timestamp =
        stacked
            .column(timestamp_column)
            .map_err(|error| UnionError::BadTimestampColumn {
                name: timestamp_column.to_owned(),
                reason: error.to_string(),
            })?;
    if !matches!(timestamp.dtype(), DataType::Int64 | DataType::Null)
        && timestamp.null_count() != timestamp.len()
    {
        // The union basis is i64 nanos by contract; anything else means the
        // caller handed over an un-normalized column rather than the accepted
        // typed time. Null-only columns (an input with no times at all) pass:
        // every row sorts last, none is dropped.
        return Err(UnionError::BadTimestampColumn {
            name: timestamp_column.to_owned(),
            reason: format!("expected Int64 nanos, found {:?}", timestamp.dtype()),
        });
    }
    let deduped = dedup_union_frame(&stacked, source_column, sequence_column)?;
    deduped
        .sort(
            [timestamp_column, INPUT_COLUMN, sequence_column],
            SortMultipleOptions::default().with_nulls_last(true),
        )
        .map_err(|error| UnionError::Engine {
            reason: error.to_string(),
        })
}

/// First-input-wins dedup over stable identity columns, in concat order.
///
/// The engine concatenates but cannot know that two rows in different inputs
/// are the same record: identity is the capture invariant, so this pass holds
/// it (the exemption documented at the top of this module). Rows keep their
/// full derived projection from the winning input; nothing is re-derived.
fn dedup_union_frame(
    stacked: &DataFrame,
    source_column: &str,
    sequence_column: &str,
) -> Result<DataFrame, UnionError> {
    let sources = stacked
        .column(source_column)
        .map_err(|error| UnionError::BadIdentityColumn {
            name: source_column.to_owned(),
            reason: error.to_string(),
        })?;
    let sequences =
        stacked
            .column(sequence_column)
            .map_err(|error| UnionError::BadIdentityColumn {
                name: sequence_column.to_owned(),
                reason: error.to_string(),
            })?;
    let mut seen: HashSet<(String, u64)> = HashSet::with_capacity(stacked.height());
    let mut keep: Vec<u32> = Vec::with_capacity(stacked.height());
    for index in 0..stacked.height() {
        let source_value = sources
            .get(index)
            .map_err(|error| UnionError::BadIdentityColumn {
                name: source_column.to_owned(),
                reason: error.to_string(),
            })?;
        if matches!(source_value, AnyValue::Null) {
            return Err(UnionError::BadIdentityColumn {
                name: source_column.to_owned(),
                reason: format!("row {index} carries a null source identity"),
            });
        }
        let source = source_value.str_value().to_string();
        let sequence_value =
            sequences
                .get(index)
                .map_err(|error| UnionError::BadIdentityColumn {
                    name: sequence_column.to_owned(),
                    reason: error.to_string(),
                })?;
        let sequence =
            sequence_value
                .try_extract::<u64>()
                .map_err(|error| UnionError::BadIdentityColumn {
                    name: sequence_column.to_owned(),
                    reason: error.to_string(),
                })?;
        if seen.insert((source, sequence)) {
            keep.push(index as u32);
        }
    }
    let keep_series = Series::new("idx".into(), keep);
    let keep_index = keep_series.u32().map_err(|error| UnionError::Engine {
        reason: error.to_string(),
    })?;
    stacked
        .take(keep_index)
        .map_err(|error| UnionError::Engine {
            reason: error.to_string(),
        })
}
