//! Native, bounded live-view query adapter.

pub mod command_columns;
mod export;
pub mod folding;
pub mod time_basis;
/// Live union views over accepted input views (Muse union-views worktree).
/// The worker and all union methods live in `union_worker.rs`; the pure
/// spec/decode/merge contract lives in `union.rs`.
pub mod union;
mod union_worker;
pub use command_columns::CommandColumns;
pub use export::*;
pub use union::{
    INPUT_COLUMN, MAX_UNION_BYTES, MAX_UNION_INPUTS, MAX_UNION_ROWS, MergedUnionRow,
    SEQUENCE_COLUMN, SOURCE_ID_COLUMN, StoredUnionInput, StoredUnionShape, UNION_TS_COLUMN,
    UnionCandidateSpec, UnionCompletion, UnionError, UnionFilterSpec, UnionFrozenInput,
    UnionFrozenRow, UnionInputRow, UnionInputSnapshot, UnionLimits, apply_union_filter,
    detect_union_cycle, frozen_identity_snapshot, merge_union_rows, union_frozen_inputs,
    union_input_stale, union_typed_frames, union_workspace_bytes, validate_union_spec,
};
pub use union_worker::{UnionPhaseTestProbe, UnionPublishTestBarrier, UnionTestBarrier};

mod appended;

use polars::prelude::{DataType, Expr, IntoLazy, col};

use crate::appended::{Appended, AppendedBuilder};
use crate::folding::{FoldConfig, FoldEngine, FoldKey, FoldScope, Normalisation};
use lvu::{
    DisplayRow, FoldNormalisation, FoldRequest, FoldScopeRequest, FoldSummary, QueryCompletion,
    QueryFailure, QueryPurpose, QueryRequest, RowId, RowPage, RowProvider, ViewportRequest,
    terminal::QueryDispatcher,
};
use lvu_core::SourceId;
use lvu_shared::AnySourceHandle;
use lvu_live::{IndexState, LiveRowProvider, SourceViewStatus};
use lvu_query::{
    BatchQuery, BatchValidity, CompiledEnrichment, CompilerHost, CompilerHostConfig, DerivedState,
    EnrichmentDefinition as NativeEnrichmentDefinition, EnrichmentStage,
    EnrichmentStageId as NativeEnrichmentStageId, ExpressionKind, SchemaContext, TextSearch,
    compile_enrichment_chain, exact_key_flags, execute_batch_with_exact_constraint, non_null_flags,
    parse_regex_enrichment, records_to_batch_with_context_and_exact_field, scalar_projection,
};
use polars::prelude::{Column, DataFrame, NamedFrom, Series};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque},
    io,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::Duration,
};
use thiserror::Error;

/// One `u64` sequence plus the `i64` basis timestamp that travels beside it.
/// Both are charged to the membership budget, so retaining timestamps cannot
/// push a view past its cap without the cap noticing.
const SEQUENCE_BYTES: u64 = 16;

/// How many rows one gap search may read from a view that keeps no membership.
/// Bounded like every other provider call: a search that finds nothing inside
/// the budget reports "not found", and the caller says so.
const MAX_GAP_SCAN_ROWS: usize = 8192;
const GAP_SCAN_CHUNK_ROWS: usize = 512;
// Conservative charge for SourceMatches, Arc allocation metadata and UUID text.
const SOURCE_OVERHEAD: u64 = 128;
/// Raw row lookups are individually enqueued into the live provider's bounded
/// request queue. Asking for a whole tall viewport at once overflows that queue
/// and silently drops every request, so each frame asks for a bounded prefix of
/// the rows it is still missing and lets the next frame continue.
pub const MAX_ROW_REQUESTS_PER_PAGE: usize = 16;
/// A satisfied membership whose rows never arrive must not retry forever. After
/// this many consecutive frames without progress the view reports `Stalled`
/// instead of continuing to redraw.
pub const MAX_ROW_FETCH_RETRIES: u32 = 64;
/// Bound for reasons copied out of the raw provider into readiness.
const MAX_READINESS_REASON_BYTES: usize = 2 * 1024;

#[derive(Clone, Debug)]
pub struct ViewConfig {
    pub artifact_dir: PathBuf,
    pub request_capacity: usize,
    pub update_capacity: usize,
    pub completion_capacity: usize,
    pub page_records: usize,
    pub page_bytes: usize,
    pub maximum_index_bytes: u64,
    pub maximum_views: usize,
    pub maximum_sources_per_view: usize,
    pub maximum_viewport_rows: usize,
    pub compiler: Option<CompilerHostConfig>,
    pub maximum_snapshot_jobs: usize,
}

impl ViewConfig {
    pub fn new(artifact_dir: impl Into<PathBuf>) -> Self {
        Self {
            artifact_dir: artifact_dir.into(),
            request_capacity: 16,
            update_capacity: 64,
            completion_capacity: 32,
            // Every scanned page pays a fixed cost — a journal round trip, a
            // Polars frame, a plan and an engine run — and at 256 records that
            // fixed cost was most of the scan. The byte limit, not this one, is
            // what actually bounds a page's memory: an ordinary log record is
            // around a hundred bytes, so 4096 records is a few hundred KiB and
            // a page of very large records still stops at `page_bytes`.
            page_records: 4096,
            page_bytes: 2 * 1024 * 1024,
            maximum_index_bytes: 256 * 1024 * 1024,
            maximum_views: 128,
            maximum_sources_per_view: 32,
            maximum_viewport_rows: 256,
            compiler: None,
            maximum_snapshot_jobs: 2,
        }
    }
}

#[derive(Debug, Error)]
pub enum ViewError {
    #[error("view adapter configuration contains a zero bound")]
    InvalidConfig,
    #[error("view adapter source is not registered")]
    UnknownSource,
    #[error("view adapter view is not registered")]
    UnknownView,
    #[error("snapshot worker capacity is full")]
    SnapshotCapacity,
    #[error("view source limit exceeded")]
    SourceLimit,
    #[error("view limit exceeded")]
    ViewLimit,
    #[error("view adapter is shut down")]
    Closed,
    #[error("live row adapter: {0}")]
    Live(String),
    #[error("I/O: {0}")]
    Io(#[from] io::Error),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScanState {
    Raw,
    Pending,
    Ready,
    Limited,
    Error,
    Shutdown,
}

/// The bounded raw-row seam every view drives to turn matched record identities
/// into displayable rows. `LiveRowProvider` is the production implementation;
/// substituting another one lets tests inject the delay, starvation, lookup
/// failure and supersession behaviour that a real journal only produces by luck.
///
/// Implementations must not block on filesystem I/O. `row_by_id` returning
/// `None` means "not resolvable yet", never "absent".
pub trait RawRowSource: Send + Sync {
    fn page(&self, view_id: &str, request: ViewportRequest) -> RowPage;
    fn row_by_id(&self, view_id: &str, id: &RowId) -> Option<DisplayRow>;
    fn index_of_id(&self, view_id: &str, id: &RowId) -> Option<usize>;
    fn context_page(
        &self,
        view_id: &str,
        anchor: &RowId,
        offset: isize,
        len: usize,
    ) -> lvu::ContextPage;
    fn revision(&self, view_id: &str) -> u64;
    fn register_source(&self, handle: AnySourceHandle) -> Result<(), String>;
    fn register_raw_view(&self, view_id: &str, sources: Vec<SourceId>) -> Result<(), String>;
    fn drain_ready_updates(&self, maximum: usize) -> usize;
    fn source_status(&self, source_id: SourceId) -> Option<SourceViewStatus>;
    fn stats(&self) -> lvu_live::AdapterStats;
}

impl RawRowSource for LiveRowProvider {
    fn page(&self, view_id: &str, request: ViewportRequest) -> RowPage {
        RowProvider::page(self, view_id, request)
    }
    fn row_by_id(&self, view_id: &str, id: &RowId) -> Option<DisplayRow> {
        RowProvider::row_by_id(self, view_id, id)
    }
    fn index_of_id(&self, view_id: &str, id: &RowId) -> Option<usize> {
        RowProvider::index_of_id(self, view_id, id)
    }
    fn context_page(
        &self,
        view_id: &str,
        anchor: &RowId,
        offset: isize,
        len: usize,
    ) -> lvu::ContextPage {
        RowProvider::context_page(self, view_id, anchor, offset, len)
    }
    fn revision(&self, view_id: &str) -> u64 {
        RowProvider::revision(self, view_id)
    }
    fn register_source(&self, handle: AnySourceHandle) -> Result<(), String> {
        LiveRowProvider::register_source(self, handle).map_err(|error| error.to_string())
    }
    fn register_raw_view(&self, view_id: &str, sources: Vec<SourceId>) -> Result<(), String> {
        LiveRowProvider::register_raw_view(self, view_id, sources).map_err(|e| e.to_string())
    }
    fn drain_ready_updates(&self, maximum: usize) -> usize {
        LiveRowProvider::drain_ready_updates(self, maximum)
    }
    fn source_status(&self, source_id: SourceId) -> Option<SourceViewStatus> {
        LiveRowProvider::source_status(self, source_id)
    }
    fn stats(&self) -> lvu_live::AdapterStats {
        LiveRowProvider::stats(self)
    }
}

/// Whether the rows the UI is about to render are the complete answer, and if
/// not, why. A view whose membership is satisfied but whose rows are not yet
/// displayable must never render as an ordinary empty result: every variant
/// other than [`RowReadiness::Ready`] has a [`RowReadiness::describe`] sentence
/// the UI is required to show in place of a blank pane.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RowReadiness {
    /// The last requested range was served completely.
    Ready,
    /// The query worker is still scanning; membership is not final yet.
    QueryPending { scanned: u64 },
    /// Membership is satisfied, but rows in the last requested range are not in
    /// the bounded raw-row cache yet. The provider is re-requesting them.
    RowsPending { pending: usize, requested: usize },
    /// A source's derived index is still being built, so matched records may not
    /// be addressable yet. Counts are physical records, not display rows.
    Indexing {
        indexed_records: u64,
        reported_records: u64,
    },
    /// A raw row lookup or index operation reported a failure. Captured data is
    /// intact; this is a read failure, not an empty result.
    LookupFailed { reason: String, pending: usize },
    /// A source's derived index is held exclusively by another owner. The live
    /// worker is retrying on a bounded schedule and recovers without any user
    /// action once the holder releases it.
    IndexContended { pending: usize },
    /// The shared derived-index total could not be accounted for, so aggregate
    /// enforcement is suspended. Each source is still held to its own size cap
    /// and captured data is intact; clearing unused indexes restores the total.
    IndexBudgetUnverified { pending: usize },
    /// Rows did not arrive within the bounded retry budget. Refreshing the view
    /// or scrolling re-requests them.
    Stalled { pending: usize, requested: usize },
    /// A presentation-only recompute — repeated-pattern folding — has not
    /// consumed the whole stream yet. The rows on screen are usable now: the
    /// part already folded is projected, the rest renders individually, and
    /// the projection replaces itself as the feed advances. Captured records,
    /// stable identities and membership are untouched by it.
    Folding {
        folded_rows: usize,
        total_rows: usize,
    },
    /// The query completed and genuinely matched no records.
    NoMatches,
    /// The query itself failed, was limited or shut down.
    QueryFailed { reason: String },
}

impl RowReadiness {
    /// True only when the served rows are the whole answer.
    pub fn is_ready(&self) -> bool {
        matches!(self, RowReadiness::Ready)
    }

    /// True when rows may still arrive without any further user action.
    pub fn is_pending(&self) -> bool {
        matches!(
            self,
            RowReadiness::QueryPending { .. }
                | RowReadiness::RowsPending { .. }
                | RowReadiness::Indexing { .. }
                | RowReadiness::IndexContended { .. }
                | RowReadiness::Folding { .. }
        )
    }

