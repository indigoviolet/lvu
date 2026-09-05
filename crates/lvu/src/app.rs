use std::{
    collections::{HashMap, VecDeque},
    time::{Duration, Instant},
};

use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::Rect;

use crate::provider::{DisplayRow, RowId, RowProvider, ViewportRequest};

pub const MAX_EDITOR_BYTES: usize = 16 * 1024;
pub const MAX_PENDING_QUERY_REQUESTS: usize = 32;
pub const SEARCH_DEBOUNCE: Duration = Duration::from_millis(120);
const MAX_SOURCE_REQUESTS: usize = 8;
const MAX_DISCOVERY_REQUESTS: usize = 4;
const MAX_AI_REQUESTS: usize = 2;
const MAX_AI_PROMPT_BYTES: usize = 8 * 1024;
const MAX_INVESTIGATION_REQUESTS: usize = 4;
const MAX_INVESTIGATION_MESSAGES: usize = 64;
const MAX_INVESTIGATION_MESSAGE_BYTES: usize = 16 * 1024;
const MAX_SAVED_INVESTIGATIONS: usize = 64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Focus {
    Selector,
    Logs,
    SearchEditor,
    AdvancedEditor,
    EnrichmentEditor,
    SourceDialog,
    ViewDialog,
    FieldPicker,
    AskAi,
    Investigation,
    Recipes,
    TimeEditor,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AskAiKind {
    Filter,
    Enrichment,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AskAiStage {
    Input,
    Snapshot,
    StartingSession,
    Proposing,
    Proposal,
    Error,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AskAiDialogState {
    pub generation: u64,
    pub view_id: String,
    pub definition_revision: u64,
    pub kind: AskAiKind,
    pub prompt: String,
    pub provider: String,
    pub mode: String,
    pub thinking: String,
    pub stage: AskAiStage,
    pub progress: String,
    pub expression: Option<String>,
    pub explanation: Option<String>,
    pub session_id: Option<String>,
    pub snapshot_dir: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AskAiRequest {
    Start {
        generation: u64,
        view_id: String,
        definition_revision: u64,
        kind: AskAiKind,
        instruction: String,
        provider: String,
        mode: String,
        thinking: String,
    },
    Cancel {
        generation: u64,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InvestigationStage {
    Input,
    Snapshot,
    StartingSession,
    Resuming,
    Sending,
    Conversation,
    Cancelling,
    Error,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InvestigationItem {
    pub id: String,
    pub view_id: String,
    pub session_id: String,
    pub snapshot_dir: String,
    pub manifest_path: String,
    pub question: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InvestigationDialogState {
    pub generation: u64,
    pub view_id: String,
    pub definition_revision: u64,
    pub stage: InvestigationStage,
    pub input: String,
    pub progress: String,
    pub selected: usize,
    pub items: Vec<InvestigationItem>,
    pub investigation_id: Option<String>,
    pub session_id: Option<String>,
    pub snapshot_dir: Option<String>,
    pub manifest_path: Option<String>,
    pub messages: VecDeque<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InvestigationRequest {
    Start {
        generation: u64,
        view_id: String,
        definition_revision: u64,
        question: String,
        provider: String,
        mode: String,
        thinking: String,
    },
    Resume {
        generation: u64,
        item: InvestigationItem,
    },
    Send {
        generation: u64,
        session_id: String,
        prompt: String,
    },
    Cancel {
        generation: u64,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceItem {
    pub id: String,
    pub name: String,
    pub health: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ViewItem {
    pub id: String,
    pub source_id: String,
    pub name: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ViewDialogMode {
    Blank,
    Clone,
    Rename,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ViewDialogState {
    pub mode: ViewDialogMode,
    pub draft: String,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ViewMutationRequest {
    pub mode: ViewDialogMode,
    pub source_id: String,
    pub view_id: String,
    pub name: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct EditorState {
    pub draft: String,
    pub applied: String,
    pub error: Option<String>,
    pub pending_generation: Option<u64>,
    pending_value: Option<String>,
    pending_revision: Option<u64>,
    search_due: Option<Instant>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ViewState {
    pub top: usize,
    pub selected: Option<RowId>,
    pub follow: bool,
    pub last_total: usize,
    pub provider_revision: u64,
    pub viewport_height: usize,
    pub search: EditorState,
    pub advanced: EditorState,
    pub enrichment: EditorState,
    pub applied_capture_time: Option<CaptureTimeRange>,
    /// User-authored policy. Rolling refreshes update the resolved range above
    /// without changing the definition revision.
    pub applied_capture_time_policy: Option<CaptureTimePolicy>,
    pub time_start_draft: String,
    pub time_end_draft: String,
    pub time_recent_draft: String,
    pub time_error: Option<String>,
    pub applied_query_revision: u64,
    pub desired_query_revision: u64,
    pub pinned_columns: Vec<String>,
    pub color_field: Option<String>,
    pub field_picker_selected: usize,
    pub field_picker_top: usize,
    pub field_picker_row: Option<RowId>,
    user_interaction_revision: u64,
    ai_definition_revision: u64,
    desired_constraints: QueryConstraints,
    desired_capture_time_policy: Option<CaptureTimePolicy>,
    pending_recipe: Option<PendingRecipe>,
    pending_time: Option<PendingTime>,
    rolling_refresh_due: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PendingTime {
    generation: u64,
    revision: u64,
    value: Option<CaptureTimeRange>,
    policy: Option<CaptureTimePolicy>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PendingRecipe {
    revision: u64,
    interaction_revision: u64,
    pinned_columns: Vec<String>,
    color_field: Option<String>,
    capture_time_policy: Option<CaptureTimePolicy>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PersistentViewState {
    pub view_name: String,
    pub applied_search: String,
    pub search_draft: String,
    pub search_error: Option<String>,
    pub applied_advanced: String,
    pub advanced_draft: String,
    pub advanced_error: Option<String>,
    pub applied_enrichment: String,
    pub enrichment_draft: String,
    pub enrichment_error: Option<String>,
    pub applied_capture_time: Option<CaptureTimeRange>,
    pub applied_capture_time_policy: Option<CaptureTimePolicy>,
    pub time_start_draft: String,
    pub time_end_draft: String,
    pub time_recent_draft: String,
    pub time_error: Option<String>,
    pub selected: Option<RowId>,
    pub follow: bool,
    pub pinned_columns: Vec<String>,
    pub color_field: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum QueryPurpose {
    Search,
    Advanced,
    Enrichment,
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// A literal substring constraint. Case-insensitive adapters use Rust's
/// locale-neutral Unicode lowercase mapping, not locale-specific case rules.
pub struct TextConstraint {
    pub literal: String,
    pub case_insensitive: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct QueryConstraints {
    pub text: Option<TextConstraint>,
    pub advanced_polars: Option<String>,
    /// One staged named enrichment encoded as `name = Python Polars expression`.
    pub enrichment: Option<String>,
    /// Fixed capture-time window, half-open `[start_unix_nanos, end_unix_nanos)`.
    pub capture_time: Option<CaptureTimeRange>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CaptureTimeRange {
    pub start_unix_nanos: i64,
    pub end_unix_nanos: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CaptureTimePolicy {
    Absolute(CaptureTimeRange),
    Recent { seconds: u64 },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueryRequest {
    pub view_id: String,
    pub generation: u64,
    /// Monotonic per-view revision of the complete AND-combined constraints.
    pub revision: u64,
    pub base_revision: u64,
    pub base_constraints: QueryConstraints,
    pub purpose: QueryPurpose,
    pub constraints: QueryConstraints,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueryCompletion {
    pub view_id: String,
    pub generation: u64,
    pub revision: u64,
    pub purpose: QueryPurpose,
    pub result: Result<(), QueryFailure>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueryFailure {
    /// The constraint which failed validation, independent of request purpose.
    pub purpose: QueryPurpose,
    pub message: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceKind {
    File,
    Command,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceLaunchRequest {
    pub kind: SourceKind,
    pub text: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceDialogState {
    pub kind: SourceKind,
    pub draft: String,
    pub error: Option<String>,
    pub mode: SourceDialogMode,
    pub discovery: DiscoveryDialogState,
    pub path_completion: PathCompletionState,
    pub ai: SourceAiDialogState,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SourceAiStage {
    #[default]
    Input,
    Preparing,
    Starting,
    Proposing,
    Proposal,
    Error,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SourceAiPreview {
    pub name: String,
    pub kind: String,
    pub launch: String,
    pub effective_path_or_cwd: String,
    pub restart: String,
    pub environment: Vec<String>,
    pub explanation: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceAiDialogState {
    pub generation: u64,
    pub instruction: String,
    pub stage: SourceAiStage,
    pub progress: String,
    pub session_id: Option<String>,
    pub preview: Option<SourceAiPreview>,
    pub preview_scroll: usize,
}

impl Default for SourceAiDialogState {
    fn default() -> Self {
        Self {
            generation: 0,
            instruction: String::new(),
            stage: SourceAiStage::Input,
            progress: "Describe the source to follow".into(),
            session_id: None,
            preview: None,
            preview_scroll: 0,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SourceAiRequest {
    Start {
        generation: u64,
        instruction: String,
        provider: String,
        mode: String,
        thinking: String,
    },
    Apply {
        generation: u64,
    },
    Cancel {
        generation: u64,
    },
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RecipeConfig {
    pub search: String,
    pub advanced: String,
    pub enrichment: String,
    pub pinned_columns: Vec<String>,
    pub color_field: Option<String>,
    pub capture_time: Option<CaptureTimeRange>,
    pub capture_time_policy: Option<CaptureTimePolicy>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TimeDialogState {
    pub editing_end: bool,
    pub anchored_row: Option<RowId>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecipeItem {
    pub id: String,
    pub revision: String,
    pub name: String,
    pub config: RecipeConfig,
    pub incompatibility: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum RecipeDialogMode {
    #[default]
    Browse,
    Save,
    Import,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RecipeDialogState {
    pub id: u64,
    pub interaction_revision: u64,
    pub pending_request_id: Option<u64>,
    pub mode: RecipeDialogMode,
    pub name: String,
    pub items: Vec<RecipeItem>,
    pub selected: usize,
    pub status: String,
    pub loading: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RecipeRequest {
    List {
        meta: RecipeRequestMeta,
    },
    Save {
        meta: RecipeRequestMeta,
        name: String,
        view_id: String,
        config: RecipeConfig,
    },
    Import {
        meta: RecipeRequestMeta,
        path: String,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RecipeRequestMeta {
    pub request_id: u64,
    pub dialog_id: u64,
    pub dialog_revision: u64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PathCompletionState {
    pub generation: u64,
    pub scanning: bool,
    pub candidates: Vec<String>,
    pub selected: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PathCompletionRequest {
    pub generation: u64,
    pub draft: String,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SourceDialogMode {
    #[default]
    Manual,
    Discovery,
    Ai,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiscoveryItem {
    pub key: String,
    pub label: String,
    pub detail: String,
    pub status: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DiscoveryDialogState {
    pub generation: u64,
    pub query: String,
    pub items: Vec<DiscoveryItem>,
    pub selected: usize,
    pub scanning: bool,
    pub status: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DiscoveryUiRequest {
    Scan { generation: u64 },
    Cancel { generation: u64 },
    Select { generation: u64, key: String },
}

impl Default for SourceDialogState {
    fn default() -> Self {
        Self {
            kind: SourceKind::File,
            draft: String::new(),
            error: None,
            mode: SourceDialogMode::Manual,
            discovery: DiscoveryDialogState::default(),
            path_completion: PathCompletionState::default(),
            ai: SourceAiDialogState::default(),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct HitRegions {
    pub log: Option<Rect>,
    pub log_rows: Option<Rect>,
    pub sidebar: Option<Rect>,
    pub sidebar_views: Vec<(Rect, usize)>,
    pub field_picker_rows: Vec<(Rect, usize)>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Action {
    Quit,
    CycleFocus,
    NextView,
    PreviousView,
    SelectSidebar(i32),
    MoveLine(i32),
    MovePage(i32),
    Top,
    End,
    ToggleDetails,
    ToggleHelp,
    ToggleFollow,
    OpenSearch,
    OpenAdvanced,
    OpenEnrichment,
    OpenAskAi,
    SelectAskAiKind(AskAiKind),
    SubmitAskAi,
    ApplyAskAi,
    OpenInvestigation,
    NewInvestigation,
    MoveInvestigation(i32),
    SubmitInvestigation,
    OpenSource,
    OpenRecipes,
    OpenTime,
    TimeInput(char),
    TimeBackspace,
    SwitchTimeField,
    SubmitTime,
    ClearTime,
    AroundSelected,
    SetRecentTime(u64),
    SelectRecipeMode(RecipeDialogMode),
    MoveRecipe(i32),
    RecipeInput(char),
    RecipeBackspace,
    SubmitRecipe,
    OpenViewDialog,
    SelectViewDialogMode(ViewDialogMode),
    SubmitViewDialog,
    ViewInput(char),
    ViewBackspace,
    OpenFieldPicker,
    MoveFieldPicker(i32),
    TogglePinnedField,
    ToggleColorField,
    ToggleDiscovery,
    ToggleSourceAi,
    RefreshDiscovery,
    MoveDiscovery(i32),
    ToggleSourceKind,
    SelectSourceKind(SourceKind),
    CompleteSourcePath,
    MovePathCompletion(i32),
    SourceInput(char),
    SourceBackspace,
    SubmitSource,
    EditorInput(char),
    EditorBackspace,
    EditorPaste(String),
    SubmitDraft,
    CancelEditor,
    Resize(u16, u16),
    Mouse(MouseEvent),
    FixtureAdvance,
    None,
}

pub struct App {
    pub title: String,
    pub demo_mode: bool,
    pub sources: Vec<SourceItem>,
    pub views: Vec<ViewItem>,
    pub selected_view: usize,
    pub focus: Focus,
    pub show_details: bool,
    pub show_help: bool,
    pub terminal_size: (u16, u16),
    pub should_quit: bool,
    pub hit_regions: HitRegions,
    pub source_dialog: Option<SourceDialogState>,
    pub view_dialog: Option<ViewDialogState>,
    pub ask_ai_dialog: Option<AskAiDialogState>,
    pub investigation_dialog: Option<InvestigationDialogState>,
    pub recipe_dialog: Option<RecipeDialogState>,
    pub time_dialog: Option<TimeDialogState>,
    pub source_notice: Option<String>,
    view_states: HashMap<String, ViewState>,
    query_requests: HashMap<(String, QueryPurpose), QueryRequest>,
    next_query_generation: u64,
    source_requests: VecDeque<SourceLaunchRequest>,
    discovery_requests: VecDeque<DiscoveryUiRequest>,
    path_completion_requests: VecDeque<PathCompletionRequest>,
    view_requests: VecDeque<ViewMutationRequest>,
    ask_ai_requests: VecDeque<AskAiRequest>,
    source_ai_requests: VecDeque<SourceAiRequest>,
    recipe_requests: VecDeque<RecipeRequest>,
    investigation_requests: VecDeque<InvestigationRequest>,
    next_ask_ai_generation: u64,
    next_source_ai_generation: u64,
    next_investigation_generation: u64,
    next_recipe_generation: u64,
    investigations: Vec<InvestigationItem>,
    ai_provider: String,
    ai_mode: String,
    ai_thinking: String,
    next_path_completion_generation: u64,
    view_runtime_status: HashMap<String, String>,
    clock_now_unix_nanos: i64,
    last_clock_unix_nanos: Option<i64>,
    next_rolling_refresh: Option<Instant>,
}

impl App {
    pub fn new(sources: Vec<SourceItem>, views: Vec<ViewItem>, demo_mode: bool) -> Self {
        let empty = views.is_empty();
        let view_states = views
            .iter()
            .map(|view| {
                (
                    view.id.clone(),
                    ViewState {
                        follow: true,
                        ..ViewState::default()
                    },
                )
            })
            .collect();
        Self {
            title: "lvu log workspace".into(),
            demo_mode,
            sources,
            views,
            selected_view: 0,
            focus: if empty {
                Focus::SourceDialog
            } else {
                Focus::Logs
            },
            show_details: false,
            show_help: false,
            terminal_size: (80, 24),
            should_quit: false,
            hit_regions: HitRegions::default(),
            source_dialog: empty.then(SourceDialogState::default),
            view_dialog: None,
            ask_ai_dialog: None,
            investigation_dialog: None,
            recipe_dialog: None,
            time_dialog: None,
            source_notice: None,
            view_states,
            query_requests: HashMap::new(),
            next_query_generation: 1,
            source_requests: VecDeque::new(),
            discovery_requests: VecDeque::new(),
            path_completion_requests: VecDeque::new(),
            view_requests: VecDeque::new(),
            ask_ai_requests: VecDeque::new(),
            source_ai_requests: VecDeque::new(),
            recipe_requests: VecDeque::new(),
            investigation_requests: VecDeque::new(),
            next_ask_ai_generation: 1,
            next_source_ai_generation: 1,
            next_investigation_generation: 1,
            next_recipe_generation: 1,
            investigations: Vec::new(),
            ai_provider: "codex/gpt-5.6-sol".into(),
            ai_mode: "full-access".into(),
            ai_thinking: "medium".into(),
            next_path_completion_generation: 1,
            view_runtime_status: HashMap::new(),
            clock_now_unix_nanos: 0,
            last_clock_unix_nanos: None,
            next_rolling_refresh: None,
        }
    }

    pub fn active_view_id(&self) -> Option<&str> {
        self.views
            .get(self.selected_view)
            .map(|view| view.id.as_str())
    }

    pub fn view_state(&self) -> Option<&ViewState> {
        self.active_view_id()
            .and_then(|id| self.view_states.get(id))
    }

    fn view_state_mut(&mut self) -> Option<&mut ViewState> {
        let id = self.active_view_id()?.to_owned();
        self.view_states.get_mut(&id)
    }

    pub fn search_state(&self) -> Option<&EditorState> {
        self.view_state().map(|state| &state.search)
    }

    pub fn advanced_state(&self) -> Option<&EditorState> {
        self.view_state().map(|state| &state.advanced)
    }

    pub fn persistent_view_state(&self, view_id: &str) -> Option<PersistentViewState> {
        let state = self.view_states.get(view_id)?;
        let name = self
            .views
            .iter()
            .find(|view| view.id == view_id)?
            .name
            .clone();
        Some(PersistentViewState {
            view_name: name,
            applied_search: state.search.applied.clone(),
            search_draft: state.search.draft.clone(),
            search_error: state.search.error.clone(),
            applied_advanced: state.advanced.applied.clone(),
            advanced_draft: state.advanced.draft.clone(),
            advanced_error: state.advanced.error.clone(),
            applied_enrichment: state.enrichment.applied.clone(),
            enrichment_draft: state.enrichment.draft.clone(),
            enrichment_error: state.enrichment.error.clone(),
            applied_capture_time: match state.applied_capture_time_policy {
                Some(CaptureTimePolicy::Recent { .. }) => None,
                _ => state.applied_capture_time,
            },
            applied_capture_time_policy: state.applied_capture_time_policy,
            time_start_draft: state.time_start_draft.clone(),
            time_end_draft: state.time_end_draft.clone(),
            time_recent_draft: state.time_recent_draft.clone(),
            time_error: state.time_error.clone(),
            selected: state.selected.clone(),
            follow: state.follow,
            pinned_columns: state.pinned_columns.clone(),
            color_field: state.color_field.clone(),
        })
    }

    /// Changes only for direct user edits/navigation, so asynchronous restore
    /// work can be fenced without treating provider-driven row arrival as input.
    pub fn view_interaction_revision(&self, view_id: &str) -> Option<u64> {
        self.view_states
            .get(view_id)
            .map(|state| state.user_interaction_revision)
    }

    pub fn view_definition_revision(&self, view_id: &str) -> Option<u64> {
        self.view_states
            .get(view_id)
            .map(|state| state.ai_definition_revision)
    }

    pub fn configure_ai(&mut self, provider: String, mode: String, thinking: String) {
        self.ai_provider = provider;
        self.ai_mode = mode;
        self.ai_thinking = thinking;
    }

    pub fn view_has_pending_query(&self, view_id: &str) -> bool {
        self.view_states
            .get(view_id)
            .is_some_and(state_has_pending_query)
    }

    /// Restores drafts/navigation immediately, but submits accepted constraints
    /// through the ordinary dispatcher before marking either filter applied.
    pub fn restore_persistent_view(
        &mut self,
        view_id: &str,
        restored: PersistentViewState,
    ) -> bool {
        if !self.view_states.contains_key(view_id) {
            return false;
        }
        if !restored.view_name.is_empty()
            && let Some(view) = self.views.iter_mut().find(|view| view.id == view_id)
        {
            // Restored names are part of the fenced snapshot, not user input.
            view.name = restored.view_name.clone();
        }
        let state = self
            .view_states
            .get_mut(view_id)
            .expect("view state checked above");
        state.ai_definition_revision = state.ai_definition_revision.saturating_add(1);
        state.search.draft = restored.search_draft;
        state.search.error = restored.search_error;
        state.advanced.draft = restored.advanced_draft;
        state.advanced.error = restored.advanced_error;
        state.enrichment.draft = restored.enrichment_draft;
        state.enrichment.error = restored.enrichment_error;
        state.time_start_draft = restored.time_start_draft;
        state.time_end_draft = restored.time_end_draft;
        state.time_recent_draft = restored.time_recent_draft;
        state.time_error = restored.time_error;
        state.selected = restored.selected;
        state.follow = restored.follow;
        state.pinned_columns = restored.pinned_columns.into_iter().take(8).collect();
        state.color_field = restored.color_field;
        let restored_policy = restored.applied_capture_time_policy.or(restored
            .applied_capture_time
            .map(CaptureTimePolicy::Absolute));
        let resolved_capture_time = restored_policy
            .and_then(|policy| resolve_capture_time_policy(policy, self.clock_now_unix_nanos));
        let constraints = QueryConstraints {
            text: nonempty_text(&restored.applied_search),
            advanced_polars: nonempty(&restored.applied_advanced),
            enrichment: nonempty(&restored.applied_enrichment),
            capture_time: resolved_capture_time,
        };
        let purpose = if constraints.enrichment.is_some() {
            QueryPurpose::Enrichment
        } else if constraints.advanced_polars.is_some() {
            QueryPurpose::Advanced
        } else {
            QueryPurpose::Search
        };
        let generation = self.next_query_generation;
        self.next_query_generation = self.next_query_generation.saturating_add(1);
        state.desired_query_revision = state.desired_query_revision.saturating_add(1);
        let revision = state.desired_query_revision;
        state.desired_constraints = constraints.clone();
        state.desired_capture_time_policy = restored_policy;
        state.search.pending_generation = Some(generation);
        state.search.pending_revision = Some(revision);
        state.search.pending_value = Some(restored.applied_search);
        state.advanced.pending_generation = Some(generation);
        state.advanced.pending_revision = Some(revision);
        state.advanced.pending_value = Some(restored.applied_advanced);
        state.enrichment.pending_generation = Some(generation);
        state.enrichment.pending_revision = Some(revision);
        state.enrichment.pending_value = Some(restored.applied_enrichment);
        state.pending_time = Some(PendingTime {
            generation,
            revision,
            value: constraints.capture_time,
            policy: restored_policy,
        });
        self.query_requests.insert(
            (view_id.to_owned(), purpose),
            QueryRequest {
                view_id: view_id.to_owned(),
                generation,
                revision,
                base_revision: state.applied_query_revision,
                base_constraints: applied_constraints(state),
                purpose,
                constraints,
            },
        );
        true
    }

    fn apply_recipe_to_active_view(&mut self, config: RecipeConfig) -> bool {
        let Some(view_id) = self.active_view_id().map(str::to_owned) else {
            return false;
        };
        let policy = config
            .capture_time_policy
            .or(config.capture_time.map(CaptureTimePolicy::Absolute));
        let resolved_capture_time =
            policy.and_then(|value| resolve_capture_time_policy(value, self.clock_now_unix_nanos));
        let Some(state) = self.view_states.get_mut(&view_id) else {
            return false;
        };
        state.user_interaction_revision = state.user_interaction_revision.saturating_add(1);
        state.ai_definition_revision = state.ai_definition_revision.saturating_add(1);
        state.search.draft = config.search.clone();
        state.advanced.draft = config.advanced.clone();
        state.enrichment.draft = config.enrichment.clone();
        if let Some(window) = resolved_capture_time {
            state.time_start_draft = format_utc_nanos(window.start_unix_nanos);
            state.time_end_draft = format_utc_nanos(window.end_unix_nanos);
        } else {
            state.time_start_draft.clear();
            state.time_end_draft.clear();
        }
        state.time_recent_draft = match config.capture_time_policy {
            Some(CaptureTimePolicy::Recent { seconds }) => format_capture_duration(seconds),
            _ => String::new(),
        };
        state.search.error = None;
        state.advanced.error = None;
        state.enrichment.error = None;
        state.time_error = None;
        let pins = config.pinned_columns;
        let color = config.color_field;
        let constraints = QueryConstraints {
            text: nonempty_text(&config.search),
            advanced_polars: nonempty(&config.advanced),
            enrichment: nonempty(&config.enrichment),
            capture_time: resolved_capture_time,
        };
        state.desired_constraints = constraints;
        state.desired_capture_time_policy = policy;
        let Some(revision) = self.enqueue_query(&view_id, QueryPurpose::Advanced) else {
            let state = self.view_states.get_mut(&view_id).expect("view state");
            state.desired_constraints = applied_constraints(state);
            state.desired_capture_time_policy = state.applied_capture_time_policy;
            return false;
        };
        let state = self.view_states.get_mut(&view_id).expect("view state");
        let generation = state
            .advanced
            .pending_generation
            .expect("recipe query generation");
        state.pending_time = Some(PendingTime {
            generation,
            revision,
            value: resolved_capture_time,
            policy,
        });
        state.pending_recipe = Some(PendingRecipe {
            revision,
            interaction_revision: state.user_interaction_revision,
            pinned_columns: pins,
            color_field: color,
            capture_time_policy: policy,
        });
        true
    }

    pub fn restore_persistent_view_if_unmodified(
        &mut self,
        view_id: &str,
        expected_interaction_revision: u64,
        restored: PersistentViewState,
    ) -> bool {
        if self.view_interaction_revision(view_id) != Some(expected_interaction_revision) {
            return false;
        }
        self.restore_persistent_view(view_id, restored)
    }

    pub fn active_editor_state(&self) -> Option<&EditorState> {
        match self.focus {
            Focus::SearchEditor => self.search_state(),
            Focus::AdvancedEditor => self.advanced_state(),
            Focus::EnrichmentEditor => self.view_state().map(|state| &state.enrichment),
            Focus::Selector
            | Focus::Logs
            | Focus::SourceDialog
            | Focus::ViewDialog
            | Focus::FieldPicker
            | Focus::AskAi
            | Focus::Investigation => None,
            Focus::Recipes | Focus::TimeEditor => None,
        }
    }

    pub fn take_source_requests(&mut self) -> Vec<SourceLaunchRequest> {
        self.source_requests.drain(..).collect()
    }

    pub fn take_discovery_requests(&mut self) -> Vec<DiscoveryUiRequest> {
        self.discovery_requests.drain(..).collect()
    }

    pub fn take_path_completion_requests(&mut self) -> Vec<PathCompletionRequest> {
        self.path_completion_requests.drain(..).collect()
    }

    pub fn active_path_completion_generation(&self) -> Option<u64> {
        self.source_dialog.as_ref().and_then(|dialog| {
            (dialog.mode == SourceDialogMode::Manual
                && dialog.kind == SourceKind::File
                && dialog.path_completion.scanning)
                .then_some(dialog.path_completion.generation)
        })
    }

    pub fn apply_path_completion_result(
        &mut self,
        generation: u64,
        original_draft: &str,
        replacement: Option<String>,
        candidates: Vec<String>,
        error: Option<String>,
    ) -> bool {
        let Some(dialog) = &mut self.source_dialog else {
            return false;
        };
        if dialog.mode != SourceDialogMode::Manual
            || dialog.kind != SourceKind::File
            || dialog.draft != original_draft
            || dialog.path_completion.generation != generation
        {
            return false;
        }
        let consumed_unique = replacement
            .as_ref()
            .is_some_and(|replacement| candidates.len() == 1 && candidates[0] == *replacement);
        if let Some(replacement) = replacement {
            dialog.draft = replacement;
        }
        dialog.path_completion.scanning = false;
        dialog.path_completion.candidates = if consumed_unique {
            Vec::new()
        } else {
            candidates
        };
        dialog.path_completion.selected = 0;
        dialog.error = error;
        true
    }

    pub fn apply_discovery_result(
        &mut self,
        generation: u64,
        items: Vec<DiscoveryItem>,
        status: String,
    ) -> bool {
        let Some(dialog) = &mut self.source_dialog else {
            return false;
        };
        if dialog.discovery.generation != generation {
            return false;
        }
        dialog.discovery.items = items;
        dialog.discovery.selected = 0;
        dialog.discovery.scanning = false;
        dialog.discovery.status = status;
        dialog.error = None;
        true
    }

    pub fn add_source_view(&mut self, source: SourceItem, view: ViewItem) {
        if self.sources.iter().all(|item| item.id != source.id) {
            self.sources.push(source);
        }
        if self.views.iter().all(|item| item.id != view.id) {
            self.view_states.insert(
                view.id.clone(),
                ViewState {
                    follow: true,
                    ..ViewState::default()
                },
            );
            self.views.push(view);
        }
        if self.views.len() == 1 {
            self.selected_view = 0;
        }
    }

    pub fn add_view(&mut self, view: ViewItem) {
        if self.views.iter().any(|item| item.id == view.id) {
            return;
        }
        self.view_states.insert(
            view.id.clone(),
            ViewState {
                follow: true,
                ..ViewState::default()
            },
        );
        self.views.push(view);
    }

    pub fn rename_view(&mut self, view_id: &str, name: String) -> bool {
        let Some(source_id) = self
            .views
            .iter()
            .find(|view| view.id == view_id)
            .map(|view| view.source_id.clone())
        else {
            return false;
        };
        if self
            .views
            .iter()
            .any(|view| view.id != view_id && view.source_id == source_id && view.name == name)
        {
            return false;
        }
        let view = self
            .views
            .iter_mut()
            .find(|view| view.id == view_id)
            .expect("view checked above");
        view.name = name;
        if let Some(state) = self.view_states.get_mut(view_id) {
            state.user_interaction_revision = state.user_interaction_revision.saturating_add(1);
        }
        true
    }

    pub fn take_view_requests(&mut self) -> Vec<ViewMutationRequest> {
        self.view_requests.drain(..).collect()
    }

    pub fn take_ask_ai_requests(&mut self) -> Vec<AskAiRequest> {
        self.ask_ai_requests.drain(..).collect()
    }

    pub fn take_source_ai_requests(&mut self) -> Vec<SourceAiRequest> {
        self.source_ai_requests.drain(..).collect()
    }
    pub fn take_recipe_requests(&mut self) -> Vec<RecipeRequest> {
        self.recipe_requests.drain(..).collect()
    }
    pub fn set_recipes(
        &mut self,
        meta: RecipeRequestMeta,
        items: Vec<RecipeItem>,
        error: Option<String>,
    ) {
        if let Some(dialog) = &mut self.recipe_dialog
            && dialog.id == meta.dialog_id
            && dialog.interaction_revision == meta.dialog_revision
            && dialog.pending_request_id == Some(meta.request_id)
        {
            dialog.items = items.into_iter().take(128).collect();
            dialog.selected = dialog.selected.min(dialog.items.len().saturating_sub(1));
            dialog.loading = false;
            dialog.pending_request_id = None;
            dialog.status =
                error.unwrap_or_else(|| format!("{} saved recipes", dialog.items.len()));
        }
    }
    pub fn recipe_saved(&mut self, meta: RecipeRequestMeta, message: String) {
        if let Some(dialog) = &mut self.recipe_dialog
            && dialog.id == meta.dialog_id
            && dialog.interaction_revision == meta.dialog_revision
            && dialog.pending_request_id == Some(meta.request_id)
        {
            dialog.mode = RecipeDialogMode::Browse;
            dialog.status = message;
            dialog.loading = true;
            let list_meta = self.next_recipe_request_meta(meta.dialog_id, meta.dialog_revision);
            if let Some(dialog) = &mut self.recipe_dialog {
                dialog.pending_request_id = Some(list_meta.request_id);
            }
            self.recipe_requests
                .push_back(RecipeRequest::List { meta: list_meta });
        } else {
            self.source_notice = Some(message);
        }
    }
    pub fn recipe_failed(&mut self, meta: RecipeRequestMeta, message: String) {
        if let Some(dialog) = &mut self.recipe_dialog
            && dialog.id == meta.dialog_id
            && dialog.interaction_revision == meta.dialog_revision
            && dialog.pending_request_id == Some(meta.request_id)
        {
            dialog.loading = false;
            dialog.pending_request_id = None;
            dialog.status = message.clone();
        }
        self.source_notice = Some(format!("recipe error: {message}"));
    }

    fn next_recipe_request_meta(
        &mut self,
        dialog_id: u64,
        dialog_revision: u64,
    ) -> RecipeRequestMeta {
        let request_id = self.next_recipe_generation;
        self.next_recipe_generation = self.next_recipe_generation.saturating_add(1);
        RecipeRequestMeta {
            request_id,
            dialog_id,
            dialog_revision,
        }
    }

    pub fn update_source_ai_progress(
        &mut self,
        generation: u64,
        stage: SourceAiStage,
        progress: String,
        session_id: Option<String>,
    ) -> bool {
        let Some(ai) = self
            .source_dialog
            .as_mut()
            .map(|dialog| &mut dialog.ai)
            .filter(|ai| ai.generation == generation)
        else {
            return false;
        };
        ai.stage = stage;
        ai.progress = progress;
        if session_id.is_some() {
            ai.session_id = session_id;
        }
        true
    }

    pub fn finish_source_ai(
        &mut self,
        generation: u64,
        result: Result<SourceAiPreview, String>,
    ) -> bool {
        let Some(ai) = self
            .source_dialog
            .as_mut()
            .map(|dialog| &mut dialog.ai)
            .filter(|ai| ai.generation == generation)
        else {
            return false;
        };
        match result {
            Ok(preview) => {
                ai.preview = Some(preview);
                ai.preview_scroll = 0;
                ai.stage = SourceAiStage::Proposal;
                ai.progress = "Review only — Enter explicitly starts this source".into();
            }
            Err(error) => {
                ai.preview = None;
                ai.stage = SourceAiStage::Error;
                ai.progress = error;
            }
        }
        true
    }

    pub fn source_ai_launch_succeeded(&mut self, generation: u64, view_id: &str) {
        if self
            .source_dialog
            .as_ref()
            .is_some_and(|dialog| dialog.ai.generation == generation)
        {
            self.select_view(view_id);
            self.source_dialog = None;
            self.focus = Focus::Logs;
            self.source_notice = Some("reviewed AI source started".into());
        }
    }

    pub fn source_ai_launch_failed(&mut self, generation: u64, message: String) {
        self.finish_source_ai(generation, Err(message));
    }

    pub fn take_investigation_requests(&mut self) -> Vec<InvestigationRequest> {
        self.investigation_requests.drain(..).collect()
    }

    pub fn set_investigations(&mut self, items: Vec<InvestigationItem>) {
        // Loading is asynchronous. Merge by durable identity so a session
        // created while the scan ran cannot be replaced by stale disk state.
        for item in items {
            if !self
                .investigations
                .iter()
                .any(|existing| existing.id == item.id)
            {
                self.investigations.push(item);
            }
        }
        self.investigations.truncate(MAX_SAVED_INVESTIGATIONS);
        if let Some(dialog) = &mut self.investigation_dialog
            && matches!(dialog.stage, InvestigationStage::Input)
        {
            dialog.items = self.investigations.clone();
            dialog.selected = dialog.selected.min(dialog.items.len().saturating_sub(1));
        }
    }

    pub fn update_investigation_progress(
        &mut self,
        generation: u64,
        stage: InvestigationStage,
        progress: String,
        session_id: Option<String>,
        snapshot_dir: Option<String>,
        manifest_path: Option<String>,
    ) -> bool {
        let Some(dialog) = self
            .investigation_dialog
            .as_mut()
            .filter(|dialog| dialog.generation == generation)
        else {
            return false;
        };
        dialog.stage = stage;
        dialog.progress = progress;
        if session_id.is_some() {
            dialog.session_id = session_id;
        }
        if snapshot_dir.is_some() {
            dialog.snapshot_dir = snapshot_dir;
        }
        if manifest_path.is_some() {
            dialog.manifest_path = manifest_path;
        }
        true
    }

    pub fn investigation_ready(&mut self, generation: u64, item: InvestigationItem) -> bool {
        let Some(dialog) = self
            .investigation_dialog
            .as_mut()
            .filter(|dialog| dialog.generation == generation)
        else {
            return false;
        };
        dialog.investigation_id = Some(item.id.clone());
        dialog.session_id = Some(item.session_id.clone());
        dialog.snapshot_dir = Some(item.snapshot_dir.clone());
        dialog.manifest_path = Some(item.manifest_path.clone());
        dialog.stage = InvestigationStage::Sending;
        dialog.progress = "prompt accepted; waiting for local agent".into();
        if let Some(existing) = self
            .investigations
            .iter_mut()
            .find(|existing| existing.id == item.id)
        {
            *existing = item;
        } else {
            if self.investigations.len() >= MAX_SAVED_INVESTIGATIONS {
                self.investigations.pop();
            }
            self.investigations.insert(0, item);
        }
        dialog.items = self.investigations.clone();
        true
    }

    pub fn push_investigation_event(
        &mut self,
        session_id: &str,
        message: String,
        terminal: Result<(), String>,
    ) -> bool {
        let Some(dialog) = self
            .investigation_dialog
            .as_mut()
            .filter(|dialog| dialog.session_id.as_deref() == Some(session_id))
        else {
            return false;
        };
        if !message.is_empty() {
            push_bounded_message(&mut dialog.messages, bounded_message(message));
        }
        match terminal {
            Ok(()) => {
                dialog.stage = InvestigationStage::Conversation;
                dialog.progress = "turn complete; type a follow-up and press Enter".into();
            }
            Err(error) => {
                dialog.stage = InvestigationStage::Error;
                dialog.progress = error;
            }
        }
        true
    }

    pub fn append_investigation_output(&mut self, session_id: &str, message: String) -> bool {
        let Some(dialog) = self
            .investigation_dialog
            .as_mut()
            .filter(|dialog| dialog.session_id.as_deref() == Some(session_id))
        else {
            return false;
        };
        push_bounded_message(&mut dialog.messages, bounded_message(message));
        true
    }

    pub fn update_ask_ai_progress(
        &mut self,
        generation: u64,
        stage: AskAiStage,
        progress: String,
        session_id: Option<String>,
        snapshot_dir: Option<String>,
    ) -> bool {
        let Some(dialog) = self
            .ask_ai_dialog
            .as_mut()
            .filter(|dialog| dialog.generation == generation)
        else {
            return false;
        };
        dialog.stage = stage;
        dialog.progress = progress;
        if session_id.is_some() {
            dialog.session_id = session_id;
        }
        if snapshot_dir.is_some() {
            dialog.snapshot_dir = snapshot_dir;
        }
        true
    }

    pub fn finish_ask_ai(
        &mut self,
        generation: u64,
        view_id: &str,
        definition_revision: u64,
        expression: Result<(String, String), String>,
    ) -> bool {
        let definition_current =
            self.view_definition_revision(view_id) == Some(definition_revision);
        let Some(dialog) = self.ask_ai_dialog.as_mut().filter(|dialog| {
            dialog.generation == generation
                && dialog.view_id == view_id
                && dialog.definition_revision == definition_revision
        }) else {
            return false;
        };
        if !definition_current {
            dialog.stage = AskAiStage::Error;
            dialog.progress = "view definition changed; request a fresh proposal".into();
            return false;
        }
        match expression {
            Ok((value, explanation)) => {
                dialog.expression = Some(value);
                dialog.explanation = Some(explanation);
                dialog.stage = AskAiStage::Proposal;
                dialog.progress = "proposal ready; Enter applies through native validation".into();
            }
            Err(message) => {
                dialog.stage = AskAiStage::Error;
                dialog.progress = message;
            }
        }
        true
    }

    pub fn view_request_succeeded(&mut self, view_id: &str) {
        self.view_dialog = None;
        self.select_view(view_id);
        self.source_notice = Some("view saved".into());
    }

    pub fn view_request_failed(&mut self, message: String) {
        if let Some(dialog) = &mut self.view_dialog {
            dialog.error = Some(message.clone());
        }
        self.source_notice = Some(format!("view error: {message}"));
    }

    pub fn select_view(&mut self, view_id: &str) {
        if let Some(index) = self.views.iter().position(|view| view.id == view_id) {
            self.selected_view = index;
            self.focus = Focus::Logs;
        }
    }

    pub fn source_request_succeeded(&mut self, request: &SourceLaunchRequest, view_id: &str) {
        self.source_notice = Some("source started".into());
        self.select_view(view_id);
        if self
            .source_dialog
            .as_ref()
            .is_some_and(|dialog| dialog.kind == request.kind && dialog.draft == request.text)
        {
            self.source_dialog = None;
            self.focus = Focus::Logs;
        }
    }

    pub fn source_request_failed(&mut self, request: SourceLaunchRequest, message: String) {
        self.source_notice = Some(format!("source error: {message}"));
        match &mut self.source_dialog {
            Some(dialog) if dialog.kind == request.kind && dialog.draft == request.text => {
                dialog.error = Some(message);
            }
            Some(_) => {}
            None => {
                self.source_dialog = Some(SourceDialogState {
                    kind: request.kind,
                    draft: request.text,
                    error: Some(message),
                    mode: SourceDialogMode::Manual,
                    discovery: DiscoveryDialogState::default(),
                    path_completion: PathCompletionState::default(),
                    ai: SourceAiDialogState::default(),
                });
                self.focus = Focus::SourceDialog;
            }
        }
    }

    pub fn discovery_selection_succeeded(&mut self, generation: u64, view_id: &str) {
        self.source_notice = Some("discovered source started".into());
        self.select_view(view_id);
        if self.source_dialog.as_ref().is_some_and(|dialog| {
            dialog.mode == SourceDialogMode::Discovery && dialog.discovery.generation == generation
        }) {
            self.source_dialog = None;
            self.focus = Focus::Logs;
        }
    }

    pub fn discovery_selection_failed(&mut self, generation: u64, message: String) {
        self.source_notice = Some(format!("source error: {message}"));
        if let Some(dialog) = &mut self.source_dialog
            && dialog.mode == SourceDialogMode::Discovery
            && dialog.discovery.generation == generation
        {
            dialog.error = Some(message);
        }
    }

    pub fn update_source_health(&mut self, source_id: &str, health: String) {
        if let Some(source) = self
            .sources
            .iter_mut()
            .find(|source| source.id == source_id)
        {
            source.health = health;
        }
    }

    pub fn update_view_runtime_status(&mut self, view_id: &str, status: String) {
        self.view_runtime_status.insert(view_id.to_owned(), status);
    }

    pub fn active_view_runtime_status(&self) -> Option<&str> {
        self.active_view_id()
            .and_then(|view_id| self.view_runtime_status.get(view_id))
            .map(String::as_str)
    }

    pub fn visible_rows<P: RowProvider>(&self, provider: &P) -> Vec<DisplayRow> {
        let (Some(view_id), Some(state)) = (self.active_view_id(), self.view_state()) else {
            return Vec::new();
        };
        provider
            .page(
                view_id,
                ViewportRequest {
                    start: state.top,
                    len: state.viewport_height,
                },
            )
            .rows
    }

    pub fn selected_row<P: RowProvider>(&self, provider: &P) -> Option<DisplayRow> {
        let view_id = self.active_view_id()?;
        provider.row_by_id(view_id, self.view_state()?.selected.as_ref()?)
    }

    /// Recalculates the viewport even when provider content is unchanged.
    pub fn sync_provider<P: RowProvider>(&mut self, provider: &P, viewport_height: usize) -> bool {
        let Some(view_id) = self.active_view_id().map(str::to_owned) else {
            return false;
        };
        let revision = provider.revision(&view_id);
        let total = provider
            .page(&view_id, ViewportRequest { start: 0, len: 0 })
            .total;
        let height = viewport_height.max(1);
        let state = self
            .view_states
            .get_mut(&view_id)
            .expect("view state exists");
        let changed = revision != state.provider_revision
            || total != state.last_total
            || height != state.viewport_height;
        if !changed {
            return false;
        }
        state.provider_revision = revision;
        state.last_total = total;
        state.viewport_height = height;
        if total == 0 {
            state.top = 0;
        } else if state.follow {
            state.top = total.saturating_sub(height);
            state.selected = provider
                .page(
                    &view_id,
                    ViewportRequest {
                        start: total - 1,
                        len: 1,
                    },
                )
                .rows
                .first()
                .map(|row| row.id.clone());
        } else {
            let selected_index = state
                .selected
                .as_ref()
                .and_then(|id| provider.index_of_id(&view_id, id));
            state.top = state.top.min(total - 1);
            if let Some(index) = selected_index {
                if index < state.top {
                    state.top = index;
                }
                if index >= state.top + height {
                    state.top = index + 1 - height;
                }
            } else if state.selected.is_none() {
                state.selected = provider
                    .page(
                        &view_id,
                        ViewportRequest {
                            start: state.top,
                            len: 1,
                        },
                    )
                    .rows
                    .first()
                    .map(|row| row.id.clone());
            }
        }
        true
    }

    /// At most one unsent request per view and purpose is retained.
    pub fn take_query_requests(&mut self) -> Vec<QueryRequest> {
        self.query_requests
            .drain()
            .map(|(_, request)| request)
            .collect()
    }

    /// Enqueues due live searches. Tests pass a future instant to avoid sleeps.
    pub fn flush_debounced_searches(&mut self, now: Instant) -> bool {
        let due: Vec<String> = self
            .view_states
            .iter()
            .filter(|(_, state)| {
                state
                    .search
                    .search_due
                    .is_some_and(|deadline| deadline <= now)
            })
            .map(|(view_id, _)| view_id.clone())
            .collect();
        for view_id in &due {
            self.enqueue_query(view_id, QueryPurpose::Search);
            self.view_states
                .get_mut(view_id)
                .expect("view state")
                .search
                .search_due = None;
        }
        !due.is_empty()
    }

    /// Advances rolling capture-time policies using a caller-controlled clock.
    /// Clock refreshes are query revisions, not user definition revisions.
    pub fn refresh_rolling_capture_times(
        &mut self,
        now_unix_nanos: i64,
        elapsed_now: Instant,
    ) -> bool {
        let clock_moved_backward = self
            .last_clock_unix_nanos
            .is_some_and(|previous| now_unix_nanos < previous);
        self.clock_now_unix_nanos = now_unix_nanos;
        self.last_clock_unix_nanos = Some(now_unix_nanos);
        let cadence_due = clock_moved_backward
            || self
                .next_rolling_refresh
                .is_none_or(|deadline| elapsed_now >= deadline);
        if cadence_due {
            self.next_rolling_refresh = elapsed_now.checked_add(Duration::from_secs(1));
            for state in self.view_states.values_mut() {
                if matches!(
                    state.desired_capture_time_policy,
                    Some(CaptureTimePolicy::Recent { .. })
                ) {
                    state.rolling_refresh_due = true;
                }
            }
        }
        let rolling: Vec<(String, CaptureTimePolicy, CaptureTimeRange)> = self
            .view_states
            .iter()
            .filter_map(|(view_id, state)| {
                if !state.rolling_refresh_due || state_has_pending_query(state) {
                    return None;
                }
                let policy = state.desired_capture_time_policy?;
                let CaptureTimePolicy::Recent { .. } = policy else {
                    return None;
                };
                let range = resolve_capture_time_policy(policy, now_unix_nanos)?;
                Some((view_id.clone(), policy, range))
            })
            .collect();
        let mut changed = false;
        for (view_id, policy, range) in rolling {
            let previous = self
                .view_states
                .get(&view_id)
                .expect("collected view")
                .desired_constraints
                .capture_time;
            {
                let state = self.view_states.get_mut(&view_id).expect("collected view");
                state.desired_constraints.capture_time = Some(range);
                state.desired_capture_time_policy = Some(policy);
            }
            if self.enqueue_time_query(&view_id).is_some() {
                self.view_states
                    .get_mut(&view_id)
                    .expect("collected view")
                    .rolling_refresh_due = false;
                changed = true;
            } else {
                self.view_states
                    .get_mut(&view_id)
                    .expect("collected view")
                    .desired_constraints
                    .capture_time = previous;
            }
        }
        changed
    }

    pub fn apply_query_completion(&mut self, completion: QueryCompletion) -> bool {
        let Some(state) = self.view_states.get_mut(&completion.view_id) else {
            return false;
        };
        if completion.revision != state.desired_query_revision {
            return false;
        }
        let request_is_pending = [&state.search, &state.advanced, &state.enrichment]
            .into_iter()
            .any(|editor| editor.pending_generation == Some(completion.generation))
            || state
                .pending_time
                .as_ref()
                .is_some_and(|pending| pending.generation == completion.generation);
        if !request_is_pending {
            return false;
        }
        match completion.result {
            Ok(()) => {
                let constraints = state.desired_constraints.clone();
                let accepted_time_policy = state
                    .pending_time
                    .as_ref()
                    .filter(|pending| pending.revision <= completion.revision)
                    .map(|pending| pending.policy);
                if state
                    .pending_time
                    .as_ref()
                    .is_some_and(|pending| pending.revision <= completion.revision)
                {
                    state.pending_time = None;
                }
                let accepted_search = pending_at_or_before(&state.search, completion.revision);
                let accepted_advanced = pending_at_or_before(&state.advanced, completion.revision);
                let accepted_enrichment =
                    pending_at_or_before(&state.enrichment, completion.revision);
                let accepted_enrichment_draft = accepted_enrichment
                    && state.enrichment.pending_value.as_deref()
                        == Some(state.enrichment.draft.as_str());
                state.search.applied = constraint_text(&constraints);
                state.advanced.applied = constraints.advanced_polars.clone().unwrap_or_default();
                state.enrichment.applied = constraints.enrichment.clone().unwrap_or_default();
                state.applied_capture_time = constraints.capture_time;
                if let Some(policy) = accepted_time_policy {
                    state.applied_capture_time_policy = policy;
                }
                state.applied_query_revision = completion.revision;
                if state
                    .pending_recipe
                    .as_ref()
                    .is_some_and(|pending| pending.revision == completion.revision)
                    && let Some(pending) = state.pending_recipe.take()
                {
                    state.applied_capture_time_policy = pending.capture_time_policy;
                    if pending.interaction_revision == state.user_interaction_revision {
                        state.pinned_columns = pending.pinned_columns;
                        state.color_field = pending.color_field;
                    }
                }
                clear_accepted_pending(&mut state.search, completion.revision);
                clear_accepted_pending(&mut state.advanced, completion.revision);
                clear_accepted_pending(&mut state.enrichment, completion.revision);
                if accepted_search {
                    state.search.error = None;
                }
                if accepted_advanced {
                    state.advanced.error = None;
                }
                if accepted_enrichment_draft {
                    state.enrichment.error = None;
                }
            }
            Err(failure) => {
                if state
                    .pending_recipe
                    .as_ref()
                    .is_some_and(|pending| pending.revision == completion.revision)
                {
                    state.pending_recipe = None;
                    let failed_purpose = failure.purpose;
                    let failure_message = failure.message;
                    clear_accepted_pending(&mut state.search, completion.revision);
                    clear_accepted_pending(&mut state.advanced, completion.revision);
                    clear_accepted_pending(&mut state.enrichment, completion.revision);
                    state.desired_constraints = applied_constraints(state);
                    state.desired_capture_time_policy = state.applied_capture_time_policy;
                    editor_mut(state, failed_purpose).error = Some(failure_message.clone());
                    let accepted = match failed_purpose {
                        QueryPurpose::Search => state.search.applied.clone(),
                        QueryPurpose::Advanced => state.advanced.applied.clone(),
                        QueryPurpose::Enrichment => state.enrichment.applied.clone(),
                    };
                    self.enqueue_query_value(&completion.view_id, failed_purpose, Some(accepted));
                    self.editor_mut(&completion.view_id, failed_purpose).error =
                        Some(failure_message);
                    return true;
                }
                let failed_purpose = failure.purpose;
                let failure_message = failure.message;
                let pending_search = (failed_purpose != QueryPurpose::Search
                    && pending_at_or_before(&state.search, completion.revision))
                .then(|| state.search.pending_value.clone())
                .flatten();
                let pending_advanced = (failed_purpose != QueryPurpose::Advanced
                    && pending_at_or_before(&state.advanced, completion.revision))
                .then(|| state.advanced.pending_value.clone())
                .flatten();
                let pending_enrichment = (failed_purpose != QueryPurpose::Enrichment
                    && pending_at_or_before(&state.enrichment, completion.revision))
                .then(|| state.enrichment.pending_value.clone())
                .flatten();
                let pending_time = state
                    .pending_time
                    .as_ref()
                    .filter(|pending| pending.revision <= completion.revision)
                    .map(|pending| pending.value);
                let pending_time_policy = state
                    .pending_time
                    .as_ref()
                    .filter(|pending| pending.revision <= completion.revision)
                    .map(|pending| pending.policy);
                if pending_time.is_some() {
                    state.pending_time = None;
                }
                let editor = editor_mut(state, failed_purpose);
                editor.pending_generation = None;
                editor.pending_revision = None;
                editor.pending_value = None;
                editor.error = Some(failure_message.clone());
                state.desired_constraints = applied_constraints(state);
                state.desired_capture_time_policy = state.applied_capture_time_policy;
                if let Some(value) = &pending_search {
                    state.desired_constraints.text = nonempty_text(value);
                }
                if let Some(value) = &pending_advanced {
                    state.desired_constraints.advanced_polars = nonempty(value);
                }
                if let Some(value) = &pending_enrichment {
                    state.desired_constraints.enrichment = nonempty(value);
                }
                if let Some(value) = pending_time {
                    state.desired_constraints.capture_time = value;
                }
                if let Some(policy) = pending_time_policy {
                    state.desired_capture_time_policy = policy;
                }
                let counterpart = pending_enrichment
                    .map(|value| (QueryPurpose::Enrichment, value))
                    .or_else(|| pending_advanced.map(|value| (QueryPurpose::Advanced, value)))
                    .or_else(|| pending_search.map(|value| (QueryPurpose::Search, value)));
                let restore_applied = counterpart
                    .is_none()
                    .then(|| match failure.purpose {
                        QueryPurpose::Advanced if !state.search.applied.is_empty() => {
                            Some((QueryPurpose::Search, state.search.applied.clone()))
                        }
                        QueryPurpose::Enrichment if !state.enrichment.applied.is_empty() => {
                            Some((QueryPurpose::Enrichment, state.enrichment.applied.clone()))
                        }
                        _ if !state.advanced.applied.is_empty() => {
                            Some((QueryPurpose::Advanced, state.advanced.applied.clone()))
                        }
                        _ if !state.search.applied.is_empty() => {
                            Some((QueryPurpose::Search, state.search.applied.clone()))
                        }
                        _ => None,
                    })
                    .flatten();
                let rebase = if let Some((purpose, value)) = counterpart {
                    // The older counterpart was never allowed to publish. Rebase it
                    // on the last accepted constraint and give it a fresh revision.
                    self.enqueue_query_value(&completion.view_id, purpose, Some(value))
                } else if let Some((purpose, value)) = restore_applied {
                    // Dispatchers advance desired composite revisions before
                    // compilation. Reaffirm the accepted snapshot so arrivals
                    // cannot remain fenced by the rejected candidate.
                    self.enqueue_query_value(&completion.view_id, purpose, Some(value))
                } else if pending_time.is_some() {
                    self.enqueue_time_query(&completion.view_id)
                } else {
                    None
                };
                if let Some(revision) = rebase
                    && pending_time.is_some()
                    && self
                        .view_states
                        .get(&completion.view_id)
                        .is_some_and(|state| state.pending_time.is_none())
                {
                    self.track_time_request(&completion.view_id, revision, pending_time.flatten());
                }
                // Internal rebase submissions must not erase the diagnostic for
                // the user's rejected draft, even when the failed constraint is
                // also the only accepted constraint available to reaffirm.
                self.editor_mut(&completion.view_id, failed_purpose).error = Some(failure_message);
            }
        }
        true
    }

    pub fn handle<P: RowProvider>(&mut self, action: Action, provider: &P) {
        match action {
            Action::Quit => self.should_quit = true,
            Action::CycleFocus => {
                self.focus = match self.focus {
                    Focus::Selector => Focus::Logs,
                    Focus::Logs if !self.views.is_empty() => Focus::Selector,
                    Focus::Logs
                    | Focus::SearchEditor
                    | Focus::AdvancedEditor
                    | Focus::EnrichmentEditor
                    | Focus::SourceDialog
                    | Focus::ViewDialog
                    | Focus::FieldPicker
                    | Focus::AskAi
                    | Focus::Investigation => Focus::Logs,
                    Focus::Recipes | Focus::TimeEditor => Focus::Logs,
                }
            }
            Action::NextView | Action::SelectSidebar(1) => self.switch_view(1, provider),
            Action::PreviousView | Action::SelectSidebar(-1) => self.switch_view(-1, provider),
            Action::SelectSidebar(_) => {}
            Action::MoveLine(delta) => self.move_selection(delta, provider),
            Action::MovePage(delta) => {
                let height = self
                    .view_state()
                    .map_or(1, |state| state.viewport_height.max(1));
                self.move_selection(delta * height as i32, provider);
            }
            Action::Top => self.select_index(0, provider),
            Action::End => {
                if let Some(total) = self.view_state().map(|state| state.last_total)
                    && total > 0
                {
                    self.select_index(total - 1, provider);
                }
            }
            Action::ToggleDetails => self.show_details = !self.show_details,
            Action::ToggleHelp => self.show_help = !self.show_help,
            Action::ToggleFollow => self.toggle_follow(provider),
            Action::OpenSearch => {
                if self.active_view_id().is_some() {
                    self.focus = Focus::SearchEditor;
                }
            }
            Action::OpenAdvanced => {
                if self.active_view_id().is_some() {
                    self.focus = Focus::AdvancedEditor;
                }
            }
            Action::OpenEnrichment => {
                if self.active_view_id().is_some() {
                    self.focus = Focus::EnrichmentEditor;
                }
            }
            Action::OpenAskAi => {
                if let Some(view_id) = self.active_view_id().map(str::to_owned) {
                    let generation = self.next_ask_ai_generation;
                    self.next_ask_ai_generation = generation.saturating_add(1);
                    self.ask_ai_dialog = Some(AskAiDialogState {
                        generation,
                        definition_revision: self
                            .view_definition_revision(&view_id)
                            .unwrap_or_default(),
                        view_id,
                        kind: AskAiKind::Filter,
                        prompt: String::new(),
                        provider: self.ai_provider.clone(),
                        mode: self.ai_mode.clone(),
                        thinking: self.ai_thinking.clone(),
                        stage: AskAiStage::Input,
                        progress: "Describe the desired filter".into(),
                        expression: None,
                        explanation: None,
                        session_id: None,
                        snapshot_dir: None,
                    });
                    self.focus = Focus::AskAi;
                }
            }
            Action::OpenInvestigation => {
                if let Some(view_id) = self.active_view_id().map(str::to_owned) {
                    let generation = self.next_investigation_generation;
                    self.next_investigation_generation = generation.saturating_add(1);
                    self.investigation_dialog = Some(InvestigationDialogState {
                        generation,
                        definition_revision: self
                            .view_definition_revision(&view_id)
                            .unwrap_or_default(),
                        view_id,
                        stage: InvestigationStage::Input,
                        input: String::new(),
                        progress: if self.investigations.is_empty() {
                            "enter a question for a new fixed snapshot".into()
                        } else {
                            "type a new question, or leave blank and Enter to resume selected"
                                .into()
                        },
                        selected: 0,
                        items: self.investigations.clone(),
                        investigation_id: None,
                        session_id: None,
                        snapshot_dir: None,
                        manifest_path: None,
                        messages: VecDeque::new(),
                    });
                    self.focus = Focus::Investigation;
                }
            }
            Action::NewInvestigation if self.focus == Focus::Investigation => {
                if let Some(dialog) = &mut self.investigation_dialog {
                    dialog.stage = InvestigationStage::Input;
                    dialog.input.clear();
                    dialog.investigation_id = None;
                    dialog.session_id = None;
                    dialog.snapshot_dir = None;
                    dialog.manifest_path = None;
                    dialog.messages.clear();
                    dialog.progress = "enter a question for a new fixed snapshot".into();
                }
            }
            Action::MoveInvestigation(delta) if self.focus == Focus::Investigation => {
                if let Some(dialog) = &mut self.investigation_dialog
                    && dialog.stage == InvestigationStage::Input
                    && !dialog.items.is_empty()
                {
                    dialog.selected = (dialog.selected as i32 + delta)
                        .rem_euclid(dialog.items.len() as i32)
                        as usize;
                }
            }
            Action::SubmitInvestigation if self.focus == Focus::Investigation => {
                self.submit_investigation();
            }
            Action::SelectAskAiKind(kind) if self.focus == Focus::AskAi => {
                if let Some(dialog) = &mut self.ask_ai_dialog
                    && dialog.stage == AskAiStage::Input
                {
                    dialog.kind = kind;
                    dialog.progress = match kind {
                        AskAiKind::Filter => "Describe the desired filter",
                        AskAiKind::Enrichment => "Describe the field to derive",
                    }
                    .into();
                }
            }
            Action::SubmitAskAi if self.focus == Focus::AskAi => {
                if self
                    .ask_ai_dialog
                    .as_ref()
                    .is_some_and(|dialog| dialog.stage == AskAiStage::Proposal)
                {
                    self.handle(Action::ApplyAskAi, provider);
                    return;
                }
                if let Some(dialog) = &mut self.ask_ai_dialog
                    && matches!(dialog.stage, AskAiStage::Input | AskAiStage::Error)
                {
                    if dialog.prompt.trim().is_empty() {
                        dialog.stage = AskAiStage::Error;
                        dialog.progress = "request cannot be empty".into();
                    } else if self.view_states.get(&dialog.view_id).is_some_and(|state| {
                        state.search.pending_generation.is_some()
                            || state.advanced.pending_generation.is_some()
                            || state.enrichment.pending_generation.is_some()
                    }) {
                        dialog.stage = AskAiStage::Error;
                        dialog.progress =
                            "wait for the current view definition to finish applying".into();
                    } else if self.ask_ai_requests.len() >= MAX_AI_REQUESTS {
                        dialog.stage = AskAiStage::Error;
                        dialog.progress = "AI request queue is full".into();
                    } else {
                        dialog.stage = AskAiStage::Snapshot;
                        dialog.progress = "freezing applied view snapshot".into();
                        dialog.expression = None;
                        dialog.explanation = None;
                        self.ask_ai_requests.push_back(AskAiRequest::Start {
                            generation: dialog.generation,
                            view_id: dialog.view_id.clone(),
                            definition_revision: dialog.definition_revision,
                            kind: dialog.kind,
                            instruction: dialog.prompt.clone(),
                            provider: dialog.provider.clone(),
                            mode: dialog.mode.clone(),
                            thinking: dialog.thinking.clone(),
                        });
                    }
                }
            }
            Action::ApplyAskAi if self.focus == Focus::AskAi => {
                let proposal = self.ask_ai_dialog.as_ref().and_then(|dialog| {
                    (dialog.stage == AskAiStage::Proposal).then(|| {
                        (
                            dialog.view_id.clone(),
                            dialog.definition_revision,
                            dialog.kind,
                            dialog.expression.clone().unwrap_or_default(),
                        )
                    })
                });
                if let Some((view_id, revision, kind, expression)) = proposal {
                    if self.active_view_id() != Some(view_id.as_str())
                        || self.view_definition_revision(&view_id) != Some(revision)
                    {
                        if let Some(dialog) = &mut self.ask_ai_dialog {
                            dialog.stage = AskAiStage::Error;
                            dialog.progress = "view changed; request a fresh proposal".into();
                        }
                    } else {
                        self.focus = match kind {
                            AskAiKind::Filter => Focus::AdvancedEditor,
                            AskAiKind::Enrichment => Focus::EnrichmentEditor,
                        };
                        self.edit_active(|editor| {
                            editor.draft = expression;
                            editor.error = None;
                        });
                        self.ask_ai_dialog = None;
                        self.submit_draft();
                    }
                }
            }
            Action::OpenSource => {
                self.source_dialog.get_or_insert_with(Default::default);
                self.focus = Focus::SourceDialog;
            }
            Action::OpenRecipes => {
                let dialog_id = self.next_recipe_generation;
                self.next_recipe_generation = self.next_recipe_generation.saturating_add(1);
                self.recipe_dialog = Some(RecipeDialogState {
                    id: dialog_id,
                    loading: true,
                    status: "loading recipes…".into(),
                    ..Default::default()
                });
                self.focus = Focus::Recipes;
                if self.recipe_requests.len() < 8 {
                    let meta = self.next_recipe_request_meta(dialog_id, 0);
                    if let Some(dialog) = &mut self.recipe_dialog {
                        dialog.pending_request_id = Some(meta.request_id);
                    }
                    self.recipe_requests.push_back(RecipeRequest::List { meta });
                }
            }
            Action::OpenTime => {
                self.time_dialog = Some(TimeDialogState {
                    editing_end: false,
                    anchored_row: self.view_state().and_then(|state| state.selected.clone()),
                });
                self.focus = Focus::TimeEditor;
            }
            Action::SwitchTimeField if self.focus == Focus::TimeEditor => {
                if let Some(dialog) = &mut self.time_dialog {
                    dialog.editing_end = !dialog.editing_end;
                }
            }
            Action::TimeInput(ch) if self.focus == Focus::TimeEditor => {
                let editing_end = self
                    .time_dialog
                    .as_ref()
                    .is_some_and(|value| value.editing_end);
                if let Some(state) = self.view_state_mut() {
                    let draft = if editing_end {
                        &mut state.time_end_draft
                    } else {
                        &mut state.time_start_draft
                    };
                    if draft.len() < 64 {
                        draft.push(ch);
                    }
                    mark_time_edit(state);
                    state.time_error = None;
                }
            }
            Action::TimeBackspace if self.focus == Focus::TimeEditor => {
                let editing_end = self
                    .time_dialog
                    .as_ref()
                    .is_some_and(|value| value.editing_end);
                if let Some(state) = self.view_state_mut() {
                    if editing_end {
                        state.time_end_draft.pop()
                    } else {
                        state.time_start_draft.pop()
                    };
                    mark_time_edit(state);
                    state.time_error = None;
                }
            }
            Action::ClearTime if self.focus == Focus::TimeEditor => {
                if let Some(state) = self.view_state_mut() {
                    state.time_start_draft.clear();
                    state.time_end_draft.clear();
                    state.time_recent_draft.clear();
                    mark_time_edit(state);
                }
                self.submit_capture_time(None, None);
            }
            Action::AroundSelected if self.focus == Focus::TimeEditor => {
                if let Some(state) = self.view_state_mut() {
                    mark_time_edit(state);
                }
                let anchored = self
                    .time_dialog
                    .as_ref()
                    .and_then(|dialog| dialog.anchored_row.as_ref());
                if let Some(row) = self
                    .active_view_id()
                    .zip(anchored)
                    .and_then(|(view, id)| provider.row_by_id(view, id))
                {
                    if let Some(center) = row.captured_at_unix_nanos {
                        let start = center.saturating_sub(30_000_000_000);
                        let end = center.saturating_add(30_000_000_000);
                        if let Some(state) = self.view_state_mut() {
                            state.time_start_draft = format_utc_nanos(start);
                            state.time_end_draft = format_utc_nanos(end);
                            state.time_error = None;
                        }
                    } else if let Some(state) = self.view_state_mut() {
                        state.time_error = Some("selected record has no capture timestamp".into());
                    }
                } else if let Some(state) = self.view_state_mut() {
                    state.time_error = Some("select a timestamped record first".into());
                }
            }
            Action::SubmitTime if self.focus == Focus::TimeEditor => {
                if let Some(state) = self.view_state_mut() {
                    mark_time_edit(state);
                }
                let parsed = self.view_state().map(|state| {
                    parse_capture_range(&state.time_start_draft, &state.time_end_draft)
                });
                match parsed {
                    Some(Ok(window)) => self.submit_capture_time(
                        Some(window),
                        Some(CaptureTimePolicy::Absolute(window)),
                    ),
                    Some(Err(error)) => {
                        if let Some(state) = self.view_state_mut() {
                            state.time_error = Some(error);
                        }
                    }
                    None => {}
                }
            }
            Action::SetRecentTime(seconds) if self.focus == Focus::TimeEditor => {
                if let Some(window) = resolve_capture_time_policy(
                    CaptureTimePolicy::Recent { seconds },
                    self.clock_now_unix_nanos,
                ) {
                    if let Some(state) = self.view_state_mut() {
                        state.time_recent_draft = format_capture_duration(seconds);
                        state.time_error = None;
                        mark_time_edit(state);
                    }
                    self.submit_capture_time(
                        Some(window),
                        Some(CaptureTimePolicy::Recent { seconds }),
                    );
                }
            }
            Action::SelectRecipeMode(mode) if self.focus == Focus::Recipes => {
                let mut refresh = None;
                if let Some(dialog) = &mut self.recipe_dialog {
                    dialog.mode = mode;
                    dialog.status.clear();
                    dialog.interaction_revision = dialog.interaction_revision.saturating_add(1);
                    if mode == RecipeDialogMode::Browse && self.recipe_requests.len() < 8 {
                        refresh = Some((dialog.id, dialog.interaction_revision));
                        dialog.loading = true;
                    }
                }
                if let Some((dialog_id, revision)) = refresh {
                    let meta = self.next_recipe_request_meta(dialog_id, revision);
                    self.recipe_dialog
                        .as_mut()
                        .expect("recipe dialog")
                        .pending_request_id = Some(meta.request_id);
                    self.recipe_requests.push_back(RecipeRequest::List { meta });
                }
            }
            Action::MoveRecipe(delta) if self.focus == Focus::Recipes => {
                if let Some(dialog) = &mut self.recipe_dialog {
                    dialog.selected = move_index(dialog.selected, dialog.items.len(), delta);
                    dialog.interaction_revision = dialog.interaction_revision.saturating_add(1);
                }
            }
            Action::RecipeInput(ch) if self.focus == Focus::Recipes => {
                if let Some(dialog) = &mut self.recipe_dialog
                    && dialog.mode != RecipeDialogMode::Browse
                    && dialog.name.len() < MAX_EDITOR_BYTES
                {
                    dialog.name.push(ch);
                    dialog.interaction_revision = dialog.interaction_revision.saturating_add(1);
                }
            }
            Action::RecipeBackspace if self.focus == Focus::Recipes => {
                if let Some(dialog) = &mut self.recipe_dialog {
                    dialog.name.pop();
                    dialog.interaction_revision = dialog.interaction_revision.saturating_add(1);
                }
            }
            Action::SubmitRecipe if self.focus == Focus::Recipes => {
                let apply = self.recipe_dialog.as_ref().and_then(|dialog| {
                    (dialog.mode == RecipeDialogMode::Browse)
                        .then(|| dialog.items.get(dialog.selected).cloned())
                        .flatten()
                });
                if let Some(item) = apply {
                    if let Some(error) = item.incompatibility {
                        if let Some(dialog) = &mut self.recipe_dialog {
                            dialog.status = error;
                        }
                    } else {
                        if self.apply_recipe_to_active_view(item.config) {
                            self.recipe_dialog = None;
                            self.focus = Focus::Logs;
                        } else if let Some(dialog) = &mut self.recipe_dialog {
                            dialog.status =
                                "query queue is full; recipe draft was preserved".into();
                        }
                    }
                } else if let (Some((mode, name, dialog_id, dialog_revision)), Some(view_id)) = (
                    self.recipe_dialog.as_ref().map(|dialog| {
                        (
                            dialog.mode,
                            dialog.name.trim().to_owned(),
                            dialog.id,
                            dialog.interaction_revision,
                        )
                    }),
                    self.active_view_id().map(str::to_owned),
                ) {
                    if mode == RecipeDialogMode::Import
                        && !name.is_empty()
                        && self.recipe_requests.len() < 8
                    {
                        let meta = self.next_recipe_request_meta(dialog_id, dialog_revision);
                        self.recipe_requests
                            .push_back(RecipeRequest::Import { meta, path: name });
                        if let Some(dialog) = &mut self.recipe_dialog {
                            dialog.loading = true;
                            dialog.pending_request_id = Some(meta.request_id);
                            dialog.status = "importing for review…".into();
                        }
                    } else if mode == RecipeDialogMode::Save
                        && !name.is_empty()
                        && self.recipe_requests.len() < 8
                    {
                        let config = self
                            .persistent_view_state(&view_id)
                            .map(|state| RecipeConfig {
                                search: state.applied_search,
                                advanced: state.applied_advanced,
                                enrichment: state.applied_enrichment,
                                pinned_columns: state.pinned_columns,
                                color_field: state.color_field,
                                capture_time: state.applied_capture_time,
                                capture_time_policy: state.applied_capture_time_policy,
                            })
                            .unwrap_or_default();
                        let meta = self.next_recipe_request_meta(dialog_id, dialog_revision);
                        self.recipe_requests.push_back(RecipeRequest::Save {
                            meta,
                            name,
                            view_id,
                            config,
                        });
                        if let Some(dialog) = &mut self.recipe_dialog {
                            dialog.loading = true;
                            dialog.pending_request_id = Some(meta.request_id);
                            dialog.status = "saving recipe…".into();
                        }
                    } else if let Some(dialog) = &mut self.recipe_dialog {
                        dialog.status = "enter a recipe name or select a recipe".into();
                    }
                }
            }
            Action::OpenViewDialog => {
                if let Some(view) = self.views.get(self.selected_view) {
                    self.view_dialog = Some(ViewDialogState {
                        mode: ViewDialogMode::Clone,
                        draft: format!("Copy of {}", view.name),
                        error: None,
                    });
                    self.focus = Focus::ViewDialog;
                }
            }
            Action::SelectViewDialogMode(mode) if self.focus == Focus::ViewDialog => {
                if let (Some(dialog), Some(view)) =
                    (&mut self.view_dialog, self.views.get(self.selected_view))
                {
                    dialog.mode = mode;
                    dialog.error = None;
                    dialog.draft = match mode {
                        ViewDialogMode::Blank => "New view".into(),
                        ViewDialogMode::Clone => format!("Copy of {}", view.name),
                        ViewDialogMode::Rename => view.name.clone(),
                    };
                }
            }
            Action::ViewInput(character) if self.focus == Focus::ViewDialog => {
                if let Some(dialog) = &mut self.view_dialog
                    && dialog.draft.len() < 128
                {
                    dialog.draft.push(character);
                    dialog.error = None;
                }
            }
            Action::ViewBackspace if self.focus == Focus::ViewDialog => {
                if let Some(dialog) = &mut self.view_dialog {
                    dialog.draft.pop();
                    dialog.error = None;
                }
            }
            Action::SubmitViewDialog if self.focus == Focus::ViewDialog => {
                let Some(dialog) = self.view_dialog.as_mut() else {
                    return;
                };
                let name = dialog.draft.trim();
                let Some(view) = self.views.get(self.selected_view) else {
                    return;
                };
                if name.is_empty() {
                    dialog.error = Some("view name cannot be empty".into());
                } else if self.view_requests.len() >= 8 {
                    dialog.error = Some("view request queue is full".into());
                } else {
                    self.view_requests.push_back(ViewMutationRequest {
                        mode: dialog.mode,
                        source_id: view.source_id.clone(),
                        view_id: view.id.clone(),
                        name: name.to_owned(),
                    });
                }
            }
            Action::OpenFieldPicker => {
                if let Some(row) = self.selected_row(provider)
                    && !row.fields.is_empty()
                    && let Some(state) = self.view_state_mut()
                {
                    state.field_picker_row = Some(row.id);
                    state.field_picker_selected = 0;
                    state.field_picker_top = 0;
                    self.focus = Focus::FieldPicker;
                }
            }
            Action::MoveFieldPicker(delta) if self.focus == Focus::FieldPicker => {
                let count = self
                    .field_picker_row(provider)
                    .map_or(0, |row| row.fields.len());
                if let Some(state) = self.view_state_mut()
                    && count > 0
                {
                    state.field_picker_selected = (state.field_picker_selected as i32 + delta)
                        .rem_euclid(count as i32)
                        as usize;
                }
            }
            Action::TogglePinnedField if self.focus == Focus::FieldPicker => {
                self.update_selected_field(provider, true);
            }
            Action::ToggleColorField if self.focus == Focus::FieldPicker => {
                self.update_selected_field(provider, false);
            }
            Action::ToggleDiscovery if self.focus == Focus::SourceDialog => {
                let dialog = self.source_dialog.as_mut().expect("source dialog");
                dialog.mode = match dialog.mode {
                    SourceDialogMode::Manual => SourceDialogMode::Discovery,
                    SourceDialogMode::Discovery | SourceDialogMode::Ai => SourceDialogMode::Manual,
                };
                clear_path_completion(dialog);
                if dialog.mode == SourceDialogMode::Discovery && dialog.discovery.generation == 0 {
                    self.start_discovery_scan();
                }
            }
            Action::ToggleSourceAi if self.focus == Focus::SourceDialog => {
                let dialog = self.source_dialog.as_mut().expect("source dialog");
                dialog.mode = if dialog.mode == SourceDialogMode::Ai {
                    SourceDialogMode::Manual
                } else {
                    SourceDialogMode::Ai
                };
                clear_path_completion(dialog);
            }
            Action::RefreshDiscovery if self.focus == Focus::SourceDialog => {
                self.start_discovery_scan();
            }
            Action::MoveDiscovery(delta) if self.focus == Focus::SourceDialog => {
                self.move_discovery(delta);
            }
            Action::ToggleSourceKind if self.focus == Focus::SourceDialog => {
                if let Some(dialog) = &mut self.source_dialog {
                    if dialog.mode != SourceDialogMode::Manual {
                        return;
                    }
                    dialog.kind = match dialog.kind {
                        SourceKind::File => SourceKind::Command,
                        SourceKind::Command => SourceKind::File,
                    };
                    dialog.error = None;
                    clear_path_completion(dialog);
                }
            }
            Action::SelectSourceKind(kind) if self.focus == Focus::SourceDialog => {
                if let Some(dialog) = &mut self.source_dialog
                    && dialog.mode == SourceDialogMode::Manual
                {
                    dialog.kind = kind;
                    dialog.error = None;
                    clear_path_completion(dialog);
                }
            }
            Action::CompleteSourcePath if self.focus == Focus::SourceDialog => {
                self.complete_source_path();
            }
            Action::MovePathCompletion(delta) if self.focus == Focus::SourceDialog => {
                if self
                    .source_dialog
                    .as_ref()
                    .is_some_and(|dialog| dialog.mode == SourceDialogMode::Discovery)
                {
                    self.move_discovery(delta);
                    return;
                }
                if let Some(dialog) = &mut self.source_dialog
                    && dialog.mode == SourceDialogMode::Ai
                    && dialog.ai.stage == SourceAiStage::Proposal
                {
                    let count = dialog
                        .ai
                        .preview
                        .as_ref()
                        .map_or(0, |preview| preview.environment.len().saturating_add(6));
                    dialog.ai.preview_scroll = dialog
                        .ai
                        .preview_scroll
                        .saturating_add_signed(delta as isize)
                        .min(count.saturating_sub(1));
                    return;
                }
                if let Some(dialog) = &mut self.source_dialog
                    && dialog.mode == SourceDialogMode::Manual
                    && dialog.kind == SourceKind::File
                    && !dialog.path_completion.candidates.is_empty()
                {
                    dialog.path_completion.selected = move_index(
                        dialog.path_completion.selected,
                        dialog.path_completion.candidates.len(),
                        delta,
                    );
                }
            }
            Action::SourceInput(character) if self.focus == Focus::SourceDialog => {
                if let Some(dialog) = &mut self.source_dialog {
                    match dialog.mode {
                        SourceDialogMode::Discovery => {
                            self.append_discovery_query(&character.to_string())
                        }
                        SourceDialogMode::Ai
                            if matches!(
                                dialog.ai.stage,
                                SourceAiStage::Input | SourceAiStage::Error
                            ) =>
                        {
                            if dialog.ai.instruction.len() < 8 * 1024 {
                                dialog.ai.instruction.push(character);
                                dialog.ai.stage = SourceAiStage::Input;
                            }
                        }
                        SourceDialogMode::Manual => self.append_source(&character.to_string()),
                        SourceDialogMode::Ai => {}
                    }
                }
            }
            Action::SourceBackspace if self.focus == Focus::SourceDialog => {
                if let Some(dialog) = &mut self.source_dialog {
                    match dialog.mode {
                        SourceDialogMode::Discovery => {
                            dialog.discovery.query.pop();
                            dialog.discovery.selected = 0;
                        }
                        SourceDialogMode::Ai
                            if matches!(
                                dialog.ai.stage,
                                SourceAiStage::Input | SourceAiStage::Error
                            ) =>
                        {
                            dialog.ai.instruction.pop();
                            dialog.ai.stage = SourceAiStage::Input;
                        }
                        SourceDialogMode::Manual => {
                            dialog.draft.pop();
                            clear_path_completion(dialog);
                        }
                        SourceDialogMode::Ai => {}
                    }
                    dialog.error = None;
                }
            }
            Action::SubmitSource if self.focus == Focus::SourceDialog => {
                match self.source_dialog.as_ref().map(|dialog| dialog.mode) {
                    Some(SourceDialogMode::Discovery) => self.submit_discovered_source(),
                    Some(SourceDialogMode::Ai) => self.submit_source_ai(),
                    _ => self.submit_source(),
                }
            }
            Action::EditorInput(character) if self.editor_open() => {
                self.append_editor(&character.to_string())
            }
            Action::EditorInput(character) if self.focus == Focus::AskAi => {
                self.append_ask_ai(&character.to_string())
            }
            Action::EditorInput(character) if self.focus == Focus::Investigation => {
                self.append_investigation(&character.to_string())
            }
            Action::EditorBackspace if self.editor_open() => {
                self.edit_active(|editor| {
                    editor.draft.pop();
                });
                self.schedule_search();
            }
            Action::EditorBackspace if self.focus == Focus::AskAi => {
                if let Some(dialog) = &mut self.ask_ai_dialog
                    && matches!(dialog.stage, AskAiStage::Input | AskAiStage::Error)
                {
                    dialog.prompt.pop();
                    dialog.stage = AskAiStage::Input;
                }
            }
            Action::EditorBackspace if self.focus == Focus::Investigation => {
                if let Some(dialog) = &mut self.investigation_dialog
                    && matches!(
                        dialog.stage,
                        InvestigationStage::Input
                            | InvestigationStage::Conversation
                            | InvestigationStage::Error
                    )
                {
                    dialog.input.pop();
                }
            }
            Action::EditorPaste(text) if self.focus == Focus::SourceDialog => {
                match self.source_dialog.as_ref().map(|dialog| dialog.mode) {
                    Some(SourceDialogMode::Discovery) => self.append_discovery_query(&text),
                    Some(SourceDialogMode::Ai) => {
                        if let Some(dialog) = &mut self.source_dialog
                            && matches!(
                                dialog.ai.stage,
                                SourceAiStage::Input | SourceAiStage::Error
                            )
                        {
                            let remaining =
                                (8_usize * 1024).saturating_sub(dialog.ai.instruction.len());
                            let mut end = text.len().min(remaining);
                            while !text.is_char_boundary(end) {
                                end -= 1;
                            }
                            dialog.ai.instruction.push_str(&text[..end]);
                            dialog.ai.stage = SourceAiStage::Input;
                        }
                    }
                    _ => self.append_source(&text),
                }
            }
            Action::EditorPaste(text) if self.focus == Focus::ViewDialog => {
                for character in text.chars() {
                    self.handle(Action::ViewInput(character), provider);
                }
            }
            Action::EditorPaste(text) if self.focus == Focus::AskAi => {
                self.append_ask_ai(&text);
            }
            Action::EditorPaste(text) if self.focus == Focus::Investigation => {
                self.append_investigation(&text);
            }
            Action::EditorPaste(text) if self.focus == Focus::TimeEditor => {
                for ch in text.chars().take(64) {
                    self.handle(Action::TimeInput(ch), provider);
                }
            }
            Action::EditorPaste(text) if self.editor_open() => self.append_editor(&text),
            Action::SubmitDraft if self.editor_open() => self.submit_draft(),
            Action::CancelEditor => {
                if self.focus == Focus::FieldPicker {
                    self.focus = Focus::Logs;
                    return;
                }
                if self.focus == Focus::Recipes {
                    self.recipe_dialog = None;
                    self.focus = Focus::Logs;
                    return;
                }
                if self.focus == Focus::TimeEditor {
                    self.time_dialog = None;
                    self.focus = Focus::Logs;
                    return;
                }
                if self.focus == Focus::SourceDialog {
                    if let Some(dialog) = &self.source_dialog
                        && dialog.discovery.scanning
                        && self.discovery_requests.len() < MAX_DISCOVERY_REQUESTS
                    {
                        self.discovery_requests
                            .push_back(DiscoveryUiRequest::Cancel {
                                generation: dialog.discovery.generation,
                            });
                    }
                    if let Some(dialog) = &self.source_dialog
                        && !matches!(dialog.ai.stage, SourceAiStage::Input | SourceAiStage::Error)
                        && self.source_ai_requests.len() < 8
                    {
                        self.source_ai_requests.push_back(SourceAiRequest::Cancel {
                            generation: dialog.ai.generation,
                        });
                    }
                    self.source_dialog = None;
                }
                if self.focus == Focus::ViewDialog {
                    self.view_dialog = None;
                }
                if self.focus == Focus::AskAi
                    && let Some(dialog) = self.ask_ai_dialog.take()
                    && !matches!(dialog.stage, AskAiStage::Input | AskAiStage::Error)
                    && self.ask_ai_requests.len() < MAX_AI_REQUESTS
                {
                    self.ask_ai_requests.push_back(AskAiRequest::Cancel {
                        generation: dialog.generation,
                    });
                }
                if self.focus == Focus::Investigation
                    && let Some(dialog) = self.investigation_dialog.take()
                    && !matches!(
                        dialog.stage,
                        InvestigationStage::Input | InvestigationStage::Error
                    )
                    && self.investigation_requests.len() < MAX_INVESTIGATION_REQUESTS
                {
                    self.investigation_requests
                        .push_back(InvestigationRequest::Cancel {
                            generation: dialog.generation,
                        });
                }
                self.focus = Focus::Logs;
            }
            Action::Resize(width, height) => self.terminal_size = (width, height),
            Action::Mouse(event) => self.handle_mouse(event, provider),
            Action::FixtureAdvance | Action::None => {}
            Action::EditorInput(_)
            | Action::EditorBackspace
            | Action::EditorPaste(_)
            | Action::SubmitDraft => {}
            Action::NewInvestigation
            | Action::MoveInvestigation(_)
            | Action::SubmitInvestigation => {}
            Action::MoveFieldPicker(_)
            | Action::TogglePinnedField
            | Action::ToggleColorField
            | Action::ToggleSourceKind
            | Action::SelectSourceKind(_)
            | Action::CompleteSourcePath
            | Action::MovePathCompletion(_)
            | Action::ToggleDiscovery
            | Action::ToggleSourceAi
            | Action::RefreshDiscovery
            | Action::MoveDiscovery(_)
            | Action::SourceInput(_)
            | Action::SourceBackspace
            | Action::SubmitSource
            | Action::SelectViewDialogMode(_)
            | Action::SubmitViewDialog
            | Action::ViewInput(_)
            | Action::ViewBackspace
            | Action::SelectAskAiKind(_)
            | Action::SubmitAskAi
            | Action::ApplyAskAi => {}
            Action::SelectRecipeMode(_)
            | Action::MoveRecipe(_)
            | Action::RecipeInput(_)
            | Action::RecipeBackspace
            | Action::SubmitRecipe
            | Action::TimeInput(_)
            | Action::TimeBackspace
            | Action::SwitchTimeField
            | Action::SubmitTime
            | Action::ClearTime
            | Action::AroundSelected
            | Action::SetRecentTime(_) => {}
        }
    }

    fn append_ask_ai(&mut self, text: &str) {
        let Some(dialog) = &mut self.ask_ai_dialog else {
            return;
        };
        if !matches!(dialog.stage, AskAiStage::Input | AskAiStage::Error) {
            return;
        }
        if dialog.stage == AskAiStage::Error {
            dialog.stage = AskAiStage::Input;
        }
        let remaining = MAX_AI_PROMPT_BYTES.saturating_sub(dialog.prompt.len());
        let mut end = text.len().min(remaining);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        dialog.prompt.push_str(&text[..end]);
    }

    fn append_investigation(&mut self, text: &str) {
        let Some(dialog) = &mut self.investigation_dialog else {
            return;
        };
        if !matches!(
            dialog.stage,
            InvestigationStage::Input
                | InvestigationStage::Conversation
                | InvestigationStage::Error
        ) {
            return;
        }
        let remaining = MAX_AI_PROMPT_BYTES.saturating_sub(dialog.input.len());
        let mut end = text.len().min(remaining);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        dialog.input.push_str(&text[..end]);
    }

    fn submit_investigation(&mut self) {
        if self.investigation_requests.len() >= MAX_INVESTIGATION_REQUESTS {
            if let Some(dialog) = &mut self.investigation_dialog {
                dialog.stage = InvestigationStage::Error;
                dialog.progress = "investigation request queue is full".into();
            }
            return;
        }
        let Some(dialog) = &mut self.investigation_dialog else {
            return;
        };
        match dialog.stage {
            InvestigationStage::Input if dialog.input.trim().is_empty() => {
                let Some(item) = dialog.items.get(dialog.selected).cloned() else {
                    dialog.stage = InvestigationStage::Error;
                    dialog.progress = "enter a question to start an investigation".into();
                    return;
                };
                dialog.stage = InvestigationStage::Resuming;
                dialog.progress = "resuming selected local Paseo session".into();
                self.investigation_requests
                    .push_back(InvestigationRequest::Resume {
                        generation: dialog.generation,
                        item,
                    });
            }
            InvestigationStage::Input => {
                let question = std::mem::take(&mut dialog.input);
                dialog.stage = InvestigationStage::Snapshot;
                dialog.progress = "freezing applied view snapshot".into();
                push_bounded_message(&mut dialog.messages, format!("You: {question}"));
                self.investigation_requests
                    .push_back(InvestigationRequest::Start {
                        generation: dialog.generation,
                        view_id: dialog.view_id.clone(),
                        definition_revision: dialog.definition_revision,
                        question,
                        provider: self.ai_provider.clone(),
                        mode: self.ai_mode.clone(),
                        thinking: self.ai_thinking.clone(),
                    });
            }
            InvestigationStage::Conversation | InvestigationStage::Error => {
                let Some(session_id) = dialog.session_id.clone() else {
                    dialog.stage = InvestigationStage::Error;
                    dialog.progress = "session is unavailable; start or resume again".into();
                    return;
                };
                if dialog.input.trim().is_empty() {
                    dialog.progress = "enter a follow-up question".into();
                    return;
                }
                let prompt = std::mem::take(&mut dialog.input);
                push_bounded_message(&mut dialog.messages, format!("You: {prompt}"));
                dialog.stage = InvestigationStage::Sending;
                dialog.progress = "sending follow-up to local agent".into();
                self.investigation_requests
                    .push_back(InvestigationRequest::Send {
                        generation: dialog.generation,
                        session_id,
                        prompt,
                    });
            }
            InvestigationStage::Snapshot
            | InvestigationStage::StartingSession
            | InvestigationStage::Resuming
            | InvestigationStage::Sending
            | InvestigationStage::Cancelling => {}
        }
    }

    fn append_source(&mut self, text: &str) {
        let Some(dialog) = &mut self.source_dialog else {
            return;
        };
        let remaining = MAX_EDITOR_BYTES.saturating_sub(dialog.draft.len());
        let mut end = text.len().min(remaining);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        dialog.draft.push_str(&text[..end]);
        dialog.error = None;
        clear_path_completion(dialog);
    }

    fn update_selected_field<P: RowProvider>(&mut self, provider: &P, pin: bool) {
        let Some(row) = self.field_picker_row(provider) else {
            return;
        };
        let selected = self
            .view_state()
            .map_or(0, |state| state.field_picker_selected);
        let Some((field, _)) = row.fields.get(selected) else {
            return;
        };
        let field = field.clone();
        let Some(state) = self.view_state_mut() else {
            return;
        };
        if pin {
            if let Some(index) = state
                .pinned_columns
                .iter()
                .position(|value| value == &field)
            {
                state.pinned_columns.remove(index);
            } else if state.pinned_columns.len() < 8 {
                state.pinned_columns.push(field);
            }
        } else if state.color_field.as_deref() == Some(&field) {
            state.color_field = None;
        } else {
            state.color_field = Some(field);
        }
        state.user_interaction_revision = state.user_interaction_revision.saturating_add(1);
    }

    pub fn field_picker_row<P: RowProvider>(&self, provider: &P) -> Option<DisplayRow> {
        let view_id = self.active_view_id()?;
        let id = self.view_state()?.field_picker_row.as_ref()?;
        provider.row_by_id(view_id, id)
    }

    pub fn set_field_picker_viewport(&mut self, visible: usize) {
        let Some(state) = self.view_state_mut() else {
            return;
        };
        if state.field_picker_selected < state.field_picker_top {
            state.field_picker_top = state.field_picker_selected;
        } else if state.field_picker_selected >= state.field_picker_top.saturating_add(visible) {
            state.field_picker_top = state
                .field_picker_selected
                .saturating_add(1)
                .saturating_sub(visible);
        }
    }

    fn complete_source_path(&mut self) {
        let Some(dialog) = &mut self.source_dialog else {
            return;
        };
        if dialog.mode != SourceDialogMode::Manual || dialog.kind != SourceKind::File {
            return;
        }
        if let Some(candidate) = dialog
            .path_completion
            .candidates
            .get(dialog.path_completion.selected)
            .cloned()
        {
            dialog.draft = candidate;
            clear_path_completion(dialog);
            return;
        }
        let generation = self.next_path_completion_generation;
        self.next_path_completion_generation = self.next_path_completion_generation.wrapping_add(1);
        dialog.path_completion.generation = generation;
        dialog.path_completion.scanning = true;
        dialog.error = None;
        self.path_completion_requests.clear();
        self.path_completion_requests
            .push_back(PathCompletionRequest {
                generation,
                draft: dialog.draft.clone(),
            });
    }

    fn append_discovery_query(&mut self, text: &str) {
        let Some(discovery) = self
            .source_dialog
            .as_mut()
            .map(|dialog| &mut dialog.discovery)
        else {
            return;
        };
        let remaining = MAX_EDITOR_BYTES.saturating_sub(discovery.query.len());
        let mut end = text.len().min(remaining);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        discovery.query.push_str(&text[..end]);
        discovery.selected = 0;
    }

    fn start_discovery_scan(&mut self) {
        let Some(discovery) = self
            .source_dialog
            .as_mut()
            .map(|dialog| &mut dialog.discovery)
        else {
            return;
        };
        if discovery.generation > 0 && discovery.scanning {
            self.discovery_requests
                .push_back(DiscoveryUiRequest::Cancel {
                    generation: discovery.generation,
                });
        }
        discovery.generation = discovery.generation.saturating_add(1).max(1);
        discovery.scanning = true;
        discovery.items.clear();
        discovery.selected = 0;
        discovery.status = "scanning bounded local providers…".into();
        if self.discovery_requests.len() < MAX_DISCOVERY_REQUESTS {
            self.discovery_requests.push_back(DiscoveryUiRequest::Scan {
                generation: discovery.generation,
            });
        } else {
            discovery.scanning = false;
            discovery.status = "discovery request queue is full".into();
        }
    }

    fn move_discovery(&mut self, delta: i32) {
        let Some(dialog) = &mut self.source_dialog else {
            return;
        };
        let count = filtered_discovery_indices(&dialog.discovery).len();
        if count == 0 {
            dialog.discovery.selected = 0;
            return;
        }
        dialog.discovery.selected = dialog
            .discovery
            .selected
            .saturating_add_signed(delta as isize)
            .min(count - 1);
    }

    fn submit_discovered_source(&mut self) {
        let Some(dialog) = &mut self.source_dialog else {
            return;
        };
        let indices = filtered_discovery_indices(&dialog.discovery);
        let Some(index) = indices.get(dialog.discovery.selected).copied() else {
            dialog.error = Some("no matching discovered source to start".into());
            return;
        };
        if self.discovery_requests.len() >= MAX_DISCOVERY_REQUESTS {
            dialog.error = Some("discovery action queue is full".into());
            return;
        }
        self.discovery_requests
            .push_back(DiscoveryUiRequest::Select {
                generation: dialog.discovery.generation,
                key: dialog.discovery.items[index].key.clone(),
            });
        dialog.error = Some("starting selected source…".into());
    }

    fn submit_source(&mut self) {
        let Some(dialog) = &mut self.source_dialog else {
            return;
        };
        if dialog.draft.is_empty() {
            dialog.error = Some("enter a file path or shell command".into());
            return;
        }
        if self.source_requests.len() >= MAX_SOURCE_REQUESTS {
            dialog.error = Some("source launch queue is full".into());
            return;
        }
        self.source_requests.push_back(SourceLaunchRequest {
            kind: dialog.kind,
            text: dialog.draft.clone(),
        });
        dialog.error = Some("starting source…".into());
    }

    fn submit_source_ai(&mut self) {
        let Some(dialog) = &mut self.source_dialog else {
            return;
        };
        if dialog.ai.stage == SourceAiStage::Proposal {
            if self.source_ai_requests.len() >= 8 {
                dialog.ai.progress = "source AI request queue is full".into();
            } else {
                self.source_ai_requests.push_back(SourceAiRequest::Apply {
                    generation: dialog.ai.generation,
                });
                dialog.ai.progress = "starting reviewed source…".into();
            }
            return;
        }
        if !matches!(dialog.ai.stage, SourceAiStage::Input | SourceAiStage::Error) {
            return;
        }
        if dialog.ai.instruction.trim().is_empty() {
            dialog.ai.stage = SourceAiStage::Error;
            dialog.ai.progress = "describe the source to follow".into();
            return;
        }
        if self.source_ai_requests.len() >= 8 {
            dialog.ai.stage = SourceAiStage::Error;
            dialog.ai.progress = "source AI request queue is full".into();
            return;
        }
        self.next_source_ai_generation = self.next_source_ai_generation.saturating_add(1);
        dialog.ai.generation = self.next_source_ai_generation;
        dialog.ai.stage = SourceAiStage::Preparing;
        dialog.ai.progress = "collecting bounded read-only discovery context".into();
        dialog.ai.preview = None;
        self.source_ai_requests.push_back(SourceAiRequest::Start {
            generation: dialog.ai.generation,
            instruction: dialog.ai.instruction.clone(),
            provider: self.ai_provider.clone(),
            mode: self.ai_mode.clone(),
            thinking: self.ai_thinking.clone(),
        });
    }

    fn append_editor(&mut self, text: &str) {
        let Some(id) = self.active_view_id().map(str::to_owned) else {
            return;
        };
        let purpose = self.editor_purpose().expect("editor open");
        let state = self.view_states.get_mut(&id).expect("view state");
        let draft = match purpose {
            QueryPurpose::Search => &mut state.search.draft,
            QueryPurpose::Advanced => &mut state.advanced.draft,
            QueryPurpose::Enrichment => &mut state.enrichment.draft,
        };
        let remaining = MAX_EDITOR_BYTES.saturating_sub(draft.len());
        let mut end = text.len().min(remaining);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        draft.push_str(&text[..end]);
        state.user_interaction_revision = state.user_interaction_revision.saturating_add(1);
        state.ai_definition_revision = state.ai_definition_revision.saturating_add(1);
        self.schedule_search();
    }

    fn submit_draft(&mut self) {
        let Some(view_id) = self.active_view_id().map(str::to_owned) else {
            return;
        };
        let Some(purpose) = self.editor_purpose() else {
            return;
        };
        if purpose == QueryPurpose::Search {
            self.view_states
                .get_mut(&view_id)
                .expect("view state")
                .search
                .search_due = None;
        }
        let state = self.view_states.get_mut(&view_id).expect("view state");
        state.user_interaction_revision = state.user_interaction_revision.saturating_add(1);
        state.ai_definition_revision = state.ai_definition_revision.saturating_add(1);
        self.enqueue_query(&view_id, purpose);
    }

    fn submit_capture_time(
        &mut self,
        window: Option<CaptureTimeRange>,
        policy: Option<CaptureTimePolicy>,
    ) {
        let Some(view_id) = self.active_view_id().map(str::to_owned) else {
            return;
        };
        let state = self.view_states.get_mut(&view_id).expect("view state");
        state.desired_constraints.capture_time = window;
        state.desired_capture_time_policy = policy;
        state.time_error = None;
        if self.enqueue_time_query(&view_id).is_some() {
            self.time_dialog = None;
            self.focus = Focus::Logs;
        } else {
            let state = self.view_states.get_mut(&view_id).expect("view state");
            state.desired_constraints = applied_constraints(state);
            state.desired_capture_time_policy = state.applied_capture_time_policy;
            state.time_error = Some("query submission queue is full; last window preserved".into());
        }
    }

    fn enqueue_query(&mut self, view_id: &str, purpose: QueryPurpose) -> Option<u64> {
        self.enqueue_query_value(view_id, purpose, None)
    }

    fn enqueue_time_query(&mut self, view_id: &str) -> Option<u64> {
        let key = (view_id.to_owned(), QueryPurpose::Advanced);
        if !self.query_requests.contains_key(&key)
            && self.query_requests.len() >= MAX_PENDING_QUERY_REQUESTS
        {
            return None;
        }
        let generation = self.next_query_generation;
        self.next_query_generation = self.next_query_generation.saturating_add(1);
        let state = self.view_states.get_mut(view_id).expect("view state");
        let base_revision = state.applied_query_revision;
        let base_constraints = applied_constraints(state);
        state.desired_query_revision = state.desired_query_revision.saturating_add(1);
        let revision = state.desired_query_revision;
        let constraints = state.desired_constraints.clone();
        state.pending_time = Some(PendingTime {
            generation,
            revision,
            value: constraints.capture_time,
            policy: state.desired_capture_time_policy,
        });
        self.query_requests.insert(
            key,
            QueryRequest {
                view_id: view_id.to_owned(),
                generation,
                revision,
                base_revision,
                base_constraints,
                purpose: QueryPurpose::Advanced,
                constraints,
            },
        );
        Some(revision)
    }

    fn track_time_request(
        &mut self,
        view_id: &str,
        revision: u64,
        value: Option<CaptureTimeRange>,
    ) {
        let Some(request) = self
            .query_requests
            .values()
            .find(|request| request.view_id == view_id && request.revision == revision)
        else {
            return;
        };
        let policy = self
            .view_states
            .get(view_id)
            .and_then(|state| state.desired_capture_time_policy);
        self.view_states
            .get_mut(view_id)
            .expect("view state")
            .pending_time = Some(PendingTime {
            generation: request.generation,
            revision: request.revision,
            value,
            policy,
        });
    }

    fn enqueue_query_value(
        &mut self,
        view_id: &str,
        purpose: QueryPurpose,
        value: Option<String>,
    ) -> Option<u64> {
        let key = (view_id.to_owned(), purpose);
        if !self.query_requests.contains_key(&key)
            && self.query_requests.len() >= MAX_PENDING_QUERY_REQUESTS
        {
            self.editor_mut(view_id, purpose).error =
                Some("query submission queue is full; draft was preserved".into());
            return None;
        }
        let generation = self.next_query_generation;
        self.next_query_generation = self.next_query_generation.saturating_add(1);
        let state = self.view_states.get_mut(view_id).expect("view state");
        let base_revision = state.applied_query_revision;
        let base_constraints = applied_constraints(state);
        let mut constraints = state.desired_constraints.clone();
        let pending_value = match purpose {
            QueryPurpose::Search => {
                let value = value.unwrap_or_else(|| state.search.draft.clone());
                constraints.text = nonempty_text(&value);
                value
            }
            QueryPurpose::Advanced => {
                let value = value.unwrap_or_else(|| state.advanced.draft.clone());
                constraints.advanced_polars = nonempty(&value);
                value
            }
            QueryPurpose::Enrichment => {
                let value = value.unwrap_or_else(|| state.enrichment.draft.clone());
                constraints.enrichment = nonempty(&value);
                value
            }
        };
        state.desired_query_revision = state.desired_query_revision.saturating_add(1);
        let revision = state.desired_query_revision;
        state.desired_constraints = constraints.clone();
        let editor = match purpose {
            QueryPurpose::Search => &mut state.search,
            QueryPurpose::Advanced => &mut state.advanced,
            QueryPurpose::Enrichment => &mut state.enrichment,
        };
        editor.pending_generation = Some(generation);
        editor.pending_revision = Some(revision);
        editor.pending_value = Some(pending_value);
        editor.error = None;
        self.query_requests.insert(
            key,
            QueryRequest {
                view_id: view_id.to_owned(),
                generation,
                revision,
                base_revision,
                base_constraints,
                purpose,
                constraints,
            },
        );
        Some(revision)
    }

    fn editor_open(&self) -> bool {
        matches!(
            self.focus,
            Focus::SearchEditor | Focus::AdvancedEditor | Focus::EnrichmentEditor
        )
    }

    fn editor_purpose(&self) -> Option<QueryPurpose> {
        match self.focus {
            Focus::SearchEditor => Some(QueryPurpose::Search),
            Focus::AdvancedEditor => Some(QueryPurpose::Advanced),
            Focus::EnrichmentEditor => Some(QueryPurpose::Enrichment),
            Focus::Selector
            | Focus::Logs
            | Focus::SourceDialog
            | Focus::ViewDialog
            | Focus::FieldPicker
            | Focus::AskAi
            | Focus::Investigation => None,
            Focus::Recipes | Focus::TimeEditor => None,
        }
    }

    fn editor_mut(&mut self, view_id: &str, purpose: QueryPurpose) -> &mut EditorState {
        let state = self.view_states.get_mut(view_id).expect("view state");
        match purpose {
            QueryPurpose::Search => &mut state.search,
            QueryPurpose::Advanced => &mut state.advanced,
            QueryPurpose::Enrichment => &mut state.enrichment,
        }
    }

    fn edit_active(&mut self, edit: impl FnOnce(&mut EditorState)) {
        let (Some(view_id), Some(purpose)) = (
            self.active_view_id().map(str::to_owned),
            self.editor_purpose(),
        ) else {
            return;
        };
        let state = self.view_states.get_mut(&view_id).expect("view state");
        edit(match purpose {
            QueryPurpose::Search => &mut state.search,
            QueryPurpose::Advanced => &mut state.advanced,
            QueryPurpose::Enrichment => &mut state.enrichment,
        });
        state.user_interaction_revision = state.user_interaction_revision.saturating_add(1);
        state.ai_definition_revision = state.ai_definition_revision.saturating_add(1);
    }

    fn schedule_search(&mut self) {
        if self.focus != Focus::SearchEditor {
            return;
        }
        let Some(view_id) = self.active_view_id().map(str::to_owned) else {
            return;
        };
        self.view_states
            .get_mut(&view_id)
            .expect("view state")
            .search
            .search_due = Some(Instant::now() + SEARCH_DEBOUNCE);
    }

    fn switch_view<P: RowProvider>(&mut self, delta: i32, provider: &P) {
        if self.views.is_empty() {
            return;
        }
        self.selected_view =
            (self.selected_view as i32 + delta).rem_euclid(self.views.len() as i32) as usize;
        let height = self
            .view_state()
            .map_or(1, |state| state.viewport_height.max(1));
        self.sync_provider(provider, height);
    }

    fn selected_index<P: RowProvider>(&self, provider: &P) -> usize {
        let (Some(view_id), Some(state)) = (self.active_view_id(), self.view_state()) else {
            return 0;
        };
        state
            .selected
            .as_ref()
            .and_then(|id| provider.index_of_id(view_id, id))
            .unwrap_or(state.top)
    }

    fn move_selection<P: RowProvider>(&mut self, delta: i32, provider: &P) {
        let Some(total) = self.view_state().map(|state| state.last_total) else {
            return;
        };
        if total == 0 {
            return;
        }
        let target = (self.selected_index(provider) as i64 + i64::from(delta))
            .clamp(0, total.saturating_sub(1) as i64);
        self.select_index(target as usize, provider);
    }

    fn select_index<P: RowProvider>(&mut self, index: usize, provider: &P) {
        let Some(view_id) = self.active_view_id().map(str::to_owned) else {
            return;
        };
        let total = self.view_states[&view_id].last_total;
        if total == 0 {
            return;
        }
        let index = index.min(total - 1);
        let selected = provider
            .page(
                &view_id,
                ViewportRequest {
                    start: index,
                    len: 1,
                },
            )
            .rows
            .first()
            .map(|row| row.id.clone());
        let state = self.view_states.get_mut(&view_id).expect("view state");
        let height = state.viewport_height.max(1);
        state.selected = selected;
        if index < state.top {
            state.top = index;
        } else if index >= state.top + height {
            state.top = index + 1 - height;
        }
        state.follow = index + 1 == total;
        state.user_interaction_revision = state.user_interaction_revision.saturating_add(1);
    }

    fn toggle_follow<P: RowProvider>(&mut self, provider: &P) {
        let Some(id) = self.active_view_id().map(str::to_owned) else {
            return;
        };
        let follow = !self.view_states[&id].follow;
        let state = self.view_states.get_mut(&id).expect("view state");
        state.follow = follow;
        state.user_interaction_revision = state.user_interaction_revision.saturating_add(1);
        if follow {
            self.handle(Action::End, provider);
        }
    }

    fn handle_mouse<P: RowProvider>(&mut self, event: MouseEvent, provider: &P) {
        if self.focus == Focus::FieldPicker {
            let point = (event.column, event.row);
            if matches!(event.kind, MouseEventKind::Down(MouseButton::Left))
                && let Some(index) = self
                    .hit_regions
                    .field_picker_rows
                    .iter()
                    .find_map(|(area, index)| contains(*area, point).then_some(*index))
                && let Some(state) = self.view_state_mut()
            {
                state.field_picker_selected = index;
            }
            return;
        }
        if self.editor_open()
            || matches!(
                self.focus,
                Focus::SourceDialog | Focus::ViewDialog | Focus::AskAi | Focus::Investigation
            )
        {
            return;
        }
        if self.show_help {
            if matches!(event.kind, MouseEventKind::Down(MouseButton::Left)) {
                self.show_help = false;
            }
            return;
        }
        let point = (event.column, event.row);
        if matches!(event.kind, MouseEventKind::Down(MouseButton::Left)) {
            if let Some((_, index)) = self
                .hit_regions
                .sidebar_views
                .iter()
                .find(|(area, _)| contains(*area, point))
            {
                self.selected_view = *index;
                self.focus = Focus::Selector;
                let height = self
                    .view_state()
                    .map_or(1, |state| state.viewport_height.max(1));
                self.sync_provider(provider, height);
                return;
            }
            if let Some(rows) = self
                .hit_regions
                .log_rows
                .filter(|area| contains(*area, point))
            {
                let index = self.view_state().map_or(0, |state| state.top)
                    + usize::from(event.row - rows.y);
                self.focus = Focus::Logs;
                self.select_index(index, provider);
            }
            return;
        }
        let over_log = self
            .hit_regions
            .log
            .is_some_and(|area| contains(area, point));
        let over_sidebar = self
            .hit_regions
            .sidebar
            .is_some_and(|area| contains(area, point));
        match event.kind {
            MouseEventKind::ScrollUp if over_log => self.move_selection(-3, provider),
            MouseEventKind::ScrollDown if over_log => self.move_selection(3, provider),
            MouseEventKind::ScrollUp if over_sidebar => self.switch_view(-1, provider),
            MouseEventKind::ScrollDown if over_sidebar => self.switch_view(1, provider),
            _ => {}
        }
    }
}

fn contains(area: Rect, point: (u16, u16)) -> bool {
    point.0 >= area.x && point.0 < area.right() && point.1 >= area.y && point.1 < area.bottom()
}

pub fn filtered_discovery_indices(state: &DiscoveryDialogState) -> Vec<usize> {
    let query = state.query.to_lowercase();
    state
        .items
        .iter()
        .enumerate()
        .filter(|(_, item)| {
            query.is_empty()
                || item.label.to_lowercase().contains(&query)
                || item.detail.to_lowercase().contains(&query)
                || item.status.to_lowercase().contains(&query)
        })
        .map(|(index, _)| index)
        .collect()
}

fn nonempty(value: &str) -> Option<String> {
    (!value.is_empty()).then(|| value.to_owned())
}

fn parse_capture_range(start: &str, end: &str) -> Result<CaptureTimeRange, String> {
    let start_unix_nanos = parse_utc_nanos(start)?;
    let end_unix_nanos = parse_utc_nanos(end)?;
    if start_unix_nanos >= end_unix_nanos {
        return Err("Capture time start must be before end; range is [start, end)".into());
    }
    Ok(CaptureTimeRange {
        start_unix_nanos,
        end_unix_nanos,
    })
}

fn parse_utc_nanos(value: &str) -> Result<i64, String> {
    let value = value.trim();
    let Some(body) = value.strip_suffix('Z') else {
        return Err("use UTC syntax YYYY-MM-DDTHH:MM:SS[.nnnnnnnnn]Z".into());
    };
    let (whole, fraction) = match body.split_once('.') {
        Some((_, "")) => return Err(utc_syntax_error()),
        Some((_, fraction)) if fraction.contains('.') => return Err(utc_syntax_error()),
        Some(parts) => parts,
        None => (body, ""),
    };
    let bytes = whole.as_bytes();
    let separators = [(4, b'-'), (7, b'-'), (10, b'T'), (13, b':'), (16, b':')];
    if bytes.len() != 19
        || separators
            .iter()
            .any(|&(index, expected)| bytes[index] != expected)
        || bytes
            .iter()
            .enumerate()
            .any(|(index, byte)| !matches!(index, 4 | 7 | 10 | 13 | 16) && !byte.is_ascii_digit())
        || fraction.len() > 9
        || !fraction.bytes().all(|b| b.is_ascii_digit())
    {
        return Err("use UTC syntax YYYY-MM-DDTHH:MM:SS[.nnnnnnnnn]Z".into());
    }
    let number = |range: std::ops::Range<usize>| {
        whole[range]
            .parse::<i64>()
            .map_err(|_| "invalid UTC number".to_owned())
    };
    let (year, month, day, hour, minute, second) = (
        number(0..4)?,
        number(5..7)?,
        number(8..10)?,
        number(11..13)?,
        number(14..16)?,
        number(17..19)?,
    );
    if year < 1 || !(1..=12).contains(&month) || hour > 23 || minute > 59 || second > 59 {
        return Err("invalid UTC date/time".into());
    }
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days_in_month = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    if day < 1 || day > days_in_month[(month - 1) as usize] {
        return Err("invalid UTC calendar date".into());
    }
    let y = year - i64::from(month <= 2);
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = month + if month > 2 { -3 } else { 9 };
    let doy = (153 * mp + 2) / 5 + day - 1;
    let days = era * 146097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719468;
    let seconds = days
        .checked_mul(86400)
        .and_then(|v| v.checked_add(hour * 3600 + minute * 60 + second))
        .ok_or_else(|| "UTC value overflows capture range".to_owned())?;
    let nanos = if fraction.is_empty() {
        0
    } else {
        format!("{fraction:0<9}")
            .parse::<i64>()
            .map_err(|_| "invalid UTC fraction".to_owned())?
    };
    seconds
        .checked_mul(1_000_000_000)
        .and_then(|v| v.checked_add(nanos))
        .ok_or_else(|| "UTC value overflows capture range".to_owned())
}

fn utc_syntax_error() -> String {
    "use UTC syntax YYYY-MM-DDTHH:MM:SS[.nnnnnnnnn]Z".into()
}

fn format_utc_nanos(value: i64) -> String {
    // Around-selection values originate in supported current journal timestamps.
    let seconds = value.div_euclid(1_000_000_000);
    let nanos = value.rem_euclid(1_000_000_000);
    let days = seconds.div_euclid(86400);
    let sod = seconds.rem_euclid(86400);
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let mut year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{nanos:09}Z",
        sod / 3600,
        sod / 60 % 60,
        sod % 60
    )
}

fn nonempty_text(value: &str) -> Option<TextConstraint> {
    (!value.is_empty()).then(|| TextConstraint {
        literal: value.to_owned(),
        case_insensitive: true,
    })
}

fn applied_constraints(state: &ViewState) -> QueryConstraints {
    QueryConstraints {
        text: nonempty_text(&state.search.applied),
        advanced_polars: nonempty(&state.advanced.applied),
        enrichment: nonempty(&state.enrichment.applied),
        capture_time: state.applied_capture_time,
    }
}

fn mark_time_edit(state: &mut ViewState) {
    state.user_interaction_revision = state.user_interaction_revision.saturating_add(1);
    state.ai_definition_revision = state.ai_definition_revision.saturating_add(1);
}

fn resolve_capture_time_policy(
    policy: CaptureTimePolicy,
    now_unix_nanos: i64,
) -> Option<CaptureTimeRange> {
    match policy {
        CaptureTimePolicy::Absolute(window) => Some(window),
        CaptureTimePolicy::Recent { seconds } => {
            let duration = i64::try_from(seconds).ok()?.checked_mul(1_000_000_000)?;
            (duration > 0).then(|| CaptureTimeRange {
                start_unix_nanos: now_unix_nanos.saturating_sub(duration),
                end_unix_nanos: now_unix_nanos,
            })
        }
    }
}

pub fn format_capture_duration(seconds: u64) -> String {
    if seconds.is_multiple_of(3600) {
        format!("{}h", seconds / 3600)
    } else if seconds.is_multiple_of(60) {
        format!("{}m", seconds / 60)
    } else {
        format!("{seconds}s")
    }
}

fn constraint_text(constraints: &QueryConstraints) -> String {
    constraints
        .text
        .as_ref()
        .map_or_else(String::new, |text| text.literal.clone())
}

fn editor_mut(state: &mut ViewState, purpose: QueryPurpose) -> &mut EditorState {
    match purpose {
        QueryPurpose::Search => &mut state.search,
        QueryPurpose::Advanced => &mut state.advanced,
        QueryPurpose::Enrichment => &mut state.enrichment,
    }
}

fn pending_at_or_before(editor: &EditorState, revision: u64) -> bool {
    editor
        .pending_revision
        .is_some_and(|pending| pending <= revision)
}

fn state_has_pending_query(state: &ViewState) -> bool {
    state.search.pending_generation.is_some()
        || state.advanced.pending_generation.is_some()
        || state.enrichment.pending_generation.is_some()
        || state.pending_time.is_some()
        || state.pending_recipe.is_some()
}

fn clear_accepted_pending(editor: &mut EditorState, revision: u64) {
    if editor
        .pending_revision
        .is_some_and(|pending| pending <= revision)
    {
        editor.pending_generation = None;
        editor.pending_revision = None;
        editor.pending_value = None;
    }
}

fn clear_path_completion(dialog: &mut SourceDialogState) {
    dialog.path_completion.generation = 0;
    dialog.path_completion.scanning = false;
    dialog.path_completion.candidates.clear();
    dialog.path_completion.selected = 0;
}

fn move_index(current: usize, length: usize, delta: i32) -> usize {
    if length == 0 {
        return 0;
    }
    (current as i32 + delta).rem_euclid(length as i32) as usize
}

fn push_bounded_message(messages: &mut VecDeque<String>, message: String) {
    let message = bounded_message(message);
    let lines = message.lines().take(16).collect::<Vec<_>>();
    if lines.is_empty() {
        return;
    }
    for line in lines {
        if messages.len() >= MAX_INVESTIGATION_MESSAGES {
            messages.pop_front();
        }
        messages.push_back(line.to_owned());
    }
}

fn bounded_message(mut message: String) -> String {
    if message.len() <= MAX_INVESTIGATION_MESSAGE_BYTES {
        return message;
    }
    let mut end = MAX_INVESTIGATION_MESSAGE_BYTES;
    while !message.is_char_boundary(end) {
        end -= 1;
    }
    message.truncate(end);
    message.push('…');
    message
}

pub fn key_to_action(key: KeyEvent, focus: Focus) -> Action {
    if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
        return Action::None;
    }
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        return Action::Quit;
    }
    if matches!(
        focus,
        Focus::SearchEditor | Focus::AdvancedEditor | Focus::EnrichmentEditor
    ) {
        return match key.code {
            KeyCode::Esc => Action::CancelEditor,
            KeyCode::Enter if key.modifiers.contains(KeyModifiers::ALT) => {
                Action::EditorInput('\n')
            }
            KeyCode::Enter => Action::SubmitDraft,
            KeyCode::Backspace => Action::EditorBackspace,
            KeyCode::Char(character) => Action::EditorInput(character),
            _ => Action::None,
        };
    }
    if focus == Focus::FieldPicker {
        return match key.code {
            KeyCode::Esc => Action::CancelEditor,
            KeyCode::Down | KeyCode::Char('j') => Action::MoveFieldPicker(1),
            KeyCode::Up | KeyCode::Char('k') => Action::MoveFieldPicker(-1),
            KeyCode::Char(' ') | KeyCode::Enter => Action::TogglePinnedField,
            KeyCode::Char('c') => Action::ToggleColorField,
            _ => Action::None,
        };
    }
    if focus == Focus::Recipes {
        return match key.code {
            KeyCode::Esc => Action::CancelEditor,
            KeyCode::Up => Action::MoveRecipe(-1),
            KeyCode::Down => Action::MoveRecipe(1),
            KeyCode::Enter => Action::SubmitRecipe,
            KeyCode::Backspace => Action::RecipeBackspace,
            KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::ALT) => {
                Action::SelectRecipeMode(RecipeDialogMode::Save)
            }
            KeyCode::Char('b') if key.modifiers.contains(KeyModifiers::ALT) => {
                Action::SelectRecipeMode(RecipeDialogMode::Browse)
            }
            KeyCode::Char('i') if key.modifiers.contains(KeyModifiers::ALT) => {
                Action::SelectRecipeMode(RecipeDialogMode::Import)
            }
            KeyCode::Char(ch) => Action::RecipeInput(ch),
            _ => Action::None,
        };
    }
    if focus == Focus::TimeEditor {
        return match key.code {
            KeyCode::Esc => Action::CancelEditor,
            KeyCode::Tab => Action::SwitchTimeField,
            KeyCode::Enter => Action::SubmitTime,
            KeyCode::Backspace => Action::TimeBackspace,
            KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::ALT) => {
                Action::AroundSelected
            }
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::ALT) => Action::ClearTime,
            KeyCode::Char('5') if key.modifiers.contains(KeyModifiers::ALT) => {
                Action::SetRecentTime(5 * 60)
            }
            KeyCode::Char('m') if key.modifiers.contains(KeyModifiers::ALT) => {
                Action::SetRecentTime(15 * 60)
            }
            KeyCode::Char('h') if key.modifiers.contains(KeyModifiers::ALT) => {
                Action::SetRecentTime(60 * 60)
            }
            KeyCode::Char(ch) => Action::TimeInput(ch),
            _ => Action::None,
        };
    }
    if focus == Focus::SourceDialog {
        return match key.code {
            KeyCode::Esc => Action::CancelEditor,
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                Action::ToggleDiscovery
            }
            KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                Action::ToggleSourceAi
            }
            KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                Action::RefreshDiscovery
            }
            KeyCode::Char('f') if key.modifiers.contains(KeyModifiers::ALT) => {
                Action::SelectSourceKind(SourceKind::File)
            }
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::ALT) => {
                Action::SelectSourceKind(SourceKind::Command)
            }
            KeyCode::Down => Action::MovePathCompletion(1),
            KeyCode::Up => Action::MovePathCompletion(-1),
            KeyCode::Tab => Action::CompleteSourcePath,
            KeyCode::Enter => Action::SubmitSource,
            KeyCode::Backspace => Action::SourceBackspace,
            KeyCode::Char(character) => Action::SourceInput(character),
            _ => Action::None,
        };
    }
    if focus == Focus::ViewDialog {
        return match key.code {
            KeyCode::Esc => Action::CancelEditor,
            KeyCode::Enter => Action::SubmitViewDialog,
            KeyCode::Backspace => Action::ViewBackspace,
            KeyCode::Char('b') if key.modifiers.contains(KeyModifiers::ALT) => {
                Action::SelectViewDialogMode(ViewDialogMode::Blank)
            }
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::ALT) => {
                Action::SelectViewDialogMode(ViewDialogMode::Clone)
            }
            KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::ALT) => {
                Action::SelectViewDialogMode(ViewDialogMode::Rename)
            }
            KeyCode::Char(character) => Action::ViewInput(character),
            _ => Action::None,
        };
    }
    if focus == Focus::AskAi {
        return match key.code {
            KeyCode::Esc => Action::CancelEditor,
            KeyCode::Enter => Action::SubmitAskAi,
            KeyCode::Backspace => Action::EditorBackspace,
            KeyCode::Char('f') if key.modifiers.contains(KeyModifiers::ALT) => {
                Action::SelectAskAiKind(AskAiKind::Filter)
            }
            KeyCode::Char('e') if key.modifiers.contains(KeyModifiers::ALT) => {
                Action::SelectAskAiKind(AskAiKind::Enrichment)
            }
            KeyCode::Char(character) => Action::EditorInput(character),
            _ => Action::None,
        };
    }
    if focus == Focus::Investigation {
        return match key.code {
            KeyCode::Esc => Action::CancelEditor,
            KeyCode::Enter => Action::SubmitInvestigation,
            KeyCode::Backspace => Action::EditorBackspace,
            KeyCode::Up => Action::MoveInvestigation(-1),
            KeyCode::Down => Action::MoveInvestigation(1),
            KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::ALT) => {
                Action::NewInvestigation
            }
            KeyCode::Char(character) => Action::EditorInput(character),
            _ => Action::None,
        };
    }
    if focus == Focus::Selector {
        return match key.code {
            KeyCode::Down | KeyCode::Char('j') => Action::SelectSidebar(1),
            KeyCode::Up | KeyCode::Char('k') => Action::SelectSidebar(-1),
            KeyCode::Tab => Action::CycleFocus,
            KeyCode::Char('q') => Action::Quit,
            KeyCode::Char('?') => Action::ToggleHelp,
            _ => Action::None,
        };
    }
    match key.code {
        KeyCode::Char('q') => Action::Quit,
        KeyCode::Tab => Action::CycleFocus,
        KeyCode::Char(']') => Action::NextView,
        KeyCode::Char('[') => Action::PreviousView,
        KeyCode::Down | KeyCode::Char('j') => Action::MoveLine(1),
        KeyCode::Up | KeyCode::Char('k') => Action::MoveLine(-1),
        KeyCode::PageDown => Action::MovePage(1),
        KeyCode::PageUp => Action::MovePage(-1),
        KeyCode::Home | KeyCode::Char('g') => Action::Top,
        KeyCode::End | KeyCode::Char('G') => Action::End,
        KeyCode::Char('d') => Action::ToggleDetails,
        KeyCode::Char('v') => Action::OpenViewDialog,
        KeyCode::Char('?') => Action::ToggleHelp,
        KeyCode::Char('f') => Action::ToggleFollow,
        KeyCode::Char('/') => Action::OpenSearch,
        KeyCode::Char('p') => Action::OpenAdvanced,
        KeyCode::Char('e') => Action::OpenEnrichment,
        KeyCode::Char('A') => Action::OpenAskAi,
        KeyCode::Char('I') => Action::OpenInvestigation,
        KeyCode::Char('n') => Action::OpenSource,
        KeyCode::Char('r') => Action::OpenRecipes,
        KeyCode::Char('t') => Action::OpenTime,
        KeyCode::Char('i') => Action::OpenFieldPicker,
        KeyCode::Char('a') => Action::FixtureAdvance,
        _ => Action::None,
    }
}
