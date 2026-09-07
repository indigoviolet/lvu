use std::fmt;

/// Stable identity used by the UI. Capture adapters should derive this from the
/// source identity and monotonically increasing record sequence.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RowId {
    pub source_id: String,
    pub sequence: u64,
}

impl RowId {
    pub fn new(source_id: impl Into<String>, sequence: u64) -> Self {
        Self {
            source_id: source_id.into(),
            sequence,
        }
    }
}

impl fmt::Display for RowId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.source_id, self.sequence)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DisplayRow {
    pub id: RowId,
    pub timestamp: String,
    pub captured_at_unix_nanos: Option<i64>,
    pub level: String,
    pub text: String,
    pub details: Vec<(String, String)>,
    /// Bounded scalar fields recognized from the original event text.
    pub fields: Vec<(String, String)>,
}

/// The first and last timestamp a view actually holds, in the basis it is
/// filtered on.
///
/// Measured *before* the view's own time window is applied, so "the last five
/// minutes of data" means five minutes of the dataset rather than five minutes
/// of whatever window is already narrowing it. Records with no value in the
/// basis do not contribute; `count` says how many did, so a dialog can explain
/// an empty answer instead of showing a blank range.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TimeBounds {
    pub first_unix_nanos: i64,
    pub last_unix_nanos: i64,
    /// Records that carried a timestamp in this basis.
    pub count: usize,
    /// Records that did not. A partial answer says so rather than implying the
    /// dataset starts where its first *readable* timestamp does.
    pub missing: usize,
}

impl TimeBounds {
    pub fn span_nanos(&self) -> i64 {
        self.last_unix_nanos.saturating_sub(self.first_unix_nanos)
    }
}

/// Where a gap search landed: the row after the gap, and how long the gap was.
///
/// The answer is a [`RowId`], not an index. Identity is what survives arrivals,
/// folding and grouping, and the caller already resolves ids to positions with
/// [`RowProvider::index_of_id`]; returning an index here would be a second,
/// weaker answer to a question that already has a good one.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GapHit {
    /// The row that *follows* the gap. Landing after the gap is what a user
    /// means by "jump to the next gap": the interesting records are the ones
    /// that resume.
    pub row: RowId,
    pub gap_nanos: i64,
    /// When the stream went quiet, so the status line can say that as well as
    /// for how long.
    pub previous_unix_nanos: i64,
    /// The row the gap started from, for a status line that names both ends.
    pub previous_row: RowId,
}

/// Which way [`RowProvider::find_gap`] searches from its starting index.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GapDirection {
    Forward,
    Backward,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ViewportRequest {
    pub start: usize,
    pub len: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RowPage {
    pub total: usize,
    pub rows: Vec<DisplayRow>,
}

/// Source-local physical records, independent of a view's filtering/grouping.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContextPage {
    pub anchor_position: Option<usize>,
    pub start: usize,
    pub total: usize,
    pub rows: Vec<DisplayRow>,
    pub pending: bool,
    pub diagnostic: Option<String>,
}

/// Which earlier rows a new row may join, as the terminal words it.
///
/// The provider crate owns the engine's own scope type; this is the request
/// side of the same choice, so `lvu` does not have to depend on the view
/// implementation to describe a view's policy.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum FoldScopeRequest {
    /// Only an immediately preceding run of the same key folds.
    #[default]
    Adjacent,
    /// A run stays open while at most `n` rows of other keys intervene.
    Lookback(usize),
}

/// How aggressively the derived `pattern` column replaces volatile substrings.
///
/// It applies to that column and to nothing else: a fold keyed on a real
/// column uses its value as-is, so a field the user did not ask about is never
/// rewritten.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum FoldNormalisation {
    Conservative,
    #[default]
    Standard,
    Aggressive,
}

impl FoldNormalisation {
    pub const ALL: [FoldNormalisation; 3] = [
        FoldNormalisation::Conservative,
        FoldNormalisation::Standard,
        FoldNormalisation::Aggressive,
    ];

    pub fn label(self) -> &'static str {
        match self {
            FoldNormalisation::Conservative => "Conservative",
            FoldNormalisation::Standard => "Standard",
            FoldNormalisation::Aggressive => "Aggressive",
        }
    }

    pub fn token(self) -> &'static str {
        match self {
            FoldNormalisation::Conservative => "conservative",
            FoldNormalisation::Standard => "standard",
            FoldNormalisation::Aggressive => "aggressive",
        }
    }

    /// Unknown tokens read as the default, so a value written by a future
    /// version degrades to the built-in rather than refusing to load.
    pub fn parse_token(token: &str) -> Self {
        match token {
            "conservative" => FoldNormalisation::Conservative,
            "aggressive" => FoldNormalisation::Aggressive,
            _ => FoldNormalisation::Standard,
        }
    }
}

