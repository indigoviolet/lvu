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
//!   union consumes the inputs' ACCEPTED TYPED evaluation — the frozen replay
//!   seam (`NativeViewAdapter::freeze_input` in `export.rs`, visited through
//!   `FrozenInput::visit` exactly like the command path in
//!   `lvu-app/src/command_snapshot.rs`) — and performs no parsing of its own.
//!   Source-specific schema differences are normalized by ordinary upstream
//!   enrichment; fields absent on one side arrive as null.
//!
//! * FORBIDDEN INPUT: `Membership.derived` (`lib.rs`) is a display projection,
//!   not enrichment output. `lvu-query::scalar_projection` (`engine.rs`)
//!   renders every typed value to a `String` AND truncates it to a byte bound,
//!   and `with_enrichment` (roles-agent owned) serves those strings to rows.
//!   Unioning, sorting or filtering those strings would compare truncated text
//!   ("1000" sorts before "9"; a 600-byte value arrives cut) and mistake one
//!   input's rendering for another's values. No API in this module accepts a
//!   display map: typed entry is `UnionFrozenInput` (JSON values plus native
//!   dtype evidence, mirroring `FrozenInputRow`), and merge keys come from the
//!   same frozen accepted basis evaluation — never from the derived map.
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
//! Engine/app division (AGENTS.md): Polars owns ALL union/sort/filter/equality
//! compute over typed values — diagonal concatenation, `unique_stable`
//! first-keep dedup on the canonical identity columns, and the total
//! timestamp ordering all execute in `lvu-query/src/union.rs`, included here
//! by path until its coordinated export lands. This module holds the
//! spec/contract layer (validation, cycles, budgets, provenance-fenced
//! decoding, persistence shape) plus `merge_union_rows`, the cheap pure
//! identity-order contract used for pre-freeze planning and pinned by
//! convergence tests against the engine output — never a second compute
//! implementation.
//!
//! PROPOSED HOOKS — exact minimal ownership (primary assigns).
//!
//! Non-touch regions for this feature: W22 owns the grouping sections of
//! `lib.rs` (folding policy, `ContinuationRule`, group publication); the
//! roles agent owns new role helpers and the narrow `with_enrichment` seam
//! (`lib.rs:3447-3468`); `merge_order`/`extends` are read-only references.
//!
//! LANDED in this worktree (executable, gated):
//! H1. `lib.rs` module lines + re-exports; `Shared.union_views` field;
//!     `register_union_view` / `unregister_union_view` / `union_inputs` /
//!     `submit_union_candidate` / `take_union_completions` (all implemented
//!     in the owned `union_worker.rs`); one `drive_unions` call in
//!     `drain_updates`; a guard in `submit_query` refusing ordinary queries
//!     for union views. New engine file `lvu-query/src/union.rs` (compute).
//! OUTSTANDING (contacts, not this worktree):
//! H2. W22: `QueryPurpose::Union` variant (`lvu/src/app.rs`) so union
//!     completions map into `QueryCompletion` like every other purpose.
//! H3. Source owner: `main.rs` union entry dispatch — dialog acceptance to
//!     `submit_union_candidate`, per-tick `take_union_completions` routing,
//!     input-advance refresh via the controller.
//! H4. Autosave owner: `StoredUnionShape` as additive `presentation_json.union`
//!     (`color_rules` precedent, no schema bump); restore re-resolves input
//!     IDs and never launches commands.
//! H5. Primary: `FrozenInput::visit_precise` (or a precise flag on `visit`)
//!     in `export.rs` — ~5 lines mirroring `visit` with precise replay — so
//!     the worker's dtype evidence comes from native `field_types` for every
//!     field. Until then the worker uses the public non-precise visit and the
//!     decoder enforces strict scalar fallback with explicit rejects.
//! H6. Primary: union-level search/advanced filters executed over merged
//!     frames in the owned `run_query` region. Inputs arrive already
//!     filtered; folding, grouping, export, correlation, time bounds and gap
//!     search read the published `Membership` generically from day one.
//! H7. Archive review (primary): union snapshots freeze input view IDs,
//!     accepted revisions/generations and per-source high-watermarks
//!     (`FrozenInputSummary` carries all three); independent input advances
//!     create new immutable boundaries.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use lvu_core::RecordId;
use polars::prelude::*;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Typed union compute lives in the engine (AGENTS.md: the query engine
/// computes): diagonal concat, stable-identity dedup and the total timestamp
/// ordering all execute in `lvu-query`, and this module holds only the
/// spec/contract layer. Single source file, included here by path until the
/// coordinated `pub mod union;` export lands in `lvu-query/src/lib.rs`, at
/// which point this include is replaced by `use lvu_query::union::...` with
/// no logic change — never two sort/dedup implementations.
#[path = "../../lvu-query/src/union.rs"]
mod query_union;

/// Canonical identity columns, re-exported so the union worker and tests name
/// exactly the columns the engine reads — never a near-miss spelling.
pub use lvu_query::{SEQUENCE_COLUMN, SOURCE_ID_COLUMN};

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

/// Upper bound on the decoded typed bytes one union merge may hold.
///
/// Row counts alone cannot bound a merge: one wide row costs what a thousand
/// narrow ones do. Every decoded input frame and the final merged frame are
/// measured with `DataFrame::estimated_size` against this budget; over it the
/// candidate is rejected and the prior union stands. Sized at four times the
/// command-enrichment input budget: a union spans up to eight inputs, each
/// frozen under its own bounded `FrozenInputLimits`.
pub const MAX_UNION_BYTES: u64 = 64 * 1024 * 1024;

/// Budgets fencing one union merge. Rows bound count; bytes bound width.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UnionLimits {
    pub maximum_rows: usize,
    pub maximum_bytes: u64,
}