    /// A bounded sentence for every state that is not [`RowReadiness::Ready`].
    /// `None` means, and only means, that the rows on screen are complete.
    pub fn describe(&self) -> Option<String> {
        Some(match self {
            RowReadiness::Ready => return None,
            RowReadiness::QueryPending { scanned } => {
                format!("Filtering: scanned {scanned} records so far.")
            }
            RowReadiness::RowsPending { pending, requested } => {
                format!("Loading {pending} of {requested} matched rows.")
            }
            RowReadiness::Indexing {
                indexed_records,
                reported_records,
            } => format!(
                "Building the record index: {indexed_records} of {reported_records} records ready."
            ),
            RowReadiness::LookupFailed { reason, pending } => {
                format!("Could not load {pending} matched rows: {reason}")
            }
            RowReadiness::IndexContended { pending } => format!(
                "The record index for this source is in use elsewhere. Waiting for it; {pending} rows load as soon as it is free."
            ),
            RowReadiness::IndexBudgetUnverified { pending } => format!(
                "The shared index cache is too large to account for, so its total is unverified. {pending} rows are still loading under this source's own limit; clearing unused indexes restores the total."
            ),
            RowReadiness::Stalled { pending, requested } => format!(
                "{pending} of {requested} matched rows did not load. Scroll or refresh to retry."
            ),
            // Short on purpose: it shares the status line with the view's own
            // fold summary, and a sentence long enough to crowd that out would
            // hide the counts it is reporting progress towards.
            RowReadiness::Folding {
                folded_rows,
                total_rows,
            } => format!("folding {folded_rows}/{total_rows} rows"),
            RowReadiness::NoMatches => "No records match this view.".to_owned(),
            RowReadiness::QueryFailed { reason } => {
                format!("This view could not be built: {reason}")
            }
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ViewQueryStatus {
    pub view_id: String,
    pub revision: u64,
    pub state: ScanState,
    pub scanned_records: u64,
    pub high_watermarks: Vec<(SourceId, Option<u64>)>,
    pub matched_records: u64,
    pub index_bytes: u64,
    pub diagnostic: Option<String>,
}

#[derive(Clone)]
struct SourceRegistration {
    handle: AnySourceHandle,
}

#[derive(Clone)]
struct PreparedDefinition {
    revision: u64,
    constraints: lvu::QueryConstraints,
    text: Option<TextSearch>,
    advanced: Option<lvu_query::CompiledDefinition>,
    enrichment: Vec<EnrichmentStage>,
    grouping: Option<ContinuationRule>,
    schema_seed: SchemaContext,
    schema: SchemaContext,
    checkpoints: HashMap<String, (u64, u64, Option<u64>)>,
    membership: Option<Arc<Membership>>,
}

#[derive(Clone)]
struct ViewRegistration {
    sources: Vec<SourceId>,
    raw_view: String,
}

#[derive(Clone)]
struct SourceMatches {
    source_id: String,
    generation: u64,
    high_watermark: Option<u64>,
    sequences: Appended<u64>,
    /// The basis timestamp of each matched record, aligned with `sequences`.
    /// [`NO_BASIS_TIME`] marks a record with no readable value in the basis;
    /// gap navigation skips those rather than inventing a distance for them.
    times: Appended<i64>,
    groups: Appended<GroupRange>,
    /// Measured before the view's own time window narrowed the set, so the
    /// dataset-relative ranges describe the dataset (see `lvu::TimeBounds`).
    bounds: SourceTimeBounds,
    /// The merge key of each displayed unit — a record, or a group when the
    /// view is grouped — aligned with `sequences` or `groups`.
    ///
    /// This is `times` with every [`NO_BASIS_TIME`] replaced by the last timed
    /// value before it in this same source (I3), so the merge never has to ask
    /// what a hole means. A leading run of untimed records takes `i64::MIN`
    /// and leads the source.
    merge_keys: Appended<i64>,
    /// Whether `merge_keys` is nondecreasing. False means this source arrives
    /// out of order in the current basis, which I2 says is reported rather
    /// than sorted away.
    ascending: bool,
}

/// "This record has no timestamp in the current basis." A sentinel rather than
/// `Option<i64>` because the vector is one per matched record and doubling its
/// width to carry a niche would cost more than the sentinel explains.
const NO_BASIS_TIME: i64 = i64::MIN;

#[derive(Clone, Copy, Debug, Default)]
struct SourceTimeBounds {
    first: Option<i64>,
    last: Option<i64>,
    count: usize,
    missing: usize,
}

impl SourceTimeBounds {
    fn observe(&mut self, timestamp: Option<i64>) {
        match timestamp {
            Some(value) => {
                self.first = Some(self.first.map_or(value, |first| first.min(value)));
                self.last = Some(self.last.map_or(value, |last| last.max(value)));
                self.count += 1;
            }
            None => self.missing += 1,
        }
    }

    fn merge(&mut self, other: &Self) {
        if let Some(value) = other.first {
            self.first = Some(self.first.map_or(value, |first| first.min(value)));
        }
        if let Some(value) = other.last {
            self.last = Some(self.last.map_or(value, |last| last.max(value)));
        }
        self.count += other.count;
        self.missing += other.missing;
    }
}

#[derive(Clone)]
struct GroupRange {
    start: usize,
    len: usize,
    logical_lines: usize,
    payload_bytes: usize,
    stream: lvu_core::StreamKind,
    orphan: bool,
    split: bool,
    oversized: bool,
    /// The group derives from unevaluated start/key flags (pending command
    /// output, failed stage, or a batch missing the column). It stands alone
    /// and never extends, so live unknowns stay visible without falsely
    /// joining or cutting the accepted groups around them.
    pending: bool,
    /// The exact typed run key of this group's head. Only Runs groups carry
    /// one; equality is byte equality over Polars-encoded values, and a new
    /// record joins only on an exact match.
    run_key: Option<Vec<u8>>,
    /// A configured Run/Filter group rather than a legacy lexical one. Only
    /// presentation differs: member lines render text first so narrow
    /// terminals show content instead of a UUID prefix, with stable IDs kept
    /// in the same details.
    configured: bool,
    /// The head's run key exceeded the exact-identity bound, so the record
    /// stands alone unfolded with a diagnostic rather than merged on a
    /// truncated identity.
    key_refused: bool,
    auto_open: bool,
    auto_structured: bool,
    auto_structure_depth: u16,
    partial_open: bool,
    partial_truncated: bool,
    structure_truncated: bool,
    partial_prefix: Vec<u8>,
    structure_prefix: Vec<u8>,
    acquisition_id: [u8; 16],
    last_chunk: lvu_core::ChunkPosition,
    first_capture_nanos: i64,
    last_capture_nanos: i64,
    projection: Arc<Vec<DisplayRow>>,
}

const MAX_GROUP_LINES: usize = 64;
const MAX_GROUP_PAYLOAD_BYTES: usize = 64 * 1024;
/// Member projections retained per configured (Run/Filter) group. The group
/// keeps every member's identity via `start`/`len` over the source sequences;
/// only the first page of projections is stored, so a giant event costs a
/// bounded page per frame while staying one group. The head text and details
/// always state shown/total explicitly, and the remaining members stay
/// reachable through the source view.
const MAX_CONFIGURED_GROUP_STORED: usize = 64;
const MAX_GROUP_REGEX_BYTES: usize = 16 * 1024;
const MAX_GROUP_REGEX_COMPILED_BYTES: usize = 1024 * 1024;
const MAX_GROUP_REGEX_NESTING: u32 = 64;
const MAX_GROUP_LINE_DISPLAY_BYTES: usize = 4 * 1024;
const MAX_GROUP_LINE_PROJECTION_BYTES: usize = 8 * 1024;
const MAX_AUTO_GROUP_SPAN_NANOS: i64 = 30_000_000_000;

fn auto_group_within_span(
    first_capture_nanos: i64,
    last_capture_nanos: i64,
    captured_at_unix_nanos: i64,
) -> bool {
    let from_head = captured_at_unix_nanos
        .checked_sub(first_capture_nanos)
        .is_some_and(|elapsed| (0..=MAX_AUTO_GROUP_SPAN_NANOS).contains(&elapsed));
    let from_previous = captured_at_unix_nanos
        .checked_sub(last_capture_nanos)
        .is_some_and(|elapsed| elapsed >= 0);
    from_head && from_previous
}

fn group_state_bytes(group: &GroupRange) -> u64 {
    group_base_state_bytes()
        .unwrap_or(u64::MAX)
        .saturating_add(u64::try_from(group.partial_prefix.capacity()).unwrap_or(u64::MAX))
        .saturating_add(u64::try_from(group.structure_prefix.capacity()).unwrap_or(u64::MAX))
        .saturating_add(
            u64::try_from(group.run_key.as_ref().map_or(0, Vec::capacity)).unwrap_or(u64::MAX),
        )
}

fn group_base_state_bytes() -> Result<u64, std::num::TryFromIntError> {
    u64::try_from(std::mem::size_of::<GroupRange>())
}

fn extended_auto_prefix(existing: &[u8], bytes: &[u8]) -> (Vec<u8>, bool) {
    const MAX_PREFIX_BYTES: usize = 512;
    let additional = bytes
        .len()
        .min(MAX_PREFIX_BYTES.saturating_sub(existing.len()));
    let mut extended = Vec::with_capacity(existing.len().saturating_add(additional));
    extended.extend_from_slice(existing);
    extended.extend_from_slice(&bytes[..additional]);
    (extended, additional < bytes.len())
}

#[derive(Clone)]
struct EvaluationBatch {
    source_id: String,
    generation: u64,
    record_count: usize,
    first_sequence: u64,
    last_sequence: u64,
    schema_before: SchemaContext,
}

struct MemoryBudget {
    used: AtomicU64,
    maximum: u64,
}

type FrozenDerived = HashMap<(String, u64, String), (serde_json::Value, String)>;

struct Membership {
    sources: Vec<SourceMatches>,
    count: u64,
    bytes: u64,
    budget: Arc<MemoryBudget>,
    enrichment_names: Vec<String>,
    derived: HashMap<(String, u64, String), Option<String>>,
    /// Per-record enrichment stage failures, keyed like `derived`: `(source,
    /// sequence, stage name)`. A ready display string can itself read
    /// `"error: ..."`, so failures are tracked structurally rather than
    /// inferred from the rendered value. Entries are added when a stage's
    /// batch projection fails for new records and removed when a later
    /// projection of the same cell succeeds; consumers proving per-row
    /// validity read this alongside `derived`, never the value text.
    derived_errors: HashSet<(String, u64, String)>,
    /// Union-only typed accepted cells keyed by stable identity and output.
    /// `None` means ordinary replay owns the authoritative stage evaluation;
    /// `Some` makes frozen union replay overlay only winning-input accepted
    /// values and suppress raw same-named fields on every other row.
    frozen_derived: Option<FrozenDerived>,
    /// Which colour rule painted each row: `(source, sequence)` to the index of
    /// the first rule whose predicate matched. Only matched rows appear.
    color_matches: HashMap<(String, u64), u16>,
    /// The rules those matches were computed for. Carrying matches forward is
    /// only sound while the list is unchanged; an edited rule must not leave a
    /// row painted by the rule it replaced.
    color_rules: Vec<lvu::ColorRule>,
    advanced: Option<lvu_query::CompiledDefinition>,
    enrichment: Vec<EnrichmentStage>,
    evaluation_page_bytes: usize,
    evaluation_batches: Arc<[EvaluationBatch]>,
    event_time_missing: usize,
    event_time_invalid: usize,
    /// The basis the retained timestamps were read in. A caller asking about a
    /// different basis is asking a question this membership cannot answer.
    basis: lvu::TimeBasis,
    grouped: bool,
    /// Display order: `(source index, unit index)` for every displayed unit,
    /// merged by `(merge key, source position, sequence)`.
    ///
    /// Built once here because membership is an immutable snapshot, so paging
    /// never merges: `page` slices this and `index_of_id` reads `ranks`.
    order: Appended<(u32, u32)>,
    /// The inverse of `order`, one vector per source: unit index to display
    /// position. Keeps `index_of_id` at the cost the prefix sum had.
    ranks: Arc<[Appended<u32>]>,
    /// The largest merge key of any unit already in `order`. A refresh whose
    /// arriving keys all exceed it extends the order instead of rebuilding it.
    max_key: i64,
}

struct Reservation {
    budget: Arc<MemoryBudget>,
    bytes: u64,
    committed: bool,
}

impl Reservation {
    fn new(budget: Arc<MemoryBudget>) -> Self {
        Self {
            budget,
            bytes: 0,
            committed: false,
        }
    }
    fn add(&mut self, bytes: u64) -> bool {
        let mut used = self.budget.used.load(Ordering::Acquire);
        loop {
            let Some(next) = used.checked_add(bytes) else {
                return false;
            };
            if next > self.budget.maximum {
                return false;
            }
            match self.budget.used.compare_exchange_weak(
                used,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    self.bytes = self
                        .bytes
                        .checked_add(bytes)
                        .expect("reservation is bounded by the budget maximum");
                    return true;
                }
                Err(actual) => used = actual,
            }
        }
    }
    fn retain(&mut self, bytes: u64) -> bool {
        if bytes > self.bytes {
            return false;
        }
        let released = self.bytes - bytes;
        self.bytes = bytes;
        self.budget.used.fetch_sub(released, Ordering::AcqRel);
        true
    }
    #[allow(clippy::too_many_arguments)]
    fn finish(
        mut self,
        sources: Vec<SourceMatches>,
        count: u64,
        enrichment_names: Vec<String>,
        derived: HashMap<(String, u64, String), Option<String>>,
        derived_errors: HashSet<(String, u64, String)>,
        color_matches: HashMap<(String, u64), u16>,
        color_rules: Vec<lvu::ColorRule>,
        advanced: Option<lvu_query::CompiledDefinition>,
        enrichment: Vec<EnrichmentStage>,
        evaluation_page_bytes: usize,
        evaluation_batches: Vec<EvaluationBatch>,
        event_time_missing: usize,
        event_time_invalid: usize,
        basis: lvu::TimeBasis,
        grouped: bool,
        prior: Option<Arc<Membership>>,
    ) -> Arc<Membership> {
        self.committed = true;
        let (order, ranks, max_key) = merge_order(
            &sources,
            grouped,
            basis != lvu::TimeBasis::Capture,
            prior.as_deref(),
        );
        Arc::new(Membership {
            sources,
            count,
            bytes: self.bytes,
            budget: Arc::clone(&self.budget),
            enrichment_names,
            derived,
            derived_errors,
            frozen_derived: None,
            basis,
            color_matches,
            color_rules,
            advanced,
            enrichment,
            evaluation_page_bytes,
            evaluation_batches: evaluation_batches.into(),
            event_time_missing,
            event_time_invalid,
            grouped,
            order,
            ranks,
            max_key,
        })
    }
}

/// One source's merge keys, and whether they are nondecreasing.
///
/// I3, applied once for every basis. Only the chosen-column basis is read by a
/// Polars expression; capture time is a field assigned at ingest, and the
/// recognized and extracted bases are read row-locally from bytes that are not
/// a frame at that point. Filling inside that one expression would mean
/// hand-rolling the same rule three more times for the others, which is the
/// duplication the engine/app rule exists to prevent — so the fill happens
/// where the four converge, over the key, after each basis has produced
/// whatever it could.
///
/// A grouped view merges groups, so its key is the group's first record's key:
/// a group is a physically contiguous run of continuation lines and splitting
/// it would tear one record apart.
fn merge_keys_for(
    prior: Option<(&Appended<i64>, bool)>,
    times: &Appended<i64>,
) -> (Appended<i64>, bool) {
    // A refresh extends the times it inherited, so the keys extend with them:
    // the fill is a running value and the flag a running comparison, and both
    // resume from where the last publication left them. Recomputing from row
    // zero would put the whole view back into every refresh.
    let (mut keys, mut ascending) = match prior {
        Some((keys, ascending)) if keys.len() <= times.len() => (keys.clone(), ascending),
        // A shorter or absent prior is not an extension of this run — a
        // generation changed under us, or nothing was retained. Start over.
        _ => (Appended::default(), true),
    };
    let mut carried = keys
        .len()
        .checked_sub(1)
        .and_then(|last| keys.get(last))
        .copied()
        .unwrap_or(i64::MIN);
    let mut previous = carried;
    let mut arrived = Vec::new();
    for time in times.iter().skip(keys.len()).copied() {
        if time != NO_BASIS_TIME {
            carried = time;
        }
        if carried < previous {
            ascending = false;
        }
        previous = carried;
        arrived.push(carried);
    }
    keys.extend(arrived);
    (keys, ascending)
}

/// A display order and its inverse: `(source index, unit index)` per position,
/// and per source the display position of each of its units.
type DisplayOrder = (Appended<(u32, u32)>, Arc<[Appended<u32>]>, i64);

/// Merges the sources' runs into one display order.
///
/// A k-way merge over runs the engine produced, by a key it produced: no value
/// is derived here, only the arrangement of rows already in hand
/// (docs/merged-view-ordering.md). Each source's run is consumed in its own
/// order and never sorted inside itself (I2), so a source whose keys are out
/// of order interleaves without being rewritten.
///
/// A grouped view merges *groups*, not records: a group is a physically
/// contiguous run of continuation lines and splitting it would tear a record
/// apart. Its key is its first record's key.
///
/// Under the capture basis the sources are concatenated in the order the user
/// put them in (I1). Capture time for files read together is an accident of
/// ingest scheduling, and the View dialog offers an explicit source order that
/// interleaving would silently overrule; the bases a user chooses *because*
/// they want time order — recognized, extracted, a chosen column — interleave.
fn merge_order(
    sources: &[SourceMatches],
    grouped: bool,
    interleave: bool,
    prior: Option<&Membership>,
) -> DisplayOrder {
    let lengths: Vec<usize> = unit_lengths(sources, grouped);
    let key_of = |source: usize, unit: usize| -> i64 {
        let source = &sources[source];
        let record = if grouped {
            source.groups.get(unit).map_or(0, |group| group.start)
        } else {
            unit
        };
        source.merge_keys.get(record).copied().unwrap_or(i64::MIN)
    };

    // Extend rather than rebuild when the arriving units all belong after
    // everything already ordered. That is the live tail — records arriving in
    // time order behind the ones already shown — and it is what keeps a
    // refresh costing what arrived rather than what the view holds.
    let mut order;
    let mut ranks: Vec<Appended<u32>>;
    let mut cursors: Vec<usize>;
    let mut max_key;
    match prior.filter(|prior| extends(prior, sources, &lengths, grouped, interleave, &key_of)) {
        Some(prior) => {
            order = prior.order.clone();
            ranks = prior.ranks.to_vec();
            cursors = ranks.iter().map(Appended::len).collect();
            cursors.resize(sources.len(), 0);
            ranks.resize_with(sources.len(), Appended::default);
            max_key = prior.max_key;
        }
        None => {
            order = Appended::default();
            ranks = lengths.iter().map(|_| Appended::default()).collect();
            cursors = vec![0usize; sources.len()];
            max_key = i64::MIN;
        }
    }

    let total: usize = lengths.iter().sum();
    let mut arrived = Vec::with_capacity(total.saturating_sub(order.len()));
    let mut arrived_ranks: Vec<Vec<u32>> = lengths
        .iter()
        .zip(cursors.iter())
        .map(|(len, done)| Vec::with_capacity(len.saturating_sub(*done)))
        .collect();
    // One cursor per source. The source count is the view's source list, so a
    // linear scan for the smallest head is cheaper than a heap and keeps the
    // tie rule — earliest source position wins — obvious.
    for position in order.len()..total {
        let mut chosen: Option<usize> = None;
        for (index, cursor) in cursors.iter().enumerate() {
            if *cursor >= lengths[index] {
                continue;
            }
            if !interleave {
                // Source order: take the first source with anything left, which
                // is the concatenation the user arranged.
                chosen = Some(index);
                break;
            }
            let key = key_of(index, *cursor);
            let better = match chosen {
                None => true,
                Some(best) => key < key_of(best, cursors[best]),
            };
            if better {
                chosen = Some(index);
            }
        }
        let Some(index) = chosen else { break };
        let unit = cursors[index];
        cursors[index] += 1;
        max_key = max_key.max(key_of(index, unit));
        arrived_ranks[index].push(u32::try_from(position).unwrap_or(u32::MAX));
        arrived.push((
            u32::try_from(index).unwrap_or(u32::MAX),
            u32::try_from(unit).unwrap_or(u32::MAX),
        ));
    }
    order.extend(arrived);
    for (ranks, arrived) in ranks.iter_mut().zip(arrived_ranks) {
        ranks.extend(arrived);
    }
    (order, ranks.into(), max_key)
}

/// Displayed units per source: records, or groups when the view is grouped.
fn unit_lengths(sources: &[SourceMatches], grouped: bool) -> Vec<usize> {
    if grouped {
        sources.iter().map(|source| source.groups.len()).collect()
    } else {
        sources
            .iter()
            .map(|source| source.sequences.len())
            .collect()
    }
}

/// Whether `prior`'s order is a prefix of the one these sources produce.
///
/// O(k): it looks at each source's length and at the first key of whatever it
/// grew by, never at the view. Two ways to qualify:
///
/// * interleaved — every arriving key is strictly greater than every key
///   already ordered. Strictly, so that no tie has to be re-broken: a tie
///   between an arriving record and an ordered one is settled by source
///   position, which could place the newcomer first and make the old order
///   something other than a prefix.
/// * concatenated — only the last source with any units grew, so nothing was
///   inserted ahead of a source that already contributed.
///
/// Anything else — a late record older than the view's maximum, a source that
/// appeared or vanished, a generation that changed — rebuilds, which is
/// correct and costs what it always cost.
fn extends(
    prior: &Membership,
    sources: &[SourceMatches],
    lengths: &[usize],
    grouped: bool,
    interleave: bool,
    key_of: &impl Fn(usize, usize) -> i64,
) -> bool {
    if prior.grouped != grouped || prior.ranks.len() > sources.len() {
        return false;
    }
    let mut grew: Option<usize> = None;
    for (index, length) in lengths.iter().enumerate() {
        let done = prior.ranks.get(index).map_or(0, Appended::len);
        if done > *length {
            // A source shrank: the prior order describes units that are gone.
            return false;
        }
        if done == *length {
            continue;
        }
        if interleave {
            // Only the first arriving key needs testing when the source's own
            // run is ascending; when it is not (I2) the run is used as it
            // stands, so every arriving key has to clear the bar.
            for unit in done..*length {
                if key_of(index, unit) <= prior.max_key {
                    return false;
                }
            }
        } else if grew.is_some() {
            return false;
        }
        grew = Some(index);
    }
    if !interleave && let Some(grew) = grew {
        // Concatenated: nothing after the grown source may already have units,
        // or the arrivals would land in front of them.
        if lengths[grew + 1..].iter().any(|length| *length > 0) {
            return false;
        }
    }
    true
}

impl Drop for Reservation {
    fn drop(&mut self) {
        if !self.committed {
            self.budget.used.fetch_sub(self.bytes, Ordering::AcqRel);
        }
    }
}

impl Drop for Membership {
    fn drop(&mut self) {
        self.budget.used.fetch_sub(self.bytes, Ordering::AcqRel);
    }
}

fn evaluation_batch_bytes(batch: &EvaluationBatch) -> u64 {
    batch
        .schema_before
        .fields()
        .keys()
        .fold(128_u64, |total, field| {
            total.saturating_add(field.len() as u64 + 24)
        })
        .saturating_add(batch.source_id.len() as u64)
}

/// Conservative managed-memory charge for one colour-match hash entry: owned
/// source text, sequence/rule values, hash-table control/storage and allocator
/// overhead. Both retained clones and newly inserted candidate entries pay it.
fn color_match_bytes(source: &str) -> u64 {
    source.len() as u64 + 48
}

fn display_projection_bytes(row: &DisplayRow) -> u64 {
    let text = row
        .timestamp
        .len()
        .saturating_add(row.level.len())
        .saturating_add(row.text.len());
    let fields = row.fields.iter().fold(0usize, |total, (key, value)| {
        total.saturating_add(key.len()).saturating_add(value.len())
    });
    let details = row.details.iter().fold(0usize, |total, (key, value)| {
        total.saturating_add(key.len()).saturating_add(value.len())
    });
    u64::try_from(
        text.saturating_add(fields)
            .saturating_add(details)
            .saturating_add(128),
    )
    .unwrap_or(u64::MAX)
}

fn group_projection_bytes(group: &GroupRange) -> u64 {
    group
        .projection
        .iter()
        .fold(group_state_bytes(group), |total, row| {
            total.saturating_add(display_projection_bytes(row))
        })
}

#[derive(Clone)]
enum Published {
    Raw,
    Filtered { membership: Arc<Membership> },
}

struct ViewState {
    registration: ViewRegistration,
    pending_sources: Option<Vec<SourceId>>,
    pending_request: Option<QueryRequest>,
    resubmit_pending: bool,
    desired_revision: u64,
    cancel: Arc<AtomicBool>,
    published: Published,
    provider_revision: u64,
    status: ViewQueryStatus,
    last_request: Option<QueryRequest>,
    applied_revision: u64,
    applied_generation: u64,
    applied_constraints: lvu::QueryConstraints,
    refreshing: bool,
    /// Row-fetch bookkeeping for the last non-empty range `page` was asked for.
    /// It is what separates "this view matched nothing" from "these rows have
    /// not been served yet".
    rows_requested: usize,
    rows_missing: usize,
    rows_served: usize,
    rows_raw_revision: u64,
    rows_retry: u32,
    /// Advances while rows are outstanding so the terminal redraws and
    /// re-requests them. Bounded by `MAX_ROW_FETCH_RETRIES`.
    rows_retry_revision: u64,
    /// Repeated-pattern folding, when the terminal has asked for it.
    fold: Option<FoldViewState>,
    /// Published command results by stage id, joined into every batch frame
    /// as `<name>.<field>` columns (docs/command-enrichment.md). Set by the
    /// app when a run publishes or a saved publication is restored; the app
    /// then reaffirms the chain so dependent steps read them.
    command_results: BTreeMap<String, Arc<CommandColumns>>,
    /// The last viewport that was served with rows in it. A presentation-only
    /// recompute must not blank the pane, so when a fresh collection comes back
    /// with nothing this is served instead and readiness says why. See
    /// [`RetainedPage`].
    retained: Option<RetainedPage>,
}

/// The last usable viewport for a view.
///
/// Rows are looked up through a bounded cache, so any frame can find the range
/// it wants missing — a fold feed that walked past it, an eviction, a dropped
/// request. Dropping to an empty pane in that frame loses the user's place for
/// no reason: nothing about the records changed. Retaining one viewport and
/// serving it again costs a bounded copy and keeps the last applied view usable
/// (AGENTS.md), while [`RowReadiness`] carries the reason it is not fresh.
struct RetainedPage {
    /// Publication the rows belong to. A new membership retires them rather
    /// than showing rows the current filter never matched.
    generation: u64,
    /// Rows requested when they were served. Only a request of the same shape
    /// may be answered from here, so a one-row probe — the terminal's selection
    /// and total sync calls — can never be answered with a whole viewport.
    request: ViewportRequest,
    rows: Vec<DisplayRow>,
}

/// What incremental refreshes have cost this session.
///
/// A refresh extends an applied view with the records that arrived since the
/// last one, so its cost should follow how many arrived, not how many the view
/// already holds. Measuring that needs the count and the time together: one
/// refresh in flight per view means a refresh that grows more expensive simply
/// happens less often, and a per-second figure flattens out while the per-
/// refresh cost keeps climbing.
#[derive(Debug, Default)]
pub struct RefreshStats {
    refreshes: AtomicU64,
    nanos: AtomicU64,
}

impl RefreshStats {
    fn record(&self, elapsed: std::time::Duration) {
        self.refreshes.fetch_add(1, Ordering::Relaxed);
        self.nanos.fetch_add(
            u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
    }

    /// Refreshes completed and nanoseconds spent in them. How many records each
    /// one took in is the caller's to know: a test drives the appends.
    pub fn snapshot(&self) -> (u64, u64) {
        (
            self.refreshes.load(Ordering::Acquire),
            self.nanos.load(Ordering::Acquire),
        )
    }
}

/// Shortest request that is a viewport rather than a probe. The terminal syncs
/// totals with `len: 0` and canonicalizes its selection with `len: 1`; both
/// want a precise answer or none, never a retained one.
const MIN_RETAINED_PAGE_ROWS: usize = 2;

/// Rows offered to the fold engine per `page` call. Feeding is incremental, so
/// a long stream costs a bounded amount of work per frame and the unconsumed
/// tail simply renders individually until folding catches up.
///
/// It is deliberately far below the raw provider's row cache. Feeding reads the
/// stream through that same bounded cache, so a budget near its size evicts the
/// viewport every frame and the pane can never be served — which is exactly how
/// folding a large capture used to blank the screen for minutes.
const FOLD_FEED_PER_PAGE: usize = 256;
/// Rows requested at a time while feeding. A batch is only folded when the
/// whole window was served, so the engine never sees a stream with a hole in
/// it and a run is never split by a partially cached page.
const FOLD_FEED_BATCH: usize = 64;
/// Rows before the visible window the feed starts at when it anchors there.
///
/// A run is only counted from the member the engine first saw, so anchoring
/// exactly at the window would report the visible part of a run rather than the
/// run. This is bounded lead-in, not a scan of everything before the window.
const FOLD_LEAD_IN: usize = 4_096;
/// How far beyond the fed frontier the window may sit before the feed abandons
/// its position and re-anchors at the window. Below this, feeding forward
/// reaches the window sooner than restarting would.
const FOLD_REANCHOR_GAP: usize = 32_768;

/// Per-view folding state.
///
/// The engine consumes the view's ordered stream from position 0 forward and
/// never re-reads it, so a fold entry is a function of the stream prefix and
/// the policy alone. Nothing about the viewport reaches it, which is what makes
/// a rendered count stable while scrolling.
struct FoldViewState {
    request: FoldRequest,
    engine: FoldEngine,
    /// Stream position the feed started at. Everything before it renders
    /// individually, exactly as an evicted entry does.
    ///
    /// Folding the whole stream from position 0 before showing anything makes a
    /// large capture unusable for as long as the walk takes. The feed instead
    /// starts just before the window the user is looking at, so the visible
    /// rows fold first, and continues forward from there.
    anchor: usize,
    /// Stream positions already consumed. Never below `anchor`.
    fed: usize,
    /// Whether a viewport has chosen this feed's anchor yet. A policy change
    /// creates the state before the terminal has asked for a range, so the
    /// first real viewport places it; probes never do.
    placed: bool,
    /// Publication the feed belongs to. A new membership replaces the stream,
    /// so the engine is reset rather than continued.
    generation: u64,
    /// Entries rendered expanded, named by their first member's identity.
    expanded: HashSet<RowId>,
    /// Advances whenever the folded projection changes shape, so the terminal
    /// redraws.
    revision: u64,
}

impl FoldViewState {
    fn new(request: &FoldRequest, generation: u64, anchor: usize) -> Self {
        Self {
            engine: FoldEngine::starting_at(
                fold_config(request),
                fold_key_column(request),
                anchor as u64,
            ),
            anchor,
            fed: anchor,
            placed: false,
            generation,
            expanded: request.expanded.iter().cloned().collect(),
            request: request.clone(),
            revision: 0,
        }
    }

    /// Whether this entry renders as one collapsed line rather than its members.
    fn collapsed(&self, entry: &crate::folding::FoldEntry) -> bool {
        entry.folded && !self.expanded.contains(&entry.first().id)
    }
}

fn fold_config(request: &FoldRequest) -> FoldConfig {
    FoldConfig {
        enabled: request.enabled,
        minimum_run: request.minimum_run.max(2),
        scope: match request.scope {
            FoldScopeRequest::Adjacent => FoldScope::Adjacent,
            FoldScopeRequest::Lookback(window) => FoldScope::Lookback(window),
        },
        // Only the derived `pattern` column is normalised; a column key is used
        // as-is, so this setting is inert for one.
        aggressiveness: match request.normalisation {
            FoldNormalisation::Conservative => Normalisation::Conservative,
            FoldNormalisation::Standard => Normalisation::Standard,
            FoldNormalisation::Aggressive => Normalisation::Aggressive,
        },
        ..FoldConfig::default()
    }
}

/// Which column supplies the key. `None` is the derived `pattern` column.
fn fold_key_column(request: &FoldRequest) -> FoldKey {
    FoldKey::from_column(request.key_column.as_deref())
}

/// What a collapsed run reports about itself.
#[derive(Clone, Debug)]
struct FoldFacts {
    count: usize,
    first: RowId,
    last: RowId,
    first_time: Option<i64>,
    last_time: Option<i64>,
    last_sample: String,
    /// The normalised shape every member shares, carrying the `<ts>`, `<num>`,
    /// `<uuid>` and `<ip>` placeholders that stand for the parts they differ in.
    ///
    /// `None` when the fold is keyed on a real column. The key is then one
    /// field's value — `shipper` — which says what the run is grouped by but
    /// not what its events say, so the entry keeps showing a member's text and
    /// the key is reported beside the count instead.
    pattern: Option<String>,
    /// The column the run is keyed on, when it is not the derived shape.
    key_column: Option<String>,
}

/// Where a member sits in the run it belongs to. An expanded run is otherwise
/// indistinguishable from ordinary rows, so scrolling through a long one gives
/// no sign that you are inside it at all.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FoldExtent {
    First,
    Middle,
    Last,
}

impl FoldExtent {
    fn at(offset: usize, count: usize) -> Self {
        match (offset, count) {
            (0, _) => FoldExtent::First,
            (offset, count) if offset + 1 == count => FoldExtent::Last,
            _ => FoldExtent::Middle,
        }
    }

    fn label(self) -> &'static str {
        match self {
            FoldExtent::First => "first",
            FoldExtent::Middle => "middle",
            FoldExtent::Last => "last",
        }
    }
}

/// One row of the folded display stream.
enum FoldSlot {
    /// An ordinary row at this stream position.
    Stream(usize),
    /// A collapsed run, resolved through its first member's identity.
    Folded(FoldFacts),
    /// One member of an expanded run, and where it sits in that run. The extent
    /// is `None` for an entry that is not a run at all — a lone event the engine
    /// retained — which is an ordinary row and must render as one.
    Member(RowId, Option<FoldExtent>, usize),
}

/// The folded stream, resolved for one requested display range only. Building
/// it is O(retained entries), which the engine caps.
struct FoldPlan {
    total: usize,
    slots: Vec<FoldSlot>,
    summary: FoldSummary,
}

struct Shared {
    accepting: bool,
    sources: HashMap<SourceId, SourceRegistration>,
    views: HashMap<String, ViewState>,
    /// Live union views by union view ID (Muse union-views worktree; the
    /// state type and all union methods live in `union_worker.rs`).
    union_views: HashMap<String, union_worker::UnionViewState>,
}

enum Work {
    Query(Box<QueryRequest>),
    Incremental(String),
    CompileUnionFilter {
        source: String,
        cancel: Arc<AtomicBool>,
        reply: mpsc::SyncSender<Result<lvu_query::CompiledDefinition, String>>,
    },
    Shutdown,
}

enum Update {
    Progress {
        view_id: String,
        revision: u64,
        scanned: u64,
        token: Arc<AtomicBool>,
    },
    Publish {
        request: QueryRequest,
        published: Published,
        scanned: u64,
        watermarks: Vec<(SourceId, Option<u64>)>,
        index_bytes: u64,
        diagnostic: Option<String>,
        token: Arc<AtomicBool>,
    },
    Failed {
        request: QueryRequest,
        purpose: QueryPurpose,
        message: String,
        limited: bool,
        token: Arc<AtomicBool>,
    },
    Correlation(Box<CorrelationLookup>),
    FieldStats(Box<FieldStats>),
}

/// How much journal one correlation lookup may read before it reports what it
/// could not reach. These are scan budgets, not storage limits.
const MAX_CORRELATION_ORIGIN_RECORDS: u64 = 500_000;
const MAX_CORRELATION_ORIGIN_BYTES: u64 = 128 * 1024 * 1024;
const MAX_CORRELATION_NAME_RECORDS: u64 = 2_048;
const MAX_CORRELATION_NAME_BYTES: u64 = 4 * 1024 * 1024;
/// Field names offered per source. More than this is a schema to browse, not a
/// choice to make in one dialog.
pub const MAX_CORRELATION_FIELD_NAMES: usize = 64;

/// Resolve one record's typed field value, and collect the field names each
/// source actually carries, so the user can map differing key names.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CorrelationLookupRequest {
    pub generation: u64,
    pub origin_view_id: String,
    /// The record the Fields dialog froze. Identity, never a displayed string.
    pub origin: lvu_core::RecordId,
    pub field: String,
    /// Every source the correlation may span, in the order it will be shown.
    pub sources: Vec<SourceId>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CorrelationSourceFields {
    pub source_id: SourceId,
    /// Observed structured field names, bounded and deduplicated.
    pub fields: Vec<String>,
    /// The bounded name scan stopped before this source's journal ended, so
    /// `fields` is a sample rather than the complete set.
    pub incomplete: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CorrelationCandidate {
    pub field: String,
    pub value: lvu_core::ExactScalar,
    pub sources: Vec<CorrelationSourceFields>,
}

/// The app's verdict about a field's type, handed to the engine so it can count.
///
/// The app names — it decides what a value *is* from the record's own bytes —
/// and this says which cast and which predicate express that verdict in Polars.
/// Nothing here classifies anything; if it did, the whole-view figures and the
/// sampled ones could disagree about the same field in the same dialog.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StatsType {
    Bool,
    Integer,
    Float,
    Timestamp,
    Text,
}

impl StatsType {
    /// How values of this type are ordered, so `min`/`max` mean what the app
    /// means. A number spelled as text compares lexically otherwise, and
    /// "1000" < "9" is the wrong answer.
    fn cast(self) -> DataType {
        match self {
            Self::Bool => DataType::Boolean,
            Self::Integer => DataType::Int64,
            Self::Float => DataType::Float64,
            // Timestamps are compared as the text the record spelled: they are
            // already ISO-8601, which sorts correctly as text, and parsing them
            // here would be this module deciding what a timestamp is.
            Self::Timestamp | Self::Text => DataType::String,
        }
    }

    /// The predicate whose count is "how many present values are of this type".
    fn predicate(self, column: &str) -> Expr {
        match self {
            Self::Text => col(column).is_not_null(),
            other => col(column).cast(other.cast()).is_not_null(),
        }
    }
}

/// One bounded, cancellable pass over a view's membership for one field.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FieldStatsRequest {
    pub generation: u64,
    pub view_id: String,
    /// The top-level column the field lives in.
    pub column: String,
    /// For a nested field, the JSON path addressing it inside that column
    /// (`$.status`). `None` for a top-level field, which is the column itself.
    pub json_path: Option<String>,
    /// What the pane calls this field, and what the figures are reported under.
    pub label: String,
    /// What the app has already decided this field is.
    pub kind: StatsType,
    pub top: usize,
    pub distinct_cap: usize,
}

#[derive(Clone, Debug)]
pub struct FieldStats {
    pub generation: u64,
    pub view_id: String,
    pub column: String,
    /// Records the pass actually read, which is what the figures rest on.
    pub scanned: u64,
    pub result: Result<lvu_query::column_stats::ColumnAggregate, String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CorrelationLookup {
    pub generation: u64,
    pub origin_view_id: String,
    pub result: Result<CorrelationCandidate, String>,
}

pub struct NativeViewAdapter {
    raw: Arc<dyn RawRowSource>,
    config: ViewConfig,
    shared: Arc<Mutex<Shared>>,
    work: Option<mpsc::SyncSender<Work>>,
    updates: mpsc::Receiver<Update>,
    update_tx: mpsc::SyncSender<Update>,
    completions: VecDeque<QueryCompletion>,
    correlation: Option<(u64, Arc<AtomicBool>, JoinHandle<()>)>,
    correlation_results: VecDeque<CorrelationLookup>,
    field_stats: Option<(u64, Arc<AtomicBool>, JoinHandle<()>)>,
    field_stats_results: VecDeque<FieldStats>,
    admitted: HashMap<String, u64>,
    worker: Option<JoinHandle<()>>,
    shutdown: Arc<AtomicBool>,
    budget: Arc<MemoryBudget>,
    snapshot_jobs: Arc<std::sync::atomic::AtomicUsize>,
    compiler_calls: Arc<AtomicU64>,
    refresh_stats: Arc<RefreshStats>,
}

/// Cloneable read-only half for terminal composition. Keep the adapter itself as
/// the mutable `QueryDispatcher` and pass this handle as the `RowProvider`.
#[derive(Clone)]
pub struct NativeViewRows {
    raw: Arc<dyn RawRowSource>,
    config: ViewConfig,
    shared: Arc<Mutex<Shared>>,
}

impl NativeViewAdapter {
    pub fn new(raw: Arc<LiveRowProvider>, config: ViewConfig) -> Result<Self, ViewError> {
        Self::with_raw_rows(raw, config)
    }

    /// Same adapter over any bounded raw-row seam. Tests use this to inject
    /// deterministic row-request delay, starvation and lookup failure.
    pub fn with_raw_rows(
        raw: Arc<dyn RawRowSource>,
        config: ViewConfig,
    ) -> Result<Self, ViewError> {
        if config.request_capacity == 0
            || config.update_capacity == 0
            || config.completion_capacity == 0
            || config.page_records == 0
            || config.page_bytes == 0
            || config.maximum_index_bytes < SOURCE_OVERHEAD
            || config.maximum_views == 0
            || config.maximum_sources_per_view == 0
            || config.maximum_viewport_rows == 0
            || config.maximum_snapshot_jobs == 0
        {
            return Err(ViewError::InvalidConfig);
        }
        let (work_tx, work_rx) = mpsc::sync_channel(config.request_capacity);
        let (update_tx, update_rx) = mpsc::sync_channel(config.update_capacity);
        let shutdown = Arc::new(AtomicBool::new(false));
        let budget = Arc::new(MemoryBudget {
            used: AtomicU64::new(0),
            maximum: config.maximum_index_bytes,
        });
        let shared = Arc::new(Mutex::new(Shared {
            accepting: true,
            sources: HashMap::new(),
            views: HashMap::new(),
            union_views: HashMap::new(),
        }));
        let correlation_tx = update_tx.clone();
        let worker_shared = Arc::clone(&shared);
        let worker_config = config.clone();
        let worker_budget = Arc::clone(&budget);
        let compiler_calls = Arc::new(AtomicU64::new(0));
        let worker_compiler_calls = Arc::clone(&compiler_calls);
        let refresh_stats = Arc::new(RefreshStats::default());
        let worker_refresh_stats = Arc::clone(&refresh_stats);
        let worker = thread::Builder::new()
            .name("lvu-view-query".into())
            .spawn(move || {
                worker_loop(
                    worker_config,
                    worker_shared,
                    work_rx,
                    update_tx,
                    worker_budget,
                    worker_compiler_calls,
                    worker_refresh_stats,
                )
            })?;
        Ok(Self {
            raw,
            config,
            shared,
            work: Some(work_tx),
            updates: update_rx,
            update_tx: correlation_tx,
            completions: VecDeque::new(),
            correlation: None,
            correlation_results: VecDeque::new(),
            field_stats: None,
            field_stats_results: VecDeque::new(),
            admitted: HashMap::new(),
            worker: Some(worker),
            shutdown,
            budget,
            snapshot_jobs: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            compiler_calls,
            refresh_stats,
        })
    }

    /// Number of Python definition compilations requested by this adapter.
    pub fn compiler_calls(&self) -> u64 {
        self.compiler_calls.load(Ordering::Acquire)
    }

    /// What incremental refreshes have cost: completed refreshes and the
    /// nanoseconds spent in them.
    pub fn refresh_stats(&self) -> (u64, u64) {
        self.refresh_stats.snapshot()
    }

    pub fn register_source(&self, handle: AnySourceHandle) -> Result<(), ViewError> {
        {
            let shared = self.shared.lock().expect("view state poisoned");
            if !shared.accepting {
                return Err(ViewError::Closed);
            }
        }
        self.raw
            .register_source(handle.clone())
            .map_err(ViewError::Live)?;
        let mut shared = self.shared.lock().expect("view state poisoned");
        let source_id = handle.source_id();
        shared
            .sources
            .insert(source_id, SourceRegistration { handle });
        for view in shared.views.values_mut().filter(|view| {
            view.registration.sources.contains(&source_id)
                || view
                    .pending_sources
                    .as_ref()
                    .is_some_and(|sources| sources.contains(&source_id))
        }) {
            view.cancel.store(true, Ordering::Release);
            view.cancel = Arc::new(AtomicBool::new(false));
            view.refreshing = false;
            view.resubmit_pending = view.pending_request.is_some();
            if let Some((_, high)) = view
                .status
                .high_watermarks
                .iter_mut()
                .find(|(id, _)| *id == source_id)
            {
                *high = None;
            }
            view.provider_revision = view.provider_revision.saturating_add(1);
        }
        Ok(())
    }

    /// Releases composition ownership when registering the source's first view
    /// fails. The caller remains responsible for stopping the capture handle.
    pub fn rollback_source_registration(&self, source_id: SourceId, view_id: &str) {
        let mut shared = self.shared.lock().expect("view state poisoned");
        shared.views.remove(view_id);
        if shared.views.values().all(|view| {
            !view.registration.sources.contains(&source_id)
                && !view
                    .pending_sources
                    .as_ref()
                    .is_some_and(|sources| sources.contains(&source_id))
        }) {
            shared.sources.remove(&source_id);
        }
    }

    /// Removes a view registration, cancelling any query in flight for it.
    ///
    /// Sources are left registered: a source always keeps its canonical view,
    /// so dropping one derived view can never orphan the capture behind it.
    pub fn unregister_view(&self, view_id: &str) {
        let mut shared = self.shared.lock().expect("view state poisoned");
        if let Some(view) = shared.views.remove(view_id) {
            view.cancel.store(true, Ordering::Release);
        }
    }

    pub fn register_view(
        &self,
        view_id: impl Into<String>,
        sources: Vec<SourceId>,
    ) -> Result<(), ViewError> {
        if sources.len() > self.config.maximum_sources_per_view {
            return Err(ViewError::SourceLimit);
        }
        let view_id = view_id.into();
        let mut deduped = Vec::new();
        for source in sources {
            if !deduped.contains(&source) {
                deduped.push(source);
            }
        }
        let raw_view = format!("_lvu_native_raw_{view_id}");
        {
            let mut shared = self.shared.lock().expect("view state poisoned");
            if !shared.accepting {
                return Err(ViewError::Closed);
            }
            if deduped.iter().any(|id| !shared.sources.contains_key(id)) {
                return Err(ViewError::UnknownSource);
            }
            if !shared.views.contains_key(&view_id)
                && shared.views.len() >= self.config.maximum_views
            {
                return Err(ViewError::ViewLimit);
            }
            let revision = shared
                .views
                .get(&view_id)
                .map_or(1, |v| v.provider_revision.saturating_add(1));
            if let Some(previous) = shared.views.get(&view_id) {
                previous.cancel.store(true, Ordering::Release);
            }
            shared.views.insert(
                view_id.clone(),
                ViewState {
                    registration: ViewRegistration {
                        sources: deduped.clone(),
                        raw_view: raw_view.clone(),
                    },
                    pending_sources: None,
                    pending_request: None,
                    resubmit_pending: false,
                    desired_revision: 0,
                    cancel: Arc::new(AtomicBool::new(false)),
                    published: Published::Raw,
                    provider_revision: revision,
                    status: ViewQueryStatus {
                        view_id,
                        revision: 0,
                        state: ScanState::Raw,
                        scanned_records: 0,
                        high_watermarks: Vec::new(),
                        matched_records: 0,
                        index_bytes: 0,
                        diagnostic: None,
                    },
                    last_request: None,
                    applied_revision: 0,
                    applied_generation: 0,
                    applied_constraints: lvu::QueryConstraints::default(),
                    refreshing: false,
                    rows_requested: 0,
                    rows_missing: 0,
                    rows_served: 0,
                    rows_raw_revision: 0,
                    rows_retry: 0,
                    rows_retry_revision: 0,
                    fold: None,
                    command_results: BTreeMap::new(),
                    retained: None,
                },
            );
        }
        self.raw
            .register_raw_view(&raw_view, deduped)
            .map_err(ViewError::Live)
    }

    pub fn status(&self, view_id: &str) -> Option<ViewQueryStatus> {
        self.shared
            .lock()
            .expect("view state poisoned")
            .views
            .get(view_id)
            .map(|v| v.status.clone())
    }

    pub fn rows(&self) -> NativeViewRows {
        NativeViewRows {
            raw: Arc::clone(&self.raw),
            config: self.config.clone(),
            shared: Arc::clone(&self.shared),
        }
    }

    pub fn raw_stats(&self) -> lvu_live::AdapterStats {
        self.raw.stats()
    }

    pub fn membership_bytes_used(&self) -> u64 {
        self.budget.used.load(Ordering::Acquire)
    }

    /// Applies bounded worker updates and schedules at most one incremental scan
    /// per view. It never waits for journal I/O, Python, or Polars work.
    pub fn drain_updates(&mut self, maximum: usize) -> usize {
        self.raw.drain_ready_updates(maximum);
        // Union freeze traffic shares the tick under its own per-call bound
        // (Muse union-views worktree); freezing is lock-plus-clones work.
        self.drive_unions();
        let mut count = 0;
        while count < maximum {
            let Ok(update) = self.updates.try_recv() else {
                break;
            };
            count += 1;
            self.apply_update(update);
        }
        if let Some(tx) = &self.work {
            {
                let mut shared = self.shared.lock().expect("view state poisoned");
                for view in shared
                    .views
                    .values_mut()
                    .filter(|view| view.resubmit_pending)
                {
                    if let Some(request) = &view.pending_request
                        && tx.try_send(Work::Query(Box::new(request.clone()))).is_err()
                    {
                        break;
                    }
                    view.resubmit_pending = false;
                }
            }
            let views: Vec<String> = {
                let shared = self.shared.lock().expect("view state poisoned");
                shared
                    .views
                    .iter()
                    .filter(|(_, v)| {
                        matches!(v.status.state, ScanState::Ready)
                            && !v.refreshing
                            && v.registration.sources.iter().any(|source_id| {
                                let current = self
                                    .raw
                                    .source_status(*source_id)
                                    .and_then(|s| s.high_watermark);
                                let scanned = v
                                    .status
                                    .high_watermarks
                                    .iter()
                                    .find(|(id, _)| id == source_id)
                                    .and_then(|(_, high)| *high);
                                current > scanned
                            })
                    })
                    .map(|(id, _)| id.clone())
                    .collect()
            };
            for view in views {
                let mut shared = self.shared.lock().expect("view state poisoned");
                if let Some(state) = shared.views.get_mut(&view) {
                    state.refreshing = true;
                    if tx.try_send(Work::Incremental(view)).is_err() {
                        state.refreshing = false;
                    }
                }
            }
        }
        count
    }

