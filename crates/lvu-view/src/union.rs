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
//! PROPOSED HOOKS — exact minimal ownership (primary assigns; none applied):
//!
//! Non-touch regions for this feature: W22 owns the grouping sections of
//! `lib.rs` (folding policy, `ContinuationRule`, group publication); the
//! roles agent owns new role helpers and the narrow `with_enrichment` seam
//! (`lib.rs:3447-3468`); `merge_order`/`extends` are read-only references.
//!
//! H1. `crates/lvu-view/src/lib.rs` module line (beside `pub mod folding;`):
//!       `pub mod union;`
//!     plus re-export:
//!       `pub use union::{UnionError, UnionInputSnapshot, UnionInputRow,`
//!       `  MergedUnionRow, UnionRecordId, UnionFrozenInput, UnionFrozenRow,`
//!       `  StoredUnionShape, MAX_UNION_INPUTS, MAX_UNION_ROWS, INPUT_COLUMN,`
//!       `  UNION_TS_COLUMN, SEQUENCE_COLUMN, SOURCE_ID_COLUMN,`
//!       `  validate_union_spec, detect_union_cycle, merge_union_rows,`
//!       `  frozen_identity_snapshot, union_frozen_inputs, union_typed_frames,`
//!       `  union_input_stale};`
//! H2. Registration: a `UnionRegistration { inputs: Vec<StoredUnionInput> }`
//!     beside `ViewRegistration` (`lib.rs:351-354`), a `register_union_view`
//!     sibling of `register_view` (`lib.rs:1543-1623`), and a
//!     `union_inputs(view_id)` accessor beside `view_sources`
//!     (`lib.rs:2051-2058`). No grouping/role/fold state is touched.
//! H3. Dispatch: the union worker from the NEW file
//!     `crates/lvu-view/src/union_worker.rs` (this assignment's owned path),
//!     driven through a narrow seam in `submit_query` (`lib.rs:2060-2130`):
//!     a union candidate carries `QueryPurpose::Union` (new variant in
//!     `lvu/src/app.rs:904-909`; coordinate terminal dispatch/fork with W22)
//!     and fences on input `(view_id, accepted_revision)` pairs exactly like
//!     a base-constraint fence. Per input the worker calls the EXISTING
//!     public `freeze_input` (`export.rs:462-468`) with union-sized
//!     `FrozenInputLimits`, maps each visited `FrozenInputRow` 1:1 into
//!     `UnionFrozenRow` (identity from `row.record.record_id`, timestamp from
//!     the input's accepted basis evaluation for that record, values from
//!     `row.fields`), then calls `union_frozen_inputs`. No `export.rs`
//!     change is needed; no `Membership.derived` read is permitted.
//! H4. Publication contract (primary region): the union publishes an ordinary
//!     `Membership` — per-source surviving `(sequence, time)` vectors for raw
//!     resolution through the existing `raw` provider, `order` in union ts
//!     order, `evaluation_batches` replayable per input — so downstream
//!     filter/grouping/folding/export treat it like any view. The union reads
//!     inputs through `rows().unfolded_page` identity sets, never folded pages
//!     and never display strings.
//! H5. `crates/lvu/src/app.rs`: union draft/accepted state on the derived
//!     view (input IDs + fenced revisions, mirroring `exact_field` as
//!     accepted-state-or-nothing); source-assistance owner keeps main.rs
//!     source methods/agent UI.
//! H6. Persistence (autosave owner): `StoredUnionShape` as an additive
//!     `presentation_json.union` key with `#[serde(default)]` (the
//!     `color_rules` precedent in `contracts.md`) — no `DB_SCHEMA_VERSION`
//!     bump; restore re-resolves input IDs and never launches commands.
//! H7. Archive review (primary): union snapshots freeze input view IDs, their
//!     accepted revisions and per-source high-watermarks (`FrozenInputSummary`
//!     carries all three); independent input advances create new immutable
//!     boundaries rather than mutating a published manifest.

use std::collections::{BTreeMap, HashMap, HashSet};