impl Default for UnionLimits {
    fn default() -> Self {
        Self {
            maximum_rows: MAX_UNION_ROWS,
            maximum_bytes: MAX_UNION_BYTES,
        }
    }
}

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

/// Internal timestamp column of worker-built frozen frames.
///
/// `i64` unix nanos with null for missing, mirroring the accepted basis
/// evaluation — never a display rendering. The export replay (`input_row` in
/// `export.rs`) already skips every `_lvu_*` column, so this name can neither
/// arrive from frozen fields nor leak into snapshots.
pub const UNION_TS_COLUMN: &str = "_lvu_union_ts";

/// One member of an input's unfolded physical membership, with its accepted
/// timestamp already extracted.
///
/// `id` is the canonical `lvu_core::RecordId` — the same identity raw
/// context, bookmarks and correlation address. `timestamp_nanos` is the
/// input's accepted typed time for this record in its union basis
/// (event/extracted/selected time as `i64` unix nanos, or capture nanos under
/// the capture basis), taken from the same frozen accepted basis evaluation
/// the typed values come from — never from the `Membership.derived` display
/// map. `None` is a missing/unreadable value in that basis — never an error,
/// never a reason to drop the record.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UnionInputRow {
    pub id: RecordId,
    pub timestamp_nanos: Option<i64>,
}

/// One frozen typed row of a union input: the worker's 1:1 mapping of a
/// `FrozenInputRow` visited through `FrozenInput::visit`.
///
/// * identity is `row.record.record_id` verbatim (canonical capture identity);
/// * `timestamp_nanos` comes from the input's accepted basis evaluation for
///   that record (the frozen summary's `time_basis` names which);
/// * `fields` is `row.fields` verbatim: TYPED `serde_json::Value`s;
/// * `field_types` is `row.field_types` verbatim: the native dtype evidence
///   (`format!("{:?}")` spellings) that makes each value's type authoritative
///   instead of inferred. Display strings cannot enter here by construction:
///   there is no field for them.
#[derive(Clone, Debug, PartialEq)]
pub struct UnionFrozenRow {
    pub record_id: RecordId,
    pub timestamp_nanos: Option<i64>,
    pub fields: BTreeMap<String, serde_json::Value>,
    pub field_types: BTreeMap<String, String>,
}

/// One input view's frozen accepted evaluation that a union candidate merges.
///
/// The worker builds one of these per input with `freeze_input` (fenced on
/// the input's applied revision and generation, candidates and later arrivals
/// excluded) and bounded `FrozenInputLimits`; the whole candidate is then a
/// pure function of these values. `timestamp_column` names the accepted basis
/// column the timestamps were evaluated under, for diagnostics only — the
/// merge reads `timestamp_nanos`, not a name lookup.
#[derive(Clone, Debug, PartialEq)]
pub struct UnionFrozenInput {
    pub view_id: String,
    pub applied_revision: u64,
    pub applied_generation: u64,
    pub timestamp_column: String,
    pub rows: Vec<UnionFrozenRow>,
}