    fn apply_update(&mut self, update: Update) {
        if let Update::FieldStats(stats) = update {
            // A pass whose generation is no longer the live one is answering
            // about a field the user has moved off, so its result is dropped
            // rather than shown beside a different selection.
            if self
                .field_stats
                .as_ref()
                .is_some_and(|(generation, cancel, _)| {
                    *generation == stats.generation && !cancel.load(Ordering::Acquire)
                })
            {
                self.field_stats = None;
                self.field_stats_results.push_back(*stats);
            }
            return;
        }
        if let Update::Correlation(lookup) = update {
            // A cancelled lookup's generation is no longer the live one, so its
            // late result is dropped rather than shown for the current field.
            if self
                .correlation
                .as_ref()
                .is_some_and(|(generation, cancel, _)| {
                    *generation == lookup.generation && !cancel.load(Ordering::Acquire)
                })
            {
                self.correlation = None;
                self.correlation_results.push_back(*lookup);
            }
            return;
        }
        let mut shared = self.shared.lock().expect("view state poisoned");
        if let Update::Publish { request, token, .. } = &update
            && !token.load(Ordering::Acquire)
            && let Some(view) = shared.views.get_mut(&request.view_id)
            && view.desired_revision == request.revision
            && !view.cancel.load(Ordering::Acquire)
            && let Some(sources) = view.pending_sources.clone()
        {
            if let Err(error) = self
                .raw
                .register_raw_view(&view.registration.raw_view, sources.clone())
            {
                let failed = Update::Failed {
                    request: request.clone(),
                    purpose: request.purpose,
                    message: format!("source membership publication: {error}"),
                    limited: false,
                    token: Arc::clone(token),
                };
                drop(shared);
                self.apply_update(failed);
                return;
            }
            view.registration.sources = sources;
            view.pending_sources = None;
        }
        let completion = match update {
            // Routed before this point; the borrow of `shared` never sees them.
            Update::Correlation(_) | Update::FieldStats(_) => return,
            Update::Progress {
                view_id,
                revision,
                scanned,
                token,
            } => {
                if token.load(Ordering::Acquire) {
                    return;
                }
                if let Some(view) = shared.views.get_mut(&view_id)
                    && view.desired_revision == revision
                {
                    view.status.scanned_records = scanned;
                }
                None
            }
            Update::Publish {
                request,
                published,
                scanned,
                watermarks,
                index_bytes,
                diagnostic,
                token,
            } => {
                if token.load(Ordering::Acquire) {
                    return;
                }
                let Some(view) = shared.views.get_mut(&request.view_id) else {
                    return;
                };
                if view.desired_revision != request.revision || view.cancel.load(Ordering::Acquire)
                {
                    return;
                }
                let matched = match &published {
                    Published::Raw => 0,
                    Published::Filtered { membership } => membership.count,
                };
                let complete_request = matches!(view.status.state, ScanState::Pending);
                view.pending_request = None;
                view.resubmit_pending = false;
                view.published = published;
                view.applied_revision = request.revision;
                view.applied_generation = request.generation;
                view.applied_constraints = request.constraints.clone();
                view.last_request = Some(request.clone());
                view.refreshing = false;
                view.provider_revision = view.provider_revision.saturating_add(1);
                view.status = ViewQueryStatus {
                    view_id: request.view_id.clone(),
                    revision: request.revision,
                    state: if matches!(view.published, Published::Raw) {
                        ScanState::Raw
                    } else {
                        ScanState::Ready
                    },
                    scanned_records: scanned,
                    high_watermarks: watermarks,
                    matched_records: matched,
                    index_bytes,
                    diagnostic,
                };
                complete_request.then_some(QueryCompletion {
                    view_id: request.view_id,
                    generation: request.generation,
                    revision: request.revision,
                    purpose: request.purpose,
                    result: Ok(()),
                })
            }
            Update::Failed {
                request,
                purpose,
                message,
                limited,
                token,
            } => {
                if token.load(Ordering::Acquire) {
                    return;
                }
                let Some(view) = shared.views.get_mut(&request.view_id) else {
                    return;
                };
                if view.desired_revision != request.revision {
                    return;
                }
                view.refreshing = false;
                view.pending_sources = None;
                view.pending_request = None;
                view.resubmit_pending = false;
                view.desired_revision = view.applied_revision;
                view.cancel = Arc::new(AtomicBool::new(false));
                view.status.state = if limited {
                    ScanState::Limited
                } else if matches!(view.published, Published::Raw) {
                    ScanState::Raw
                } else {
                    ScanState::Ready
                };
                view.status.diagnostic = Some(message.clone());
                Some(QueryCompletion {
                    view_id: request.view_id,
                    generation: request.generation,
                    revision: request.revision,
                    purpose: request.purpose,
                    result: Err(QueryFailure { purpose, message }),
                })
            }
        };
        drop(shared);
        if let Some(completion) = completion {
            if self.admitted.get(&completion.view_id) == Some(&completion.revision) {
                self.admitted.remove(&completion.view_id);
            }
            self.push_completion(completion);
        }
    }

    fn push_completion(&mut self, completion: QueryCompletion) {
        debug_assert!(self.completions.len() < self.config.completion_capacity);
        self.completions.push_back(completion);
    }

    /// Start one bounded, cancellable correlation lookup. A previous lookup is
    /// cancelled first: the Fields dialog only ever has one in flight, and a
    /// stale scan must not spend the disk budget of the live one.
    pub fn submit_correlation_lookup(
        &mut self,
        request: CorrelationLookupRequest,
    ) -> Result<(), ViewError> {
        let handles = {
            let shared = self.shared.lock().expect("view state poisoned");
            if !shared.accepting {
                return Err(ViewError::Closed);
            }
            let mut handles = Vec::new();
            for source_id in &request.sources {
                let Some(registration) = shared.sources.get(source_id) else {
                    return Err(ViewError::UnknownSource);
                };
                handles.push(registration.handle.clone());
            }
            match shared.sources.get(&request.origin.source_id) {
                Some(registration) => handles.push(registration.handle.clone()),
                None => return Err(ViewError::UnknownSource),
            }
            handles
        };
        self.cancel_correlation_lookup();
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel);
        let tx = self.update_tx.clone();
        let generation = request.generation;
        let handle = thread::Builder::new()
            .name("lvu-view-correlate".into())
            .spawn(move || correlation_lookup_loop(request, handles, tx, worker_cancel))?;
        self.correlation = Some((generation, cancel, handle));
        Ok(())
    }

    /// Start one bounded, cancellable pass over a view's membership for one
    /// field. A previous pass is superseded, which is what "the selection
    /// moved" means here: the answer to a question nobody is asking any more.
    ///
    /// The sample the dialog already shows stays on screen throughout. This
    /// never blocks it and never replaces it with nothing.
    pub fn submit_field_stats(&mut self, request: FieldStatsRequest) -> Result<(), ViewError> {
        let (membership, handles) = {
            let shared = self.shared.lock().expect("view state poisoned");
            let Some(view) = shared.views.get(&request.view_id) else {
                return Err(ViewError::UnknownView);
            };
            let membership = match &view.published {
                Published::Filtered { membership } => Some(Arc::clone(membership)),
                // A raw view has no membership to walk: every record is in it,
                // which the pass discovers by reading the journal to its end.
                Published::Raw => None,
            };
            let handles = view
                .registration
                .sources
                .iter()
                .filter_map(|id| shared.sources.get(id).map(|source| source.handle.clone()))
                .collect::<Vec<_>>();
            (membership, handles)
        };
        self.cancel_field_stats();
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel);
        let tx = self.update_tx.clone();
        let generation = request.generation;
        let page_records = self.config.page_records;
        let page_bytes = self.config.page_bytes;
        let handle = thread::Builder::new()
            .name("lvu-view-field-stats".into())
            .spawn(move || {
                field_stats_loop(
                    request,
                    membership,
                    handles,
                    page_records,
                    page_bytes,
                    tx,
                    worker_cancel,
                );
            })?;
        self.field_stats = Some((generation, cancel, handle));
        Ok(())
    }

    /// Cancel any pass in flight. Cancellation is between bounded pages.
    pub fn cancel_field_stats(&mut self) {
        if let Some((_, cancel, _)) = self.field_stats.take() {
            cancel.store(true, Ordering::Release);
        }
    }

    /// Drained by the composition tick alongside query completions.
    pub fn take_field_stats(&mut self) -> Vec<FieldStats> {
        self.field_stats_results.drain(..).collect()
    }

    /// Cancel any lookup in flight. Cancellation is between bounded pages, so
    /// this returns immediately and the thread settles on its own.
    pub fn cancel_correlation_lookup(&mut self) {
        if let Some((_, cancel, _)) = self.correlation.take() {
            cancel.store(true, Ordering::Release);
        }
    }

    /// Drained by the composition tick alongside query completions.
    pub fn take_correlation_lookups(&mut self) -> Vec<CorrelationLookup> {
        self.correlation_results.drain(..).collect()
    }

    pub fn shutdown(&mut self) {
        self.cancel_correlation_lookup();
        self.cancel_field_stats();
        {
            let mut shared = self.shared.lock().expect("view state poisoned");
            shared.accepting = false;
            for view in shared.views.values_mut() {
                view.cancel.store(true, Ordering::Release);
                view.status.state = ScanState::Shutdown;
            }
        }
        self.shutdown.store(true, Ordering::Release);
        if let Some(tx) = self.work.take() {
            let _ = tx.try_send(Work::Shutdown);
            drop(tx);
        }
        if let Some(worker) = self.worker.take() {
            while !worker.is_finished() {
                while self.updates.try_recv().is_ok() {}
                thread::sleep(Duration::from_millis(2));
            }
            let _ = worker.join();
        }
    }
}

impl NativeViewAdapter {
    /// Atomically change ordered source membership with a composite query.
    /// Existing rows, constraints and source registration remain applied until
    /// the candidate succeeds. This never starts or stops source acquisition.
    pub fn submit_source_change(
        &mut self,
        request: QueryRequest,
        sources: Vec<SourceId>,
    ) -> Result<(), String> {
        self.submit_query(request, Some(sources))
    }

    pub fn view_sources(&self, view_id: &str) -> Option<Vec<SourceId>> {
        self.shared
            .lock()
            .expect("view state poisoned")
            .views
            .get(view_id)
            .map(|view| view.registration.sources.clone())
    }

    fn submit_query(
        &mut self,
        request: QueryRequest,
        sources: Option<Vec<SourceId>>,
    ) -> Result<(), String> {
        if !self.admitted.contains_key(&request.view_id)
            && self.completions.len().saturating_add(self.admitted.len())
                >= self.config.completion_capacity
        {
            return Err("query completion capacity is full".into());
        }
        let tx = self.work.as_ref().ok_or("view adapter is shut down")?;
        {
            let mut shared = self.shared.lock().expect("view state poisoned");
            if !shared.accepting {
                return Err("view adapter is shut down".into());
            }
            if let Some(sources) = &sources {
                if sources.is_empty() || sources.len() > self.config.maximum_sources_per_view {
                    return Err("source membership must contain between one and the configured source limit".into());
                }
                let mut seen = HashSet::new();
                if sources.iter().any(|id| !seen.insert(*id)) {
                    return Err("source membership contains duplicate identities".into());
                }
                if sources.iter().any(|id| !shared.sources.contains_key(id)) {
                    return Err("source membership references an unavailable source".into());
                }
            }
            // Union views take union candidates through `submit_union_candidate`,
            // never ordinary queries: a source scan over the union's raw
            // sources would silently bypass the merged membership (Muse
            // union-views worktree).
            if shared.union_views.contains_key(&request.view_id) {
                return Err("union views take union candidates, not ordinary queries".into());
            }
            let view = shared
                .views
                .get_mut(&request.view_id)
                .ok_or("unknown view")?;
            if request.revision <= view.desired_revision {
                return Ok(());
            }
            // The staleness check asks whether the *definition* the caller
            // last saw is still the one applied. Colour rules are not part of
            // a definition — they paint rows, they do not select them — so a
            // repaint that has not been acknowledged yet must not make every
            // later filter look stale.
            if request.base_revision != view.applied_revision
                || !definitions_match(&request.base_constraints, &view.applied_constraints)
            {
                return Err("query base snapshot does not match the applied view".into());
            }
            let old_cancel = Arc::clone(&view.cancel);
            let old_desired = view.desired_revision;
            let old_status = view.status.clone();
            let old_sources = view.pending_sources.take();
            view.pending_sources = sources;
            view.cancel = Arc::new(AtomicBool::new(false));
            view.desired_revision = request.revision;
            view.status.state = ScanState::Pending;
            view.status.revision = request.revision;
            view.status.diagnostic = None;
            if tx.try_send(Work::Query(Box::new(request.clone()))).is_err() {
                view.cancel = old_cancel;
                view.desired_revision = old_desired;
                view.status = old_status;
                view.pending_sources = old_sources;
                return Err("query queue is full".into());
            }
            view.pending_request = Some(request.clone());
            view.resubmit_pending = false;
            old_cancel.store(true, Ordering::Release);
        }
        self.admitted
            .insert(request.view_id.clone(), request.revision);
        Ok(())
    }
}

impl QueryDispatcher for NativeViewAdapter {
    fn submit(&mut self, request: QueryRequest) -> Result<(), String> {
        self.submit_query(request, None)
    }

    fn poll(&mut self) -> Option<QueryCompletion> {
        self.completions.pop_front()
    }
}

impl RowProvider for NativeViewAdapter {
    fn enrichment_outputs(&self, view_id: &str) -> Vec<String> {
        self.rows().enrichment_outputs(view_id)
    }

    fn page(&self, view_id: &str, request: ViewportRequest) -> RowPage {
        self.rows().page(view_id, request)
    }

    fn row_by_id(&self, view_id: &str, id: &RowId) -> Option<DisplayRow> {
        self.rows().row_by_id(view_id, id)
    }

    fn index_of_id(&self, view_id: &str, id: &RowId) -> Option<usize> {
        self.rows().index_of_id(view_id, id)
    }

    fn context_page(
        &self,
        view_id: &str,
        anchor: &RowId,
        offset: isize,
        len: usize,
    ) -> lvu::ContextPage {
        self.rows().context_page(view_id, anchor, offset, len)
    }

    fn revision(&self, view_id: &str) -> u64 {
        self.rows().revision(view_id)
    }

    fn unfolded_page(&self, view_id: &str, request: ViewportRequest) -> RowPage {
        self.rows().unfolded_page(view_id, request)
    }

    fn set_fold(&self, view_id: &str, request: &FoldRequest) {
        self.rows().set_fold(view_id, request);
    }

    fn fold_summary(&self, view_id: &str) -> Option<FoldSummary> {
        self.rows().fold_summary(view_id)
    }

    fn fold_members(&self, view_id: &str, id: &RowId) -> Vec<RowId> {
        self.rows().fold_members(view_id, id)
    }

    fn time_bounds(&self, view_id: &str, basis: lvu::TimeBasis) -> Option<lvu::TimeBounds> {
        self.rows().time_bounds(view_id, basis)
    }

    fn find_gap(
        &self,
        view_id: &str,
        from: Option<&RowId>,
        direction: lvu::GapDirection,
        threshold_nanos: i64,
        basis: lvu::TimeBasis,
    ) -> Option<lvu::GapHit> {
        self.rows()
            .find_gap(view_id, from, direction, threshold_nanos, basis)
    }
}

impl NativeViewRows {
    /// The capture timestamp of one raw row, by position.
    fn raw_capture_at(&self, raw_view: &str, index: usize) -> Option<i64> {
        self.raw
            .page(
                raw_view,
                ViewportRequest {
                    start: index,
                    len: 1,
                },
            )
            .rows
            .first()
            .and_then(|row| row.captured_at_unix_nanos)
    }

    /// Bounded page scan for a gap in a raw view.
    ///
    /// Raw views hold no membership, so there is no retained timestamp vector
    /// to walk. Reading the stream in chunks and stopping at a fixed budget is
    /// the same bound every other provider call obeys; a caller that gets
    /// `None` has learned "no gap within the budget", which is what it reports.
    fn find_raw_gap(
        &self,
        raw_view: &str,
        from: Option<&RowId>,
        direction: lvu::GapDirection,
        threshold_nanos: i64,
    ) -> Option<lvu::GapHit> {
        if threshold_nanos <= 0 {
            return None;
        }
        let total = self
            .raw
            .page(raw_view, ViewportRequest { start: 0, len: 0 })
            .total;
        if total < 2 {
            return None;
        }
        let start = from
            .and_then(|row| self.raw.index_of_id(raw_view, row))
            .unwrap_or(match direction {
                lvu::GapDirection::Forward => 0,
                // One past the last record, so the final gap is reachable.
                lvu::GapDirection::Backward => total,
            });
        // The window of positions the scan may look at. Each comparison needs
        // the row before it, so a forward search includes the anchor itself as
        // the first "previous" and a backward search ends at the anchor.
        let (low, high) = match direction {
            lvu::GapDirection::Forward => {
                (start, start.saturating_add(MAX_GAP_SCAN_ROWS).min(total))
            }
            lvu::GapDirection::Backward => {
                (start.saturating_sub(MAX_GAP_SCAN_ROWS), start.min(total))
            }
        };
        if high.saturating_sub(low) < 2 {
            return None;
        }
        let mut window: Vec<(RowId, i64)> = Vec::new();
        let mut cursor = low;
        while cursor < high {
            let len = GAP_SCAN_CHUNK_ROWS.min(high - cursor);
            let page = self
                .raw
                .page(raw_view, ViewportRequest { start: cursor, len });
            if page.rows.is_empty() {
                break;
            }
            let read = page.rows.len();
            window.extend(page.rows.into_iter().filter_map(|row| {
                row.captured_at_unix_nanos
                    .map(|timestamp| (row.id, timestamp))
            }));
            cursor += read;
        }
        if window.len() < 2 {
            return None;
        }
        let hit = |index: usize| {
            let (row, time) = &window[index];
            let (previous_row, previous) = &window[index - 1];
            lvu::GapHit {
                row: row.clone(),
                gap_nanos: time.saturating_sub(*previous),
                previous_unix_nanos: *previous,
                previous_row: previous_row.clone(),
            }
        };
        let exceeds =
            |index: usize| window[index].1.saturating_sub(window[index - 1].1) > threshold_nanos;
        match direction {
            lvu::GapDirection::Forward => (1..window.len()).find(|index| exceeds(*index)).map(hit),
            lvu::GapDirection::Backward => (1..window.len())
                .rev()
                .find(|index| exceeds(*index))
                .map(hit),
        }
    }

    /// The view's ordered stream before folding: `total` display rows and the
    /// requested window of them, with how many of that window were requested
    /// and how many could not be served. No bookkeeping happens here, so the
    /// fold feeder can call it without disturbing readiness accounting.
    fn collect_rows(&self, view: &ViewState, request: ViewportRequest) -> (RowPage, usize, usize) {
        let raw_view = view.registration.raw_view.clone();
        match &view.published {
            Published::Raw => {
                let page = self.raw.page(&raw_view, request);
                let requested = request.len.min(page.total.saturating_sub(request.start));
                let missing = requested.saturating_sub(page.rows.len());
                (page, requested, missing)
            }
            Published::Filtered { membership } => {
                let total = membership_display_count(membership);
                let len = request
                    .len
                    .min(self.config.maximum_viewport_rows)
                    .min(total.saturating_sub(request.start));
                let mut rows = Vec::with_capacity(len);
                let mut missing = 0usize;
                if membership.grouped {
                    // Grouped rows are projected inside membership; they never
                    // need a raw lookup and so are never partially available.
                    for group in membership_groups(membership, request.start, len) {
                        rows.push(project_group(group));
                    }
                } else {
                    let ids = membership_ids(membership, request.start, len);
                    let mut issued = 0usize;
                    let mut probed = 0usize;
                    for id in &ids {
                        // Each miss costs one slot in the raw provider's bounded
                        // request queue. Probing a whole tall viewport overflows
                        // it and loses every request, leaving a blank pane that
                        // nothing will ever refill.
                        if issued >= MAX_ROW_REQUESTS_PER_PAGE {
                            break;
                        }
                        probed += 1;
                        match self.raw.row_by_id(&raw_view, id) {
                            // A page is positional: rows after a hole cannot be
                            // placed. Keep probing anyway so the whole visible
                            // range is requested, not just its first missing row.
                            Some(row) if missing == 0 => {
                                rows.push(with_enrichment(row, membership));
                            }
                            Some(_) => {}
                            None => {
                                missing += 1;
                                issued += 1;
                            }
                        }
                    }
                    missing = missing.saturating_add(ids.len().saturating_sub(probed));
                }
                let requested = len;
                (RowPage { total, rows }, requested, missing)
            }
        }
    }

    /// Readiness accounting for the last non-empty range served.
    fn record_fetch(
        &self,
        shared: &mut Shared,
        view_id: &str,
        raw_view: &str,
        page: &RowPage,
        requested: usize,
        missing: usize,
    ) {
        // A zero-length probe (the terminal's total-only sync call) asks for no
        // rows and must not overwrite what the last real viewport observed.
        if requested > 0 {
            let raw_revision = self.raw.revision(raw_view);
            let served = page.rows.len();
            let view = shared.views.get_mut(view_id).expect("view exists");
            let progressed = raw_revision != view.rows_raw_revision || served != view.rows_served;
            if missing == 0 || progressed {
                view.rows_retry = 0;
            } else {
                view.rows_retry = view.rows_retry.saturating_add(1);
            }
            view.rows_raw_revision = raw_revision;
            view.rows_served = served;
            view.rows_requested = requested;
            view.rows_missing = missing;
            if missing > 0 && view.rows_retry <= MAX_ROW_FETCH_RETRIES {
                // Nothing else will wake the terminal when a row request was
                // dropped at the bounded queue, so advance our own revision and
                // let the next frame re-request. Bounded: once the budget is
                // spent the view reports `Stalled` instead of spinning.
                view.rows_retry_revision = view.rows_retry_revision.wrapping_add(1);
            }
        }
    }

    /// Consume more of the ordered stream into the fold engine, bounded per
    /// call. Feeding is strictly forward from the anchor, so nothing already
    /// folded is recomputed and a rendered count cannot change under scrolling.
    ///
    /// [`Self::prepare_fold`] has already chosen where this feed starts.
    fn advance_fold(&self, shared: &mut Shared, view_id: &str) {
        let Some(view) = shared.views.get(view_id) else {
            return;
        };
        let Some(fold) = &view.fold else {
            return;
        };
        if !fold.request.enabled {
            return;
        }
        let mut fed = shared.views[view_id]
            .fold
            .as_ref()
            .map_or(0, |fold| fold.fed);
        let mut budget = FOLD_FEED_PER_PAGE;
        while budget > 0 {
            let view = shared.views.get(view_id).expect("view exists");
            let remaining = self.stream_total(view).saturating_sub(fed);
            if remaining == 0 {
                break;
            }
            let take = budget.min(FOLD_FEED_BATCH).min(remaining);
            let (page, _, _) = self.collect_rows(
                view,
                ViewportRequest {
                    start: fed,
                    len: take,
                },
            );
            let Some(first) = page.rows.first() else {
                break;
            };
            // A page may be short because part of the window is not cached yet.
            // Feeding is only safe when what came back really starts at the next
            // unconsumed position: folding a stream with a hole in it would break
            // a run at an accidental boundary and report a count that is not the
            // run's length.
            if self.unfolded_index_of(view, &first.id) != Some(fed) {
                break;
            }
            let consumed = page.rows.len();
            let view = shared.views.get_mut(view_id).expect("view exists");
            let Some(fold) = &mut view.fold else {
                return;
            };
            fold.engine.extend_rows(page.rows.iter());
            fed += consumed;
            fold.fed = fed;
            fold.revision = fold.revision.wrapping_add(1);
            budget -= consumed;
        }
    }

    /// Keep the served viewport usable across a frame that could not build one.
    ///
    /// A page with rows in it becomes this view's retained page. A page with
    /// none, over a stream that has rows, is answered from the retained one
    /// instead of blanking the pane. Only requests of the same shape are
    /// answered this way, so a probe never receives a viewport, and only within
    /// one publication, so rows the current membership never matched are never
    /// shown. Bounded: one viewport of rows per view.
    ///
    /// [`RowReadiness`] is recorded from the fresh attempt before this runs, so
    /// substituting cannot make a stalled view look ready.
    fn retain_or_restore(
        &self,
        shared: &mut Shared,
        view_id: &str,
        request: ViewportRequest,
        page: RowPage,
    ) -> RowPage {
        if request.len < MIN_RETAINED_PAGE_ROWS {
            return page;
        }
        let Some(view) = shared.views.get_mut(view_id) else {
            return page;
        };
        let generation = view.applied_revision;
        if !page.rows.is_empty() {
            view.retained = Some(RetainedPage {
                generation,
                request,
                rows: page.rows.clone(),
            });
            return page;
        }
        if page.total == 0 {
            // A genuinely empty stream is an answer, not a gap.
            view.retained = None;
            return page;
        }
        // Whether rows are still on their way is a readiness question, and
        // `readiness` answers it independently: a view past its retry budget
        // reports `Stalled` either way. It is not a reason to draw nothing.
        // Rows the user could read a moment ago, under a status line saying
        // they are stale, beat an empty pane under the same status line — and
        // gating this on the retry budget meant a burst of cache churn during
        // capture emptied the pane for as long as the burst lasted.
        match &view.retained {
            Some(retained)
                if retained.generation == generation && retained.request.len == request.len =>
            {
                RowPage {
                    total: page.total,
                    rows: retained.rows.clone(),
                }
            }
            _ => page,
        }
    }

    /// Settle where this view's fold feed is pointed, before anything is
    /// collected from it. A new publication restarts the feed, and so does a
    /// window the current feed can no longer reach usefully.
    ///
    /// Restarting is the cancellation this recompute needs: the abandoned work
    /// is dropped whole, nothing partial from it survives into the new feed, and
    /// because folding is presentation the discarded entries cost only the walk
    /// that produced them. Records, identities and membership never move.
    fn prepare_fold(&self, shared: &mut Shared, view_id: &str, window: ViewportRequest) {
        let Some(view) = shared.views.get(view_id) else {
            return;
        };
        let Some(fold) = view.fold.as_ref().filter(|fold| fold.request.enabled) else {
            return;
        };
        // A new publication replaces the stream, so the engine restarts rather
        // than continuing over rows that are no longer in this view.
        let generation = view.applied_revision;
        if fold.generation != generation {
            let request = fold.request.clone();
            let view = shared.views.get_mut(view_id).expect("view exists");
            view.fold = Some(FoldViewState::new(&request, generation, 0));
        }
        // A zero-length or one-row call is a probe — the terminal syncing a
        // total or canonicalizing its selection — not a viewport. Anchoring to
        // one would restart the feed somewhere the user is not looking.
        if window.len < MIN_RETAINED_PAGE_ROWS {
            return;
        }
        let view = shared.views.get(view_id).expect("view exists");
        let Some(fold) = view.fold.as_ref() else {
            return;
        };
        // The window arrives in display positions, which the fold itself
        // defines; ask the current projection where in the stream it lands.
        let start = fold_stream_index(view, window.start);
        let desired = start.saturating_sub(FOLD_LEAD_IN);
        // A feed that has never been placed takes this window: folding the rows
        // the user is looking at is the whole point, and starting at position 0
        // over a large capture reaches them last.
        let unplaced = !fold.placed;
        // The window sits entirely before the first folded entry, so feeding
        // forward will never reach it.
        //
        // "Entirely" matters. Collapsing a run shortens the stream, which moves
        // a window that follows the tail earlier every time the feed advances.
        // Treating that as the user scrolling back would drag the anchor
        // backwards behind its own progress and restart the fold for ever.
        let first_entry = fold
            .engine
            .entries()
            .first()
            .map_or(fold.fed, |entry| entry.first().position as usize);
        let before_the_fold = window.start.saturating_add(window.len) <= first_entry;
        let behind = before_the_fold && desired < fold.anchor;
        // It has moved so far past the frontier that restarting there arrives
        // sooner than walking to it would.
        let far_ahead = start > fold.fed.saturating_add(FOLD_REANCHOR_GAP);
        if !unplaced && !behind && !far_ahead {
            return;
        }
        if desired == fold.anchor {
            // Placing a feed where it already starts would throw away everything
            // it has folded to arrive at the same position.
            let view = shared.views.get_mut(view_id).expect("view exists");
            if let Some(fold) = view.fold.as_mut() {
                fold.placed = true;
            }
            return;
        }
        let request = fold.request.clone();
        let generation = fold.generation;
        let revision = fold.revision.wrapping_add(1);
        let view = shared.views.get_mut(view_id).expect("view exists");
        let mut state = FoldViewState::new(&request, generation, desired);
        state.placed = true;
        state.revision = revision;
        view.fold = Some(state);
    }

    /// Serve one folded page. Collapsed entries resolve through their first
    /// member's identity; every other display row is an ordinary stream row, so
    /// selection, hitboxes, horizontal scrolling and text selection see exactly
    /// what they see when folding is off.
    fn collect_folded(&self, view: &ViewState, plan: &FoldPlan) -> (RowPage, usize, usize) {
        let raw_view = view.registration.raw_view.clone();
        let mut rows = Vec::with_capacity(plan.slots.len());
        let mut missing = 0usize;
        let mut issued = 0usize;
        let mut index = 0usize;
        while index < plan.slots.len() {
            match &plan.slots[index] {
                FoldSlot::Stream(start) => {
                    // Contiguous stream slots are served in one call so the raw
                    // provider's bounded request queue is used once, not once
                    // per row.
                    let mut run = 1usize;
                    while let Some(FoldSlot::Stream(next)) = plan.slots.get(index + run) {
                        if *next != start + run {
                            break;
                        }
                        run += 1;
                    }
                    let (page, requested, gap) = self.collect_rows(
                        view,
                        ViewportRequest {
                            start: *start,
                            len: run,
                        },
                    );
                    let served = page.rows.len();
                    if missing == 0 {
                        rows.extend(page.rows);
                    }
                    missing = missing.saturating_add(gap.max(requested.saturating_sub(served)));
                    index += run;
                }
                FoldSlot::Folded(facts) => {
                    if issued >= MAX_ROW_REQUESTS_PER_PAGE {
                        missing = missing.saturating_add(1);
                        index += 1;
                        continue;
                    }
                    match self.resolve_row(view, &raw_view, &facts.first) {
                        Some(row) if missing == 0 => rows.push(project_fold(row, facts)),
                        Some(_) => {}
                        None => {
                            missing += 1;
                            issued += 1;
                        }
                    }
                    index += 1;
                }
                FoldSlot::Member(id, extent, members) => {
                    if issued >= MAX_ROW_REQUESTS_PER_PAGE {
                        missing = missing.saturating_add(1);
                        index += 1;
                        continue;
                    }
                    match self.resolve_row(view, &raw_view, id) {
                        Some(row) if missing == 0 => {
                            rows.push(match extent {
                                Some(extent) => project_fold_member(row, *extent, *members),
                                None => row,
                            });
                        }
                        Some(_) => {}
                        None => {
                            missing += 1;
                            issued += 1;
                        }
                    }
                    index += 1;
                }
            }
        }
        let requested = plan.slots.len();
        (
            RowPage {
                total: plan.total,
                rows,
            },
            requested,
            missing,
        )
    }

