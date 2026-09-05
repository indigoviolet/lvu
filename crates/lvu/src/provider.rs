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

/// Read-only, bounded display seam. Implementations must format or copy no more
/// than the requested range. `revision` changes when visible membership/order may
/// have changed and lets the terminal avoid unnecessary redraws.
pub trait RowProvider {
    fn page(&self, view_id: &str, request: ViewportRequest) -> RowPage;
    fn row_by_id(&self, view_id: &str, id: &RowId) -> Option<DisplayRow>;
    fn index_of_id(&self, view_id: &str, id: &RowId) -> Option<usize>;
    fn revision(&self, view_id: &str) -> u64;
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