/// The identity-plus-timestamp view of a frozen input for [`merge_union_rows`].
///
/// Drops values and keeps merge keys, so there is exactly one ordering
/// implementation no matter which entry point a caller uses.
pub fn frozen_identity_snapshot(input: &UnionFrozenInput) -> UnionInputSnapshot {
    UnionInputSnapshot {
        view_id: input.view_id.clone(),
        accepted_revision: input.applied_revision,
        rows: input
            .rows
            .iter()
            .map(|row| UnionInputRow {
                id: row.record_id,
                timestamp_nanos: row.timestamp_nanos,
            })
            .collect(),
    }
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

/// One merged row: its preserved canonical identity, the winning input
/// position, and the timestamp it sorted under.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MergedUnionRow {
    pub id: RecordId,
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

/// One persisted union input reference: the input view ID plus the accepted
/// revision AND generation the union published under.
///
/// Both fence the candidate: a revision move means new definitions, a
/// generation move means the source restarted underneath the membership.
/// `applied_generation` is additive (`serde(default)`) so older rows read as
/// generation 0 and simply refresh once.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct StoredUnionInput {
    #[serde(default)]
    pub view_id: String,
    #[serde(default)]
    pub accepted_revision: u64,
    #[serde(default)]
    pub applied_generation: u64,
}

/// A submitted-but-unpublished union candidate: the union view ID, the union
/// revision it will publish, the submission generation, and the fenced input
/// revisions. Shared by the dialog, the controller and the worker — one shape,
/// defined once.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnionCandidateSpec {
    pub union_view_id: String,
    pub union_revision: u64,
    pub generation: u64,
    pub inputs: Vec<StoredUnionInput>,
}

/// A finished union job for the composition tick to collect.
///
/// `error` is `None` on publication. The published membership itself is
/// already installed under the union view ID when the worker reports success;
/// this is only the acknowledgement the shell routes like a query completion.
/// (Terminal mapping to `QueryCompletion` needs `QueryPurpose::Union` — W22
/// contact — so the worker reports here and the tick translates later.)
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnionCompletion {
    pub union_view_id: String,
    pub union_revision: u64,
    pub generation: u64,
    pub error: Option<String>,
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
    #[error("union would hold {bytes} bytes, above the {maximum} bound")]
    ByteLimit { bytes: u64, maximum: u64 },
    #[error("view identity exceeds {maximum} bytes")]
    ViewIdTooLong { maximum: usize },
    #[error("input frame already carries the protected column '{name}'")]
    ProtectedColumn { name: String },
    #[error("timestamp column '{name}' is missing or unreadable: {reason}")]
    BadTimestampColumn { name: String, reason: String },
    #[error("identity column '{name}' is missing or unreadable: {reason}")]
    BadIdentityColumn { name: String, reason: String },
    #[error(
        "union input '{view_id}' field '{field}' has unsupported native type '{dtype}': {reason}"
    )]
    UnsupportedValue {
        view_id: String,
        field: String,
        dtype: String,
        reason: String,
    },
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

/// Reject a union whose dependency graph contains a cycle.
///
/// `resolve` maps a union view ID to its own STORED input view IDs and returns
/// `None` for ordinary (non-union) views, which are leaves. This is a real
/// depth-first cycle search with white/gray/black marks, so it reports ANY
/// cycle in the reachable subgraph — through the new union or between older
/// unions — with the path, not just reachability back to the new ID. Direct
/// self-reference is still reported first by [`validate_union_spec`] with its
/// precise error.
///
/// No edge is ever truncated: a stored union declaring more than
/// [`MAX_UNION_INPUTS`] inputs is an invalid graph and fails closed with
/// [`UnionError::TooManyInputs`]. A separate visited cap bounds the walk
/// itself, so a hostile graph cannot make validation unbounded.
pub fn detect_union_cycle(
    union_view_id: &str,
    input_view_ids: &[&str],
    resolve: impl Fn(&str) -> Option<Vec<String>>,
) -> Result<(), UnionError> {
    const MAX_VISITED: usize = 64;
    #[derive(Clone, Copy, Eq, PartialEq)]
    enum Mark {
        Gray,
        Black,
    }
    fn visit<F>(
        node: &str,
        resolve: &F,
        marks: &mut HashMap<String, Mark>,
        stack: &mut Vec<String>,
    ) -> Result<(), UnionError>
    where
        F: Fn(&str) -> Option<Vec<String>>,
    {
        match marks.get(node) {
            Some(Mark::Black) => return Ok(()),
            Some(Mark::Gray) => {
                let start = stack.iter().position(|id| id == node).unwrap_or(0);
                let mut path = stack[start..].to_vec();
                path.push(node.to_owned());
                return Err(UnionError::UnionCycle {
                    path: path.join(" -> "),
                });
            }
            None => {}
        }
        if marks.len() >= MAX_VISITED {
            return Err(UnionError::UnionCycle {
                path: format!("dependency graph exceeds {MAX_VISITED} views"),
            });
        }
        marks.insert(node.to_owned(), Mark::Gray);
        stack.push(node.to_owned());
        if let Some(inputs) = resolve(node) {
            if inputs.len() > MAX_UNION_INPUTS {
                return Err(UnionError::TooManyInputs {
                    count: inputs.len(),
                    maximum: MAX_UNION_INPUTS,
                });
            }
            for input in &inputs {
                visit(input, resolve, marks, stack)?;
            }
        }
        stack.pop();
        marks.insert(node.to_owned(), Mark::Black);
        Ok(())
    }
    let mut marks = HashMap::new();
    let mut stack = vec![union_view_id.to_owned()];
    marks.insert(union_view_id.to_owned(), Mark::Gray);
    for input in input_view_ids {
        visit(input, &resolve, &mut marks, &mut stack)?;
    }
    Ok(())
}