    /// Position of `id` in the view's ordered stream before folding.
    fn unfolded_index_of(&self, view: &ViewState, id: &RowId) -> Option<usize> {
        match &view.published {
            Published::Raw => self.raw.index_of_id(&view.registration.raw_view, id),
            Published::Filtered { membership } if membership.grouped => {
                membership_group_index(membership, id)
            }
            Published::Filtered { membership } => membership_index(membership, id),
        }
    }

    /// Display rows the view's ordered stream holds before folding.
    fn stream_total(&self, view: &ViewState) -> usize {
        match &view.published {
            Published::Raw => {
                self.raw
                    .page(
                        &view.registration.raw_view,
                        ViewportRequest { start: 0, len: 0 },
                    )
                    .total
            }
            Published::Filtered { membership } => membership_display_count(membership),
        }
    }

    fn resolve_row(&self, view: &ViewState, raw_view: &str, id: &RowId) -> Option<DisplayRow> {
        match &view.published {
            Published::Raw => self.raw.row_by_id(raw_view, id),
            Published::Filtered { membership } => self
                .raw
                .row_by_id(raw_view, id)
                .map(|row| with_enrichment(row, membership)),
        }
    }
}

impl RowProvider for NativeViewRows {
    fn page(&self, view_id: &str, request: ViewportRequest) -> RowPage {
        let mut shared = self.shared.lock().expect("view state poisoned");
        self.prepare_fold(&mut shared, view_id, request);
        self.advance_fold(&mut shared, view_id);
        let Some(view) = shared.views.get(view_id) else {
            return RowPage {
                total: 0,
                rows: Vec::new(),
            };
        };
        let raw_view = view.registration.raw_view.clone();
        // A view that is not folding must cost exactly what it cost before:
        // measuring the stream is only worth a raw call when a plan needs it.
        let folding = view.fold.as_ref().is_some_and(|fold| fold.request.enabled);
        let plan = folding
            .then(|| fold_plan(view, self.stream_total(view), request))
            .flatten();
        let (page, requested, missing) = match plan {
            Some(plan) => self.collect_folded(view, &plan),
            None => self.collect_rows(view, request),
        };
        self.record_fetch(&mut shared, view_id, &raw_view, &page, requested, missing);
        self.retain_or_restore(&mut shared, view_id, request, page)
    }

    fn row_by_id(&self, view_id: &str, id: &RowId) -> Option<DisplayRow> {
        let shared = self.shared.lock().expect("view state poisoned");
        let view = shared.views.get(view_id)?;
        match &view.published {
            Published::Raw => self.raw.row_by_id(&view.registration.raw_view, id),
            Published::Filtered { membership } => {
                if membership.grouped {
                    let group = membership_group_for_id(membership, id)?;
                    Some(project_group(group))
                } else {
                    membership_index(membership, id)?;
                    self.raw
                        .row_by_id(&view.registration.raw_view, id)
                        .map(|row| with_enrichment(row, membership))
                }
            }
        }
    }

    fn index_of_id(&self, view_id: &str, id: &RowId) -> Option<usize> {
        let shared = self.shared.lock().expect("view state poisoned");
        let view = shared.views.get(view_id)?;
        let unfolded = self.unfolded_index_of(view, id);
        // Selection, scrolling and bookmark jumps index the displayed stream,
        // so a folded view answers with the display position of the line that
        // stands for the record. Every record still resolves.
        unfolded.map(|position| fold_display_index(view, position))
    }

    fn view_order(&self, view_id: &str) -> Option<lvu::provider::ViewOrder> {
        let shared = self.shared.lock().expect("view state poisoned");
        let view = shared.views.get(view_id)?;
        let Published::Filtered { membership } = &view.published else {
            // A raw view is one source in its own arrival order; there is
            // nothing a merge could have done to it.
            return None;
        };
        let contributing = membership
            .sources
            .iter()
            .filter(|source| !source.sequences.is_empty());
        Some(lvu::provider::ViewOrder {
            basis: membership.basis,
            sources: contributing.clone().count(),
            out_of_order: contributing.filter(|source| !source.ascending).count(),
            interleaved: membership.basis != lvu::TimeBasis::Capture,
        })
    }

    fn time_bounds(&self, view_id: &str, basis: lvu::TimeBasis) -> Option<lvu::TimeBounds> {
        let raw_view = {
            let shared = self.shared.lock().expect("view state poisoned");
            let view = shared.views.get(view_id)?;
            match &view.published {
                Published::Filtered { membership } if membership.basis == basis => {
                    let mut bounds = SourceTimeBounds::default();
                    for source in &membership.sources {
                        bounds.merge(&source.bounds);
                    }
                    return Some(lvu::TimeBounds {
                        first_unix_nanos: bounds.first?,
                        last_unix_nanos: bounds.last?,
                        count: bounds.count,
                        missing: bounds.missing,
                    });
                }
                // A raw view carries no constraints at all — that is what makes
                // it raw — so it has capture times and nothing else. Its rows
                // are in capture order, so its first and last row *are* its
                // bounds, and two one-row pages answer that without reading the
                // stream. Any other basis is a question it cannot answer.
                Published::Raw if basis == lvu::TimeBasis::Capture => {
                    view.registration.raw_view.clone()
                }
                _ => return None,
            }
        };
        let total = self
            .raw
            .page(&raw_view, ViewportRequest { start: 0, len: 0 })
            .total;
        if total == 0 {
            return None;
        }
        let first = self.raw_capture_at(&raw_view, 0)?;
        let last = self.raw_capture_at(&raw_view, total - 1)?;
        Some(lvu::TimeBounds {
            first_unix_nanos: first.min(last),
            last_unix_nanos: first.max(last),
            count: total,
            missing: 0,
        })
    }

    fn find_gap(
        &self,
        view_id: &str,
        from: Option<&RowId>,
        direction: lvu::GapDirection,
        threshold_nanos: i64,
        basis: lvu::TimeBasis,
    ) -> Option<lvu::GapHit> {
        let raw_view = {
            let shared = self.shared.lock().expect("view state poisoned");
            let view = shared.views.get(view_id)?;
            match &view.published {
                Published::Filtered { membership } if membership.basis == basis => {
                    return find_membership_gap(membership, from, direction, threshold_nanos);
                }
                Published::Raw if basis == lvu::TimeBasis::Capture => {
                    view.registration.raw_view.clone()
                }
                _ => return None,
            }
        };
        // A raw view keeps no membership to scan, so the search reads pages —
        // bounded to `MAX_GAP_SCAN_ROWS`, which is the same discipline as every
        // other provider call. Not finding a gap inside the budget is reported
        // as "not found", and the caller says so rather than implying there is
        // none.
        self.find_raw_gap(&raw_view, from, direction, threshold_nanos)
    }

    fn context_page(
        &self,
        view_id: &str,
        anchor: &RowId,
        offset: isize,
        len: usize,
    ) -> lvu::ContextPage {
        let shared = self.shared.lock().expect("view state poisoned");
        let Some(view) = shared.views.get(view_id) else {
            return lvu::ContextPage {
                anchor_position: None,
                start: 0,
                total: 0,
                rows: Vec::new(),
                pending: false,
                diagnostic: Some("context view is no longer registered".into()),
            };
        };
        self.raw
            .context_page(&view.registration.raw_view, anchor, offset, len)
    }

    fn revision(&self, view_id: &str) -> u64 {
        self.shared
            .lock()
            .expect("view state poisoned")
            .views
            .get(view_id)
            .map_or(0, |v| {
                v.provider_revision
                    .wrapping_add(self.raw.revision(&v.registration.raw_view))
                    .wrapping_add(v.rows_retry_revision)
                    .wrapping_add(v.fold.as_ref().map_or(0, |fold| fold.revision))
            })
    }

    fn enrichment_outputs(&self, view_id: &str) -> Vec<String> {
        // The accepted membership's declared output inventory, independent of
        // which rows (if any) are currently served: a pending page or a
        // settled zero-row filter must not hide accepted outputs from
        // callers binding new display state to them.
        let shared = self.shared.lock().expect("view state poisoned");
        let Some(view) = shared.views.get(view_id) else {
            return Vec::new();
        };
        match &view.published {
            Published::Raw => Vec::new(),
            Published::Filtered { membership } => membership.enrichment_names.clone(),
        }
    }

    fn unfolded_page(&self, view_id: &str, request: ViewportRequest) -> RowPage {
        let shared = self.shared.lock().expect("view state poisoned");
        let Some(view) = shared.views.get(view_id) else {
            return RowPage {
                total: 0,
                rows: Vec::new(),
            };
        };
        // Deliberately no fold projection and no readiness bookkeeping: this is
        // a sampling read, not the viewport.
        self.collect_rows(view, request).0
    }

    /// Apply a folding policy. Changing the policy or the expansion set
    /// restarts the engine, because both change what the stream projects to;
    /// the records themselves are never touched.
    fn set_fold(&self, view_id: &str, request: &FoldRequest) {
        let mut shared = self.shared.lock().expect("view state poisoned");
        let Some(view) = shared.views.get_mut(view_id) else {
            return;
        };
        let generation = view.applied_revision;
        match &mut view.fold {
            Some(fold) if fold.request == *request => {}
            Some(fold)
                if fold.request.enabled == request.enabled
                    && fold.request.minimum_run == request.minimum_run
                    && fold.request.key_column == request.key_column
                    && fold.request.scope == request.scope
                    && fold.request.normalisation == request.normalisation =>
            {
                // Only the expansion set moved: the fold itself is unchanged,
                // so keep the engine and re-project.
                fold.request = request.clone();
                fold.expanded = request.expanded.iter().cloned().collect();
                fold.revision = fold.revision.wrapping_add(1);
            }
            _ => {
                let mut state = FoldViewState::new(request, generation, 0);
                state.revision = view
                    .fold
                    .as_ref()
                    .map_or(0, |fold| fold.revision.wrapping_add(1));
                view.fold = Some(state);
            }
        }
    }

    fn fold_summary(&self, view_id: &str) -> Option<FoldSummary> {
        let shared = self.shared.lock().expect("view state poisoned");
        let view = shared.views.get(view_id)?;
        let fold = view.fold.as_ref()?;
        if !fold.request.enabled {
            return Some(FoldSummary::default());
        }
        fold_plan(
            view,
            self.stream_total(view),
            ViewportRequest { start: 0, len: 0 },
        )
        .map(|plan| plan.summary)
    }

    fn fold_members(&self, view_id: &str, id: &RowId) -> Vec<RowId> {
        let shared = self.shared.lock().expect("view state poisoned");
        let Some(fold) = shared
            .views
            .get(view_id)
            .and_then(|view| view.fold.as_ref())
        else {
            return vec![id.clone()];
        };
        fold.engine
            .entries()
            .iter()
            .find(|entry| entry.contains(id))
            .map_or_else(|| vec![id.clone()], |entry| entry.expand())
    }
}

/// Resolve the folded display stream for one requested range.
///
/// `None` means the view is not folding and the ordinary path serves the page
/// unchanged. A folded stream has three segments in order: rows the engine has
/// evicted, which render individually; the retained entries; and the tail the
/// engine has not consumed yet, which also renders individually. Only the
/// entries segment needs a scan, and the engine caps how many entries exist.
fn fold_plan(view: &ViewState, stream_total: usize, request: ViewportRequest) -> Option<FoldPlan> {
    let fold = view.fold.as_ref()?;
    if !fold.request.enabled {
        return None;
    }
    let entries = fold.engine.entries();
    let stats = fold.engine.stats();
    // Positions below the first retained entry were evicted; they stay in the
    // stream and render unfolded.
    let head = entries
        .first()
        .map_or(fold.fed, |entry| entry.first().position as usize);
    let consumed = entries
        .last()
        .map_or(head, |entry| entry.last().position as usize + 1);
    let tail_start = consumed.max(fold.fed);
    let pending = stream_total.saturating_sub(tail_start);

    let mut runs = 0usize;
    let mut folded_entries = 0usize;
    let mut hidden = 0usize;
    let mut entry_rows = 0usize;
    for entry in entries {
        if entry.folded {
            runs += 1;
        }
        if entry.folded && !fold.expanded.contains(&entry.first().id) {
            folded_entries += 1;
            hidden += entry.count().saturating_sub(1);
            entry_rows += 1;
        } else {
            entry_rows += entry.count();
        }
    }
    let total = head + entry_rows + pending;
    let summary = FoldSummary {
        enabled: true,
        entries: entries.len(),
        runs,
        folded_entries,
        hidden_rows: hidden,
        evicted_entries: stats.evicted_entries,
        pending_rows: pending,
    };

    let start = request.start.min(total);
    let end = start.saturating_add(request.len).min(total);
    let mut slots = Vec::with_capacity(end.saturating_sub(start));
    for position in start..head.min(end) {
        slots.push(FoldSlot::Stream(position));
    }
    let mut display = head;
    for entry in entries {
        if display >= end {
            break;
        }
        if entry.folded && !fold.expanded.contains(&entry.first().id) {
            if display >= start {
                let last = entry.last();
                slots.push(FoldSlot::Folded(FoldFacts {
                    count: entry.count(),
                    first: entry.first().id.clone(),
                    last: last.id.clone(),
                    first_time: entry.first().timestamp_unix_nanos,
                    last_time: last.timestamp_unix_nanos,
                    last_sample: entry.last_sample.clone(),
                    pattern: fold
                        .request
                        .key_column
                        .is_none()
                        .then(|| entry.pattern.to_string()),
                    key_column: fold.request.key_column.clone(),
                }));
            }
            display += 1;
        } else {
            let members = entry.count();
            // Only a real run has an extent to draw. A single retained event is
            // not a run, however folding got to it.
            let run = entry.folded && members > 1;
            for (offset, member) in entry.members.iter().enumerate() {
                if display >= end {
                    break;
                }
                if display >= start {
                    slots.push(FoldSlot::Member(
                        member.id.clone(),
                        run.then(|| FoldExtent::at(offset, members)),
                        members,
                    ));
                }
                display += 1;
            }
        }
    }
    let entries_end = head + entry_rows;
    // The head and entry segments only advance `display` over what they cover.
    // A window that starts beyond both — the tail of a stream whose fold began
    // near it — would otherwise be served every row from the entries onwards
    // rather than the range it asked for.
    display = display.max(start);
    while display < end {
        slots.push(FoldSlot::Stream(tail_start + (display - entries_end)));
        display += 1;
    }
    Some(FoldPlan {
        total,
        slots,
        summary,
    })
}

/// Map an unfolded stream position to the display position of the line that
/// stands for it. The identity map when the view is not folding.
fn fold_display_index(view: &ViewState, position: usize) -> usize {
    let Some(fold) = view.fold.as_ref().filter(|fold| fold.request.enabled) else {
        return position;
    };
    let entries = fold.engine.entries();
    let head = entries
        .first()
        .map_or(fold.fed, |entry| entry.first().position as usize);
    if position < head {
        return position;
    }
    let mut display = head;
    for entry in entries {
        let collapsed = entry.folded && !fold.expanded.contains(&entry.first().id);
        let first = entry.first().position as usize;
        let last = entry.last().position as usize;
        // Membership decides which entry stands for a position, not the entry's
        // span. Under `FoldScope::Adjacent` the two are the same thing — members
        // are contiguous, so the span test alone answers and the member scan
        // stops on the offset the old arithmetic computed. Under a lookback
        // window runs interleave, and the span of one entry covers positions
        // that belong to another; answering from the span there returns a
        // display position outside the stream.
        if position >= first
            && position <= last
            && let Some(offset) = entry
                .members
                .iter()
                .position(|member| member.position as usize == position)
        {
            return if collapsed { display } else { display + offset };
        }
        display += if collapsed { 1 } else { entry.count() };
    }
    let tail_start = entries
        .last()
        .map_or(head, |entry| entry.last().position as usize + 1)
        .max(fold.fed);
    display + position.saturating_sub(tail_start)
}

/// Map a display position back to the stream position of the row that stands at
/// it. The inverse of [`fold_display_index`], and the identity map when the view
/// is not folding.
///
/// A collapsed run occupies one display line for many stream positions, so this
/// answers with the run's first member: the record that line stands for.
fn fold_stream_index(view: &ViewState, display: usize) -> usize {
    let Some(fold) = view.fold.as_ref().filter(|fold| fold.request.enabled) else {
        return display;
    };
    let entries = fold.engine.entries();
    let head = entries
        .first()
        .map_or(fold.fed, |entry| entry.first().position as usize);
    if display < head {
        return display;
    }
    let mut cursor = head;
    for entry in entries {
        let collapsed = fold.collapsed(entry);
        let lines = if collapsed { 1 } else { entry.count() };
        if display < cursor + lines {
            // Membership, not the entry's span: under a lookback window runs
            // interleave, so the nth line of an expanded entry is its nth
            // member rather than its first member's position plus n. This is
            // the same rule `fold_display_index` answers with, inverted.
            let offset = if collapsed { 0 } else { display - cursor };
            return entry.members[offset].position as usize;
        }
        cursor += lines;
    }
    let tail_start = entries
        .last()
        .map_or(head, |entry| entry.last().position as usize + 1)
        .max(fold.fed);
    tail_start + (display - cursor)
}

/// Project a collapsed run onto its first member's row.
///
/// The row keeps its own identity, timestamp, level and fields; the display
/// text gains the count and the fold facts join the record's own details,
/// exactly as multiline grouping does. Nothing about the record changes, and
/// `row_by_id` still returns the unadorned record.
fn project_fold(mut head: DisplayRow, facts: &FoldFacts) -> DisplayRow {
    head.details.push((
        "folding".into(),
        "display-only; physical records unchanged".into(),
    ));
    head.details
        .push(("fold_count".into(), facts.count.to_string()));
    head.details
        .push(("fold_first".into(), facts.first.to_string()));
    head.details
        .push(("fold_last".into(), facts.last.to_string()));
    if let Some(nanos) = facts.first_time {
        head.details
            .push(("fold_first_unix_nanos".into(), nanos.to_string()));
    }
    if let Some(nanos) = facts.last_time {
        head.details
            .push(("fold_last_unix_nanos".into(), nanos.to_string()));
    }
    head.details
        .push(("fold_last_sample".into(), facts.last_sample.clone()));
    // The pane's cue that this row is a collapsed run rather than one more log
    // line it happens to sit next to.
    head.details.push(("fold_entry".into(), "collapsed".into()));
    // An entry stands for a run, so where the run has a shape of its own it
    // displays that shape rather than one member's text.
    if let Some(pattern) = &facts.pattern {
        head.details
            .push(("fold_pattern".into(), fold_pattern_text(pattern)));
    }
    if let Some(column) = &facts.key_column {
        head.details
            .push(("fold_key_column".into(), column.clone()));
    }
    // The span the run covers. The row's own time column is the first
    // occurrence, so only the last has nowhere else to be said.
    if let Some(nanos) = facts.last_time {
        head.details
            .push(("fold_last_time".into(), lvu_live::display_timestamp(nanos)));
    }
    head.text = format!("{}  [x{} repeated]", head.text, facts.count);
    head
}

/// The pattern as the pane shows it: without the level prefix the key carries,
/// which the row's own level column already says.
fn fold_pattern_text(pattern: &str) -> String {
    match pattern.split_once('|') {
        Some((_, rest)) if !rest.is_empty() => rest.to_owned(),
        _ => pattern.to_owned(),
    }
}

/// Mark one member of an expanded run with where it sits in that run.
///
/// Display-only, like every other fold projection: the record, its identity,
/// its timestamp, level and fields are exactly what `row_by_id` returns. The
/// pane reads these to draw the run's extent, so a long run is visibly a run
/// however far into it you have scrolled.
fn project_fold_member(mut row: DisplayRow, extent: FoldExtent, members: usize) -> DisplayRow {
    row.details
        .push(("fold_member".into(), extent.label().to_owned()));
    row.details.push(("fold_count".into(), members.to_string()));
    row
}

impl NativeViewRows {
    /// Typed reason the pane looks the way it does. Call it every time rows are
    /// rendered: whenever it is not [`RowReadiness::Ready`], the UI must show
    /// [`RowReadiness::describe`] rather than an ordinary empty result.
    ///
    /// It reads only state already published for this view, so a stale query or
    /// row reply that lost its generation fence cannot influence it.
    pub fn readiness(&self, view_id: &str) -> RowReadiness {
        let shared = self.shared.lock().expect("view state poisoned");
        let Some(view) = shared.views.get(view_id) else {
            return RowReadiness::QueryFailed {
                reason: "this view is no longer registered".into(),
            };
        };
        match view.status.state {
            ScanState::Error => {
                return RowReadiness::QueryFailed {
                    reason: bounded_text(
                        view.status
                            .diagnostic
                            .clone()
                            .unwrap_or_else(|| "the filter could not be evaluated".into()),
                        MAX_READINESS_REASON_BYTES,
                    ),
                };
            }
            ScanState::Shutdown => {
                return RowReadiness::QueryFailed {
                    reason: "the query worker has shut down".into(),
                };
            }
            _ => {}
        }

        let total = match &view.published {
            Published::Raw => {
                self.raw
                    .page(
                        &view.registration.raw_view,
                        ViewportRequest { start: 0, len: 0 },
                    )
                    .total
            }
            Published::Filtered { membership } => membership_display_count(membership),
        };
        let sources: Vec<SourceViewStatus> = view
            .registration
            .sources
            .iter()
            .filter_map(|id| self.raw.source_status(*id))
            .collect();
        let failure = sources
            .iter()
            .find_map(|status| status.last_error.clone())
            .map(|reason| bounded_text(reason, MAX_READINESS_REASON_BYTES));
        // Contention is not a failure and not progress: it is a wait that ends
        // by itself. It has to be read before `last_error`, because the worker
        // reports the conflict through the same field while it retries.
        let contended = sources
            .iter()
            .any(|status| status.index == IndexState::Contended);
        let budget_unverified = sources
            .iter()
            .any(|status| status.index == IndexState::BudgetUnverified);
        let indexing = sources
            .iter()
            .find(|status| {
                !matches!(
                    status.index,
                    IndexState::Ready
                        | IndexState::Limited
                        | IndexState::BudgetUnverified
                        | IndexState::Error
                ) || status.indexed_records < status.reported_records
            })
            .map(|status| RowReadiness::Indexing {
                indexed_records: status.indexed_records,
                reported_records: status.reported_records.max(status.indexed_records),
            });

        if view.rows_missing > 0 {
            let pending = view.rows_missing;
            let requested = view.rows_requested;
            // A read failure explains the gap better than "still loading", and
            // index progress explains it better than a bare retry count.
            if contended {
                return RowReadiness::IndexContended { pending };
            }
            if budget_unverified {
                return RowReadiness::IndexBudgetUnverified { pending };
            }
            if let Some(reason) = failure {
                return RowReadiness::LookupFailed { reason, pending };
            }
            if let Some(state) = indexing {
                return state;
            }
            if view.rows_retry > MAX_ROW_FETCH_RETRIES {
                return RowReadiness::Stalled { pending, requested };
            }
            return RowReadiness::RowsPending { pending, requested };
        }

        // A presentation-only recompute still walking the stream is not a gap in
        // the rows: what is folded is projected, the rest renders individually,
        // and the pane is usable throughout. It is reported only after the
        // row-delivery states above, so a real gap is never described as folding.
        if let Some(fold) = view.fold.as_ref().filter(|fold| fold.request.enabled)
            && fold.fed < total
        {
            return RowReadiness::Folding {
                folded_rows: fold.fed.saturating_sub(fold.anchor),
                total_rows: total,
            };
        }

        if total > 0 {
            return RowReadiness::Ready;
        }
        // Nothing is displayable. Only a settled query over a settled index may
        // be reported as a genuine zero-match result.
        if matches!(view.status.state, ScanState::Pending) {
            return RowReadiness::QueryPending {
                scanned: view.status.scanned_records,
            };
        }
        if contended {
            return RowReadiness::IndexContended { pending: 0 };
        }
        if budget_unverified {
            return RowReadiness::IndexBudgetUnverified { pending: 0 };
        }
        if let Some(reason) = failure {
            return RowReadiness::LookupFailed { reason, pending: 0 };
        }
        if let Some(state) = indexing {
            return state;
        }
        RowReadiness::NoMatches
    }
}

impl NativeViewAdapter {
    /// See [`NativeViewRows::readiness`].
    pub fn readiness(&self, view_id: &str) -> RowReadiness {
        self.rows().readiness(view_id)
    }

    /// Whether any source behind this view is running with an unaccountable
    /// shared index total.
    ///
    /// Separate from [`RowReadiness`] because it stays true while rows are
    /// being served perfectly well: the pane is right and the cache is not, and
    /// the second fact has nowhere else to be said.
    pub fn index_budget_unverified(&self, view_id: &str) -> bool {
        let rows = self.rows();
        let shared = rows.shared.lock().expect("view state poisoned");
        let Some(view) = shared.views.get(view_id) else {
            return false;
        };
        view.registration
            .sources
            .iter()
            .filter_map(|id| rows.raw.source_status(*id))
            .any(|status| status.index == IndexState::BudgetUnverified)
    }
}

/// The presentation-metadata key a matched colour rule travels under.
///
/// `details` is where the view already reports what it did to a row —
/// `grouping`, `group_line_*`, `derived.*` — so a rule match rides the same
/// channel rather than widening `DisplayRow` for one display concern. The value
/// is the rule's 1-based position, which is also what the dialog lists.
pub const COLOR_RULE_DETAIL: &str = "color_rule";

/// The presentation-metadata key a row's validated basis instant travels
/// under, as decimal UTC nanoseconds.
///
/// `SourceMatches.times` already holds the active basis's per-record instants
/// (`NO_BASIS_TIME` where unreadable), computed by the same typed machinery
/// that filters, orders and bounds the view. Projecting it here reuses that
/// result instead of parsing display text elsewhere. Capture basis carries no
/// such detail: capture time is already on the row.
pub const BASIS_NANOS_DETAIL: &str = "basis_nanos";

/// The presentation-metadata key marking a row's *ready* derived value for
/// one enrichment output: `derived_ready.{name}`.
///
/// Validity travels structurally, never as display-string sniffing. The view
/// writes this marker exactly when the accepted chain evaluated the output
/// for the batch serving the row *and* the cell is not a recorded stage
/// failure; a valid null, a failure, and a missing cell all carry no ready
/// marker. Display roles consume only ready values through this key. The
/// long-standing `derived.{name}` marker is still written for every declared
/// output (ready text, `"null"`, or `"error: ...") so Details and existing
/// diagnostics keep reading what they always read. The two new literals are
/// additive and ignored by grouping segmentation, which reads the `derived`
/// map rather than row details.
pub const DERIVED_READY_DETAIL_PREFIX: &str = "derived_ready.";

/// The presentation-metadata key marking a row's *failed* derived cell for
/// one enrichment output: `derived_error.{name}`, carrying the worker's
/// bounded error text. A ready display string may itself read `"error:
/// ..."`, so failures are signalled by this key's presence, never by parsing
/// the value. Rows carrying it never feed display roles.
pub const DERIVED_ERROR_DETAIL_PREFIX: &str = "derived_error.";

fn with_enrichment(mut row: DisplayRow, membership: &Membership) -> DisplayRow {
    if let Some(index) = membership
        .color_matches
        .get(&(row.id.source_id.clone(), row.id.sequence))
    {
        row.details.push((
            COLOR_RULE_DETAIL.into(),
            index.saturating_add(1).to_string(),
        ));
    }
    // Project the row's validated basis instant for display. The sequences
    // are only documented as a contiguous run where they are read positionally,
    // so the index is verified against the sequence before its time is used:
    // a wrong instant here would paint one record's time on another.
    if membership.basis != lvu::TimeBasis::Capture
        && let Some(source) = membership
            .sources
            .iter()
            .find(|source| source.source_id == row.id.source_id)
        && let Ok(index) = source.sequences.binary_search(&row.id.sequence)
        && source.sequences.get(index) == Some(&row.id.sequence)
        && let Some(nanos) = source.times.get(index).copied()
        && nanos != NO_BASIS_TIME
    {
        row.details
            .push((BASIS_NANOS_DETAIL.into(), nanos.to_string()));
    }
    for name in &membership.enrichment_names {
        let key = (row.id.source_id.clone(), row.id.sequence, name.clone());
        let failed = membership.derived_errors.contains(&key);
        let value = membership
            .derived
            .get(&key)
            .and_then(Clone::clone)
            .unwrap_or_else(|| "null".into());
        row.fields.retain(|(field, _)| field != name);
        row.fields.push((name.clone(), value.clone()));
        row.details.push((format!("derived.{name}"), value.clone()));
        // Structural validity: readiness is key presence, never value text.
        // A failure carries the error marker and no ready marker; a valid
        // null carries neither; only a successfully evaluated cell carries
        // the ready marker — even when its text reads `"null"` or starts
        // with `"error:"`, which display roles must not parse.
        if failed {
            row.details
                .push((format!("{DERIVED_ERROR_DETAIL_PREFIX}{name}"), value));
        } else if membership.derived.get(&key).is_some_and(Option::is_some) {
            row.details
                .push((format!("{DERIVED_READY_DETAIL_PREFIX}{name}"), value));
        }
    }
    row
}

impl NativeViewAdapter {
    /// Installs a command step's published results for `view_id` as the
    /// `<name>.<field>` columns later steps read (docs/command-enrichment.md).
    /// Nothing is re-evaluated here: the caller reaffirms the chain, so the
    /// join lands as one accepted query rather than a silent change under
    /// the applied view. Returns whether anything changed.
    pub fn set_command_results(
        &self,
        view_id: &str,
        stage_id: &str,
        name: &str,
        rows: HashMap<(String, u64), BTreeMap<String, serde_json::Value>>,
    ) -> bool {
        let mut shared = self.shared.lock().expect("view state poisoned");
        let Some(view) = shared.views.get_mut(view_id) else {
            return false;
        };
        let next = CommandColumns::new(name.to_owned(), rows);
        if view
            .command_results
            .get(stage_id)
            .is_some_and(|current| **current == next)
        {
            return false;
        }
        view.command_results
            .insert(stage_id.to_owned(), Arc::new(next));
        true
    }

    /// Forgets a command step's results (the step was removed, or its
    /// publication cleared). Returns whether anything changed.
    pub fn clear_command_results(&self, view_id: &str, stage_id: &str) -> bool {
        let mut shared = self.shared.lock().expect("view state poisoned");
        shared
            .views
            .get_mut(view_id)
            .is_some_and(|view| view.command_results.remove(stage_id).is_some())
    }
}