/// Repeated-run folding policy the terminal asks a provider to apply.
///
/// Folding is reversible presentation over the ordered row stream: it never
/// drops, reorders, merges or rewrites records, never changes what a filter
/// matches, and every constituent event stays addressable by its stable
/// [`RowId`]. `expanded` names entries by the identity of their first member,
/// which is stable across arrivals and therefore persistable.
///
/// A run is a group of rows sharing one key, and the key is the value of
/// exactly one column. `key_column` names it; `None` means the derived
/// `pattern` column, which is the normalised row text with the level prefixed
/// and is what folding used before a column could be chosen.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct FoldRequest {
    pub enabled: bool,
    /// Smallest run that collapses. Values below 2 are treated as 2.
    pub minimum_run: usize,
    /// The column whose value is the fold key. `None` is the derived
    /// `pattern` column.
    pub key_column: Option<String>,
    pub scope: FoldScopeRequest,
    /// Only consulted for the derived `pattern` column.
    pub normalisation: FoldNormalisation,
    /// Entries rendered as their constituent rows instead of one folded row.
    pub expanded: Vec<RowId>,
}

/// Honest accounting for the fold shown in the current view, so a count is
/// never silently wrong.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FoldSummary {
    pub enabled: bool,
    /// Presentation entries the folded stream currently contains.
    pub entries: usize,
    /// Runs the fold has found: entries with enough members to be a fold,
    /// whether or not they are currently collapsed. Expanding one does not stop
    /// it being a run, and the status line has to keep saying how many there
    /// are while the user is scrolling through one.
    pub runs: usize,
    /// Entries actually rendered collapsed.
    pub folded_entries: usize,
    /// Rows the collapse currently hides.
    pub hidden_rows: usize,
    /// Entries dropped because a retention cap was reached. Their rows stay in
    /// the stream, unfolded; a non-zero value means older runs are no longer
    /// counted together.
    pub evicted_entries: u64,
    /// Rows at the tail that folding has not consumed yet. They render
    /// individually until it catches up.
    pub pending_rows: usize,
}

/// Read-only, bounded display seam. Implementations must format or copy no more
/// than the requested range. `revision` changes when visible membership/order may
/// have changed and lets the terminal avoid unnecessary redraws.
pub trait RowProvider {
    fn page(&self, view_id: &str, request: ViewportRequest) -> RowPage;
    fn row_by_id(&self, view_id: &str, id: &RowId) -> Option<DisplayRow>;
    fn index_of_id(&self, view_id: &str, id: &RowId) -> Option<usize>;
    fn revision(&self, view_id: &str) -> u64;
    /// The view's rows with folding not applied, whatever the view's folding
    /// policy is.
    ///
    /// Folding is presentation. Anything that *samples* rows — assistance
    /// prompts, snapshot export, recipe suggestions, editor completion, an
    /// expression preview — must read this, so that turning a display option on
    /// can never change what is sampled. Providers that do not fold serve the
    /// same rows as [`RowProvider::page`].
    fn unfolded_page(&self, view_id: &str, request: ViewportRequest) -> RowPage {
        self.page(view_id, request)
    }

    /// Apply a folding policy to `view_id`. Providers that do not fold ignore
    /// it; the row stream they serve is then simply unfolded.
    fn set_fold(&self, _view_id: &str, _request: &FoldRequest) {}

    /// What folding is currently doing to `view_id`, if the provider folds.
    fn fold_summary(&self, _view_id: &str) -> Option<FoldSummary> {
        None
    }

    /// Constituent identities of the folded entry containing `id`, in original
    /// order. A row that is not part of a collapsed run yields just itself.
    fn fold_members(&self, _view_id: &str, id: &RowId) -> Vec<RowId> {
        vec![id.clone()]
    }

    /// The view's first and last timestamp in `basis`, ignoring its own time
    /// window (see [`TimeBounds`]).
    ///
    /// `basis` is passed rather than assumed because a provider that cannot
    /// answer in the basis the user chose must say so. Answering in capture
    /// time when the user asked about event time would be worse than answering
    /// nothing: the dataset-relative ranges are offered with the reason
    /// instead.
    fn time_bounds(&self, _view_id: &str, _basis: crate::TimeBasis) -> Option<TimeBounds> {
        None
    }

    /// The next row, starting from `from` and searching in `direction`, whose
    /// distance from the record before it exceeds `threshold_nanos`.
    ///
    /// `from` is a row identity; `None` starts at the first row when searching
    /// forward and at the last when searching backward. Records with no value
    /// in the view's basis take no part: a gap is measured between two records
    /// that both have a time, not across a record whose time is unknown.
    ///
    /// Bounded and nonblocking like every other provider call: the engine
    /// answers from the membership it already holds, and a provider that keeps
    /// no timestamps answers `None` rather than reading rows to find out.
    fn find_gap(
        &self,
        _view_id: &str,
        _from: Option<&RowId>,
        _direction: GapDirection,
        _threshold_nanos: i64,
        _basis: crate::TimeBasis,
    ) -> Option<GapHit> {
        None
    }

    /// Bounded, nonblocking raw context. Offset is relative to the anchor's
    /// physical source position; implementations must not cross sources.
    fn context_page(
        &self,
        _view_id: &str,
        _anchor: &RowId,
        _offset: isize,
        _len: usize,
    ) -> ContextPage {
        ContextPage {
            anchor_position: None,
            start: 0,
            total: 0,
            rows: Vec::new(),
            pending: false,
            diagnostic: Some("raw context is unavailable for this provider".into()),
        }
    }
}