/// Merge unfolded input memberships into one deterministic timestamp order.
///
/// This is the identity-level CONTRACT: cheap, pure, and dependency-free, used
/// for pre-freeze planning and pinned by tests. Production ORDER OF RECORD is
/// the engine's [`union_sorted_frames`](query_union::union_sorted_frames)
/// output; convergence between the two on identical data is asserted in tests,
/// and any divergence is a bug in one of them — there is still exactly one
/// compute implementation.
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
    let mut winners: HashMap<RecordId, (usize, Option<i64>)> =
        HashMap::with_capacity(total.min(1 << 20));
    for (position, input) in inputs.iter().enumerate() {
        for row in &input.rows {
            winners
                .entry(row.id)
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

/// Union frozen accepted evaluations in timestamp order: the worker-facing
/// entry point.
///
/// Each input is the worker's `freeze_input` output mapped 1:1 into
/// [`UnionFrozenInput`] (see its docs for the exact field mapping). This
/// function decodes every input's TYPED json values back into a Polars frame
/// under the AUTHORITY of each row's `field_types` native dtype evidence —
/// the inverse of the frozen `json_value` encoding (`export.rs`), so an
/// `Int64` 1000 stays an `Int64` 1000 and a 600-byte string stays 600 bytes —
/// then delegates the merge itself to [`union_typed_frames`], which executes
/// the engine's [`union_sorted_frames`](query_union::union_sorted_frames).
/// Decoding rules:
///
/// * the declared native dtype decides the column dtype, never JSON-kind
///   inference: `Boolean`, all `Int*`/`UInt*` widths, `Float32`/`Float64`,
///   `String` decode exactly (narrow widths range-checked, `Float32`
///   round-trip-checked); every value must conform or the candidate is
///   rejected — a whole number where a float was declared is a schema
///   conflict, not a coercion (the `precise_json_value_for_field` precedent);
/// * temporal (`Date`/`Datetime`/`Duration`/`Time`), nested
///   (`List`/`Struct`/arrays/objects), decimal, categorical, binary and
///   unknown dtypes are REJECTED with [`UnionError::UnsupportedValue`]:
///   stringifying them would silently change downstream query semantics
///   (comparisons, grouping keys, time predicates), so values stay
///   semantically identical or the candidate fails and the prior union stands;
/// * a field with no dtype evidence is a corrupt handoff and is rejected;
/// * missing fields decode as null through diagonal concat; a field whose
///   every value is null decodes as TYPED nulls of its declared dtype, so the
///   schema survives and `is_null` filters keep working. Only a `Null`
///   declaration (unknown type) contributes no column.
/// * budgets from `limits` fence rows AND decoded bytes (`estimated_size` per
///   input frame and for the final frame): over either, the candidate is
///   rejected and the prior union stands.
///
/// Identity (`SOURCE_ID_COLUMN`, `SEQUENCE_COLUMN` — the engine's canonical
/// columns) and the internal timestamp column ([`UNION_TS_COLUMN`]) are built
/// from the frozen identity and basis evaluation, never from value fields. A
/// frozen field colliding with any of those names is rejected: enrichment can
/// never produce a `_lvu_*` field (protected names), so a collision means a
/// corrupt handoff, not user data.
pub fn union_frozen_inputs(
    union_view_id: &str,
    inputs: &[UnionFrozenInput],
    limits: &UnionLimits,
) -> Result<DataFrame, UnionError> {
    let ids: Vec<&str> = inputs.iter().map(|input| input.view_id.as_str()).collect();
    validate_union_spec(union_view_id, &ids)?;
    let total: usize = inputs.iter().map(|input| input.rows.len()).sum();
    if total > limits.maximum_rows {
        return Err(UnionError::RowLimit {
            rows: total,
            maximum: limits.maximum_rows,
        });
    }
    let mut frames = Vec::with_capacity(inputs.len());
    for input in inputs {
        let frame = frozen_frame(input)?;
        let bytes = u64::try_from(frame.estimated_size()).unwrap_or(u64::MAX);
        if bytes > limits.maximum_bytes {
            return Err(UnionError::ByteLimit {
                bytes,
                maximum: limits.maximum_bytes,
            });
        }
        frames.push(frame);
    }
    let merged = union_typed_frames(frames, UNION_TS_COLUMN, SOURCE_ID_COLUMN, SEQUENCE_COLUMN)?;
    let bytes = u64::try_from(merged.estimated_size()).unwrap_or(u64::MAX);
    if bytes > limits.maximum_bytes {
        return Err(UnionError::ByteLimit {
            bytes,
            maximum: limits.maximum_bytes,
        });
    }
    Ok(merged)
}

/// Decode one frozen input into a typed frame with canonical identity columns.
fn frozen_frame(input: &UnionFrozenInput) -> Result<DataFrame, UnionError> {
    for row in &input.rows {
        for name in row.fields.keys() {
            if name == INPUT_COLUMN || name == UNION_TS_COLUMN {
                return Err(UnionError::ProtectedColumn { name: name.clone() });
            }
        }
    }
    let mut columns = Vec::with_capacity(3);
    columns.push(Column::new(
        SOURCE_ID_COLUMN.into(),
        input
            .rows
            .iter()
            .map(|row| row.record_id.source_id.0.to_string())
            .collect::<Vec<_>>(),
    ));
    columns.push(Column::new(
        SEQUENCE_COLUMN.into(),
        input
            .rows
            .iter()
            .map(|row| row.record_id.sequence)
            .collect::<Vec<_>>(),
    ));
    columns.push(Column::new(
        UNION_TS_COLUMN.into(),
        input
            .rows
            .iter()
            .map(|row| row.timestamp_nanos)
            .collect::<Vec<_>>(),
    ));
    // One dtype per field across all rows of this input, under the authority
    // of the frozen native dtype evidence; absent fields are simply missing
    // here and arrive as null through diagonal concat.
    let mut names: BTreeSet<&str> = BTreeSet::new();
    for row in &input.rows {
        names.extend(row.fields.keys().map(String::as_str));
    }
    for name in names {
        if let Some(column) = decode_union_field(input, name)? {
            columns.push(column);
        }
    }
    // An empty input still contributes its (empty) identity columns so the
    // diagonal concat keeps a stable schema and the sort keys always exist.
    DataFrame::new_infer_height(columns).map_err(|error| UnionError::Engine {
        reason: error.to_string(),
    })
}

/// Decode one field's typed JSON values into a single Polars column.
///
/// Dtype authority first: when the frozen rows carry native dtype evidence
/// (`field_types`, `format!("{:?}")` spellings from a precise replay), the
/// declared dtype decides and values must conform — see
/// [`decode_typed_field`]. Without evidence (the public non-precise replay
/// does not populate `field_types`), decoding falls back to strict scalar
/// JSON kinds — booleans, whole numbers, floats and strings decode to their
/// single natural dtype; anything else is rejected, never inferred into a
/// schema it cannot prove. In particular the fallback NEVER invents dtypes
/// for null-only fields (rejected: schema unknown without evidence) and NEVER
/// stringifies arrays/objects (rejected: validity-changing coercion).
/// Temporal values cannot reach either path through a replay that encodes
/// them losslessly: the non-precise replay fails on them first, and the
/// precise replay marks them [`UnionError::UnsupportedValue`].
///
/// `Null` decodes alongside every supported dtype as validity: a field whose
/// every value is null still decodes as TYPED nulls of its declared dtype,
/// so the schema survives and `is_null` filters keep working. Only a `Null`
/// declaration (genuinely unknown type) contributes no column.
///
/// Supported declarations: `Boolean`, all `Int*`/`UInt*` widths (values
/// range-checked, never wrapped), `Float32` (round-trip-checked widening to
/// `Float64`, which is value-identical), `Float64`, `String`.
/// Explicitly rejected with [`UnionError::UnsupportedValue`] (values stay
/// semantically identical or the candidate fails; nothing is silently
/// coerced): temporal (`Date`, `Datetime(..)`, `Duration(..)`, `Time`),
/// nested (`List(..)`, `Array`, `Struct(..)`), decimal, categorical, binary,
/// object, unknown spellings, and any JSON array/object value (including the
/// frozen big-integer/datetime `{"kind":..}` wrappers) — until a native type
/// carries them.
fn decode_union_field(input: &UnionFrozenInput, name: &str) -> Result<Option<Column>, UnionError> {
    let rows = &input.rows;
    let unsupported = |dtype: String, reason: &str| UnionError::UnsupportedValue {
        view_id: input.view_id.clone(),
        field: name.to_owned(),
        dtype,
        reason: reason.to_owned(),
    };
    let reject = |reason: String| UnionError::Engine {
        reason: format!("union input '{}' field '{name}': {reason}", input.view_id),
    };
    // One authority per field per input: the distinct native dtype evidence
    // across the rows carrying the field. Evolution mid-input is a conflict
    // and fails closed rather than inferred.
    let mut declared: Option<&str> = None;
    let mut evidenced = false;
    for row in rows {
        if !row.fields.contains_key(name) {
            continue;
        }
        evidenced = true;
        match row.field_types.get(name) {
            Some(dtype) => match declared {
                None => declared = Some(dtype),
                Some(prior) if prior == dtype => {}
                Some(_) => {
                    return Err(reject(
                        "conflicting native dtype evidence across records".into(),
                    ));
                }
            },
            // No evidence on this row: the authority path is unavailable;
            // the fallback below decides per JSON kinds. A row WITH evidence
            // mixed with rows WITHOUT is itself a conflict.
            None => {
                if declared.is_some() {
                    return Err(reject(
                        "conflicting native dtype evidence across records".into(),
                    ));
                }
            }
        }
    }
    if !evidenced {
        return Ok(None);
    }
    match declared {
        Some(dtype) => decode_typed_field(name, rows, dtype, &reject, &unsupported),
        None => decode_inferred_field(name, rows, &reject, &unsupported),
    }
}

/// Strict scalar fallback without dtype evidence (see [`decode_union_field`]).
///
/// Decodes ONLY single-kind scalar JSON — booleans, whole numbers, floats,
/// strings — to their natural dtype (`Int64`, `UInt64` when every whole
/// number needs it, `Float64`, `Boolean`, `String`). Whole-number/float mixes
/// reject like the precise frozen path; null-only fields reject (no schema
/// can be proven without evidence — use a precise replay or materialize a
/// value upstream); arrays, objects and kind-tagged wrappers reject as
/// [`UnionError::UnsupportedValue`]. Widths widen to 64-bit, which is
/// value-identical; exact declared widths need the authority path.
fn decode_inferred_field(
    name: &str,
    rows: &[UnionFrozenRow],
    reject: &dyn Fn(String) -> UnionError,
    unsupported: &dyn Fn(String, &str) -> UnionError,
) -> Result<Option<Column>, UnionError> {
    use serde_json::Value as Json;
    let get = |index: usize| rows[index].fields.get(name);
    let mut has_bool = false;
    let mut has_int = false;
    let mut has_float = false;
    let mut has_string = false;
    for row in rows {
        match row.fields.get(name) {
            None | Some(Json::Null) => {}
            Some(Json::Bool(_)) => has_bool = true,
            Some(Json::Number(number)) => {
                if number.is_f64() {
                    has_float = true;
                } else {
                    has_int = true;
                }
            }
            Some(Json::String(_)) => has_string = true,
            Some(Json::Array(_)) | Some(Json::Object(_)) => {
                return Err(unsupported(
                    "unknown".into(),
                    "nested values need native dtype evidence; no coercion to text",
                ));
            }
        }
    }
    let kinds = [has_bool, has_int, has_float, has_string]
        .into_iter()
        .filter(|present| *present)
        .count();
    if kinds == 0 {
        return Err(reject(
            "field has values but no native dtype evidence; use a precise replay or materialize a value upstream"
                .into(),
        ));
    }
    if kinds > 1 {
        return Err(reject("conflicting value kinds across records".into()));
    }
    if has_bool {
        return Ok(Some(Column::new(
            name.into(),
            (0..rows.len())
                .map(|index| get(index).and_then(Json::as_bool))
                .collect::<Vec<_>>(),
        )));
    }
    if has_int {
        let ints: Vec<Option<i64>> = (0..rows.len())
            .map(|index| get(index).and_then(Json::as_i64))
            .collect();
        let values = present_count(rows, name);
        if ints.iter().flatten().count() == values {
            return Ok(Some(Column::new(name.into(), ints)));
        }
        let uints: Vec<Option<u64>> = (0..rows.len())
            .map(|index| get(index).and_then(Json::as_u64))
            .collect();
        if uints.iter().flatten().count() == values {
            return Ok(Some(Column::new(name.into(), uints)));
        }
        return Err(reject("whole numbers fit neither Int64 nor UInt64".into()));
    }
    if has_float {
        return Ok(Some(Column::new(
            name.into(),
            (0..rows.len())
                .map(|index| get(index).and_then(Json::as_f64))
                .collect::<Vec<_>>(),
        )));
    }
    Ok(Some(Column::new(
        name.into(),
        (0..rows.len())
            .map(|index| get(index).and_then(Json::as_str).map(str::to_owned))
            .collect::<Vec<_>>(),
    )))
}

/// Non-null values present for a field across frozen rows.
fn present_count(rows: &[UnionFrozenRow], name: &str) -> usize {
    rows.iter()
        .filter_map(|row| row.fields.get(name))
        .filter(|value| !value.is_null())
        .count()
}

/// Decode under an authoritative declared dtype (see [`union_frozen_inputs`]).
///
/// Values must conform exactly; all-null decodes as typed nulls so the schema
/// survives for `is_null` filters.
#[allow(clippy::too_many_lines)]
fn decode_typed_field(
    name: &str,
    rows: &[UnionFrozenRow],
    dtype: &str,
    reject: &dyn Fn(String) -> UnionError,
    unsupported: &dyn Fn(String, &str) -> UnionError,
) -> Result<Option<Column>, UnionError> {
    use serde_json::Value as Json;
    let get = |index: usize| rows[index].fields.get(name);
    match dtype {
        "Boolean" => Ok(Some(Column::new(
            name.into(),
            (0..rows.len())
                .map(|index| match get(index) {
                    None | Some(Json::Null) => Ok(None),
                    Some(Json::Bool(value)) => Ok(Some(*value)),
                    Some(other) => Err(reject(format!("declared Boolean but found {other}"))),
                })
                .collect::<Result<Vec<_>, _>>()?,
        ))),
        "String" => Ok(Some(Column::new(
            name.into(),
            (0..rows.len())
                .map(|index| match get(index) {
                    None | Some(Json::Null) => Ok(None),
                    Some(Json::String(value)) => Ok(Some(value.clone())),
                    Some(other) => Err(reject(format!("declared String but found {other}"))),
                })
                .collect::<Result<Vec<_>, _>>()?,
        ))),
        "Float64" => Ok(Some(Column::new(
            name.into(),
            (0..rows.len())
                .map(|index| match get(index) {
                    None | Some(Json::Null) => Ok(None),
                    Some(Json::Number(number)) => number
                        .as_f64()
                        .map(Some)
                        .ok_or_else(|| reject(format!("declared Float64 but found {number}"))),
                    Some(other) => Err(reject(format!("declared Float64 but found {other}"))),
                })
                .collect::<Result<Vec<_>, _>>()?,
        ))),
        "Float32" => {
            // Widening `f32` to `f64` is value-identical; anything that does
            // not round-trip is rejected rather than rounded.
            let mut out: Vec<Option<f32>> = Vec::with_capacity(rows.len());
            for index in 0..rows.len() {
                match get(index) {
                    None | Some(Json::Null) => out.push(None),
                    Some(Json::Number(number)) => {
                        let value = number.as_f64().ok_or_else(|| {
                            reject(format!("declared Float32 but found {number}"))
                        })?;
                        let narrow = value as f32;
                        if f64::from(narrow) != value {
                            return Err(reject(format!(
                                "declared Float32 but {value} does not round-trip"
                            )));
                        }
                        out.push(Some(narrow));
                    }
                    Some(other) => {
                        return Err(reject(format!("declared Float32 but found {other}")));
                    }
                }
            }
            Ok(Some(Column::new(name.into(), out)))
        }
        "Int8" | "Int16" | "Int32" | "Int64" => {
            let declared_dtype = match dtype {
                "Int8" => DataType::Int8,
                "Int16" => DataType::Int16,
                "Int32" => DataType::Int32,
                _ => DataType::Int64,
            };
            let mut out: Vec<Option<i64>> = Vec::with_capacity(rows.len());
            for index in 0..rows.len() {
                match get(index) {
                    None | Some(Json::Null) => out.push(None),
                    Some(Json::Number(number)) => {
                        let value = number.as_i64().ok_or_else(|| {
                            reject(format!("declared {dtype} but found {number}"))
                        })?;
                        out.push(Some(value));
                    }
                    Some(other) => {
                        return Err(reject(format!("declared {dtype} but found {other}")));
                    }
                }
            }
            // Exact declared width via strict cast: in-range values keep
            // their dtype, overflow rejects rather than wraps.
            Series::new(name.into(), out)
                .strict_cast(&declared_dtype)
                .map(|series| {
                    let column: Column = series.into();
                    Some(column)
                })
                .map_err(|error| {
                    reject(format!(
                        "declared {dtype} but a value overflows it: {error}"
                    ))
                })
        }
        "UInt8" | "UInt16" | "UInt32" | "UInt64" => {
            let declared_dtype = match dtype {
                "UInt8" => DataType::UInt8,
                "UInt16" => DataType::UInt16,
                "UInt32" => DataType::UInt32,
                _ => DataType::UInt64,
            };
            let mut out: Vec<Option<u64>> = Vec::with_capacity(rows.len());
            for index in 0..rows.len() {
                match get(index) {
                    None | Some(Json::Null) => out.push(None),
                    Some(Json::Number(number)) => {
                        // `as_u64` fails negatives and non-integers alike:
                        // neither wraps nor coerces into an unsigned column.
                        let value = number.as_u64().ok_or_else(|| {
                            reject(format!("declared {dtype} but found {number}"))
                        })?;
                        out.push(Some(value));
                    }
                    Some(other) => {
                        return Err(reject(format!("declared {dtype} but found {other}")));
                    }
                }
            }
            Series::new(name.into(), out)
                .strict_cast(&declared_dtype)
                .map(|series| {
                    let column: Column = series.into();
                    Some(column)
                })
                .map_err(|error| {
                    reject(format!(
                        "declared {dtype} but a value overflows it: {error}"
                    ))
                })
        }
        "Null" => Ok(None),
        _ => Err(unsupported(
            dtype.to_owned(),
            "no native union encoding carries this type yet; normalize upstream in enrichment",
        )),
    }
}

/// Union accepted typed frames in timestamp order: structural validation in
/// the view, ALL value compute in the engine.
///
/// Each frame is one input view's accepted typed output: its ordinary columns
/// plus `source_column` (string source identity), `sequence_column` (`u64`
/// physical sequence) and the accepted `timestamp_column` (`i64` unix nanos,
/// null where the basis had no value).
///
/// PRODUCTION CALLERS MUST BUILD THESE FRAMES FROM FROZEN ACCEPTED EVALUATION
/// (see [`union_frozen_inputs`], the one blessed constructor): frames
/// projected out of the `Membership.derived` display map are truncated,
/// type-erased strings and are rejected by contract.
///
/// Frames may have different column sets: diagonal concatenation keeps every
/// column and fills absent fields with null, which is how "retain raw missing
/// fields as null where semantically known" is executed rather than asserted.
/// A column present with conflicting dtypes across inputs is a Polars schema
/// error, surfaced here as [`UnionError::Engine`]: the candidate is rejected,
/// the prior union stands, and normalization stays where it belongs —
/// ordinary upstream enrichment.
///
/// The merge itself — tagging, diagonal concat, stable-identity dedup with
/// first keep, and the total `(timestamp, input, sequence, source)` sort with
/// nulls last — executes in
/// [`union_sorted_frames`](query_union::union_sorted_frames). The output
/// retains [`INPUT_COLUMN`] as per-row provenance.
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
    if !frames.iter().any(|frame| {
        frame
            .get_column_names()
            .iter()
            .any(|name| name.as_str() == timestamp_column)
    }) {
        return Err(UnionError::BadTimestampColumn {
            name: timestamp_column.to_owned(),
            reason: "no input carries this column".into(),
        });
    }
    for frame in &frames {
        if let Ok(column) = frame.column(timestamp_column) {
            // The union basis is i64 nanos by contract; anything else means
            // the caller handed over an un-normalized column rather than the
            // accepted typed time. Null-only columns (an input with no times
            // at all, or an empty frame) pass: their rows sort last and none
            // is dropped.
            let uninformative = column.null_count() == column.len();
            if !uninformative && !matches!(column.dtype(), DataType::Int64) {
                return Err(UnionError::BadTimestampColumn {
                    name: timestamp_column.to_owned(),
                    reason: format!("expected Int64 nanos, found {:?}", column.dtype()),
                });
            }
        }
    }
    query_union::union_sorted_frames(
        frames,
        timestamp_column,
        source_column,
        sequence_column,
        INPUT_COLUMN,
    )
    .map_err(|reason| UnionError::Engine { reason })
}