impl Drop for NativeViewAdapter {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn worker_loop(
    config: ViewConfig,
    shared: Arc<Mutex<Shared>>,
    rx: mpsc::Receiver<Work>,
    tx: mpsc::SyncSender<Update>,
    budget: Arc<MemoryBudget>,
    compiler_calls: Arc<AtomicU64>,
    refresh_stats: Arc<RefreshStats>,
) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("view runtime");
    let mut compiler = config.compiler.clone().map(CompilerHost::new);
    let mut prepared: HashMap<(String, u64), PreparedDefinition> = HashMap::new();
    while let Ok(work) = rx.recv() {
        {
            let state = shared.lock().expect("view state poisoned");
            prepared.retain(|(view_id, revision), _| {
                state.views.get(view_id).is_some_and(|view| {
                    *revision == view.applied_revision || *revision == view.desired_revision
                })
            });
        }
        match work {
            Work::Shutdown => break,
            Work::CompileUnionFilter {
                source,
                cancel,
                reply,
            } => {
                let result = match compiler.as_mut() {
                    Some(host) => {
                        compiler_calls.fetch_add(1, Ordering::Relaxed);
                        host.compile(&source, ExpressionKind::Filter, &cancel)
                            .map_err(|error| error.to_string())
                    }
                    None => Err("advanced expression compiler is not configured".into()),
                };
                let _ = reply.send(result);
            }
            Work::Incremental(view_id) => {
                let snapshot = {
                    let state = shared.lock().expect("view state poisoned");
                    let Some(view) = state.views.get(&view_id) else {
                        continue;
                    };
                    let Some(request) = view.last_request.clone() else {
                        continue;
                    };
                    if view.desired_revision != request.revision {
                        continue;
                    }
                    let sources = view
                        .registration
                        .sources
                        .iter()
                        .filter_map(|id| state.sources.get(id).map(|s| s.handle.clone()))
                        .collect::<Vec<_>>();
                    (
                        request,
                        sources,
                        Arc::clone(&view.cancel),
                        view.command_results.values().cloned().collect::<Vec<_>>(),
                    )
                };
                // Only the incremental arm is timed: it is the one whose cost
                // is supposed to follow what just arrived rather than what the
                // view already holds.
                let started = std::time::Instant::now();
                run_query(
                    &runtime,
                    &config,
                    &mut compiler,
                    snapshot.0,
                    snapshot.1,
                    snapshot.2,
                    snapshot.3,
                    &tx,
                    &mut prepared,
                    Arc::clone(&budget),
                    Arc::clone(&compiler_calls),
                );
                refresh_stats.record(started.elapsed());
            }
            Work::Query(request) => {
                let snapshot = {
                    let state = shared.lock().expect("view state poisoned");
                    let Some(view) = state.views.get(&request.view_id) else {
                        continue;
                    };
                    if view.desired_revision != request.revision {
                        continue;
                    }
                    let sources = view
                        .pending_sources
                        .as_ref()
                        .unwrap_or(&view.registration.sources)
                        .iter()
                        .filter_map(|id| state.sources.get(id).map(|s| s.handle.clone()))
                        .collect::<Vec<_>>();
                    (
                        sources,
                        Arc::clone(&view.cancel),
                        view.command_results.values().cloned().collect::<Vec<_>>(),
                    )
                };
                // Explicit candidates (including a restart resubmission) scan
                // a fresh snapshot. Only accepted incremental work resumes a
                // checkpoint; an unpublished candidate is never a durable base.
                prepared.remove(&(request.view_id.clone(), request.revision));
                run_query(
                    &runtime,
                    &config,
                    &mut compiler,
                    *request,
                    snapshot.0,
                    snapshot.1,
                    snapshot.2,
                    &tx,
                    &mut prepared,
                    Arc::clone(&budget),
                    Arc::clone(&compiler_calls),
                );
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn run_query(
    runtime: &tokio::runtime::Runtime,
    config: &ViewConfig,
    compiler: &mut Option<CompilerHost>,
    request: QueryRequest,
    sources: Vec<AnySourceHandle>,
    cancelled: Arc<AtomicBool>,
    command_results: Vec<Arc<CommandColumns>>,
    tx: &mpsc::SyncSender<Update>,
    prepared: &mut HashMap<(String, u64), PreparedDefinition>,
    budget: Arc<MemoryBudget>,
    compiler_calls: Arc<AtomicU64>,
) {
    let grouping_rule = match request.constraints.grouping.as_deref() {
        Some(source) => match prepared
            .values()
            .find(|value| value.constraints.grouping.as_deref() == Some(source))
            .and_then(|value| value.grouping.clone())
            .map_or_else(|| ContinuationRule::parse(source), Ok)
        {
            Ok(rule) => Some(rule),
            Err(message) => {
                fail(
                    tx,
                    &request,
                    &cancelled,
                    QueryPurpose::Grouping,
                    &message,
                    false,
                );
                return;
            }
        },
        None => None,
    };
    if request
        .constraints
        .capture_time
        .is_some_and(|window| window.start_unix_nanos >= window.end_unix_nanos)
    {
        fail(
            tx,
            &request,
            &cancelled,
            QueryPurpose::Advanced,
            "time window start must be before end; range is [start, end)",
            false,
        );
        return;
    }
    // A view with nothing to evaluate is published raw, straight from the
    // journal. Colour rules *are* something to evaluate — they are predicates
    // the engine runs — so a view that only has rules still takes the batch
    // path, and All events can be painted without being filtered.
    if request.constraints.text.is_none()
        && request.constraints.advanced_polars.is_none()
        && request.constraints.enrichments.is_empty()
        && request.constraints.capture_time.is_none()
        && request.constraints.grouping.is_none()
        && request.constraints.exact_field.is_none()
        && request.constraints.color_rules.is_empty()
    {
        prepared.retain(|(view_id, _), _| view_id != &request.view_id);
        let _ = send_update(
            tx,
            Update::Publish {
                request,
                published: Published::Raw,
                scanned: 0,
                watermarks: Vec::new(),
                index_bytes: 0,
                diagnostic: None,
                token: Arc::clone(&cancelled),
            },
            cancelled.as_ref(),
        );
        return;
    }
    let cache_key = (request.view_id.clone(), request.revision);
    let evaluation_provenance = (request.constraints.enrichments
        != request.base_constraints.enrichments)
        .then(|| {
            prepared
                .get(&(request.view_id.clone(), request.base_revision))
                .filter(|value| value.constraints == request.base_constraints)
                .and_then(|value| value.membership.clone())
        })
        .flatten();
    let cached = prepared
        .get(&cache_key)
        .filter(|value| {
            value.revision == request.revision && value.constraints == request.constraints
        })
        .cloned();
    let cached_refresh = cached.is_some();
    let advanced_changed =
        request.constraints.advanced_polars != request.base_constraints.advanced_polars;
    let reusable_definitions = cached
        .is_none()
        .then(|| {
            prepared
                .iter()
                .filter(|((view_id, _), value)| {
                    view_id == &request.view_id
                        && value.constraints.text == request.constraints.text
                        && value.constraints.advanced_polars == request.constraints.advanced_polars
                        && value.constraints.enrichments == request.constraints.enrichments
                })
                .max_by_key(|((_, revision), _)| *revision)
                .map(|(_, value)| value.clone())
        })
        .flatten();
    let (
        text,
        advanced,
        enrichment,
        affected_outputs,
        schema_seed,
        mut schema,
        mut checkpoints,
        prior_membership,
    ) = if let Some(cached) = cached {
        (
            cached.text,
            cached.advanced,
            cached.enrichment,
            HashSet::new(),
            cached.schema_seed,
            cached.schema,
            cached.checkpoints,
            cached.membership,
        )
    } else if let Some(reusable) = reusable_definitions {
        // A time-only revision must rescan membership so expired rows are
        // removed, but its immutable expression sources do not need Python
        // compilation again. Schema/checkpoints intentionally restart so a
        // changed journal generation or schema cannot inherit scan state.
        let schema_seed = SchemaContext::default();
        (
            reusable.text,
            reusable.advanced,
            reusable.enrichment,
            HashSet::new(),
            schema_seed.clone(),
            schema_seed,
            HashMap::new(),
            None,
        )
    } else {
        let text = match request.constraints.text.as_ref() {
            Some(value) if !value.case_insensitive => {
                fail(
                    tx,
                    &request,
                    &cancelled,
                    QueryPurpose::Search,
                    "case-sensitive literal search is not supported by the native TextSearch contract",
                    false,
                );
                return;
            }
            Some(value) => {
                let compiled = if TextSearch::is_polars(&value.literal) {
                    let Some(host) = compiler.as_mut() else {
                        fail(
                            tx,
                            &request,
                            &cancelled,
                            QueryPurpose::Search,
                            "search expression compiler is not configured",
                            false,
                        );
                        return;
                    };
                    compiler_calls.fetch_add(1, Ordering::AcqRel);
                    match host.compile(&value.literal, ExpressionKind::Filter, cancelled.as_ref()) {
                        Ok(compiled) => Some(compiled),
                        Err(error) => {
                            fail(
                                tx,
                                &request,
                                &cancelled,
                                QueryPurpose::Search,
                                &error.to_string(),
                                false,
                            );
                            return;
                        }
                    }
                } else {
                    None
                };
                match TextSearch::parse(value.literal.clone(), compiled.as_ref()) {
                    Ok(search) => Some(search),
                    Err(error) => {
                        fail(
                            tx,
                            &request,
                            &cancelled,
                            QueryPurpose::Search,
                            &error,
                            false,
                        );
                        return;
                    }
                }
            }
            None => None,
        };
        let advanced = match request.constraints.advanced_polars.as_deref() {
            Some(source) => {
                let Some(host) = compiler.as_mut() else {
                    fail(
                        tx,
                        &request,
                        &cancelled,
                        QueryPurpose::Advanced,
                        "advanced compiler is not configured",
                        false,
                    );
                    return;
                };
                compiler_calls.fetch_add(1, Ordering::AcqRel);
                match host.compile(source, ExpressionKind::Filter, cancelled.as_ref()) {
                    Ok(v) => Some(v),
                    Err(e) => {
                        fail(
                            tx,
                            &request,
                            &cancelled,
                            QueryPurpose::Advanced,
                            &e.to_string(),
                            false,
                        );
                        return;
                    }
                }
            }
            None => None,
        };
        let definitions = native_enrichment_definitions(&request.constraints);
        compiler_calls.fetch_add(
            definitions
                .iter()
                .filter(|definition| !definition.source.starts_with('/'))
                .count() as u64,
            Ordering::AcqRel,
        );
        let compiled_enrichment =
            match compile_enrichment_chain(&definitions, compiler.as_mut(), cancelled.as_ref()) {
                Ok(compiled) => compiled,
                Err(error) => {
                    fail(
                        tx,
                        &request,
                        &cancelled,
                        QueryPurpose::Enrichment,
                        &error.to_string(),
                        false,
                    );
                    return;
                }
            };
        if let Err(message) =
            validate_command_order(&request.constraints.enrichments, &compiled_enrichment)
        {
            fail(
                tx,
                &request,
                &cancelled,
                QueryPurpose::Enrichment,
                &message,
                false,
            );
            return;
        }
        let enrichment = compiled_enrichment
            .iter()
            .flat_map(|definition| definition.stages().iter().cloned())
            .collect::<Vec<_>>();
        let mut affected_outputs = changed_enrichment_outputs(
            &request.base_constraints.enrichments,
            &request.constraints.enrichments,
            &compiled_enrichment,
        );
        propagate_affected_outputs(&enrichment, &mut affected_outputs);
        let schema_seed = SchemaContext::default();
        (
            text,
            advanced,
            enrichment,
            affected_outputs,
            schema_seed.clone(),
            schema_seed,
            HashMap::new(),
            None,
        )
    };
    // Colour rules are presentation, so a rule that will not compile is
    // *skipped* rather than failing the query: the view keeps rendering with
    // the rules that do work, and the applied filter is never disturbed by a
    // palette mistake. They are named by their position so the terminal can
    // map a match back to the rule the user wrote.
    let mut color_rules: Vec<(String, TextSearch)> = Vec::new();
    // Column classification rules: `(rule index, output column, exact value)`.
    // They never reach the predicate compiler below. Their match is an exact
    // lookup of the worker's ready derived cells where the derived values are
    // inserted, so slash-shorthand and assignment outputs classify alike and
    // null/failed cells — which share display text with real values — can
    // never match. A rule with an empty column or value is skipped rather
    // than failing the view: the dialog refuses to submit one, and a rule
    // that can match nothing must not break painting.
    // Column classification rules: `(rule name, output column, exact value)`,
    // named by position like legacy rules so first-match-wins merging and
    // indexed failure messages treat both kinds uniformly. They never reach
    // the predicate compiler below: the engine evaluates them natively over
    // the typed frame. A column rule is malformed when its column is
    // missing/blank or its value is missing (an empty-string value is
    // valid: it matches literal empty-string ready cells); malformed shapes
    // fail the candidate with an indexed diagnostic like any other rule
    // defect, so the last good view stays applied instead of silently
    // keeping a rule that paints nothing.
    let mut column_rules: Vec<(String, String, String)> = Vec::new();
    for (index, rule) in request.constraints.color_rules.iter().enumerate() {
        let position = index + 1;
        if rule.is_column() {
            let column = rule.column.clone().unwrap_or_default();
            if column.trim().is_empty() {
                fail(
                    tx,
                    &request,
                    &cancelled,
                    QueryPurpose::Advanced,
                    &format!("colour rule {position}: no column to classify"),
                    false,
                );
                return;
            }
            let Some(value) = rule.value.clone() else {
                fail(
                    tx,
                    &request,
                    &cancelled,
                    QueryPurpose::Advanced,
                    &format!("colour rule {position}: no value to match"),
                    false,
                );
                return;
            };
            column_rules.push((index.to_string(), column, value));
            continue;
        }
        if rule.predicate.trim().is_empty() {
            fail(
                tx,
                &request,
                &cancelled,
                QueryPurpose::Advanced,
                &format!("colour rule {position}: predicate is empty"),
                false,
            );
            return;
        }
        let compiled = if TextSearch::is_polars(&rule.predicate) {
            let Some(host) = compiler.as_mut() else {
                fail(
                    tx,
                    &request,
                    &cancelled,
                    QueryPurpose::Advanced,
                    &format!("colour rule {position}: Polars compiler is unavailable"),
                    false,
                );
                return;
            };
            compiler_calls.fetch_add(1, Ordering::AcqRel);
            match host.compile(&rule.predicate, ExpressionKind::Filter, cancelled.as_ref()) {
                Ok(definition) => TextSearch::parse(rule.predicate.clone(), Some(&definition)),
                Err(error) => Err(error.to_string()),
            }
        } else {
            TextSearch::parse(rule.predicate.clone(), None).map_err(|error| error.to_string())
        };
        let compiled = match compiled {
            Ok(compiled) => compiled,
            Err(error) => {
                fail(
                    tx,
                    &request,
                    &cancelled,
                    QueryPurpose::Advanced,
                    &format!("colour rule {position}: {error}"),
                    false,
                );
                return;
            }
        };
        color_rules.push((index.to_string(), compiled));
    }
    if cancelled.load(Ordering::Acquire) {
        return;
    }
    if request.constraints.capture_time.is_some()
        && request.constraints.time_basis == lvu::TimeBasis::Extracted
        && !enrichment.iter().any(|stage| stage.name == "timestamp_utc")
    {
        let purpose = if request.constraints.enrichments != request.base_constraints.enrichments {
            QueryPurpose::Enrichment
        } else {
            request.purpose
        };
        fail(
            tx,
            &request,
            &cancelled,
            purpose,
            "extracted time requires an accepted timestamp_utc enrichment; add it with e or use Alt-T in Time",
            false,
        );
        return;
    }
    // The `<name>.<field>` columns the chain, the filter and the search read
    // from command steps of this chain; each exists in every batch frame,
    // published or (as nulls) not yet.
    let required_command_columns = required_command_columns(
        &request.constraints.enrichments,
        &enrichment,
        advanced.as_ref(),
        text.as_ref(),
    );
    // A command step nobody has run yet: the steps and the filter that read
    // it are valid and wait (docs/command-enrichment.md). Its columns are
    // typed null, so what can be evaluated over null is; what cannot is a
    // diagnostic on those steps, never a rejection of the chain. A waiting
    // filter is not applied: applying it would hide the very rows the
    // command needs as its input.
    let unpublished_commands: Vec<String> = request
        .constraints
        .enrichments
        .iter()
        .filter_map(|step| step.output_prefix())
        .filter(|name| !command_results.iter().any(|columns| columns.name == *name))
        .map(str::to_owned)
        .collect();
    let waiting_stages = waiting_stages(&enrichment, &unpublished_commands);
    let filter_waiting = advanced.as_ref().is_some_and(|filter| {
        filter.dependencies().iter().any(|dependency| {
            command_columns::command_prefix(
                dependency,
                unpublished_commands.iter().map(String::as_str),
            )
            .is_some()
        })
    });
    let mut reservation = Reservation::new(budget);
    let mut derived = prior_membership
        .as_ref()
        .map_or_else(HashMap::new, |membership| membership.derived.clone());
    let mut derived_errors = prior_membership
        .as_ref()
        .map_or_else(HashSet::new, |membership| membership.derived_errors.clone());
    // Matches carry forward with the rest of the membership, but only while
    // the rules that produced them are unchanged: an edited rule must not
    // leave a row painted by the rule it replaced.
    let mut color_matches: HashMap<(String, u64), u16> = prior_membership
        .as_ref()
        .filter(|membership| membership.color_rules == request.constraints.color_rules)
        .map_or_else(HashMap::new, |membership| membership.color_matches.clone());
    let mut evaluation_batches = prior_membership
        .as_ref()
        .map_or_else(Vec::new, |membership| {
            membership.evaluation_batches.to_vec()
        });
    if !reservation.add((sources.len() as u64).saturating_mul(SOURCE_OVERHEAD)) {
        fail(
            tx,
            &request,
            &cancelled,
            request.purpose,
            "membership memory cap reached; previous applied view preserved",
            true,
        );
        return;
    }
    let prior_sequence_bytes = prior_membership
        .as_ref()
        .map_or(0, |value| value.count.saturating_mul(SEQUENCE_BYTES));
    if !reservation.add(prior_sequence_bytes) {
        fail(
            tx,
            &request,
            &cancelled,
            request.purpose,
            "membership memory cap reached while retaining the applied snapshot",
            true,
        );
        return;
    }
    let prior_group_bytes = prior_membership.as_ref().map_or(0, |membership| {
        membership.sources.iter().fold(0_u64, |total, source| {
            source.groups.iter().fold(total, |subtotal, group| {
                subtotal.saturating_add(group_projection_bytes(group))
            })
        })
    });
    if !reservation.add(prior_group_bytes) {
        fail(
            tx,
            &request,
            &cancelled,
            QueryPurpose::Grouping,
            "display grouping projection memory cap reached while retaining the applied view",
            true,
        );
        return;
    }
    let prior_derived_bytes = derived
        .iter()
        .fold(0_u64, |total, ((source, _, field), value)| {
            total.saturating_add(
                value.as_ref().map_or(1, String::len) as u64
                    + source.len() as u64
                    + field.len() as u64
                    + 24,
            )
        });
    if !reservation.add(prior_derived_bytes) {
        fail(
            tx,
            &request,
            &cancelled,
            QueryPurpose::Enrichment,
            "derived value memory cap reached while retaining the applied snapshot",
            true,
        );
        return;
    }
    let prior_error_bytes = derived_errors
        .iter()
        .fold(0_u64, |total, (source, _, field)| {
            total.saturating_add(source.len() as u64 + field.len() as u64 + 16)
        });
    if !reservation.add(prior_error_bytes) {
        fail(
            tx,
            &request,
            &cancelled,
            QueryPurpose::Enrichment,
            "derived value memory cap reached while retaining the applied snapshot",
            true,
        );
        return;
    }
    let prior_color_match_bytes = color_matches.iter().fold(0_u64, |total, ((source, _), _)| {
        total.saturating_add(color_match_bytes(source))
    });
    if !reservation.add(prior_color_match_bytes) {
        fail(
            tx,
            &request,
            &cancelled,
            QueryPurpose::Advanced,
            "colour-rule match memory cap reached while retaining the applied snapshot",
            true,
        );
        return;
    }
    let prior_batch_bytes = evaluation_batches.iter().fold(0_u64, |total, batch| {
        total.saturating_add(evaluation_batch_bytes(batch))
    });
    if !reservation.add(prior_batch_bytes) {
        fail(
            tx,
            &request,
            &cancelled,
            request.purpose,
            "evaluation provenance memory cap reached while retaining the applied snapshot",
            true,
        );
        return;
    }
    let mut count = 0_u64;
    let mut scanned = 0u64;
    let mut runtime_diagnostic = None;
    let mut event_time_missing = prior_membership
        .as_ref()
        .map_or(0, |value| value.event_time_missing);
    let mut event_time_invalid = prior_membership
        .as_ref()
        .map_or(0, |value| value.event_time_invalid);
    let mut watermarks = Vec::new();
    let mut matched_sources = Vec::with_capacity(sources.len());
    let correlation = request.constraints.exact_field.clone();
    for source in sources {
        let source_id = source.source_id().0.to_string();
        let generation = source.progress().generation;
        // A correlation names the field per source. A source the user did not
        // map contributes no records; its key name is never inferred from the
        // origin's, so the view stays honest about what it searched.
        let exact = match correlation.as_ref() {
            None => None,
            Some(correlation) => match correlation.constraint_for(&source_id) {
                Some(constraint) => Some(constraint),
                None => {
                    matched_sources.push(SourceMatches {
                        source_id,
                        generation,
                        high_watermark: source.progress().high_watermark.map(|id| id.sequence),
                        sequences: Appended::default(),
                        times: Appended::default(),
                        groups: Appended::default(),
                        bounds: SourceTimeBounds::default(),
                        merge_keys: Appended::default(),
                        ascending: true,
                    });
                    continue;
                }
            },
        };
        let prior_source_any = prior_membership.as_ref().and_then(|membership| {
            membership
                .sources
                .iter()
                .find(|item| item.source_id == source_id)
        });
        let prior_source = prior_source_any.filter(|item| item.generation == generation);
        if prior_source_any.is_some_and(|item| item.generation != generation) {
            derived.retain(|(derived_source, _, _), _| derived_source != &source_id);
            derived_errors.retain(|(derived_source, _, _)| derived_source != &source_id);
            evaluation_batches.retain(|batch| batch.source_id != source_id);
        }
        // A refresh extends what was published; it does not rebuild it. The
        // builder holds the published chunks untouched and collects only the
        // records this pass matched.
        let mut sequences = AppendedBuilder::new(
            prior_source.map_or_else(Appended::default, |item| item.sequences.clone()),
        );
        let mut times = AppendedBuilder::new(
            prior_source.map_or_else(Appended::default, |item| item.times.clone()),
        );
        // Safe to inherit: `prior_membership` is only carried across a refresh
        // whose constraints — `time_basis` among them — are identical, so every
        // retained timestamp was read in the basis this pass is using.
        let mut source_bounds =
            prior_source.map_or_else(SourceTimeBounds::default, |item| item.bounds);
        let mut groups = AppendedBuilder::new(
            prior_source.map_or_else(Appended::default, |item| item.groups.clone()),
        );
        count = count.saturating_add(sequences.len() as u64);
        let target = source.progress().high_watermark.map(|id| id.sequence);
        watermarks.push((source.source_id(), target));
        let (mut offset, mut last_sequence) = checkpoints
            .get(&source_id)
            .map(|(known_generation, offset, last)| {
                (
                    if *known_generation == generation {
                        *offset
                    } else {
                        0
                    },
                    *last,
                )
            })
            .unwrap_or((0, None));
        if target.is_none() {
            let (times, groups) = (times.finish(), groups.finish());
            let (merge_keys, ascending) = merge_keys_for(
                prior_source.map(|item| (&item.merge_keys, item.ascending)),
                &times,
            );
            matched_sources.push(SourceMatches {
                source_id,
                generation,
                high_watermark: target,
                sequences: sequences.finish(),
                times,
                groups,
                bounds: source_bounds,
                merge_keys,
                ascending,
            });
            continue;
        }
        let provenance_batches =
            evaluation_provenance
                .as_ref()
                .map_or_else(Vec::new, |membership| {
                    membership
                        .evaluation_batches
                        .iter()
                        .filter(|batch| {
                            batch.source_id == source_id && batch.generation == generation
                        })
                        .collect::<Vec<_>>()
                });
        let mut provenance_cursor = 0usize;
        loop {
            if cancelled.load(Ordering::Acquire) {
                return;
            }
            let provenance = provenance_batches.get(provenance_cursor).copied();
            let page = match runtime.block_on(
                source.read_page(
                    offset,
                    provenance.map_or(config.page_records, |batch| batch.record_count),
                    evaluation_provenance
                        .as_ref()
                        .map_or(config.page_bytes, |membership| {
                            membership.evaluation_page_bytes
                        }),
                ),
            ) {
                Ok(v) => v,
                Err(e) => {
                    fail(
                        tx,
                        &request,
                        &cancelled,
                        request.purpose,
                        &e.to_string(),
                        false,
                    );
                    return;
                }
            };
            if page.records.is_empty() {
                break;
            }
            let end_of_journal = page.end_of_journal;
            let page_start = offset;
            let saw_beyond_target = page
                .records
                .iter()
                .any(|record| target.is_some_and(|high| record.record_id.sequence > high));
            offset = if saw_beyond_target {
                page_start
            } else {
                page.next_offset
            };
            let records = page
                .records
                .into_iter()
                .filter(|record| last_sequence.is_none_or(|last| record.record_id.sequence > last))
                .take_while(|record| target.is_some_and(|high| record.record_id.sequence <= high))
                .collect::<Vec<_>>();
            if records.is_empty() {
                if end_of_journal || saw_beyond_target {
                    break;
                }
                continue;
            }
            if let Some(boundary) = provenance
                && (records.first().map(|record| record.record_id.sequence)
                    != Some(boundary.first_sequence)
                    || records.last().map(|record| record.record_id.sequence)
                        != Some(boundary.last_sequence))
            {
                fail(
                    tx,
                    &request,
                    &cancelled,
                    QueryPurpose::Enrichment,
                    "applied enrichment evaluation boundary no longer matches the journal",
                    false,
                );
                return;
            }
            let schema_before =
                provenance.map_or_else(|| schema.clone(), |batch| batch.schema_before.clone());
            let frame = if advanced.is_none()
                && enrichment.is_empty()
                && exact.is_none()
                && !text.as_ref().is_some_and(TextSearch::requires_projection)
                // A rule over a named column needs the projection too, exactly
                // as the search does.
                && !color_rules
                    .iter()
                    .any(|(_, rule)| rule.requires_projection())
            {
                literal_frame(&records)
            } else {
                let mut batch_schema = schema_before.clone();
                records_to_batch_with_context_and_exact_field(
                    &records,
                    &mut batch_schema,
                    exact.as_ref().map(|constraint| constraint.field()),
                )
                .and_then(|batch| {
                    schema = batch_schema;
                    let mut frame = batch.frame;
                    command_columns::join_command_columns(
                        &mut frame,
                        &records,
                        &command_results,
                        &required_command_columns,
                    )?;
                    Ok(frame)
                })
            };
            let frame = match frame {
                Ok(frame) => frame,
                Err(e) => {
                    fail(
                        tx,
                        &request,
                        &cancelled,
                        QueryPurpose::Advanced,
                        &format!("schema projection failed: {e}"),
                        false,
                    );
                    return;
                }
            };
            let result = execute_batch_with_exact_constraint(
                &frame,
                BatchQuery {
                    generation: request.generation,
                    definition_generation: request.revision,
                    stages: enrichment.as_slice(),
                    filter: if filter_waiting {
                        None
                    } else {
                        advanced.as_ref()
                    },
                    text_search: text.as_ref(),
                    colors: &color_rules,
                    column_colors: &column_rules,
                },
                exact.as_ref(),
            );
            if let Some(diagnostic) = result
                .color_diagnostics
                .iter()
                .find(|diagnostic| diagnostic.state == DerivedState::Error)
            {
                let position = diagnostic
                    .field
                    .as_deref()
                    .and_then(|field| field.parse::<usize>().ok())
                    .unwrap_or(0)
                    .saturating_add(1);
                fail(
                    tx,
                    &request,
                    &cancelled,
                    QueryPurpose::Advanced,
                    &format!("colour rule {position}: {}", diagnostic.message),
                    false,
                );
                return;
            }
            if !enrichment.is_empty()
                && let (Some(first), Some(last)) = (records.first(), records.last())
            {
                let batch = EvaluationBatch {
                    source_id: source_id.clone(),
                    generation,
                    record_count: records.len(),
                    first_sequence: first.record_id.sequence,
                    last_sequence: last.record_id.sequence,
                    schema_before,
                };
                if !reservation.add(evaluation_batch_bytes(&batch)) {
                    fail(
                        tx,
                        &request,
                        &cancelled,
                        request.purpose,
                        "evaluation provenance memory cap reached; previous applied view preserved",
                        true,
                    );
                    return;
                }
                evaluation_batches.push(batch);
            }
            let projected = enrichment
                .iter()
                .map(|stage| {
                    let stage_error = result.diagnostics.iter().find(|diagnostic| {
                        diagnostic.field.as_deref() == Some(stage.name.as_str())
                            && diagnostic.state == DerivedState::Error
                    });
                    let values = match stage_error {
                        Some(_) if waiting_stages.contains(&stage.name) => {
                            if runtime_diagnostic.is_none() {
                                runtime_diagnostic = Some(bounded_text(
                                    format!(
                                        "enrichment {} waits for a command step that has not run",
                                        stage.name
                                    ),
                                    512,
                                ));
                            }
                            Ok(records
                                .iter()
                                .map(|record| {
                                    (
                                        lvu_query::StableRecordId {
                                            source_id: record.record_id.source_id.0.to_string(),
                                            sequence: record.record_id.sequence,
                                        },
                                        None,
                                    )
                                })
                                .collect())
                        }
                        Some(diagnostic) => Err(diagnostic.message.clone()),
                        None => scalar_projection(&result.enriched_rows, &stage.name, 512),
                    };
                    (stage, values)
                })
                .collect::<Vec<_>>();
            let candidate_projection_error = projected.iter().find_map(|(stage, values)| {
                affected_outputs
                    .contains(&stage.name)
                    .then(|| values.as_ref().err().cloned())
                    .flatten()
            });
            if let Some(message) = &candidate_projection_error {
                fail(
                    tx,
                    &request,
                    &cancelled,
                    QueryPurpose::Enrichment,
                    message,
                    false,
                );
                return;
            }
            let projection_error = projected
                .iter()
                .find_map(|(_, values)| values.as_ref().err());
            // An accepted enrichment can fail on newly appended records. In
            // that refresh case the uncertain records stay unmatched and the
            // per-stage diagnostics below remain visible; this is not a new
            // candidate failure and must not enqueue a terminal completion.
            let accepted_refresh_filter_error = cached_refresh
                && result.validity == BatchValidity::InvalidFilter
                && projection_error.is_some();
            if result.validity != BatchValidity::Valid && !accepted_refresh_filter_error {
                let message = result
                    .diagnostics
                    .iter()
                    .map(|d| d.message.as_str())
                    .collect::<Vec<_>>()
                    .join("; ");
                let advanced_depends_on_changed_enrichment =
                    advanced.as_ref().is_some_and(|filter| {
                        filter
                            .dependencies()
                            .iter()
                            .any(|dependency| affected_outputs.contains(dependency))
                    });
                let search_depends_on_changed_enrichment = text.as_ref().is_some_and(|search| {
                    search
                        .dependencies()
                        .iter()
                        .any(|field| affected_outputs.contains(field))
                });
                let purpose = if (!advanced_changed && advanced_depends_on_changed_enrichment)
                    || (request.constraints.text == request.base_constraints.text
                        && search_depends_on_changed_enrichment)
                {
                    QueryPurpose::Enrichment
                } else if advanced.is_some() {
                    QueryPurpose::Advanced
                } else {
                    QueryPurpose::Search
                };
                fail(tx, &request, &cancelled, purpose, &message, false);
                return;
            }
            for (stage, values) in projected {
                let (projection, failed) = match values {
                    Err(error) => {
                        let message = bounded_text(format!("error: {error}"), 512);
                        let filter_diagnostic = (result.validity == BatchValidity::InvalidFilter)
                            .then_some("; dependent filter could not be evaluated");
                        runtime_diagnostic = Some(bounded_text(
                            format!(
                                "enrichment {} failed for new records: {}{}",
                                stage.name,
                                error,
                                filter_diagnostic.unwrap_or_default()
                            ),
                            512,
                        ));
                        let projection = records
                            .iter()
                            .map(|record| {
                                (
                                    lvu_query::StableRecordId {
                                        source_id: record.record_id.source_id.0.to_string(),
                                        sequence: record.record_id.sequence,
                                    },
                                    Some(message.clone()),
                                )
                            })
                            .collect();
                        (projection, true)
                    }
                    Ok(values) => (values, false),
                };
                for (id, value) in projection {
                    let bytes = value.as_ref().map_or(1, String::len) as u64
                        + id.source_id.len() as u64
                        + stage.name.len() as u64
                        + 24;
                    if !reservation.add(bytes) {
                        fail(
                            tx,
                            &request,
                            &cancelled,
                            QueryPurpose::Enrichment,
                            "derived value memory cap reached; previous applied view preserved",
                            true,
                        );
                        return;
                    }
                    let key = (id.source_id, id.sequence, stage.name.clone());
                    if failed {
                        // Structural failure record: the display string in
                        // `derived` stays for Details compat, but validity
                        // consumers read this set, never the value text — a
                        // ready string may itself read `"error: ..."`.
                        if !reservation.add(key.0.len() as u64 + key.2.len() as u64 + 16) {
                            fail(
                                tx,
                                &request,
                                &cancelled,
                                QueryPurpose::Enrichment,
                                "derived value memory cap reached; previous applied view preserved",
                                true,
                            );
                            return;
                        }
                        derived_errors.insert(key.clone());
                    } else {
                        // A refreshed success retires a prior failure for the
                        // same cell; stale errors never outlive their fix.
                        derived_errors.remove(&key);
                    }
                    derived.insert(key, value);
                }
            }
            // Configured grouping consumes engine verdicts computed from the
            // same evaluated batch as the derived values above, so batch
            // boundaries cannot change the flags: every batch carries its own
            // complete verdicts for the records it holds.
            let group_flags = match &grouping_rule {
                Some(rule) if rule.is_configured() => {
                    match configured_batch_flags(
                        rule,
                        &enrichment,
                        &waiting_stages,
                        &result,
                        &source_id,
                    ) {
                        Ok(flags) => Some(flags),
                        Err(message) => {
                            fail(
                                tx,
                                &request,
                                &cancelled,
                                QueryPurpose::Grouping,
                                &message,
                                false,
                            );
                            return;
                        }
                    }
                }
                _ => None,
            };
            if filter_waiting && runtime_diagnostic.is_none() {
                // The filter reads a command step's output before the run:
                // valid, and not applied until results exist. The rows stay,
                // so the command still has its input to run over.
                runtime_diagnostic =
                    Some("filter waits for a command step that has not run".into());
            }
            for (name, ids) in &result.color_matches {
                let Ok(index) = name.parse::<u16>() else {
                    continue;
                };
                for id in ids {
                    // Rules are ordered and the first match wins, so a later
                    // rule never repaints a row an earlier one already claimed.
                    match color_matches.entry((id.source_id.clone(), id.sequence)) {
                        std::collections::hash_map::Entry::Occupied(mut entry) => {
                            *entry.get_mut() = (*entry.get()).min(index);
                        }
                        std::collections::hash_map::Entry::Vacant(entry) => {
                            if !reservation.add(color_match_bytes(&id.source_id)) {
                                fail(
                                    tx,
                                    &request,
                                    &cancelled,
                                    QueryPurpose::Advanced,
                                    "colour-rule match memory cap reached; previous applied view preserved",
                                    true,
                                );
                                return;
                            }
                            entry.insert(index);
                        }
                    }
                }
            }
            let mut matched_ids = if result.validity == BatchValidity::InvalidFilter {
                Vec::new()
            } else {
                result.matched_ids
            };
            // A declared field basis is read once for the whole batch rather
            // than per record, because an enriched column is only readable as a
            // compiled expression over the column as a whole.
            // `timestamp_utc` is a designated column too, so its reading is
            // compiled by the engine rather than parsed here — one evaluator
            // for both, and the same one the export replays.
            let extracted: Option<Result<Vec<Option<i64>>, String>> =
                (request.constraints.time_basis == lvu::TimeBasis::Extracted).then(|| {
                    let values: Vec<Option<&str>> = records
                        .iter()
                        .map(|record| {
                            derived
                                .get(&(
                                    source_id.clone(),
                                    record.record_id.sequence,
                                    crate::time_basis::EXTRACTED_COLUMN.into(),
                                ))
                                .and_then(|value| value.as_deref())
                        })
                        .collect();
                    crate::time_basis::read_extracted(&values)
                });
            let selected = match (
                request.constraints.time_basis,
                request.constraints.time_field.as_deref(),
            ) {
                (lvu::TimeBasis::Selected, Some(token)) => {
                    Some(crate::time_basis::read_records(token, &records, |record| {
                        let column = match lvu_live::time::TimeFieldSelection::parse_token(token) {
                            Ok(selection) => match selection.field {
                                lvu_live::time::TimeFieldRef::Column(name) => name,
                                _ => return None,
                            },
                            Err(_) => return None,
                        };
                        derived
                            .get(&(source_id.clone(), record.record_id.sequence, column))
                            .and_then(|value| value.as_deref())
                    }))
                }
                (lvu::TimeBasis::Selected, None) => Some(crate::time_basis::SelectedTimes {
                    error: Some("no event-time field is declared for this basis".into()),
                    ..Default::default()
                }),
                _ => None,
            };
            if let Some(selected) = &selected {
                event_time_invalid += selected.invalid;
                event_time_missing += selected.missing;
            }
            // The basis timestamp of every record in the batch, computed once
            // whether or not a window is applied. The window filter uses it;
            // so do the dataset bounds and gap navigation, which have to answer
            // for a view that carries no window at all.
            //
            // The two diagnostics stay windowed. They report why a *filter*
            // dropped records, and counting them for a view that is not
            // filtering on time would report a problem the user does not have.
            let mut basis_invalid = 0usize;
            let mut basis_missing = 0usize;
            // Positional, not keyed: the page is one contiguous ascending run
            // of records, so the timestamp of record `i` lives at index `i` and
            // the handful of lookups that only know a sequence can find it by
            // binary search. Hashing every scanned record to answer questions
            // about the matched ones cost a hash insert per record of every
            // scan, whether or not the view even carries a time window.
            let basis_times: Vec<Option<i64>> = records
                .iter()
                .enumerate()
                .map(|(index, record)| match request.constraints.time_basis {
                    lvu::TimeBasis::Capture => Some(record.captured_at_unix_nanos),
                    lvu::TimeBasis::Extracted => {
                        // A record with no value is missing; one the reading
                        // could not use is invalid. The distinction is the
                        // caller's, because only the diagnostics say it.
                        let had_value = derived
                            .get(&(
                                source_id.clone(),
                                record.record_id.sequence,
                                crate::time_basis::EXTRACTED_COLUMN.into(),
                            ))
                            .is_some_and(|value| value.is_some());
                        let read = extracted
                            .as_ref()
                            .and_then(|read| read.as_ref().ok())
                            .and_then(|times| times.get(index).copied())
                            .flatten();
                        match (had_value, read) {
                            (_, Some(timestamp)) => Some(timestamp),
                            (true, None) => {
                                basis_invalid += 1;
                                None
                            }
                            (false, None) => {
                                basis_missing += 1;
                                None
                            }
                        }
                    }
                    lvu::TimeBasis::Selected => selected
                        .as_ref()
                        .and_then(|selected| selected.by_sequence.get(&record.record_id.sequence))
                        .copied(),
                    lvu::TimeBasis::Event => match lvu_live::recognize_event_time(&record.bytes) {
                        lvu_live::EventTimeRecognition::Valid { unix_nanos, .. } => {
                            Some(unix_nanos)
                        }
                        lvu_live::EventTimeRecognition::Invalid { .. } => {
                            basis_invalid += 1;
                            None
                        }
                        lvu_live::EventTimeRecognition::Missing => {
                            basis_missing += 1;
                            None
                        }
                    },
                })
                .collect();
            let basis_time_of = |sequence: u64| -> Option<i64> {
                records
                    .binary_search_by(|record| record.record_id.sequence.cmp(&sequence))
                    .ok()
                    .and_then(|index| basis_times[index])
            };
            // Bounds are measured before the window narrows the set, so "the
            // last five minutes of data" means five minutes of the dataset
            // rather than five minutes of the window already applied. Only
            // records that survived every *other* constraint count.
            for id in &matched_ids {
                source_bounds.observe(basis_time_of(id.sequence));
            }
            if let Some(window) = request.constraints.capture_time {
                event_time_invalid += basis_invalid;
                event_time_missing += basis_missing;
                matched_ids.retain(|id| {
                    basis_time_of(id.sequence).is_some_and(|timestamp| {
                        timestamp >= window.start_unix_nanos && timestamp < window.end_unix_nanos
                    })
                });
            }
            // `matched_ids` is a subsequence of `records` in the same order,
            // so walking the two together answers "did this record match" without
            // hashing every scanned record into a set first.
            let mut matched_cursor = 0usize;
            let mut previous_physical_matched = last_sequence
                .zip(sequences.last().copied())
                .is_some_and(|(last, matched)| last == matched);
            let mut previous_sequence = last_sequence;
            for (position, record) in records.iter().enumerate() {
                let physically_adjacent = previous_sequence.is_some_and(|previous| {
                    previous.checked_add(1) == Some(record.record_id.sequence)
                });
                previous_sequence = Some(record.record_id.sequence);
                while matched_ids
                    .get(matched_cursor)
                    .is_some_and(|id| id.sequence < record.record_id.sequence)
                {
                    matched_cursor += 1;
                }
                if !matched_ids
                    .get(matched_cursor)
                    .is_some_and(|id| id.sequence == record.record_id.sequence)
                {
                    previous_physical_matched = false;
                    if grouping_rule
                        .as_ref()
                        .is_some_and(ContinuationRule::is_auto)
                        && let Some(group) = groups.last_mut()
                    {
                        // Auto classifier state may survive an incremental
                        // refresh, but never an unmatched physical record.
                        // Otherwise a filtered-out head/chunk could be bridged
                        // by a later continuation in this or a later batch.
                        group.auto_open = false;
                        group.auto_structured = false;
                        group.auto_structure_depth = 0;
                        group.partial_open = false;
                    }
                    continue;
                }
                matched_cursor += 1;
                if !reservation.add(SEQUENCE_BYTES) {
                    fail(
                        tx,
                        &request,
                        &cancelled,
                        request.purpose,
                        "membership memory cap reached; previous applied view preserved",
                        true,
                    );
                    return;
                }
                let sequence_index = sequences.len();
                sequences.push(record.record_id.sequence);
                times.push(basis_times[position].unwrap_or(NO_BASIS_TIME));
                count += 1;
                // Display projection is rule-independent: every grouped record
                // carries its bounded text plus the accepted enrichment values
                // shown beside it, whichever rule segments the groups. Each
                // branch below reserves what it actually retains.
                let mut projection = grouping_rule.as_ref().map(|_| {
                    let mut projection = lvu_live::display_projection(
                        record,
                        MAX_GROUP_LINE_DISPLAY_BYTES,
                        MAX_GROUP_LINE_PROJECTION_BYTES,
                    );
                    for stage in &enrichment {
                        // Same structural validity contract as
                        // `with_enrichment` above: readiness and failure ride
                        // dedicated detail keys, never value text. Grouping
                        // segmentation itself is untouched — only the
                        // per-stage display projection gains the markers.
                        let key = (
                            source_id.clone(),
                            record.record_id.sequence,
                            stage.name.clone(),
                        );
                        let failed = derived_errors.contains(&key);
                        let value = derived
                            .get(&key)
                            .and_then(Clone::clone)
                            .unwrap_or_else(|| "null".into());
                        projection.fields.retain(|(field, _)| field != &stage.name);
                        projection.fields.push((stage.name.clone(), value.clone()));
                        projection
                            .details
                            .push((format!("derived.{}", stage.name), value.clone()));
                        if failed {
                            projection.details.push((
                                format!("{DERIVED_ERROR_DETAIL_PREFIX}{}", stage.name),
                                value,
                            ));
                        } else if derived.get(&key).is_some_and(Option::is_some) {
                            projection.details.push((
                                format!("{DERIVED_READY_DETAIL_PREFIX}{}", stage.name),
                                value,
                            ));
                        }
                    }
                    projection
                });
                if let Some(rule) = &grouping_rule
                    && rule.is_configured()
                {
                    let projection = projection.take().expect("built for grouped records");
                    // Configured enrichment grouping: Polars computed the
                    // flags for this batch above; this only segments stable
                    // ordered membership. Raw bytes are never classified here.
                    let sequence_index = sequences.len().saturating_sub(1);
                    let flag = match &group_flags {
                        Some(BatchFlags::Known(flags)) => flags
                            .get(&record.record_id.sequence)
                            .cloned()
                            .unwrap_or(ConfiguredFlag::Unknown),
                        _ => ConfiguredFlag::Unknown,
                    };
                    let same_stream = groups
                        .last()
                        .is_some_and(|group| group.stream == record.stream);
                    let same_acquisition = groups.last().is_some_and(|group| {
                        group.acquisition_id == *record.acquisition_id.as_bytes()
                    });
                    // Physical fragments join their head whatever the flags
                    // say: acquisition framing split one logical line, and
                    // splitting it on flags would corrupt the line.
                    let chunk_join = groups.last().is_some_and(|group| {
                        group.partial_open
                            && group.acquisition_id == *record.acquisition_id.as_bytes()
                            && matches!(
                                (group.last_chunk, record.chunk),
                                (
                                    lvu_core::ChunkPosition::Start
                                        | lvu_core::ChunkPosition::Continue,
                                    lvu_core::ChunkPosition::Continue
                                        | lvu_core::ChunkPosition::End
                                )
                            )
                    }) && previous_physical_matched
                        && physically_adjacent
                        && same_stream;
                    // An accepted group extends across batches, streams aside:
                    // pending and orphan units never absorb records, so live
                    // unknowns stay visible without falsely joining or cutting
                    // the groups around them. New rules carry no span or size
                    // bound: a new start or a changed key is the only thing
                    // that cuts, so batch geometry cannot invent boundaries.
                    let join_open = !chunk_join
                        && previous_physical_matched
                        && physically_adjacent
                        && same_stream
                        && same_acquisition
                        && groups
                            .last()
                            .is_some_and(|group| !group.orphan && !group.pending && !group.split)
                        && match (&flag, rule) {
                            (ConfiguredFlag::Start, ContinuationRule::Filter { .. }) => false,
                            (ConfiguredFlag::Continue, ContinuationRule::Filter { .. }) => true,
                            (ConfiguredFlag::Key(key), ContinuationRule::Run { .. }) => groups
                                .last()
                                .is_some_and(|group| group.run_key.as_ref() == Some(key)),
                            _ => false,
                        };
                    if chunk_join || join_open {
                        // A retained member page is charged; records past the
                        // stored page cost only their scalars, so a giant live
                        // group cannot exhaust the budget on projections no
                        // frame reads.
                        let retained = groups.last().is_some_and(|group| {
                            group.projection.len() < MAX_CONFIGURED_GROUP_STORED
                        });
                        if retained {
                            let projection_bytes = display_projection_bytes(&projection);
                            if !reservation.add(projection_bytes) {
                                fail(
                                    tx,
                                    &request,
                                    &cancelled,
                                    QueryPurpose::Grouping,
                                    "display grouping projection memory cap reached; previous view preserved",
                                    true,
                                );
                                return;
                            }
                        }
                        let group = groups.last_mut().expect("joinable group");
                        group.len += 1;
                        let fragment_join = chunk_join
                            && matches!(
                                record.chunk,
                                lvu_core::ChunkPosition::Continue | lvu_core::ChunkPosition::End
                            );
                        if !fragment_join {
                            group.logical_lines = group.logical_lines.saturating_add(1);
                        }
                        group.payload_bytes =
                            group.payload_bytes.saturating_add(record.bytes.len());
                        group.last_chunk = record.chunk;
                        group.last_capture_nanos = record.captured_at_unix_nanos;
                        if matches!(record.chunk, lvu_core::ChunkPosition::Start) {
                            group.partial_open = true;
                        }
                        if matches!(record.chunk, lvu_core::ChunkPosition::End) {
                            group.partial_open = false;
                        }
                        if retained {
                            Arc::make_mut(&mut group.projection).push(projection);
                        }
                    } else {
                        let (orphan, pending, key_refused, run_key) = match (&flag, rule) {
                            (ConfiguredFlag::Start, ContinuationRule::Filter { .. }) => {
                                (false, false, false, None)
                            }
                            (ConfiguredFlag::Key(key), ContinuationRule::Run { .. }) => {
                                (false, false, false, Some(key.clone()))
                            }
                            (ConfiguredFlag::KeyOversize, _) => {
                                if runtime_diagnostic.is_none() {
                                    runtime_diagnostic = Some(format!(
                                        "grouping key exceeds the {}-byte exact-identity bound; those records stay unfolded",
                                        lvu_query::MAX_EXACT_KEY_BYTES
                                    ));
                                }
                                (false, false, true, None)
                            }
                            // A produced null or NaN stands alone without
                            // claiming a head; an unevaluated record stands
                            // alone pending it. Anything else without an open
                            // head is an orphan, kept visible rather than
                            // joined upward.
                            (ConfiguredFlag::KeyNull, _) | (ConfiguredFlag::KeyNan, _) => {
                                (false, false, false, None)
                            }
                            (ConfiguredFlag::Unknown, _) => (true, true, false, None),
                            _ => (true, false, false, None),
                        };
                        // A new group reserves its base state, its run-key
                        // heap and its retained head projection up front, so
                        // membership never holds unaccounted bytes. Prior
                        // generations price the same shape through
                        // group_projection_bytes, which includes run-key
                        // capacity.
                        let state_bytes = group_base_state_bytes()
                            .unwrap_or(u64::MAX)
                            .saturating_add(
                                u64::try_from(run_key.as_ref().map_or(0, Vec::capacity))
                                    .unwrap_or(u64::MAX),
                            )
                            .saturating_add(display_projection_bytes(&projection));
                        if !reservation.add(state_bytes) {
                            fail(
                                tx,
                                &request,
                                &cancelled,
                                QueryPurpose::Grouping,
                                "display grouping index memory cap reached; previous view preserved",
                                true,
                            );
                            return;
                        }
                        groups.push(GroupRange {
                            start: sequence_index,
                            len: 1,
                            logical_lines: 1,
                            payload_bytes: record.bytes.len(),
                            stream: record.stream,
                            orphan,
                            split: false,
                            oversized: false,
                            pending,
                            run_key,
                            key_refused,
                            configured: true,
                            auto_open: false,
                            auto_structured: false,
                            auto_structure_depth: 0,
                            partial_open: matches!(record.chunk, lvu_core::ChunkPosition::Start),
                            partial_truncated: false,
                            structure_truncated: false,
                            partial_prefix: Vec::new(),
                            structure_prefix: Vec::new(),
                            acquisition_id: *record.acquisition_id.as_bytes(),
                            last_chunk: record.chunk,
                            first_capture_nanos: record.captured_at_unix_nanos,
                            last_capture_nanos: record.captured_at_unix_nanos,
                            projection: Arc::new(vec![projection]),
                        });
                    }
                    previous_physical_matched = true;
                    continue;
                }
                if let Some(rule) = &grouping_rule {
                    let projection = projection.take().expect("built for grouped records");
                    // Legacy groups retain every member projection, so the
                    // built page is always charged here.
                    let projection_bytes = display_projection_bytes(&projection);
                    if !reservation.add(projection_bytes) {
                        fail(
                            tx,
                            &request,
                            &cancelled,
                            QueryPurpose::Grouping,
                            "display grouping projection memory cap reached; previous view preserved",
                            true,
                        );
                        return;
                    }
                    let prior = groups.last();
                    let auto_open = prior.is_some_and(|group| group.auto_open);
                    let possible_chunk_continuation = prior.is_some_and(|group| {
                        group.partial_open
                            && group.acquisition_id == *record.acquisition_id.as_bytes()
                            && matches!(
                                (group.last_chunk, record.chunk),
                                (
                                    lvu_core::ChunkPosition::Start
                                        | lvu_core::ChunkPosition::Continue,
                                    lvu_core::ChunkPosition::Continue
                                        | lvu_core::ChunkPosition::End
                                )
                            )
                    });
                    let auto_line = rule.auto_line(&record.bytes, auto_open);
                    let continuation = rule.custom_matches(&record.bytes).unwrap_or_else(|| {
                        matches!(
                            record.chunk,
                            lvu_core::ChunkPosition::Continue | lvu_core::ChunkPosition::End
                        ) || auto_line == Some(AutoLine::Continuation)
                    });
                    let credible_start = rule.custom_matches(&record.bytes).is_some()
                        || auto_line == Some(AutoLine::Start);
                    let same_stream = groups
                        .last()
                        .is_some_and(|group| group.stream == record.stream);
                    let same_acquisition = !rule.is_auto()
                        || groups.last().is_some_and(|group| {
                            group.acquisition_id == *record.acquisition_id.as_bytes()
                        });
                    let within_time = !rule.is_auto()
                        || groups.last().is_none_or(|group| {
                            auto_group_within_span(
                                group.first_capture_nanos,
                                group.last_capture_nanos,
                                record.captured_at_unix_nanos,
                            )
                        });
                    let chunk_continuation = possible_chunk_continuation
                        && previous_physical_matched
                        && physically_adjacent
                        && same_stream
                        && same_acquisition
                        && within_time;
                    let chunk_provenance = !rule.is_auto()
                        || !matches!(
                            record.chunk,
                            lvu_core::ChunkPosition::Continue | lvu_core::ChunkPosition::End
                        )
                        || chunk_continuation;
                    let can_extend = continuation
                        && previous_physical_matched
                        && physically_adjacent
                        && same_stream
                        && same_acquisition
                        && within_time
                        && chunk_provenance
                        && groups
                            .last()
                            .is_some_and(|group| group.auto_open || chunk_continuation)
                        && groups.last().is_some_and(|group| {
                            group.len < MAX_GROUP_LINES
                                && group.payload_bytes.saturating_add(record.bytes.len())
                                    <= MAX_GROUP_PAYLOAD_BYTES
                        });
                    if can_extend {
                        let (
                            next_partial_prefix,
                            next_partial_truncated,
                            next_structure_prefix,
                            next_structure_truncated,
                            prefix_capacity_growth,
                        ) = if rule.is_auto() {
                            let group = groups.last().expect("checked group");
                            let (next_partial, partial_truncated) = if chunk_continuation {
                                extended_auto_prefix(&group.partial_prefix, &record.bytes)
                            } else if matches!(record.chunk, lvu_core::ChunkPosition::Start) {
                                extended_auto_prefix(&[], &record.bytes)
                            } else {
                                (group.partial_prefix.clone(), group.partial_truncated)
                            };
                            let (next_structure, structure_truncated) = if group.auto_structured {
                                if matches!(record.chunk, lvu_core::ChunkPosition::Complete) {
                                    let (prefix, truncated) = extended_auto_prefix(
                                        &group.structure_prefix,
                                        &record.bytes,
                                    );
                                    (prefix, group.structure_truncated || truncated)
                                } else if matches!(record.chunk, lvu_core::ChunkPosition::End) {
                                    let (prefix, truncated) = extended_auto_prefix(
                                        &group.structure_prefix,
                                        &next_partial,
                                    );
                                    (prefix, group.structure_truncated || truncated)
                                } else {
                                    (group.structure_prefix.clone(), group.structure_truncated)
                                }
                            } else {
                                (group.structure_prefix.clone(), group.structure_truncated)
                            };
                            let old_capacity = group
                                .partial_prefix
                                .capacity()
                                .saturating_add(group.structure_prefix.capacity());
                            let new_capacity = next_partial
                                .capacity()
                                .saturating_add(next_structure.capacity());
                            (
                                next_partial,
                                group.partial_truncated || partial_truncated,
                                next_structure,
                                structure_truncated,
                                new_capacity.saturating_sub(old_capacity),
                            )
                        } else {
                            (Vec::new(), false, Vec::new(), false, 0)
                        };
                        if !reservation
                            .add(u64::try_from(prefix_capacity_growth).unwrap_or(u64::MAX))
                        {
                            fail(
                                tx,
                                &request,
                                &cancelled,
                                QueryPurpose::Grouping,
                                "display grouping classifier state memory cap reached; previous view preserved",
                                true,
                            );
                            return;
                        }
                        let group = groups.last_mut().expect("checked group");
                        group.len += 1;
                        if !rule.is_auto()
                            || matches!(
                                record.chunk,
                                lvu_core::ChunkPosition::Complete | lvu_core::ChunkPosition::Start
                            )
                        {
                            group.logical_lines = group.logical_lines.saturating_add(1);
                        }
                        group.payload_bytes =
                            group.payload_bytes.saturating_add(record.bytes.len());
                        group.last_chunk = record.chunk;
                        group.last_capture_nanos = record.captured_at_unix_nanos;
                        if rule.is_auto() {
                            group.partial_prefix = next_partial_prefix;
                            group.partial_truncated = next_partial_truncated;
                            group.structure_prefix = next_structure_prefix;
                            group.structure_truncated = next_structure_truncated;
                            if matches!(record.chunk, lvu_core::ChunkPosition::Start) {
                                group.partial_open = true;
                            }
                        }
                        if rule.is_auto() && matches!(record.chunk, lvu_core::ChunkPosition::End) {
                            group.partial_open = false;
                            let completed = if group.partial_truncated {
                                AutoLine::Ambiguous
                            } else {
                                classify_auto_line(&group.partial_prefix, false)
                            };
                            if !group.auto_structured {
                                group.auto_structured = completed == AutoLine::Start
                                    && is_auto_structured_start(&group.partial_prefix);
                                if group.auto_structured {
                                    let structure_prefix = group.partial_prefix.clone();
                                    let capacity_growth = structure_prefix
                                        .capacity()
                                        .saturating_sub(group.structure_prefix.capacity());
                                    if !reservation
                                        .add(u64::try_from(capacity_growth).unwrap_or(u64::MAX))
                                    {
                                        fail(
                                            tx,
                                            &request,
                                            &cancelled,
                                            QueryPurpose::Grouping,
                                            "display grouping classifier state memory cap reached; previous view preserved",
                                            true,
                                        );
                                        return;
                                    }
                                    group.structure_prefix = structure_prefix;
                                    group.structure_truncated = group.partial_truncated;
                                }
                            }
                            group.partial_prefix.clear();
                            group.partial_truncated = false;
                            group.auto_structure_depth = if group.auto_structured {
                                structured_depth(&group.structure_prefix).unwrap_or(1)
                            } else {
                                0
                            };
                            group.auto_open = !group.structure_truncated
                                && completed == AutoLine::Start
                                && (!group.auto_structured || group.auto_structure_depth > 0);
                        } else if rule.is_auto() && group.auto_structured {
                            group.auto_structure_depth =
                                structured_depth(&group.structure_prefix).unwrap_or(1);
                            group.auto_open =
                                !group.structure_truncated && group.auto_structure_depth > 0;
                        }
                        Arc::make_mut(&mut group.projection).push(projection);
                    } else {
                        let partial_open = rule.is_auto()
                            && (matches!(record.chunk, lvu_core::ChunkPosition::Start)
                                || chunk_continuation);
                        let auto_structured = rule.is_auto()
                            && matches!(record.chunk, lvu_core::ChunkPosition::Complete)
                            && auto_line == Some(AutoLine::Start)
                            && is_auto_structured_start(&record.bytes);
                        let (partial_prefix, partial_truncated) = if partial_open {
                            extended_auto_prefix(&[], &record.bytes)
                        } else {
                            (Vec::new(), false)
                        };
                        let (structure_prefix, structure_truncated) = if auto_structured {
                            extended_auto_prefix(&[], &record.bytes)
                        } else {
                            (Vec::new(), false)
                        };
                        let auto_structure_depth = if auto_structured {
                            structured_depth(&structure_prefix).unwrap_or(1)
                        } else {
                            0
                        };
                        let state_bytes = group_base_state_bytes()
                            .unwrap_or(u64::MAX)
                            .saturating_add(
                                u64::try_from(partial_prefix.capacity()).unwrap_or(u64::MAX),
                            )
                            .saturating_add(
                                u64::try_from(structure_prefix.capacity()).unwrap_or(u64::MAX),
                            );
                        if !reservation.add(state_bytes) {
                            fail(
                                tx,
                                &request,
                                &cancelled,
                                QueryPurpose::Grouping,
                                "display grouping index memory cap reached; previous view preserved",
                                true,
                            );
                            return;
                        }
                        groups.push(GroupRange {
                            start: sequence_index,
                            len: 1,
                            logical_lines: 1,
                            payload_bytes: record.bytes.len(),
                            stream: record.stream,
                            orphan: continuation,
                            split: continuation
                                && previous_physical_matched
                                && physically_adjacent
                                && same_stream
                                && same_acquisition
                                && (rule.custom_matches(&record.bytes).is_some()
                                    || auto_open
                                    || chunk_continuation),
                            oversized: record.bytes.len() > MAX_GROUP_PAYLOAD_BYTES,
                            pending: false,
                            run_key: None,
                            key_refused: false,
                            configured: false,
                            auto_open: (credible_start
                                || (continuation
                                    && previous_physical_matched
                                    && physically_adjacent
                                    && same_stream
                                    && same_acquisition
                                    && within_time
                                    && auto_open))
                                && !partial_truncated
                                && !structure_truncated
                                && (!auto_structured || auto_structure_depth > 0),
                            auto_structured,
                            auto_structure_depth,
                            partial_open,
                            partial_truncated,
                            structure_truncated,
                            partial_prefix,
                            structure_prefix,
                            acquisition_id: *record.acquisition_id.as_bytes(),
                            last_chunk: record.chunk,
                            first_capture_nanos: record.captured_at_unix_nanos,
                            last_capture_nanos: record.captured_at_unix_nanos,
                            projection: Arc::new(vec![projection]),
                        });
                    }
                }
                previous_physical_matched = true;
            }
            scanned = scanned.saturating_add(records.len() as u64);
            provenance_cursor += usize::from(provenance.is_some());
            last_sequence = records
                .last()
                .map(|record| record.record_id.sequence)
                .or(last_sequence);
            let _ = tx.try_send(Update::Progress {
                view_id: request.view_id.clone(),
                revision: request.revision,
                scanned,
                token: Arc::clone(&cancelled),
            });
            if end_of_journal
                || records
                    .last()
                    .is_some_and(|record| Some(record.record_id.sequence) == target)
            {
                break;
            }
        }
        checkpoints.insert(source_id.clone(), (generation, offset, last_sequence));
        let (times, groups) = (times.finish(), groups.finish());
        let (merge_keys, ascending) = merge_keys_for(
            prior_source.map(|item| (&item.merge_keys, item.ascending)),
            &times,
        );
        matched_sources.push(SourceMatches {
            source_id,
            generation,
            high_watermark: target,
            sequences: sequences.finish(),
            times,
            groups,
            bounds: source_bounds,
            merge_keys,
            ascending,
        });
    }
    if request.constraints.time_basis != lvu::TimeBasis::Capture
        && request.constraints.capture_time.is_some()
        && (event_time_missing > 0 || event_time_invalid > 0)
    {
        let event_diagnostic = format!(
            "event time: {event_time_missing} missing, {event_time_invalid} invalid/ambiguous; unmatched (no capture-time fallback; basis {:?})",
            request.constraints.time_basis
        );
        let combined = match runtime_diagnostic {
            Some(existing) => format!("{existing}; {event_diagnostic}"),
            None => event_diagnostic,
        };
        runtime_diagnostic = Some(bounded_text(combined, 512));
    }
    if cancelled.load(Ordering::Acquire) {
        return;
    }
    let index_bytes = reservation.bytes;
    let membership = reservation.finish(
        matched_sources,
        count,
        enrichment.iter().map(|stage| stage.name.clone()).collect(),
        derived,
        derived_errors,
        color_matches,
        request.constraints.color_rules.clone(),
        advanced.clone(),
        enrichment.clone(),
        config.page_bytes,
        evaluation_batches,
        event_time_missing,
        event_time_invalid,
        request.constraints.time_basis,
        grouping_rule.is_some(),
        prior_membership.clone(),
    );
    prepared.insert(
        cache_key,
        PreparedDefinition {
            revision: request.revision,
            constraints: request.constraints.clone(),
            text,
            advanced,
            enrichment,
            grouping: grouping_rule,
            schema_seed,
            schema,
            checkpoints,
            membership: Some(Arc::clone(&membership)),
        },
    );
    let _ = send_update(
        tx,
        Update::Publish {
            request,
            published: Published::Filtered { membership },
            scanned,
            watermarks,
            index_bytes,
            diagnostic: runtime_diagnostic,
            token: Arc::clone(&cancelled),
        },
        cancelled.as_ref(),
    );
}

fn bounded_text(mut value: String, maximum_bytes: usize) -> String {
    if value.len() > maximum_bytes {
        let mut end = maximum_bytes;
        while !value.is_char_boundary(end) {
            end -= 1;
        }
        value.truncate(end);
    }
    value
}

/// The expression steps of the chain. Command steps are not compiled: their
/// output is joined as columns, and their place in the order is enforced by
/// `validate_command_order`.
fn native_enrichment_definitions(
    constraints: &lvu::QueryConstraints,
) -> Vec<NativeEnrichmentDefinition> {
    constraints
        .enrichments
        .iter()
        .filter(|definition| !definition.is_command())
        .map(|definition| NativeEnrichmentDefinition {
            id: NativeEnrichmentStageId(definition.id.0.clone()),
            source: definition.source.clone(),
        })
        .collect()
}

/// A step may read a command's output only if the command step comes before
/// it in the chain (docs/command-enrichment.md): the order is the meaning.
fn validate_command_order(
    chain: &[lvu::EnrichmentDefinition],
    compiled: &[CompiledEnrichment],
) -> Result<(), String> {
    let commands: Vec<(usize, &str)> = chain
        .iter()
        .enumerate()
        .filter_map(|(index, step)| step.output_prefix().map(|name| (index, name)))
        .collect();
    if commands.is_empty() {
        return Ok(());
    }
    for definition in compiled {
        let Some(position) = chain
            .iter()
            .position(|step| step.id.0 == definition.definition.id.0)
        else {
            continue;
        };
        for stage in definition.stages() {
            for dependency in stage.definition.dependencies() {
                if let Some((command_position, name)) = commands
                    .iter()
                    .find(|(_, name)| {
                        command_columns::command_prefix(dependency, std::iter::once(*name))
                            .is_some()
                    })
                    .copied()
                    && command_position > position
                {
                    return Err(format!(
                        "step {} reads {dependency} before command step {name} runs; move the command step above it",
                        stage.name
                    ));
                }
            }
        }
    }
    Ok(())
}

/// The stages that read, directly or through another stage, a column of a
/// command step in `unpublished` (by output name).
fn waiting_stages(stages: &[EnrichmentStage], unpublished: &[String]) -> HashSet<String> {
    let mut waiting = HashSet::new();
    if unpublished.is_empty() {
        return waiting;
    }
    for stage in stages {
        if stage.definition.dependencies().iter().any(|dependency| {
            waiting.contains(dependency)
                || command_columns::command_prefix(
                    dependency,
                    unpublished.iter().map(String::as_str),
                )
                .is_some()
        }) {
            waiting.insert(stage.name.clone());
        }
    }
    waiting
}

/// Every `<name>.<field>` column of a command step in `chain` that a stage,
/// the filter or the search reads.
fn required_command_columns(
    chain: &[lvu::EnrichmentDefinition],
    stages: &[EnrichmentStage],
    advanced: Option<&lvu_query::CompiledDefinition>,
    text: Option<&TextSearch>,
) -> BTreeSet<String> {
    let names: Vec<&str> = chain
        .iter()
        .filter_map(|step| step.output_prefix())
        .collect();
    if names.is_empty() {
        return BTreeSet::new();
    }
    stages
        .iter()
        .flat_map(|stage| stage.definition.dependencies().iter())
        .chain(
            advanced
                .into_iter()
                .flat_map(|filter| filter.dependencies().iter()),
        )
        .chain(
            text.into_iter()
                .flat_map(|search| search.dependencies().iter()),
        )
        .filter(|dependency| {
            command_columns::command_prefix(dependency, names.iter().copied()).is_some()
        })
        .cloned()
        .collect()
}

fn changed_enrichment_outputs(
    base: &[lvu::EnrichmentDefinition],
    candidate: &[lvu::EnrichmentDefinition],
    compiled: &[CompiledEnrichment],
) -> HashSet<String> {
    let unchanged = |id: &str, source: &str, definitions: &[lvu::EnrichmentDefinition]| {
        definitions
            .iter()
            .any(|item| item.id.0 == id && item.source == source)
    };
    let mut affected_prefixes: HashSet<String> = HashSet::new();
    // A command step that changed, moved in or out: every column named by
    // its prefix counts as changed, so readers of it are re-evaluated.
    for side in [base, candidate] {
        for step in side.iter().filter(|step| step.is_command()) {
            let other = if std::ptr::eq(side, base) {
                candidate
            } else {
                base
            };
            if !other.iter().any(|item| item == step) {
                affected_command_prefixes_into(step, &mut affected_prefixes);
            }
        }
    }
    let mut affected = HashSet::new();
    for definition in compiled {
        if !unchanged(
            &definition.definition.id.0,
            &definition.definition.source,
            base,
        ) {
            affected.extend(definition.outputs.iter().map(|output| output.name.clone()));
        }
    }
    for definition in base {
        if !unchanged(&definition.id.0, &definition.source, candidate) {
            affected.extend(enrichment_output_names(&definition.source));
        }
    }
    for definition in compiled {
        for stage in definition.stages() {
            if stage.definition.dependencies().iter().any(|dependency| {
                command_columns::command_prefix(
                    dependency,
                    affected_prefixes.iter().map(String::as_str),
                )
                .is_some()
            }) {
                affected.insert(stage.name.clone());
            }
        }
    }
    affected
}

fn affected_command_prefixes_into(step: &lvu::EnrichmentDefinition, into: &mut HashSet<String>) {
    if let Some(name) = step.output_prefix() {
        into.insert(name.to_owned());
    }
}

fn enrichment_output_names(source: &str) -> Vec<String> {
    match parse_regex_enrichment(source) {
        Ok(Some(plan)) => plan
            .outputs()
            .iter()
            .map(|output| output.name.clone())
            .collect(),
        _ => source
            .split_once('=')
            .map(|(name, _)| vec![name.trim().to_owned()])
            .unwrap_or_default(),
    }
}

fn propagate_affected_outputs(stages: &[EnrichmentStage], affected: &mut HashSet<String>) {
    loop {
        let mut changed = false;
        for stage in stages {
            if !affected.contains(&stage.name)
                && stage
                    .definition
                    .dependencies()
                    .iter()
                    .any(|dependency| affected.contains(dependency))
            {
                changed |= affected.insert(stage.name.clone());
            }
        }
        if !changed {
            break;
        }
    }
}

fn fail(
    tx: &mpsc::SyncSender<Update>,
    request: &QueryRequest,
    token: &Arc<AtomicBool>,
    purpose: QueryPurpose,
    message: &str,
    limited: bool,
) {
    let _ = tx.send(Update::Failed {
        request: request.clone(),
        purpose,
        message: message.chars().take(512).collect(),
        limited,
        token: Arc::clone(token),
    });
}

fn send_update(tx: &mpsc::SyncSender<Update>, mut update: Update, cancelled: &AtomicBool) -> bool {
    loop {
        match tx.try_send(update) {
            Ok(()) => return true,
            Err(mpsc::TrySendError::Disconnected(_)) => return false,
            Err(mpsc::TrySendError::Full(returned)) => {
                if cancelled.load(Ordering::Acquire) {
                    return false;
                }
                update = returned;
                thread::sleep(Duration::from_millis(2));
            }
        }
    }
}

/// The minimal projection a plain literal search needs: identity and raw text.
///
/// A page is read from one journal, so its source identity is one value however
/// many records it carries. Formatting the UUID per record was 36 bytes of
/// allocation and a hyphenated format call for every row of every scan, which is
/// why this interns the rendered identity and hands Polars borrowed strings.
/// `from_utf8_lossy` also borrows for the overwhelmingly common valid-UTF-8
/// record, so only genuinely invalid bytes are copied before Arrow copies them.
fn literal_frame(records: &[lvu_core::RawRecord]) -> polars::prelude::PolarsResult<DataFrame> {
    let mut rendered: Vec<(lvu_core::SourceId, String)> = Vec::new();
    for record in records {
        let id = record.record_id.source_id;
        if rendered.last().is_none_or(|(known, _)| *known != id)
            && !rendered.iter().any(|(known, _)| *known == id)
        {
            rendered.push((id, id.0.to_string()));
        }
    }
    let source_ids = match rendered.as_slice() {
        // A page is read from one journal, so this is the case that runs.
        [(_, only)] => vec![only.as_str(); records.len()],
        _ => records
            .iter()
            .map(|record| {
                rendered
                    .iter()
                    .find(|(known, _)| *known == record.record_id.source_id)
                    .map_or("", |(_, text)| text.as_str())
            })
            .collect::<Vec<&str>>(),
    };
    let raw = records
        .iter()
        .map(|record| String::from_utf8_lossy(&record.bytes))
        .collect::<Vec<_>>();
    DataFrame::new(
        records.len(),
        vec![
            Column::from(Series::new(lvu_query::SOURCE_ID_COLUMN.into(), source_ids)),
            Column::from(Series::new(
                lvu_query::SEQUENCE_COLUMN.into(),
                records
                    .iter()
                    .map(|record| record.record_id.sequence)
                    .collect::<Vec<_>>(),
            )),
            Column::from(Series::new(
                lvu_query::RAW_COLUMN.into(),
                raw.iter()
                    .map(std::convert::AsRef::as_ref)
                    .collect::<Vec<&str>>(),
            )),
        ],
    )
}

/// The identities at `start..start + len` of the display order.
///
/// A slice of the merged order rather than a walk over concatenated sources:
/// the order is built once when the membership is published.
fn membership_ids(membership: &Membership, start: usize, len: usize) -> Vec<RowId> {
    membership
        .order
        .iter()
        .skip(start)
        .take(len)
        .filter_map(|(source, unit)| {
            let source = membership.sources.get(*source as usize)?;
            let sequence = source.sequences.get(*unit as usize)?;
            Some(RowId::new(source.source_id.clone(), *sequence))
        })
        .collect()
}

#[derive(Clone)]
struct DisplayGroup {
    orphan: bool,
    split: bool,
    oversized: bool,
    pending: bool,
    key_refused: bool,
    configured: bool,
    logical_lines: usize,
    /// Every member the group holds. The stored projection page may be
    /// shorter (see `MAX_CONFIGURED_GROUP_STORED`); this total is what the
    /// head text and details report, never the page length.
    record_total: usize,
    projection: Arc<Vec<DisplayRow>>,
}

/// Scan the membership's ordered records for the first gap past `from`.
///
/// The whole membership is walked in display order — the merged order, which
/// is the order the viewport shows — and only records with a readable basis
/// timestamp take part. A gap is the distance between two consecutive *timed*
/// records, so a run of records with no time neither creates a gap nor hides
/// one. The merge key is not used here: it carries a filled value forward for
/// ordering, and a gap measured against a filled value would be a gap between
/// a record and itself.
fn find_membership_gap(
    membership: &Membership,
    from: Option<&RowId>,
    direction: lvu::GapDirection,
    threshold_nanos: i64,
) -> Option<lvu::GapHit> {
    if threshold_nanos <= 0 {
        return None;
    }
    // (identity, timestamp) for every timed record, in display order. Bounded
    // by the membership cap, which is what bounds the viewport itself.
    let timed: Vec<(RowId, i64)> = membership
        .order
        .iter()
        .filter_map(|(source, unit)| {
            let source = membership.sources.get(*source as usize)?;
            let sequence = source.sequences.get(*unit as usize)?;
            let time = source.times.get(*unit as usize)?;
            (*time != NO_BASIS_TIME)
                .then(|| (RowId::new(source.source_id.clone(), *sequence), *time))
        })
        .collect();
    if timed.len() < 2 {
        return None;
    }
    // Where the search starts. An unknown or untimed `from` starts at the end
    // the direction implies — one *past* the last record when searching
    // backward, so the final gap is reachable — which keeps the key useful when
    // the selected record has no timestamp in this basis.
    let start = from
        .and_then(|row| timed.iter().position(|(id, _)| id == row))
        .unwrap_or(match direction {
            lvu::GapDirection::Forward => 0,
            lvu::GapDirection::Backward => timed.len(),
        });
    let hit = |index: usize| {
        let (row, time) = &timed[index];
        let (previous_row, previous) = &timed[index - 1];
        lvu::GapHit {
            row: row.clone(),
            gap_nanos: time.saturating_sub(*previous),
            previous_unix_nanos: *previous,
            previous_row: previous_row.clone(),
        }
    };
    match direction {
        // The gap *at* the starting row is behind the user already, so the
        // forward search begins with the row after it.
        lvu::GapDirection::Forward => (start + 1..timed.len())
            .find(|index| timed[*index].1.saturating_sub(timed[index - 1].1) > threshold_nanos)
            .map(hit),
        lvu::GapDirection::Backward => (1..start)
            .rev()
            .find(|index| timed[*index].1.saturating_sub(timed[index - 1].1) > threshold_nanos)
            .map(hit),
    }
}

fn membership_display_count(membership: &Membership) -> usize {
    if membership.grouped {
        membership
            .sources
            .iter()
            .map(|source| source.groups.len())
            .sum()
    } else {
        usize::try_from(membership.count).unwrap_or(usize::MAX)
    }
}

fn membership_groups(membership: &Membership, start: usize, len: usize) -> Vec<DisplayGroup> {
    membership
        .order
        .iter()
        .skip(start)
        .take(len)
        .filter_map(|(source, unit)| {
            let group = membership
                .sources
                .get(*source as usize)?
                .groups
                .get(*unit as usize)?;
            Some(DisplayGroup {
                orphan: group.orphan,
                split: group.split,
                oversized: group.oversized,
                pending: group.pending,
                key_refused: group.key_refused,
                configured: group.configured,
                logical_lines: group.logical_lines,
                record_total: group.len,
                projection: Arc::clone(&group.projection),
            })
        })
        .collect()
}

fn membership_group_index(membership: &Membership, wanted: &RowId) -> Option<usize> {
    let (position, source) = membership
        .sources
        .iter()
        .enumerate()
        .find(|(_, source)| source.source_id == wanted.source_id)?;
    let sequence = source.sequences.binary_search(&wanted.sequence).ok()?;
    let group = group_index_for_sequence(&source.groups, sequence)?;
    membership
        .ranks
        .get(position)
        .and_then(|ranks| ranks.get(group))
        .map(|rank| *rank as usize)
}

fn group_index_for_sequence(groups: &Appended<GroupRange>, sequence_index: usize) -> Option<usize> {
    // Groups are ordered by `start`, so this is the same partition point the
    // slice form used, expressed over the published chunks.
    let boundary = groups.partition_point(|group| group.start <= sequence_index);
    let index = boundary.checked_sub(1)?;
    let group = groups.get(index)?;
    (sequence_index < group.start.saturating_add(group.len)).then_some(index)
}

fn membership_group_for_id(membership: &Membership, wanted: &RowId) -> Option<DisplayGroup> {
    let index = membership_group_index(membership, wanted)?;
    membership_groups(membership, index, 1).pop()
}

fn project_group(group: DisplayGroup) -> DisplayRow {
    let mut members = group.projection.to_vec();
    let shown = members.len();
    // The total is the group's membership, never the stored page: a capped
    // page must not shrink the reported event.
    let record_count = group.record_total.max(shown).max(1);
    let truncated = shown < record_count;
    let mut head = members.remove(0);
    head.details.push((
        "grouping".into(),
        "display-only; physical records unchanged".into(),
    ));
    head.details
        .push(("group_line_count".into(), group.logical_lines.to_string()));
    head.details
        .push(("group_record_count".into(), record_count.to_string()));
    if group.orphan {
        head.details
            .push(("group_boundary".into(), "orphan continuation".into()));
    }
    if group.split {
        head.details
            .push(("group_overflow".into(), "bounded split".into()));
    }
    if group.pending {
        head.details.push((
            "group_pending".into(),
            "start evaluation unavailable for this group; shown alone until its enrichment settles"
                .into(),
        ));
    }
    if group.key_refused {
        head.details.push((
            "group_key_unavailable".into(),
            format!(
                "run key exceeds the {}-byte exact-identity bound; left unfolded rather than merged",
                lvu_query::MAX_EXACT_KEY_BYTES
            ),
        ));
    }
    if group.oversized {
        head.details.push((
            "group_oversized_record".into(),
            format!(
                "leading physical record exceeds the {MAX_GROUP_PAYLOAD_BYTES}-byte payload soft group limit; preserved alone"
            ),
        ));
    }
    // Configured groups render member text first: a full UUID:sequence
    // prefix hides content on narrow terminals and defeats multiline review.
    // Stable IDs stay in the same details, after the text. Legacy groups keep
    // the established `id: text` shape byte for byte.
    let member_line = |id: &RowId, text: &str| {
        if group.configured {
            format!("{text} [{id}]")
        } else {
            format!("{id}: {text}")
        }
    };
    let first_text = head.text.clone();
    let first_id = head.id.clone();
    head.details
        .push(("group_line_1".into(), member_line(&first_id, &first_text)));
    for (index, member) in members.into_iter().enumerate() {
        head.details.push((
            format!("group_line_{}", index + 2),
            member_line(&member.id, &member.text),
        ));
    }
    if truncated {
        head.details.push((
            "group_truncated".into(),
            format!(
                "showing first {shown} of {record_count} records; every member stays in the source view"
            ),
        ));
    }
    if record_count > 1 || group.orphan || group.pending {
        let label = if group.pending {
            "pending grouping evaluation"
        } else if group.orphan {
            "orphan continuation"
        } else if record_count != group.logical_lines {
            "physical records"
        } else {
            "physical lines"
        };
        head.text = if truncated {
            format!(
                "{}  [{} {label}, first {shown} shown]",
                head.text, record_count
            )
        } else if record_count != group.logical_lines && !group.orphan && !group.pending {
            format!(
                "{}  [{} {label} / {} logical {}]",
                head.text,
                record_count,
                group.logical_lines,
                if group.logical_lines == 1 {
                    "line"
                } else {
                    "lines"
                }
            )
        } else {
            format!("{}  [{} {label}]", head.text, record_count)
        };
    }
    head
}

/// Where a record sits in the display order.
///
/// The source is found by identity, the record inside it by binary search on
/// its ascending sequences, and the display position read from that source's
/// rank vector — the cost the prefix sum had, with the order no longer implied
/// by the source list.
fn membership_index(membership: &Membership, wanted: &RowId) -> Option<usize> {
    let (position, source) = membership
        .sources
        .iter()
        .enumerate()
        .find(|(_, source)| source.source_id == wanted.source_id)?;
    let unit = source.sequences.binary_search(&wanted.sequence).ok()?;
    membership
        .ranks
        .get(position)
        .and_then(|ranks| ranks.get(unit))
        .map(|rank| *rank as usize)
}

/// Whether two constraint snapshots describe the same *view definition*,
/// ignoring the presentation-only colour rules.
fn definitions_match(left: &lvu::QueryConstraints, right: &lvu::QueryConstraints) -> bool {
    if left.color_rules == right.color_rules {
        return left == right;
    }
    let mut left = left.clone();
    let mut right = right.clone();
    left.color_rules.clear();
    right.color_rules.clear();
    left == right
}

#[derive(Clone)]
enum ContinuationRule {
    Auto,
    Custom(regex::bytes::Regex),
    /// Consecutive records whose exact typed enrichment key matches form one
    /// run. The key comes from [`lvu_query::exact_key_flags`]; this never
    /// compares truncated display strings.
    Run {
        column: String,
    },
    /// Every non-null value in the enrichment column opens an event; every
    /// record until the next non-null value continues it. Flags come from
    /// [`lvu_query::non_null_flags`]; a produced null is an evaluated
    /// continue, never unknown.
    Filter {
        column: String,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AutoLine {
    Start,
    Continuation,
    Ambiguous,
}

impl ContinuationRule {
    fn is_auto(&self) -> bool {
        matches!(self, Self::Auto)
    }

    /// A configured enrichment rule (Run/Filter) rather than a legacy lexical
    /// one. Configured rules segment engine-computed flags and never read raw
    /// bytes for classification.
    fn is_configured(&self) -> bool {
        matches!(self, Self::Run { .. } | Self::Filter { .. })
    }

    fn configured_column(&self) -> Option<&str> {
        match self {
            Self::Run { column } | Self::Filter { column } => Some(column),
            Self::Auto | Self::Custom(_) => None,
        }
    }

    fn parse(source: &str) -> Result<Self, String> {
        match lvu::grouping::parse_grouping(source)? {
            lvu::grouping::GroupingSpec::Auto => return Ok(Self::Auto),
            lvu::grouping::GroupingSpec::Run { column } => {
                return Ok(Self::Run {
                    column: column.to_owned(),
                });
            }
            lvu::grouping::GroupingSpec::Filter { column } => {
                return Ok(Self::Filter {
                    column: column.to_owned(),
                });
            }
            lvu::grouping::GroupingSpec::Custom(_) => {}
        }
        if source.len() > MAX_GROUP_REGEX_BYTES {
            return Err(format!(
                "display grouping regex exceeds the {MAX_GROUP_REGEX_BYTES}-byte source limit"
            ));
        }
        regex::bytes::RegexBuilder::new(source)
            .size_limit(MAX_GROUP_REGEX_COMPILED_BYTES)
            .nest_limit(MAX_GROUP_REGEX_NESTING)
            .build()
            .map(Self::Custom)
            .map_err(|error| format!("invalid display grouping regex: {error}"))
    }

    fn custom_matches(&self, bytes: &[u8]) -> Option<bool> {
        match self {
            Self::Auto => None,
            Self::Custom(regex) => Some(regex.is_match(bytes)),
            // Configured rules never classify raw bytes: the engine already
            // expressed the criterion as per-record flags.
            Self::Run { .. } | Self::Filter { .. } => None,
        }
    }

    fn auto_line(&self, bytes: &[u8], open: bool) -> Option<AutoLine> {
        matches!(self, Self::Auto).then(|| classify_auto_line(bytes, open))
    }
}

/// One record's configured-grouping input for the current batch.
///
/// Every variant is an engine verdict, never a view inference: `Start` and
/// `Continue` come from the native `is_not_null()` kernel, run keys from
/// exact typed encodings proven against native `==`, and `Unknown` marks
/// only genuinely unevaluated batches (pending command output, a failed
/// stage, or a batch missing the column). A produced null is
/// `Continue`/`KeyNull`, never `Unknown`; a produced NaN stands alone like
/// native equality reports it.
#[derive(Clone, Debug, PartialEq)]
enum ConfiguredFlag {
    Start,
    Continue,
    Key(Vec<u8>),
    KeyNull,
    KeyNan,
    KeyOversize,
    Unknown,
}

/// Engine-computed grouping flags for one source batch: either the whole
/// batch is unevaluated, or flags are keyed by record sequence.
enum BatchFlags {
    Unknown,
    Known(HashMap<u64, ConfiguredFlag>),
}

fn configured_batch_flags(
    rule: &ContinuationRule,
    enrichment: &[EnrichmentStage],
    waiting: &HashSet<String>,
    result: &lvu_query::BatchResult,
    source_id: &str,
) -> Result<BatchFlags, String> {
    let Some(column) = rule.configured_column() else {
        return Err("grouping rule needs an enrichment column".to_owned());
    };
    if !enrichment.iter().any(|stage| stage.name == column) {
        return Err(format!(
            "grouping column {column:?} is not an accepted enrichment output; add it in Enrichment first"
        ));
    }
    let failed = result.diagnostics.iter().any(|diagnostic| {
        diagnostic.field.as_deref() == Some(column) && diagnostic.state == DerivedState::Error
    });
    if failed || waiting.contains(column) {
        return Ok(BatchFlags::Unknown);
    }
    let mut known = HashMap::new();
    match rule {
        ContinuationRule::Filter { .. } => {
            // Batch-scoped absence (a column this batch does not carry)
            // is unevaluated, never a silent false; the batch diagnostics
            // already name the cause.
            let Ok(mask) = non_null_flags(&result.enriched_rows, column) else {
                return Ok(BatchFlags::Unknown);
            };
            for (id, start) in mask {
                if id.source_id == source_id {
                    known.insert(id.sequence, {
                        if start {
                            ConfiguredFlag::Start
                        } else {
                            ConfiguredFlag::Continue
                        }
                    });
                }
            }
        }
        ContinuationRule::Run { .. } => {
            // An unsupported dtype is deterministic for the chain: reject
            // the candidate actionably with the whole last-good view intact.
            // Only genuinely batch-scoped absence stays unevaluated.
            let keys = match exact_key_flags(&result.enriched_rows, column) {
                Ok(keys) => keys,
                Err(lvu_query::KeyError::Unavailable(_)) => {
                    return Ok(BatchFlags::Unknown);
                }
                Err(lvu_query::KeyError::Unsupported(message)) => {
                    return Err(message);
                }
            };
            for (id, key) in keys {
                if id.source_id == source_id {
                    known.insert(
                        id.sequence,
                        match key {
                            lvu_query::KeyFlag::Value(bytes) => ConfiguredFlag::Key(bytes),
                            lvu_query::KeyFlag::Null => ConfiguredFlag::KeyNull,
                            lvu_query::KeyFlag::Nan => ConfiguredFlag::KeyNan,
                            lvu_query::KeyFlag::Oversize => ConfiguredFlag::KeyOversize,
                        },
                    );
                }
            }
        }
        ContinuationRule::Auto | ContinuationRule::Custom(_) => {
            return Err("lexical grouping rules have no engine flags".to_owned());
        }
    }
    Ok(BatchFlags::Known(known))
}

fn classify_auto_line(bytes: &[u8], open: bool) -> AutoLine {
    // Classification observes, but never rewrites, original bytes. Limit the
    // lexical prefix so a huge physical record cannot turn grouping into an
    // unbounded presentation scan.
    let prefix = auto_lexical_prefix(bytes);
    // ANSI is not removed or copied into derived data. For start recognition
    // only, step over leading CSI styling so a coloured timestamp/level has
    // the same classification as its uncoloured form. Complete display text
    // is still cleaned solely by `lvu::ansi::without_ansi` in the renderer.
    let text = String::from_utf8_lossy(prefix);
    let trimmed = text.trim_matches(['\r', '\n']);
    let leading = trimmed.trim_start_matches([' ', '\t']);
    let lower = leading.to_ascii_lowercase();

    let bytes = leading.as_bytes();
    let iso_timestamp = bytes.len() >= 10
        && bytes[0..4].iter().all(u8::is_ascii_digit)
        && bytes[4] == b'-'
        && bytes[5..7].iter().all(u8::is_ascii_digit)
        && bytes[7] == b'-'
        && bytes[8..10].iter().all(u8::is_ascii_digit);
    let level = [
        "trace", "debug", "info", "warn", "warning", "error", "fatal", "critical",
    ]
    .iter()
    .any(|level| {
        lower.starts_with(level)
            && lower[level.len()..]
                .chars()
                .next()
                .is_none_or(|next| !next.is_ascii_alphanumeric())
    });
    let known_head = lower.starts_with("traceback (most recent call last):")
        || lower.starts_with("exception in thread ")
        || lower.starts_with("goroutine ")
        || lower.starts_with("panic:");

    // A source-shaped event header wins even when upstream pretty-printing
    // indented it. Generic indentation is only continuation evidence after
    // credible starts have been excluded.
    if iso_timestamp || level || known_head {
        return AutoLine::Start;
    }

    if leading.is_empty() {
        return if open {
            AutoLine::Continuation
        } else {
            AutoLine::Ambiguous
        };
    }
    if trimmed.len() != leading.len()
        || lower.starts_with("at ")
        || lower.starts_with("caused by:")
        || lower.starts_with("suppressed:")
        || (lower.starts_with("...") && lower.ends_with(" more"))
        || (open && matches!(leading.as_bytes().first(), Some(b'}' | b']' | b')')))
        || (open
            && ["file \"", "runtime error:"]
                .iter()
                .any(|marker| lower.starts_with(marker)))
        || (open
            && lower
                .split_once(':')
                .is_some_and(|(kind, _)| kind.ends_with("error") || kind.ends_with("exception")))
        || (open && leading.ends_with(')') && leading.contains('.') && !leading.contains(' '))
    {
        return AutoLine::Continuation;
    }

    let structured =
        trimmed.len() == leading.len() && (leading.starts_with('{') || leading.starts_with('['));
    let yaml_document =
        trimmed.len() == leading.len() && (leading == "---" || lower.starts_with("%yaml "));
    if structured || yaml_document {
        AutoLine::Start
    } else {
        AutoLine::Ambiguous
    }
}

fn auto_lexical_prefix(bytes: &[u8]) -> &[u8] {
    let prefix = &bytes[..bytes.len().min(512)];
    let mut offset = 0;
    while prefix.get(offset..offset + 2) == Some(b"\x1b[") {
        offset += 2;
        while let Some(byte) = prefix.get(offset) {
            offset += 1;
            if (0x40..=0x7e).contains(byte) {
                break;
            }
        }
    }
    &prefix[offset..]
}

fn is_auto_structured_start(bytes: &[u8]) -> bool {
    let text = String::from_utf8_lossy(auto_lexical_prefix(bytes));
    let trimmed = text.trim_matches(['\r', '\n']);
    let leading = trimmed.trim_start_matches([' ', '\t']);
    trimmed.len() == leading.len() && matches!(leading.as_bytes().first(), Some(b'{' | b'['))
}

/// Bounded JSON-like bracket balance for presentation state. Quotes and
/// escapes are observed so braces in strings do not keep a completed payload
/// open. `None` means the lexical prefix ended in an incomplete string/escape;
/// callers conservatively keep the group open until more physical input.
fn structured_depth(bytes: &[u8]) -> Option<u16> {
    let mut depth = 0u16;
    let mut quoted = false;
    let mut escaped = false;
    let bytes = &bytes[..bytes.len().min(512)];
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if !quoted && byte == b'\x1b' && bytes.get(index + 1) == Some(&b'[') {
            index += 2;
            while let Some(parameter) = bytes.get(index) {
                index += 1;
                if (0x40..=0x7e).contains(parameter) {
                    break;
                }
            }
            continue;
        }
        index += 1;
        if quoted {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                quoted = false;
            }
            continue;
        }
        match byte {
            b'"' => quoted = true,
            b'{' | b'[' => depth = depth.saturating_add(1),
            b'}' | b']' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    (!quoted && !escaped).then_some(depth)
}

/// One correlation lookup: resolve the frozen record's typed value, then
/// collect each source's observed field names so differing key names can be
/// mapped explicitly. Both passes are bounded and check cancellation between
/// pages; neither rewrites, reorders or consumes any record.
/// Records gathered before one aggregation step. Bounded by count rather than
/// bytes because what it bounds is the number of Polars plans a pass runs, and
/// the records themselves are already bounded by the page that produced them.
const STATS_AGGREGATE_RECORDS: usize = 65_536;

/// One pass over a view's membership, aggregating one column.
///
/// The pass reads the journal because a field's values live in the record's
/// bytes, and keeps the records the membership matched — `SourceMatches`
/// already holds those sequences in ascending order, so deciding whether a
/// record is in the view is a binary search rather than a second evaluation of
/// the filter. A raw view has no membership and every record counts.
///
/// Cancellation is between pages, like every other bounded worker here: the
/// selection moving supersedes this, and a superseded pass costs at most one
/// more page before it notices.
fn field_stats_loop(
    request: FieldStatsRequest,
    membership: Option<Arc<Membership>>,
    handles: Vec<AnySourceHandle>,
    page_records: usize,
    page_bytes: usize,
    tx: mpsc::SyncSender<Update>,
    cancel: Arc<AtomicBool>,
) {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            let _ = tx.send(Update::FieldStats(Box::new(FieldStats {
                generation: request.generation,
                view_id: request.view_id,
                column: request.label,
                scanned: 0,
                result: Err(format!("field statistics runtime: {error}")),
            })));
            return;
        }
    };
    let mut scanned = 0u64;
    let outcome = field_stats_pass(
        &runtime,
        &request,
        membership.as_deref(),
        &handles,
        page_records,
        page_bytes,
        &cancel,
        &mut scanned,
    );
    if cancel.load(Ordering::Acquire) {
        return;
    }
    let _ = tx.send(Update::FieldStats(Box::new(FieldStats {
        generation: request.generation,
        view_id: request.view_id,
        column: request.label,
        scanned,
        result: outcome,
    })));
}

#[allow(clippy::too_many_arguments)]
fn field_stats_pass(
    runtime: &tokio::runtime::Runtime,
    request: &FieldStatsRequest,
    membership: Option<&Membership>,
    handles: &[AnySourceHandle],
    page_records: usize,
    page_bytes: usize,
    cancel: &Arc<AtomicBool>,
    scanned: &mut u64,
) -> Result<lvu_query::column_stats::ColumnAggregate, String> {
    let mut aggregator = lvu_query::column_stats::ColumnAggregator::new(
        request.label.clone(),
        Some(request.kind.predicate(&request.label)),
        request.top,
        request.distinct_cap,
    );
    for handle in handles {
        let source_id = handle.source_id().0.to_string();
        let matched = membership.and_then(|membership| {
            membership
                .sources
                .iter()
                .find(|source| source.source_id == source_id)
        });
        // A source the filter matched nothing in contributes nothing, and
        // reading its journal to discover that would be work for no answer.
        if membership.is_some() && matched.is_none_or(|source| source.sequences.is_empty()) {
            continue;
        }
        let mut offset = 0u64;
        let mut schema = SchemaContext::default();
        let mut buffered: Vec<lvu_core::RawRecord> = Vec::new();
        loop {
            if cancel.load(Ordering::Acquire) {
                return Err("field statistics cancelled".into());
            }
            let page = runtime
                .block_on(handle.read_page(offset, page_records, page_bytes))
                .map_err(|error| error.to_string())?;
            if page.records.is_empty() {
                break;
            }
            let end_of_journal = page.end_of_journal;
            offset = page.next_offset;
            let records = match matched {
                None => page.records,
                Some(source) => page
                    .records
                    .into_iter()
                    .filter(|record| {
                        source
                            .sequences
                            .binary_search(&record.record_id.sequence)
                            .is_ok()
                    })
                    .collect::<Vec<_>>(),
            };
            if !records.is_empty() {
                *scanned = scanned.saturating_add(records.len() as u64);
                buffered.extend(records);
            }
            // Aggregating costs a fixed amount per batch — several Polars plans
            // and their collects — so a pass that aggregated once per journal
            // page paid that 151 times over 620k records. Pages stay whatever
            // size the journal wants; how much is aggregated at once is this
            // pass's own business.
            if (end_of_journal || buffered.len() >= STATS_AGGREGATE_RECORDS) && !buffered.is_empty()
            {
                let records = std::mem::take(&mut buffered);
                // Only the column being counted. The canonical projection
                // builds one column per field it finds, ten of metadata and
                // three copies of the record's text; a pass over one field
                // needs none of that, and paying for it per batch is what made
                // this cost several times a filter scan over the same records.
                let frame =
                    lvu_query::records_to_field_column(&records, &mut schema, &request.column)
                        .map_err(|error| error.to_string())?;
                // A field absent from this batch is absent, not an error: the
                // schema widens as records arrive and older records simply do
                // not carry a field later ones introduced.
                if frame
                    .column(&request.column)
                    .is_ok_and(|column| column.dtype() != &polars::prelude::DataType::Null)
                {
                    let cast = frame
                        .clone()
                        .lazy()
                        .select([lvu_query::column_stats::column_expr(
                            &request.column,
                            request.json_path.as_deref(),
                            &request.label,
                        )
                        .cast(request.kind.cast())
                        .alias(&request.label)])
                        .collect()
                        .map_err(|error| error.to_string())?;
                    aggregator.push(&cast)?;
                } else {
                    aggregator.push_absent(records.len())?;
                }
            }
            if end_of_journal {
                break;
            }
        }
    }
    aggregator.finish()
}

fn correlation_lookup_loop(
    request: CorrelationLookupRequest,
    handles: Vec<AnySourceHandle>,
    tx: mpsc::SyncSender<Update>,
    cancel: Arc<AtomicBool>,
) {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            let _ = tx.send(Update::Correlation(Box::new(CorrelationLookup {
                generation: request.generation,
                origin_view_id: request.origin_view_id,
                result: Err(format!("correlation runtime: {error}")),
            })));
            return;
        }
    };
    let result = correlation_lookup(&runtime, &request, &handles, &cancel);
    if cancel.load(Ordering::Acquire) {
        return;
    }
    let _ = tx.send(Update::Correlation(Box::new(CorrelationLookup {
        generation: request.generation,
        origin_view_id: request.origin_view_id.clone(),
        result,
    })));
}

fn correlation_lookup(
    runtime: &tokio::runtime::Runtime,
    request: &CorrelationLookupRequest,
    handles: &[AnySourceHandle],
    cancel: &Arc<AtomicBool>,
) -> Result<CorrelationCandidate, String> {
    let origin = handles
        .iter()
        .find(|handle| handle.source_id() == request.origin.source_id)
        .ok_or_else(|| "the record's source is no longer open".to_owned())?;
    let value = correlation_origin_value(runtime, origin, request, cancel)?;
    let mut sources = Vec::with_capacity(request.sources.len());
    for source_id in &request.sources {
        let Some(handle) = handles
            .iter()
            .find(|handle| handle.source_id() == *source_id)
        else {
            continue;
        };
        let (fields, incomplete) = correlation_field_names(runtime, handle, cancel)?;
        sources.push(CorrelationSourceFields {
            source_id: *source_id,
            fields,
            incomplete,
        });
    }
    Ok(CorrelationCandidate {
        field: request.field.clone(),
        value,
        sources,
    })
}

/// The typed value the correlation equals, read from the record's original
/// bytes. The displayed string is a projection and is never used here.
fn correlation_origin_value(
    runtime: &tokio::runtime::Runtime,
    handle: &AnySourceHandle,
    request: &CorrelationLookupRequest,
    cancel: &Arc<AtomicBool>,
) -> Result<lvu_core::ExactScalar, String> {
    let mut offset = 0u64;
    let mut records = 0u64;
    let mut bytes = 0u64;
    loop {
        if cancel.load(Ordering::Acquire) {
            return Err("correlation cancelled".into());
        }
        if records >= MAX_CORRELATION_ORIGIN_RECORDS || bytes >= MAX_CORRELATION_ORIGIN_BYTES {
            return Err(format!(
                "record {} is beyond the bounded correlation scan of this source; \
                 no correlation was applied",
                request.origin.sequence
            ));
        }
        let page = runtime
            .block_on(handle.read_page(offset, 512, 1024 * 1024))
            .map_err(|error| error.to_string())?;
        if page.records.is_empty() {
            return Err("the record is no longer in this source's journal".into());
        }
        for record in &page.records {
            records = records.saturating_add(1);
            bytes = bytes.saturating_add(record.bytes.len() as u64);
            if record.record_id.sequence == request.origin.sequence {
                return lvu_query::resolve_exact_field(record, &request.field)
                    .map_err(|error| error.to_string());
            }
            if record.record_id.sequence > request.origin.sequence {
                return Err("the record is no longer in this source's journal".into());
            }
        }
        if page.end_of_journal && page.next_offset == offset {
            return Err("the record is no longer in this source's journal".into());
        }
        offset = page.next_offset;
    }
}

/// A bounded sample of a source's structured field names. Reports explicitly
/// when the sample stopped early instead of implying it saw everything.
fn correlation_field_names(
    runtime: &tokio::runtime::Runtime,
    handle: &AnySourceHandle,
    cancel: &Arc<AtomicBool>,
) -> Result<(Vec<String>, bool), String> {
    let mut names: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut offset = 0u64;
    let mut records = 0u64;
    let mut bytes = 0u64;
    loop {
        if cancel.load(Ordering::Acquire) {
            return Err("correlation cancelled".into());
        }
        if records >= MAX_CORRELATION_NAME_RECORDS
            || bytes >= MAX_CORRELATION_NAME_BYTES
            || names.len() >= MAX_CORRELATION_FIELD_NAMES
        {
            return Ok((
                names
                    .into_iter()
                    .take(MAX_CORRELATION_FIELD_NAMES)
                    .collect(),
                true,
            ));
        }
        let page = runtime
            .block_on(handle.read_page(offset, 256, 512 * 1024))
            .map_err(|error| error.to_string())?;
        if page.records.is_empty() {
            return Ok((names.into_iter().collect(), false));
        }
        for record in &page.records {
            records = records.saturating_add(1);
            bytes = bytes.saturating_add(record.bytes.len() as u64);
            for name in lvu_query::structured_field_names(record, MAX_CORRELATION_FIELD_NAMES) {
                names.insert(name);
            }
        }
        if page.end_of_journal && page.next_offset == offset {
            return Ok((names.into_iter().collect(), false));
        }
        offset = page.next_offset;
    }
}

#[cfg(test)]
mod grouping_tests {
    use super::*;

    #[test]
    fn rust_regex_semantics_and_complexity_limits_are_enforced() {
        let rule = ContinuationRule::parse(r"^(?:[[:space:]]+|Caused\s+by:|\[continued\]\s{1,3})")
            .unwrap();
        assert_eq!(rule.custom_matches(b"  at frame"), Some(true));
        assert_eq!(rule.custom_matches(b"Caused   by: disk"), Some(true));
        assert_eq!(rule.custom_matches(b"[continued] \xff"), Some(true));
        assert_eq!(rule.custom_matches(b"ordinary event"), Some(false));
        let auto_token = std::hint::black_box(lvu::grouping::AUTO_GROUPING_TOKEN);
        assert!(regex::bytes::Regex::new(auto_token).is_err());
        assert!(ContinuationRule::parse(r"(?=lookaround)").is_err());
        assert!(ContinuationRule::parse(r"^(a)\1$").is_err());

        let nested = format!("^{}x{}", "(".repeat(65), ")".repeat(65));
        assert!(ContinuationRule::parse(&nested).is_err());
        assert!(ContinuationRule::parse(&"x".repeat(MAX_GROUP_REGEX_BYTES + 1)).is_err());
    }

    #[test]
    fn automatic_grouping_is_conservative_across_common_multiline_shapes() {
        for start in [
            b"2026-09-09T12:00:00Z ERROR failed".as_slice(),
            b"ERROR request failed",
            b"Traceback (most recent call last):",
            b"Exception in thread \"main\" java.lang.IllegalStateException",
            b"goroutine 17 [running]:",
            b"{",
            b"---",
            b"  2026-09-09T12:00:00Z INFO indented but new",
            b"  ERROR indented but new",
            b"\x1b[31mERROR coloured\x1b[0m",
        ] {
            assert_eq!(
                classify_auto_line(start, false),
                AutoLine::Start,
                "{start:?}"
            );
        }
        for continuation in [
            b"  wrapped message".as_slice(),
            b"\tat main.go:42",
            b"Caused by: java.io.IOException",
            b"Suppressed: secondary",
            b"ValueError: bad payload",
            b"}",
            b"",
            b"  invalid \xff",
        ] {
            assert_eq!(
                classify_auto_line(continuation, true),
                AutoLine::Continuation,
                "{continuation:?}"
            );
        }
        for stray in [b"ordinary standalone".as_slice(), b"", b"ValueError: stray"] {
            assert_eq!(
                classify_auto_line(stray, false),
                AutoLine::Ambiguous,
                "{stray:?}"
            );
        }
    }

    #[test]
    fn automatic_group_span_is_fixed_to_persisted_head_capture_time() {
        let first = 1_700_000_000_000_000_000;
        assert!(auto_group_within_span(first, first, first));
        assert!(auto_group_within_span(
            first,
            first,
            first + MAX_AUTO_GROUP_SPAN_NANOS
        ));
        assert!(!auto_group_within_span(
            first,
            first,
            first + MAX_AUTO_GROUP_SPAN_NANOS + 1
        ));
        assert!(!auto_group_within_span(first, first, first - 1));
        assert!(!auto_group_within_span(first, first + 100, first + 50));
        assert!(!auto_group_within_span(i64::MIN, i64::MIN, i64::MAX));
    }

    #[test]
    fn structured_depth_ignores_quoted_braces_and_observes_top_level_close() {
        assert_eq!(structured_depth(br#"{"text":"}"}"#), Some(0));
        assert_eq!(structured_depth(br#"{"items":[1,2]}"#), Some(0));
        assert_eq!(structured_depth(b"\x1b[31m{\x1b[0m}\n"), Some(0));
        assert_eq!(structured_depth(br#"{"items":["#), Some(2));
        assert_eq!(structured_depth(br#"{"unterminated"#), None);
    }

    #[test]
    fn group_state_bytes_prices_run_key_capacity() {
        let plain = GroupRange {
            start: 0,
            len: 200,
            logical_lines: 200,
            payload_bytes: 0,
            stream: lvu_core::StreamKind::File,
            orphan: false,
            split: false,
            oversized: false,
            pending: false,
            run_key: None,
            key_refused: false,
            configured: true,
            auto_open: false,
            auto_structured: false,
            auto_structure_depth: 0,
            partial_open: false,
            partial_truncated: false,
            structure_truncated: false,
            partial_prefix: Vec::new(),
            structure_prefix: Vec::new(),
            acquisition_id: [0; 16],
            last_chunk: lvu_core::ChunkPosition::Complete,
            first_capture_nanos: 0,
            last_capture_nanos: 0,
            projection: Arc::new(Vec::new()),
        };
        let mut keyed = plain.clone();
        keyed.run_key = Some(vec![7u8; 100]);
        // Exactly the key heap, no more: prior-generation accounting prices
        // the same shape on refresh.
        assert_eq!(group_state_bytes(&keyed) - group_state_bytes(&plain), 100);
        assert_eq!(
            group_projection_bytes(&keyed) - group_projection_bytes(&plain),
            100
        );
    }

    #[test]
    fn group_projection_bytes_counts_stored_pages_not_members() {
        let row = |sequence: u64| DisplayRow {
            id: RowId::new("s", sequence),
            timestamp: String::new(),
            captured_at_unix_nanos: None,
            level: String::new(),
            text: "x".to_owned(),
            details: Vec::new(),
            fields: Vec::new(),
        };
        let stored = GroupRange {
            start: 0,
            len: 200,
            logical_lines: 200,
            payload_bytes: 0,
            stream: lvu_core::StreamKind::File,
            orphan: false,
            split: false,
            oversized: false,
            pending: false,
            run_key: None,
            key_refused: false,
            configured: true,
            auto_open: false,
            auto_structured: false,
            auto_structure_depth: 0,
            partial_open: false,
            partial_truncated: false,
            structure_truncated: false,
            partial_prefix: Vec::new(),
            structure_prefix: Vec::new(),
            acquisition_id: [0; 16],
            last_chunk: lvu_core::ChunkPosition::Complete,
            first_capture_nanos: 0,
            last_capture_nanos: 0,
            projection: Arc::new((0..64).map(row).collect::<Vec<_>>()),
        };
        let per_row = display_projection_bytes(&row(0));
        // 200 members but only the 64-row stored page is charged.
        assert_eq!(
            group_projection_bytes(&stored),
            group_state_bytes(&stored) + 64 * per_row
        );
    }

    #[test]
    fn group_index_uses_ordered_boundaries_for_large_memberships() {
        let groups = (0..10_000)
            .map(|index| GroupRange {
                start: index * 3,
                len: 3,
                logical_lines: 3,
                payload_bytes: 0,
                stream: lvu_core::StreamKind::File,
                orphan: false,
                split: false,
                oversized: false,
                pending: false,
                run_key: None,
                key_refused: false,
                configured: false,
                auto_open: true,
                auto_structured: false,
                auto_structure_depth: 0,
                partial_open: false,
                partial_truncated: false,
                structure_truncated: false,
                partial_prefix: Vec::new(),
                structure_prefix: Vec::new(),
                acquisition_id: [0; 16],
                last_chunk: lvu_core::ChunkPosition::Complete,
                first_capture_nanos: 0,
                last_capture_nanos: 0,
                projection: Arc::new(Vec::new()),
            })
            .collect::<Appended<_>>();
        assert_eq!(group_index_for_sequence(&groups, 0), Some(0));
        assert_eq!(group_index_for_sequence(&groups, 17), Some(5));
        assert_eq!(group_index_for_sequence(&groups, 29_999), Some(9_999));
        assert_eq!(group_index_for_sequence(&groups, 30_000), None);
    }
}

#[cfg(test)]
mod gap_tests {
    use super::*;

    /// A membership with one source and the given basis timestamps, where
    /// `NO_BASIS_TIME` stands for a record with no readable time.
    fn membership_of(times: &[i64]) -> Membership {
        let keys = merge_keys_for(None, &times.iter().copied().collect::<Appended<i64>>());
        Membership {
            sources: vec![SourceMatches {
                source_id: "src".into(),
                generation: 1,
                high_watermark: None,
                sequences: (0..times.len() as u64).collect(),
                times: times.iter().copied().collect(),
                groups: Appended::default(),
                bounds: SourceTimeBounds::default(),
                merge_keys: keys.0,
                ascending: keys.1,
            }],
            count: times.len() as u64,
            bytes: 0,
            budget: Arc::new(MemoryBudget {
                used: AtomicU64::new(0),
                maximum: 1 << 20,
            }),
            enrichment_names: Vec::new(),
            derived: HashMap::new(),
            derived_errors: HashSet::new(),
            frozen_derived: None,
            color_matches: HashMap::new(),
            color_rules: Vec::new(),
            advanced: None,
            enrichment: Vec::new(),
            evaluation_page_bytes: 0,
            evaluation_batches: Vec::new().into(),
            event_time_missing: 0,
            event_time_invalid: 0,
            basis: lvu::TimeBasis::Capture,
            grouped: false,
            order: (0..times.len() as u32).map(|unit| (0u32, unit)).collect(),
            ranks: vec![(0..times.len() as u32).collect::<Appended<u32>>()].into(),
            max_key: times.iter().copied().max().unwrap_or(i64::MIN),
        }
    }

    const SECOND: i64 = 1_000_000_000;

    #[test]
    fn a_gap_is_found_forward_and_backward_and_names_both_of_its_ends() {
        // Records at 0s, 1s, 60s, 61s: one 59-second gap, between index 1 and 2.
        let membership = membership_of(&[0, SECOND, 60 * SECOND, 61 * SECOND]);
        let hit = find_membership_gap(&membership, None, lvu::GapDirection::Forward, 10 * SECOND)
            .expect("the 59s gap");
        assert_eq!(hit.row, RowId::new("src", 2), "landing after the gap");
        assert_eq!(hit.previous_row, RowId::new("src", 1));
        assert_eq!(hit.gap_nanos, 59 * SECOND);
        assert_eq!(hit.previous_unix_nanos, SECOND);

        // From the last record, the same gap is the one behind.
        let back = find_membership_gap(
            &membership,
            Some(&RowId::new("src", 3)),
            lvu::GapDirection::Backward,
            10 * SECOND,
        )
        .expect("the same gap, from the other side");
        assert_eq!(back.row, RowId::new("src", 2));

        // A threshold above the gap finds nothing rather than the nearest one.
        assert_eq!(
            find_membership_gap(&membership, None, lvu::GapDirection::Forward, 120 * SECOND),
            None
        );
    }

    #[test]
    fn the_search_moves_past_the_gap_it_is_already_standing_on() {
        // Two gaps: 1→2 (59s) and 3→4 (59s).
        let membership = membership_of(&[0, SECOND, 60 * SECOND, 61 * SECOND, 120 * SECOND]);
        // Standing on the row after the first gap, forward must reach the
        // second one; repeating a jump that does not move is not navigation.
        let hit = find_membership_gap(
            &membership,
            Some(&RowId::new("src", 2)),
            lvu::GapDirection::Forward,
            10 * SECOND,
        )
        .expect("the second gap");
        assert_eq!(hit.row, RowId::new("src", 4));
        // And backward from there returns to the first.
        let back = find_membership_gap(
            &membership,
            Some(&RowId::new("src", 4)),
            lvu::GapDirection::Backward,
            10 * SECOND,
        )
        .expect("the first gap");
        assert_eq!(back.row, RowId::new("src", 2));
    }

    #[test]
    fn records_without_a_time_in_the_basis_neither_create_nor_hide_a_gap() {
        // The middle record has no value in the basis. The gap either side of
        // it is one 59-second gap, not two half-gaps and not none.
        let membership = membership_of(&[0, NO_BASIS_TIME, 59 * SECOND]);
        let hit = find_membership_gap(&membership, None, lvu::GapDirection::Forward, 10 * SECOND)
            .expect("the gap across the untimed record");
        assert_eq!(hit.row, RowId::new("src", 2));
        assert_eq!(hit.gap_nanos, 59 * SECOND);

        // With only one timed record there is no distance to measure.
        let sparse = membership_of(&[NO_BASIS_TIME, 5 * SECOND, NO_BASIS_TIME]);
        assert_eq!(
            find_membership_gap(&sparse, None, lvu::GapDirection::Forward, 1),
            None
        );
    }

    #[test]
    fn a_non_positive_threshold_finds_nothing_rather_than_everything() {
        let membership = membership_of(&[0, SECOND, 2 * SECOND]);
        for threshold in [0, -1, i64::MIN] {
            assert_eq!(
                find_membership_gap(&membership, None, lvu::GapDirection::Forward, threshold),
                None,
                "threshold {threshold}"
            );
        }
    }

    #[test]
    fn an_unknown_anchor_searches_from_the_end_the_direction_implies() {
        let membership = membership_of(&[0, 60 * SECOND, 61 * SECOND, 200 * SECOND]);
        let missing = RowId::new("other", 99);
        assert_eq!(
            find_membership_gap(
                &membership,
                Some(&missing),
                lvu::GapDirection::Forward,
                10 * SECOND
            )
            .map(|hit| hit.row),
            Some(RowId::new("src", 1)),
            "forward from an unknown anchor starts at the first record"
        );
        assert_eq!(
            find_membership_gap(
                &membership,
                Some(&missing),
                lvu::GapDirection::Backward,
                10 * SECOND
            )
            .map(|hit| hit.row),
            Some(RowId::new("src", 3)),
            "backward from an unknown anchor starts at the last"
        );
    }
}

#[cfg(test)]
mod order_tests {
    use super::*;

    /// One source whose records carry `times` as their basis timestamps.
    fn source(id: &str, times: &[i64]) -> SourceMatches {
        let times: Appended<i64> = times.iter().copied().collect();
        let (merge_keys, ascending) = merge_keys_for(None, &times);
        SourceMatches {
            source_id: id.into(),
            generation: 1,
            high_watermark: None,
            sequences: (0..times.len() as u64).collect(),
            times,
            groups: Appended::default(),
            bounds: SourceTimeBounds::default(),
            merge_keys,
            ascending,
        }
    }

    /// A membership carrying only what the order depends on.
    fn membership(sources: Vec<SourceMatches>, interleave: bool) -> Membership {
        let (order, ranks, max_key) = merge_order(&sources, false, interleave, None);
        Membership {
            sources,
            count: order.len() as u64,
            bytes: 0,
            budget: Arc::new(MemoryBudget {
                used: AtomicU64::new(0),
                maximum: 1 << 20,
            }),
            enrichment_names: Vec::new(),
            derived: HashMap::new(),
            derived_errors: HashSet::new(),
            frozen_derived: None,
            color_matches: HashMap::new(),
            color_rules: Vec::new(),
            advanced: None,
            enrichment: Vec::new(),
            evaluation_page_bytes: 0,
            evaluation_batches: Vec::new().into(),
            event_time_missing: 0,
            event_time_invalid: 0,
            basis: if interleave {
                lvu::TimeBasis::Event
            } else {
                lvu::TimeBasis::Capture
            },
            grouped: false,
            order,
            ranks,
            max_key,
        }
    }

    fn flat(order: &Appended<(u32, u32)>) -> Vec<(u32, u32)> {
        order.iter().copied().collect()
    }

    /// The fast path has to produce the order the slow path would.
    ///
    /// A wrong extension is silent — every record is still there, in the wrong
    /// place — so the two are compared directly rather than through anything
    /// they both feed.
    #[test]
    fn extending_gives_the_order_a_rebuild_gives() {
        for interleave in [true, false] {
            let before = membership(
                vec![source("a", &[10, 30, 50]), source("b", &[20, 40, 60])],
                interleave,
            );
            let grown = vec![
                source("a", &[10, 30, 50, 70, 90]),
                source("b", &[20, 40, 60, 80]),
            ];
            let (extended, extended_ranks, extended_max) =
                merge_order(&grown, false, interleave, Some(&before));
            let (rebuilt, rebuilt_ranks, rebuilt_max) =
                merge_order(&grown, false, interleave, None);
            assert_eq!(flat(&extended), flat(&rebuilt), "interleave={interleave}");
            assert_eq!(extended_max, rebuilt_max, "interleave={interleave}");
            for (left, right) in extended_ranks.iter().zip(rebuilt_ranks.iter()) {
                assert_eq!(
                    left.iter().copied().collect::<Vec<_>>(),
                    right.iter().copied().collect::<Vec<_>>(),
                    "interleave={interleave}"
                );
            }
            // Interleaved, this really did take the fast path, or the
            // comparison proves nothing: the prior order is a prefix of the
            // result. Concatenated it cannot, because `a` grew while `b`
            // already had units, so the arrivals belong ahead of `b` — the
            // rebuild `concatenation_extends_only_at_the_end` pins.
            if interleave {
                assert_eq!(
                    flat(&extended)[..before.order.len()],
                    flat(&before.order)[..]
                );
            }
        }
    }

    /// A record older than everything already ordered cannot extend: it
    /// belongs before rows that are already placed, so the order is rebuilt.
    #[test]
    fn a_late_older_record_rebuilds_rather_than_appending() {
        let before = membership(vec![source("a", &[10, 30]), source("b", &[20, 40])], true);
        // 5 sorts before every key already ordered.
        let grown = vec![source("a", &[10, 30, 5]), source("b", &[20, 40])];
        let (extended, _, _) = merge_order(&grown, false, true, Some(&before));
        let (rebuilt, _, _) = merge_order(&grown, false, true, None);
        assert_eq!(flat(&extended), flat(&rebuilt));
        // I2: the late record is placed among the *other* sources by its key,
        // but inside its own source it stays after the records it arrived
        // behind — it is not lifted to the front for being older.
        let order = flat(&rebuilt);
        let a: Vec<u32> = order
            .iter()
            .filter(|(source, _)| *source == 0)
            .map(|(_, unit)| *unit)
            .collect();
        assert_eq!(a, vec![0, 1, 2], "source a keeps its own arrival order");
        assert!(
            order.iter().position(|unit| *unit == (0, 2)).unwrap()
                < order.iter().position(|unit| *unit == (1, 1)).unwrap(),
            "and the key still places it before b's later record: {order:?}"
        );
    }

    /// Concatenated, growth in anything but the last contributing source
    /// would insert ahead of rows already placed.
    #[test]
    fn concatenation_extends_only_at_the_end() {
        let before = membership(vec![source("a", &[10, 20]), source("b", &[30])], false);
        let middle = vec![source("a", &[10, 20, 25]), source("b", &[30])];
        let (extended, _, _) = merge_order(&middle, false, false, Some(&before));
        let (rebuilt, _, _) = merge_order(&middle, false, false, None);
        assert_eq!(
            flat(&extended),
            flat(&rebuilt),
            "growth in a leading source rebuilds"
        );
        assert_eq!(flat(&rebuilt), vec![(0, 0), (0, 1), (0, 2), (1, 0)]);

        let last = vec![source("a", &[10, 20]), source("b", &[30, 40])];
        let (extended, _, _) = merge_order(&last, false, false, Some(&before));
        assert_eq!(flat(&extended), vec![(0, 0), (0, 1), (1, 0), (1, 1)]);
    }

    /// The keys extend too, and an untimed tail keeps carrying the last value
    /// it saw across the refresh boundary (I3).
    #[test]
    fn keys_extend_and_carry_the_fill_across_a_refresh() {
        let first: Appended<i64> = [10, NO_BASIS_TIME, 30].into_iter().collect();
        let (keys, ascending) = merge_keys_for(None, &first);
        assert_eq!(keys.iter().copied().collect::<Vec<_>>(), vec![10, 10, 30]);
        assert!(ascending);

        let grown: Appended<i64> = [10, NO_BASIS_TIME, 30, NO_BASIS_TIME, 50]
            .into_iter()
            .collect();
        let (extended, ascending) = merge_keys_for(Some((&keys, true)), &grown);
        let (rebuilt, rebuilt_ascending) = merge_keys_for(None, &grown);
        assert_eq!(
            extended.iter().copied().collect::<Vec<_>>(),
            rebuilt.iter().copied().collect::<Vec<_>>()
        );
        assert_eq!(
            extended.iter().copied().collect::<Vec<_>>(),
            vec![10, 10, 30, 30, 50]
        );
        assert_eq!(ascending, rebuilt_ascending);

        // A key that goes backwards clears the flag, and the extension must
        // notice it as a rebuild would.
        let backwards: Appended<i64> = [10, NO_BASIS_TIME, 30, 20].into_iter().collect();
        let (_, ascending) = merge_keys_for(Some((&keys, true)), &backwards);
        assert!(!ascending);
        assert_eq!(ascending, merge_keys_for(None, &backwards).1);
    }
}