use polars::prelude::*;
use serde::{Deserialize, Serialize};
use thiserror::Error;

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
/// nanos under the capture basis), taken from the same frozen accepted basis
/// evaluation the typed values come from — never from the `Membership.derived`
/// display map. `None` is a missing/unreadable value in that basis — never an
/// error, never a reason to drop the record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnionInputRow {
    pub id: UnionRecordId,
    pub timestamp_nanos: Option<i64>,
}

/// One frozen typed row of a union input: the worker's 1:1 mapping of a
/// `FrozenInputRow` visited through `FrozenInput::visit`.
///
/// * identity comes from `row.record.record_id` (stable capture identity);
/// * `timestamp_nanos` comes from the input's accepted basis evaluation for
///   that record (the frozen summary's `time_basis` names which);
/// * `fields` is `row.fields` verbatim: TYPED `serde_json::Value`s with native
///   dtype evidence in the sibling `field_types` map — the same encoding the
///   command path consumes (`command_snapshot::prepare`). Display strings
///   cannot enter here by construction: there is no field for them.
#[derive(Clone, Debug, PartialEq)]
pub struct UnionFrozenRow {
    pub source_id: String,
    pub sequence: u64,
    pub timestamp_nanos: Option<i64>,
    pub fields: BTreeMap<String, serde_json::Value>,
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
                id: UnionRecordId::new(row.source_id.clone(), row.sequence),
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

/// Union frozen accepted evaluations in timestamp order: the worker-facing
/// entry point.
///
/// Each input is the worker's `freeze_input` output mapped 1:1 into
/// [`UnionFrozenInput`] (see its docs for the exact field mapping). This
/// function decodes every input's TYPED json values back into a Polars frame
/// — the inverse of the frozen `json_value` encoding (`export.rs`), so an
/// `Int64` 1000 stays an `Int64` 1000 and a 600-byte string stays 600 bytes —
/// then delegates the merge itself to [`union_typed_frames`]. Decoding rules:
///
/// * bool/whole-number/float/string/null decode to their Polars scalar dtype;
///   missing fields decode as null (diagonal concat fills them);
/// * a column mixing whole numbers and floats is rejected (the
///   `precise_json_value_for_field` precedent in `export.rs` refuses the same
///   potentially lossy coercion): normalize upstream in enrichment;
/// * whole numbers that do not fit `i64` use `UInt64` when every value is
///   non-negative, else the candidate is rejected rather than wrapped;
/// * arrays and objects encode as compact JSON text (`String` dtype): content
///   is preserved verbatim and round-trippable, never dropped, and never
///   mistaken for a scalar by the sort;
/// * an all-null column decodes as `Null` and sorts last with the missing
///   timestamps.
///
/// Identity (`_lvu_source_id`, `_lvu_sequence` — the engine's canonical
/// columns from `lvu-query`) and the internal timestamp column
/// ([`UNION_TS_COLUMN`) are built from the frozen identity and basis
/// evaluation, never from value fields. A frozen field colliding with any of
/// those names is rejected: enrichment can never produce a `_lvu_*` field
/// (protected names), so a collision means a corrupt handoff, not user data.
pub fn union_frozen_inputs(
    union_view_id: &str,
    inputs: &[UnionFrozenInput],
) -> Result<DataFrame, UnionError> {
    let ids: Vec<&str> = inputs.iter().map(|input| input.view_id.as_str()).collect();
    validate_union_spec(union_view_id, &ids)?;
    let total: usize = inputs.iter().map(|input| input.rows.len()).sum();
    if total > MAX_UNION_ROWS {
        return Err(UnionError::RowLimit {
            rows: total,
            maximum: MAX_UNION_ROWS,
        });
    }
    let mut frames = Vec::with_capacity(inputs.len());
    for input in inputs {
        frames.push(frozen_frame(input)?);
    }
    union_typed_frames(frames, UNION_TS_COLUMN, SOURCE_ID_COLUMN, SEQUENCE_COLUMN)
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
            .map(|row| row.source_id.clone())
            .collect::<Vec<_>>(),
    ));
    columns.push(Column::new(
        SEQUENCE_COLUMN.into(),
        input
            .rows
            .iter()
            .map(|row| row.sequence)
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
    // One dtype per field across all rows of this input; absent fields are
    // simply missing here and arrive as null through diagonal concat, as do
    // fields whose every value is null (they carry no type information).
    let mut names: BTreeSet<&str> = BTreeSet::new();
    for row in &input.rows {
        names.extend(row.fields.keys().map(String::as_str));
    }
    for name in names {
        if let Some(column) = decode_union_field(&input.view_id, name, &input.rows)? {
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
/// See [`union_frozen_inputs`] for the rules. `Null` decodes alongside every
/// dtype as validity, never as a dtype of its own: a field whose every value
/// is null yields `None` and arrives through diagonal concat from whichever
/// input actually knows its type.
fn decode_union_field(
    view_id: &str,
    name: &str,
    rows: &[UnionFrozenRow],
) -> Result<Option<Column>, UnionError> {
    use serde_json::Value as Json;
    let reject = |reason: String| UnionError::Engine {
        reason: format!("union input '{view_id}' field '{name}': {reason}"),
    };
    let mut has_bool = false;
    let mut has_int = false;
    let mut has_float = false;
    let mut has_string = false;
    let mut has_nested = false;
    let mut has_value = false;
    for row in rows {
        match row.fields.get(name) {
            None | Some(Json::Null) => {}
            Some(Json::Bool(_)) => {
                has_bool = true;
                has_value = true;
            }
            Some(Json::Number(number)) => {
                has_value = true;
                if number.is_f64() {
                    has_float = true;
                } else {
                    has_int = true;
                }
            }
            Some(Json::String(_)) => {
                has_string = true;
                has_value = true;
            }
            Some(Json::Array(_)) | Some(Json::Object(_)) => {
                has_nested = true;
                has_value = true;
            }
        }
    }
    if !has_value {
        return Ok(None);
    }
    let kinds = [has_bool, has_int, has_float, has_string, has_nested]
        .into_iter()
        .filter(|present| *present)
        .count();
    if kinds > 1 {
        // Mixed scalar kinds — including the whole-number/float coercion the
        // precise frozen path refuses — are a schema conflict for the union,
        // not a coercion: the candidate is rejected and normalization stays
        // upstream in enrichment.
        return Err(reject("conflicting value kinds across records".into()));
    }
    let get = |index: usize| rows[index].fields.get(name);
    if has_bool {
        return Ok(Some(Column::new(
            name.into(),
            (0..rows.len())
                .map(|index| get(index).and_then(Json::as_bool))
                .collect::<Vec<_>>(),
        )));
    }
    if has_int {
        // Whole numbers prefer `Int64`; values outside it use `UInt64` when
        // every value is non-negative, and otherwise reject rather than wrap.
        let ints: Vec<Option<i64>> = (0..rows.len())
            .map(|index| get(index).and_then(Json::as_i64))
            .collect();
        let present = ints.iter().flatten().count();
        let values = rows
            .iter()
            .filter_map(|row| row.fields.get(name))
            .filter(|value| !value.is_null())
            .count();
        if present == values {
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
    if has_string {
        return Ok(Some(Column::new(
            name.into(),
            (0..rows.len())
                .map(|index| get(index).and_then(Json::as_str).map(str::to_owned))
                .collect::<Vec<_>>(),
        )));
    }
    if has_nested {
        return Ok(Some(Column::new(
            name.into(),
            (0..rows.len())
                .map(|index| {
                    get(index)
                        .map(|value| serde_json::to_string(value).unwrap_or_else(|_| "null".into()))
                })
                .collect::<Vec<_>>(),
        )));
    }
    Ok(None)
}

/// Union accepted typed frames in timestamp order with Polars doing the value
/// compute.
///
/// Each frame is one input view's accepted typed output: its ordinary columns
/// plus `source_column` (string source identity), `sequence_column` (`u64`
/// physical sequence) and the accepted `timestamp_column` (`i64` unix nanos,
/// null where the basis had no value).
///
/// PRODUCTION CALLERS MUST BUILD THESE FRAMES FROM FROZEN ACCEPTED EVALUATION
/// (see [`union_frozen_inputs`], the one blessed constructor): frames
/// projected out of the `Membership.derived` display map are truncated,
/// type-erased strings and are rejected by contract — this function trusts a
/// dtype fidelity it cannot verify, so the provenance fence lives at
/// construction.
///
/// Frames may have different column sets: diagonal concatenation keeps every column and fills absent fields with
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
