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

/// Repeated-pattern folding policy the terminal asks a provider to apply.
///
/// Folding is reversible presentation over the ordered row stream: it never
/// drops, reorders, merges or rewrites records, never changes what a filter
/// matches, and every constituent event stays addressable by its stable
/// [`RowId`]. `expanded` names entries by the identity of their first member,
/// which is stable across arrivals and therefore persistable.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct FoldRequest {
    pub enabled: bool,
    /// Smallest run that collapses. Values below 2 are treated as 2.
    pub minimum_run: usize,
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
