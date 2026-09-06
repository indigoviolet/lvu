use std::{
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    path::PathBuf,
    time::{Duration, Instant},
};

use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use lvu_core::{CommandDefinition, CommandProgram, RestartPolicy};
use ratatui::layout::Rect;

use crate::provider::{DisplayRow, RowId, RowProvider, ViewportRequest};
use crate::text_edit::{CursorBank, EditCommand, EditPolicy, TextCursor, TextTarget, edit};
use crate::theme::ThemeId;

pub const MAX_EDITOR_BYTES: usize = 16 * 1024;
pub const MAX_PENDING_QUERY_REQUESTS: usize = 32;
pub const TIMESTAMP_PROMPT: &str = "Use the prepared typed schema, sample values and coverage to derive exactly one field named timestamp_utc from an existing usable timestamp column. When that inline evidence is sufficient, do not read files or invoke tools. If no prepared context is supplied, inspect the fixed snapshot schema and bounded samples first. Do not extract a JSON field from raw when its value is available in a usable named column. Fall back to raw extraction only for unstructured timestamps or documented projection/type conflicts and explain why. Do not substitute capture time for an event timestamp. Return a Polars enrichment expression producing UTC RFC3339 strings in the exact format %Y-%m-%dT%H:%M:%S%.6fZ. Use str.extract when needed, str.to_datetime or str.strptime with an explicit input format and strict=False, then dt.convert_time_zone('UTC') and dt.strftime. Preserve raw and prior enrichment stages. Missing, malformed, or ambiguous timestamps must produce null. Never infer a missing year, day/month order, epoch unit, or timezone; explain what user-provided information is needed instead. Explicit numeric offsets must be normalized to UTC. Explain the detected source field/input format, timezone evidence, output format, and unmatched cases. Only propose the enrichment; do not modify files.";

pub const SEARCH_DEBOUNCE: Duration = Duration::from_millis(300);
const MAX_SOURCE_REQUESTS: usize = 8;
const MAX_DISCOVERY_REQUESTS: usize = 4;
const MAX_AI_REQUESTS: usize = 2;
const MAX_AI_PROMPT_BYTES: usize = 8 * 1024;
const MAX_INVESTIGATION_REQUESTS: usize = 4;
const MAX_INVESTIGATION_MESSAGES: usize = 64;
const MAX_INVESTIGATION_MESSAGE_BYTES: usize = 16 * 1024;
const MAX_SAVED_INVESTIGATIONS: usize = 64;
const MAX_COMPLETION_ROWS: usize = 128;
const MAX_COMPLETION_FIELDS: usize = 128;
const MAX_COMPLETION_VALUES: usize = 256;
const MAX_COMPLETION_TEXT_BYTES: usize = 512;
const MAX_COMMAND_PROGRAM_BYTES: usize = 4096;
const MAX_COMMAND_ARGUMENTS: usize = 128;
const MAX_COMMAND_ENVIRONMENT: usize = 128;
const MAX_COMMAND_FIELD_BYTES: usize = 16 * 1024;
const MAX_COMMAND_REQUESTS: usize = 8;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Focus {
    Selector,
    Logs,
    Details,
    SearchEditor,
    AdvancedEditor,
    EnrichmentEditor,
    CommandEnrichment,
    GroupingEditor,
    SourceDialog,
    Help,
    ViewDialog,
    FieldPicker,
    AskAi,
    Investigation,
    Storage,
    Settings,
    Recipes,
    TimeEditor,
    Context,
    Bookmarks,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AskAiKind {
    Filter,
    Enrichment,
    Recipe,
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
    pub recipe: Option<RecipeConfig>,
    pub recipe_outcome: Option<RecipeOutcome>,
    pub review_scroll: u16,
    pub review_scroll_limit: u16,
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
pub struct SourceControlRequest {
    pub source_id: String,
    pub restart: bool,
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
    Sources,
    Blank,
    Clone,
    Rename,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ViewDialogState {
    pub source_ids: Vec<String>,
    pub selected_source: usize,
    pub mode: ViewDialogMode,
    pub draft: String,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ViewMutationRequest {
    pub source_ids: Vec<String>,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EditorCompletionKind {
    Field,
    SampledValue,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EditorCompletionItem {
    pub label: String,
    pub insertion: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EditorCompletionState {
    pub generation: u64,
    pub view_id: String,
    pub purpose: QueryPurpose,
    pub draft: String,
    pub target: TextTarget,
    pub cursor: usize,
    pub kind: EditorCompletionKind,
    pub items: Vec<EditorCompletionItem>,
    pub selected: usize,
    pub top: usize,
    pub status: String,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum EnrichmentControl {
    Steps,
    #[default]
    Editor,
    Add,
    Edit,
    Remove,
    ExternalCommand,
}

impl EnrichmentControl {
    const ALL: [Self; 6] = [
        Self::Steps,
        Self::Editor,
        Self::Add,
        Self::Edit,
        Self::Remove,
        Self::ExternalCommand,
    ];
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ViewState {
    pub source_ids: Vec<String>,
    pending_source_change: Option<(u64, u64, Vec<String>)>,
    pub top: usize,
    pub horizontal_offset: usize,
    pub details_scroll: usize,
    pub details_scroll_limit: usize,
    details_row: Option<RowId>,
    pub bookmarks: Vec<Bookmark>,
    pub selected: Option<RowId>,
    pub follow: bool,
    pub last_total: usize,
    pub provider_revision: u64,
    pub viewport_height: usize,
    pub search: EditorState,
    pub advanced: EditorState,
    pub enrichment: EditorState,
    pub enrichments: Vec<EnrichmentDefinition>,
    pub command_enrichment: Option<CommandEnrichmentStage>,
    pub command_enrichment_revision: u64,
    pub command_publication: Option<String>,
    pub enrichment_selected: usize,
    pub enrichment_editing: Option<EnrichmentStageId>,
    pub enrichment_control: EnrichmentControl,
    pub grouping: EditorState,
    pub applied_capture_time: Option<CaptureTimeRange>,
    /// User-authored policy. Rolling refreshes update the resolved range above
    /// without changing the definition revision.
    pub applied_capture_time_policy: Option<CaptureTimePolicy>,
    pub applied_time_basis: TimeBasis,
    pub time_start_draft: String,
    pub time_end_draft: String,
    pub time_recent_draft: String,
    pub time_error: Option<String>,
    pub time_draft_touched: bool,
    pub time_window_draft: TimeWindowChoice,
    pub time_basis_draft: TimeBasis,
    pub time_start_date_draft: String,
    pub time_start_clock_draft: String,
    pub time_start_zone_draft: String,
    pub time_end_date_draft: String,
    pub time_end_clock_draft: String,
    pub time_end_zone_draft: String,
    pub time_structured_draft_present: bool,
    pub applied_query_revision: u64,
    pub desired_query_revision: u64,
    pub pinned_columns: Vec<String>,
    pub color_field: Option<String>,
    pub field_picker_selected: usize,
    pub field_picker_top: usize,
    pub field_picker_row: Option<RowId>,
    pub expanded_groups: HashSet<RowId>,
    user_interaction_revision: u64,
    ai_definition_revision: u64,
    desired_constraints: QueryConstraints,
    desired_capture_time_policy: Option<CaptureTimePolicy>,
    desired_time_basis: TimeBasis,
    pending_recipe: Option<PendingRecipe>,
    pending_time: Option<PendingTime>,
    pending_enrichment_mutation: Option<PendingEnrichmentMutation>,
    rolling_refresh_due: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PendingEnrichmentMutation {
    Add,
    Edit,
    Remove,
    Reaffirm,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PendingTime {
    generation: u64,
    revision: u64,
    value: Option<CaptureTimeRange>,
    policy: Option<CaptureTimePolicy>,
    basis: TimeBasis,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PendingRecipe {
    revision: u64,
    interaction_revision: u64,
    pinned_columns: Vec<String>,
    color_field: Option<String>,
    capture_time_policy: Option<CaptureTimePolicy>,
    time_basis: TimeBasis,
    suggestion: Option<RecipeOutcome>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecipeOutcome {
    pub source_id: String,
    pub recipe_id: String,
    pub revision: String,
    pub accepted: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PersistentViewState {
    pub source_ids: Vec<String>,
    pub view_name: String,
    pub applied_search: String,
    pub search_draft: String,
    pub search_error: Option<String>,
    pub applied_advanced: String,
    pub advanced_draft: String,
    pub advanced_error: Option<String>,
    pub applied_enrichment: String,
    pub applied_enrichments: Vec<EnrichmentDefinition>,
    pub enrichment_draft: String,
    pub enrichment_error: Option<String>,
    pub enrichment_editing: Option<EnrichmentStageId>,
    pub enrichment_selected: usize,
    pub command_enrichment: Option<CommandEnrichmentStage>,
    pub command_enrichment_revision: u64,
    pub command_publication: Option<String>,
    pub applied_grouping: String,
    pub grouping_draft: String,
    pub grouping_error: Option<String>,
    pub applied_capture_time: Option<CaptureTimeRange>,
    pub applied_capture_time_policy: Option<CaptureTimePolicy>,
    pub applied_time_basis: TimeBasis,
    pub time_start_draft: String,
    pub time_end_draft: String,
    pub time_recent_draft: String,
    pub time_error: Option<String>,
    pub time_draft_touched: bool,
    pub time_window_draft: TimeWindowChoice,
    pub time_basis_draft: TimeBasis,
    pub time_start_date_draft: String,
    pub time_start_clock_draft: String,
    pub time_start_zone_draft: String,
    pub time_end_date_draft: String,
    pub time_end_clock_draft: String,
    pub time_end_zone_draft: String,
    pub time_structured_draft_present: bool,
    pub bookmarks: Vec<Bookmark>,
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
    Grouping,
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// A literal substring constraint. Case-insensitive adapters use Rust's
/// locale-neutral Unicode lowercase mapping, not locale-specific case rules.
pub struct TextConstraint {
    pub literal: String,
    pub case_insensitive: bool,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct EnrichmentStageId(pub String);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EnrichmentDefinition {
    pub id: EnrichmentStageId,
    /// Either `/regex with (?P<name>...) groups/` or `name = Python Polars Expr`.
    pub source: String,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct CommandEnrichmentStageId(pub String);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandEnrichmentStage {
    pub id: CommandEnrichmentStageId,
    pub definition: CommandDefinition,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum CommandEnrichmentField {
    #[default]
    Program,
    Arguments,
    Cwd,
    Environment,
}

impl CommandEnrichmentField {
    const ALL: [Self; 4] = [Self::Program, Self::Arguments, Self::Cwd, Self::Environment];
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum CommandEnrichmentControl {
    #[default]
    Field,
    NewLine,
    Save,
    Review,
    Remove,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum CommandEnrichmentRunState {
    #[default]
    Unrun,
    Saving,
    Preparing,
    Ready,
    Running,
    SavingResults,
    Complete,
    Error,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandEnrichmentReview {
    pub review_token: String,
    pub record_count: usize,
    pub source_count: usize,
    pub executable: String,
    pub arguments: Vec<String>,
    pub cwd: Option<String>,
    pub environment_keys: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandEnrichmentDialogState {
    pub generation: u64,
    pub view_id: String,
    pub base_definition_revision: u64,
    pub selected_field: CommandEnrichmentField,
    pub selected_control: CommandEnrichmentControl,
    pub program: String,
    pub arguments: String,
    pub cwd: String,
    pub environment: String,
    pub accepted: Option<CommandEnrichmentStage>,
    pub error: Option<String>,
    pub run_state: CommandEnrichmentRunState,
    pub run_status: String,
    pub review: Option<CommandEnrichmentReview>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CommandEnrichmentRequest {
    Save {
        generation: u64,
        view_id: String,
        base_definition_revision: u64,
        candidate: Option<CommandEnrichmentStage>,
    },
    PrepareRun {
        generation: u64,
        view_id: String,
        definition_revision: u64,
        stage_id: CommandEnrichmentStageId,
    },
    Execute {
        generation: u64,
        view_id: String,
        definition_revision: u64,
        review_token: String,
    },
    Cancel {
        generation: u64,
        view_id: String,
    },
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct QueryConstraints {
    pub text: Option<TextConstraint>,
    pub advanced_polars: Option<String>,
    /// Ordered stages. Later definitions may reference fields from earlier ones.
    pub enrichments: Vec<EnrichmentDefinition>,
    /// Compatibility input for pre-chain adapters. New UI requests leave this unset.
    pub enrichment: Option<String>,
    /// Fixed time window for `time_basis`, half-open `[start_unix_nanos, end_unix_nanos)`.
    pub capture_time: Option<CaptureTimeRange>,
    pub time_basis: TimeBasis,
    /// Display-only continuation prefix-regex. Physical membership is unchanged.
    pub grouping: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum TimeBasis {
    #[default]
    Capture,
    Event,
    /// UTC RFC3339 strings from the accepted timestamp_utc enrichment.
    Extracted,
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
    pub controls_focused: bool,
    pub control: SourceControl,
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
    pub enrichments: Vec<EnrichmentDefinition>,
    pub pinned_columns: Vec<String>,
    pub color_field: Option<String>,
    pub capture_time: Option<CaptureTimeRange>,
    pub capture_time_policy: Option<CaptureTimePolicy>,
    pub time_basis: TimeBasis,
    pub grouping: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TimeDialogState {
    pub focus: TimeControl,
    pub dropdown: Option<TimeDropdown>,
    pub window: TimeWindowChoice,
    pub scroll: usize,
    pub reveal_focus: bool,
    pub has_overflow: bool,
    pub dropdown_scroll: usize,
    pub segment_cursor: usize,
    pub start_date: String,
    pub start_clock: String,
    pub start_zone: String,
    pub end_date: String,
    pub end_clock: String,
    pub end_zone: String,
    pub start_zone_custom: bool,
    pub end_zone_custom: bool,
    pub highlighted: usize,
    pub window_choices: Vec<TimeWindowChoice>,
    pub anchored_row: Option<RowId>,
    pub anchored_capture_nanos: Option<i64>,
    pub anchored_event_nanos: Option<i64>,
    pub anchored_extracted_nanos: Option<i64>,
    pub basis: TimeBasis,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum TimeControl {
    #[default]
    Basis,
    Window,
    StartDate,
    StartClock,
    StartZone,
    StartZoneMenu,
    EndDate,
    EndClock,
    EndZone,
    EndZoneMenu,
    Apply,
    Clear,
    Recognize,
    ScrollUp,
    ScrollDown,
}

impl TimeControl {
    fn focusable(has_overflow: bool, start_custom: bool, end_custom: bool) -> Vec<Self> {
        let mut controls = vec![Self::Basis, Self::Window, Self::StartDate, Self::StartClock];
        if start_custom {
            controls.push(Self::StartZone);
        }
        controls.extend([Self::StartZoneMenu, Self::EndDate, Self::EndClock]);
        if end_custom {
            controls.push(Self::EndZone);
        }
        controls.extend([Self::EndZoneMenu, Self::Apply, Self::Clear, Self::Recognize]);
        if has_overflow {
            controls.extend([Self::ScrollUp, Self::ScrollDown]);
        }
        controls
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TimeDropdown {
    Basis,
    Window,
    StartZone,
    EndZone,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum TimeWindowChoice {
    #[default]
    All,
    Absolute,
    Recent(u64),
    AroundSelected,
}

#[derive(Clone)]
enum EitherTimeChoice {
    Basis(TimeBasis),
    Window(TimeWindowChoice),
    Zone(String),
    CustomZone,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecipeItem {
    pub id: String,
    pub revision: String,
    pub name: String,
    pub config: RecipeConfig,
    pub incompatibility: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecipeSuggestion {
    pub recipe_id: String,
    pub evidence: Vec<String>,
    pub missing_fields: Vec<String>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum RecipeDialogMode {
    #[default]
    Browse,
    Save,
    Import,
    Export,
    History,
    Update,
}

impl RecipeDialogMode {
    pub fn is_list(self) -> bool {
        matches!(self, Self::Browse | Self::History)
    }
    pub fn is_editable(self) -> bool {
        matches!(self, Self::Save | Self::Import | Self::Export)
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RecipeDialogState {
    pub id: u64,
    pub interaction_revision: u64,
    pub pending_request_id: Option<u64>,
    pub mode: RecipeDialogMode,
    pub name: String,
    pub items: Vec<RecipeItem>,
    pub suggestions: Vec<RecipeSuggestion>,
    pub selected: usize,
    pub status: String,
    pub loading: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RecipeRequest {
    List {
        meta: RecipeRequestMeta,
    },
    History {
        meta: RecipeRequestMeta,
        recipe_id: String,
    },
    Save {
        update: Option<(String, String)>,
        meta: RecipeRequestMeta,
        name: String,
        view_id: String,
        config: Box<RecipeConfig>,
    },
    Import {
        meta: RecipeRequestMeta,
        path: String,
    },
    Export {
        meta: RecipeRequestMeta,
        path: String,
        recipe_id: String,
        revision: String,
    },
    Outcome(RecipeOutcome),
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

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SourceControl {
    #[default]
    Input,
    Manual,
    Discovery,
    Agent,
    File,
    Command,
    Open,
    Refresh,
}

impl SourceControl {
    fn visible(mode: SourceDialogMode) -> &'static [Self] {
        match mode {
            SourceDialogMode::Manual => &[
                Self::Input,
                Self::Manual,
                Self::Discovery,
                Self::Agent,
                Self::File,
                Self::Command,
                Self::Open,
            ],
            SourceDialogMode::Discovery => &[
                Self::Input,
                Self::Manual,
                Self::Discovery,
                Self::Agent,
                Self::Open,
                Self::Refresh,
            ],
            SourceDialogMode::Ai => &[
                Self::Input,
                Self::Manual,
                Self::Discovery,
                Self::Agent,
                Self::Open,
            ],
        }
    }
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
    pub status_scroll: usize,
    pub status_scroll_limit: usize,
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
            controls_focused: false,
            control: SourceControl::Input,
            discovery: DiscoveryDialogState::default(),
            path_completion: PathCompletionState::default(),
            ai: SourceAiDialogState::default(),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct HitRegions {
    pub selection_modal: Option<Rect>,
    pub log: Option<Rect>,
    pub log_rows: Option<Rect>,
    pub details: Option<Rect>,
    pub dialog_scroll: Option<Rect>,
    pub log_row_indices: Vec<(Rect, usize)>,
    pub sidebar: Option<Rect>,
    pub sidebar_views: Vec<(Rect, usize)>,
    pub field_picker_rows: Vec<(Rect, usize)>,
    pub storage_rows: Vec<(Rect, usize)>,
    pub bookmark_rows: Vec<(Rect, usize)>,
    pub view_source_rows: Vec<(Rect, usize)>,
    pub discovery_rows: Vec<(Rect, usize)>,
    pub editor_completion_rows: Vec<(Rect, usize)>,
    pub enrichment_rows: Vec<(Rect, usize)>,
    pub enrichment_controls: Vec<(Rect, EnrichmentControl)>,
    pub command_enrichment_controls: Vec<(Rect, CommandEnrichmentControl)>,
    pub source_controls: Vec<(Rect, SourceControl)>,
    pub settings_controls: Vec<(Rect, SettingsControl)>,
    pub settings_theme_choices: Vec<(Rect, usize)>,
    pub time_controls: Vec<(Rect, TimeControl)>,
    pub time_choices: Vec<(Rect, usize)>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Action {
    Quit,
    CycleFocus,
    NextView,
    PreviousView,
    SelectSidebar(i32),
    MoveLine(i32),
    MoveHorizontal(i32),
    ResetHorizontal,
    MovePage(i32),
    Top,
    End,
    ToggleDetails,
    ScrollDetails(i32),
    ResetDetails,
    OpenContext,
    MoveContext(isize),
    ToggleBookmark,
    OpenBookmarks,
    MoveBookmark(i32),
    SelectBookmark(usize),
    EditBookmarkNote,
    BookmarkInput(char),
    BookmarkBackspace,
    SubmitBookmark,
    DeleteBookmark,
    ToggleHelp,
    ScrollHelp(i32),
    ScrollDialog(i32),
    ScrollDiscoveryStatus(i32),
    ToggleDialogScrollFocus,
    ModalVertical(i32),
    ScrollHoveredDialog(i32),
    ToggleFollow,
    StopCapture,
    RestartCapture,
    OpenSearch,
    OpenAdvanced,
    OpenEnrichment,
    AddEnrichment,
    EditEnrichment,
    RemoveEnrichment,
    MoveEnrichment(i32),
    MoveEnrichmentControl(i32),
    FocusEnrichmentControl(EnrichmentControl),
    ActivateEnrichmentControl,
    OpenCommandEnrichment,
    CommandEnrichmentNextField,
    FocusCommandEnrichmentControl(CommandEnrichmentControl),
    ActivateCommandEnrichmentControl,
    CommandEnrichmentInput(char),
    CommandEnrichmentBackspace,
    SaveCommandEnrichment,
    RemoveCommandEnrichment,
    PrepareCommandEnrichmentRun,
    ConfirmCommandEnrichmentRun,
    OpenGrouping,
    ToggleExpandedGroup,
    OpenStorage,
    OpenSettings,
    MoveSettings(i32),
    CycleSetting,
    FocusSettings(SettingsControl),
    ActivateSettings,
    MoveSettingsTheme(i32),
    ChooseSettingsTheme(usize),
    CloseSettingsTheme,
    ScrollSettingsDetails(i32),
    SettingsInput(char),
    SettingsBackspace,
    SaveSettings,
    RefreshStorage,
    ClearStorage,
    MoveStorage(i32),
    OpenAskAi,
    OpenTimestampAssistant,
    SelectAskAiKind(AskAiKind),
    SubmitAskAi,
    ApplyAskAi,
    ScrollAskAi(i32),
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
    SetTimeBasis(TimeBasis),
    TimeMoveFocus(i32),
    TimeOpenFocused,
    TimeMoveChoice(i32),
    TimeChoose,
    TimeFocus(TimeControl),
    TimeChooseIndex(usize),
    TimeScroll(i32),
    TimeMoveCursor(i32),
    SelectRecipeMode(RecipeDialogMode),
    MoveRecipe(i32),
    RefreshRecipeSuggestions,
    RecipeInput(char),
    RecipeBackspace,
    SubmitRecipe,
    RejectRecipeSuggestion,
    AdaptRecipeSuggestion,
    OpenViewDialog,
    SelectViewDialogMode(ViewDialogMode),
    SubmitViewDialog,
    ViewInput(char),
    ViewBackspace,
    MoveViewSource(i32),
    ReorderViewSource(i32),
    ToggleViewSource,
    OpenFieldPicker,
    MoveFieldPicker(i32),
    TogglePinnedField,
    ToggleColorField,
    ToggleDiscovery,
    ToggleSourceAi,
    ToggleSourceControlFocus,
    FocusSourceControl(SourceControl),
    MoveSourceMode(i32),
    ActivateSourceControl,
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
    TextStartOfLine,
    TextEndOfLine,
    TextKillToEndOfLine,
    TextMoveLeft,
    TextMoveRight,
    TextMoveUp,
    TextMoveDown,
    EditorPaste(String),
    ToggleEditorCompletion,
    MoveEditorCompletion(i32),
    AcceptEditorCompletion,
    SubmitDraft,
    CancelEditor,
    Resize(u16, u16),
    Mouse(MouseEvent),
    FixtureAdvance,
    None,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StorageCategory {
    Capture,
    Derived,
    Workspace,
    Investigation,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StorageEntry {
    pub category: StorageCategory,
    pub label: String,
    pub bytes: u64,
    pub reclaimable: u64,
    pub status: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct StorageSnapshot {
    pub entries: Vec<StorageEntry>,
    pub total_bytes: u64,
    pub reclaimable_bytes: u64,
    pub row_cache_bytes: u64,
    pub row_cache_limit: u64,
    pub query_index_bytes: u64,
    pub query_index_limit: u64,
    pub derived_index_limit_per_source: u64,
    pub derived_index_limit_total: u64,
    pub truncated: bool,
    pub errors: Vec<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StorageRequestKind {
    Scan,
    ClearUnusedDerived,
    Cancel,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StorageRequest {
    pub generation: u64,
    pub kind: StorageRequestKind,
}

#[derive(Clone, Debug)]
pub struct StorageDialogState {
    pub generation: u64,
    pub snapshot: StorageSnapshot,
    pub selected: usize,
    pub scanning: bool,
    pub confirm_clear: bool,
    pub status: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SettingsField {
    Provider,
    Mode,
    Thinking,
    Theme,
    Delight,
    ReducedMotion,
    Ascii,
    RowCache,
    Membership,
    DiskTotal,
    IndexPerSource,
}

impl SettingsField {
    pub const ALL: [Self; 11] = [
        Self::Provider,
        Self::Mode,
        Self::Thinking,
        Self::Theme,
        Self::Delight,
        Self::ReducedMotion,
        Self::Ascii,
        Self::RowCache,
        Self::Membership,
        Self::DiskTotal,
        Self::IndexPerSource,
    ];
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SettingsControl {
    Field(SettingsField),
    Save,
    More,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SettingsStatus {
    #[default]
    Saved,
    Pending,
    Error,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SettingsValues {
    pub provider: String,
    pub mode: String,
    pub thinking: String,
    pub theme: ThemeId,
    pub delight_enabled: bool,
    pub reduced_motion: bool,
    pub ascii: bool,
    pub rows_mib: String,
    pub membership_mib: String,
    pub disk_total_mib: String,
    pub index_per_source_mib: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SettingsContext {
    pub saved: SettingsValues,
    pub effective_provider: String,
    pub effective_mode: String,
    pub effective_thinking: String,
    pub effective_theme: ThemeId,
    pub effective_delight_enabled: bool,
    pub effective_reduced_motion: bool,
    pub effective_ascii: bool,
    pub provider_source: String,
    pub mode_source: String,
    pub thinking_source: String,
    pub delight_source: String,
    pub reduced_motion_source: String,
    pub ascii_source: String,
    pub settings_path: String,
    pub data_path: String,
    pub cache_path: String,
    pub capture_path: String,
    pub applied_rows_mib: u64,
    pub applied_membership_mib: u64,
    pub applied_disk_total_mib: u64,
    pub applied_index_per_source_mib: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SettingsDialogState {
    pub generation: u64,
    pub selected: usize,
    pub focus: SettingsControl,
    pub draft: SettingsValues,
    pub context: SettingsContext,
    pub saving: bool,
    pub status_kind: SettingsStatus,
    pub status: String,
    pub theme_dropdown: bool,
    pub theme_selected: usize,
    pub details_scroll: usize,
    pub details_scroll_limit: usize,
}

pub const MAX_BOOKMARKS: usize = 128;
pub const MAX_BOOKMARK_NOTE_BYTES: usize = 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Bookmark {
    pub id: RowId,
    pub note: String,
}

#[derive(Clone, Debug)]
pub struct BookmarkDialogState {
    pub view_id: String,
    pub selected: usize,
    pub editing: Option<RowId>,
    pub draft: String,
    pub status: String,
}

#[derive(Clone, Debug)]
pub struct ContextDialogState {
    pub view_id: String,
    pub anchor: RowId,
    pub offset: isize,
    pub return_focus: Focus,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SettingsRequest {
    pub generation: u64,
    pub values: SettingsValues,
}

pub struct App {
    pub title: String,
    pub demo_mode: bool,
    pub sources: Vec<SourceItem>,
    pub views: Vec<ViewItem>,
    pub selected_view: usize,
    pub focus: Focus,
    pub show_details: bool,
    pub context_dialog: Option<ContextDialogState>,
    pub bookmark_dialog: Option<BookmarkDialogState>,
    pub show_help: bool,
    pub help_scroll: usize,
    pub help_scroll_limit: usize,
    pub dialog_scroll: usize,
    pub dialog_scroll_limit: usize,
    pub dialog_scroll_focused: bool,
    help_return_focus: Focus,
    pub terminal_size: (u16, u16),
    pub should_quit: bool,
    pub hit_regions: HitRegions,
    pub source_dialog: Option<SourceDialogState>,
    pub view_dialog: Option<ViewDialogState>,
    pub ask_ai_dialog: Option<AskAiDialogState>,
    pub investigation_dialog: Option<InvestigationDialogState>,
    pub recipe_dialog: Option<RecipeDialogState>,
    pub time_dialog: Option<TimeDialogState>,
    pub storage_dialog: Option<StorageDialogState>,
    pub settings_dialog: Option<SettingsDialogState>,
    pub command_enrichment_dialog: Option<CommandEnrichmentDialogState>,
    pub source_notice: Option<String>,
    pub action_notice: Option<String>,
    pub editor_completion: Option<EditorCompletionState>,
    pub theme_id: ThemeId,
    pub delight_enabled: bool,
    pub reduced_motion: bool,
    pub ascii: bool,
    /// Whether an interactive source-less launch should show the startup modal.
    /// This is deliberately independent from the footer delight setting.
    pub show_startup_title: bool,
    view_states: HashMap<String, ViewState>,
    query_requests: HashMap<(String, QueryPurpose), QueryRequest>,
    next_query_generation: u64,
    source_requests: VecDeque<SourceLaunchRequest>,
    source_controls: VecDeque<SourceControlRequest>,
    discovery_requests: VecDeque<DiscoveryUiRequest>,
    path_completion_requests: VecDeque<PathCompletionRequest>,
    view_requests: VecDeque<ViewMutationRequest>,
    ask_ai_requests: VecDeque<AskAiRequest>,
    source_ai_requests: VecDeque<SourceAiRequest>,
    recipe_requests: VecDeque<RecipeRequest>,
    investigation_requests: VecDeque<InvestigationRequest>,
    storage_requests: VecDeque<StorageRequest>,
    settings_requests: VecDeque<SettingsRequest>,
    command_enrichment_requests: VecDeque<CommandEnrichmentRequest>,
    pending_command_enrichment_saves: HashMap<u64, (String, u64)>,
    pending_command_enrichment_runs: HashMap<u64, (String, u64)>,
    settings_context: Option<SettingsContext>,
    next_ask_ai_generation: u64,
    next_source_ai_generation: u64,
    next_investigation_generation: u64,
    next_recipe_generation: u64,
    next_storage_generation: u64,
    next_settings_generation: u64,
    next_command_enrichment_generation: u64,
    next_editor_completion_generation: u64,
    investigations: Vec<InvestigationItem>,
    ai_provider: String,
    ai_mode: String,
    ai_thinking: String,
    next_path_completion_generation: u64,
    view_runtime_status: HashMap<String, String>,
    clock_now_unix_nanos: i64,
    last_clock_unix_nanos: Option<i64>,
    next_rolling_refresh: Option<Instant>,
    text_cursors: CursorBank,
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
            context_dialog: None,
            bookmark_dialog: None,
            show_help: false,
            help_scroll: 0,
            help_scroll_limit: 0,
            dialog_scroll: 0,
            dialog_scroll_limit: 0,
            dialog_scroll_focused: false,
            help_return_focus: Focus::Logs,
            terminal_size: (80, 24),
            should_quit: false,
            hit_regions: HitRegions::default(),
            source_dialog: empty.then(SourceDialogState::default),
            view_dialog: None,
            ask_ai_dialog: None,
            investigation_dialog: None,
            recipe_dialog: None,
            time_dialog: None,
            storage_dialog: None,
            settings_dialog: None,
            command_enrichment_dialog: None,
            source_notice: None,
            action_notice: None,
            editor_completion: None,
            theme_id: ThemeId::Terminal,
            delight_enabled: std::env::var_os("LVU_NO_DELIGHT").is_none(),
            reduced_motion: std::env::var_os("LVU_REDUCED_MOTION").is_some(),
            ascii: std::env::var_os("LVU_ASCII").is_some(),
            show_startup_title: true,
            view_states,
            query_requests: HashMap::new(),
            next_query_generation: 1,
            source_requests: VecDeque::new(),
            source_controls: VecDeque::new(),
            discovery_requests: VecDeque::new(),
            path_completion_requests: VecDeque::new(),
            view_requests: VecDeque::new(),
            ask_ai_requests: VecDeque::new(),
            source_ai_requests: VecDeque::new(),
            recipe_requests: VecDeque::new(),
            investigation_requests: VecDeque::new(),
            storage_requests: VecDeque::new(),
            settings_requests: VecDeque::new(),
            command_enrichment_requests: VecDeque::new(),
            pending_command_enrichment_saves: HashMap::new(),
            pending_command_enrichment_runs: HashMap::new(),
            settings_context: None,
            next_ask_ai_generation: 1,
            next_source_ai_generation: 1,
            next_investigation_generation: 1,
            next_recipe_generation: 1,
            next_storage_generation: 1,
            next_settings_generation: 1,
            next_command_enrichment_generation: 1,
            next_editor_completion_generation: 1,
            investigations: Vec::new(),
            ai_provider: "codex/gpt-5.6-sol".into(),
            ai_mode: "full-access".into(),
            ai_thinking: "medium".into(),
            next_path_completion_generation: 1,
            view_runtime_status: HashMap::new(),
            clock_now_unix_nanos: 0,
            last_clock_unix_nanos: None,
            next_rolling_refresh: None,
            text_cursors: CursorBank::default(),
        }
    }

    pub fn active_view_id(&self) -> Option<&str> {
        self.views
            .get(self.selected_view)
            .map(|view| view.id.as_str())
    }

    /// Identifies the concrete editable field without exposing its mutable draft.
    /// Dialog generations and view IDs fence ephemeral cursors from unrelated inputs.
    pub fn active_text_target(&self) -> Option<TextTarget> {
        if self.dialog_scroll_focused {
            return None;
        }
        let view = || self.active_view_id().map(str::to_owned);
        let target = match self.focus {
            Focus::SearchEditor => TextTarget {
                identity: view()?,
                field: "search",
            },
            Focus::AdvancedEditor => TextTarget {
                identity: view()?,
                field: "advanced",
            },
            Focus::EnrichmentEditor => {
                if self.view_state()?.enrichment_control != EnrichmentControl::Editor {
                    return None;
                }
                TextTarget {
                    identity: view()?,
                    field: "enrichment",
                }
            }
            Focus::GroupingEditor => TextTarget {
                identity: view()?,
                field: "grouping",
            },
            Focus::CommandEnrichment => {
                let dialog = self.command_enrichment_dialog.as_ref()?;
                if dialog.selected_control != CommandEnrichmentControl::Field {
                    return None;
                }
                if !matches!(
                    dialog.run_state,
                    CommandEnrichmentRunState::Unrun
                        | CommandEnrichmentRunState::Error
                        | CommandEnrichmentRunState::Ready
                        | CommandEnrichmentRunState::Complete
                ) {
                    return None;
                }
                TextTarget {
                    identity: format!("command:{}:{}", dialog.view_id, dialog.generation),
                    field: match dialog.selected_field {
                        CommandEnrichmentField::Program => "program",
                        CommandEnrichmentField::Arguments => "arguments",
                        CommandEnrichmentField::Cwd => "cwd",
                        CommandEnrichmentField::Environment => "environment",
                    },
                }
            }
            Focus::SourceDialog => {
                let dialog = self.source_dialog.as_ref()?;
                if dialog.control != SourceControl::Input {
                    return None;
                }
                TextTarget {
                    identity: "source-dialog".into(),
                    field: match dialog.mode {
                        SourceDialogMode::Manual => "source",
                        SourceDialogMode::Discovery => "discovery-search",
                        SourceDialogMode::Ai
                            if matches!(
                                dialog.ai.stage,
                                SourceAiStage::Input | SourceAiStage::Error
                            ) =>
                        {
                            "source-assistance"
                        }
                        SourceDialogMode::Ai => return None,
                    },
                }
            }
            Focus::ViewDialog => {
                let dialog = self.view_dialog.as_ref()?;
                if dialog.mode == ViewDialogMode::Sources {
                    return None;
                }
                TextTarget {
                    identity: format!("view-dialog:{}", view()?),
                    field: "name",
                }
            }
            Focus::AskAi => {
                let dialog = self.ask_ai_dialog.as_ref()?;
                if !matches!(dialog.stage, AskAiStage::Input | AskAiStage::Error) {
                    return None;
                }
                TextTarget {
                    identity: format!("ask:{}:{}", dialog.view_id, dialog.generation),
                    field: "prompt",
                }
            }
            Focus::Investigation => {
                let dialog = self.investigation_dialog.as_ref()?;
                if !matches!(
                    dialog.stage,
                    InvestigationStage::Input
                        | InvestigationStage::Conversation
                        | InvestigationStage::Error
                ) {
                    return None;
                }
                TextTarget {
                    identity: format!("investigation:{}:{}", dialog.view_id, dialog.generation),
                    field: "input",
                }
            }
            Focus::Recipes => {
                let dialog = self.recipe_dialog.as_ref()?;
                if !dialog.mode.is_editable() {
                    return None;
                }
                TextTarget {
                    identity: format!("recipe:{}", dialog.id),
                    field: "name-or-path",
                }
            }
            Focus::Bookmarks => {
                let dialog = self.bookmark_dialog.as_ref()?;
                let id = dialog.editing.as_ref()?;
                TextTarget {
                    identity: format!("bookmark:{}:{id:?}", dialog.view_id),
                    field: "note",
                }
            }
            Focus::Settings => {
                let dialog = self.settings_dialog.as_ref()?;
                let SettingsControl::Field(selected) = dialog.focus else {
                    return None;
                };
                let field = match selected {
                    SettingsField::Provider => "provider",
                    SettingsField::Mode => "mode",
                    SettingsField::Thinking => "thinking",
                    SettingsField::RowCache => "row-cache",
                    SettingsField::Membership => "membership",
                    SettingsField::DiskTotal => "disk-total",
                    SettingsField::IndexPerSource => "index-per-source",
                    _ => return None,
                };
                TextTarget {
                    identity: format!("settings:{}", dialog.generation),
                    field,
                }
            }
            Focus::TimeEditor => {
                let dialog = self.time_dialog.as_ref()?;
                if dialog.dropdown.is_some() {
                    return None;
                }
                let field = match dialog.focus {
                    TimeControl::StartDate => "start-date",
                    TimeControl::StartClock => "start-clock",
                    TimeControl::StartZone if dialog.start_zone_custom => "start-zone",
                    TimeControl::EndDate => "end-date",
                    TimeControl::EndClock => "end-clock",
                    TimeControl::EndZone if dialog.end_zone_custom => "end-zone",
                    _ => return None,
                };
                TextTarget {
                    identity: format!("time:{}", view()?),
                    field,
                }
            }
            _ => return None,
        };
        Some(target)
    }

    pub fn is_text_editing(&self) -> bool {
        self.active_text_target().is_some()
    }

    pub fn key_to_action(&self, key: KeyEvent) -> Action {
        if self.focus == Focus::Settings
            && matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat)
            && let Some(dialog) = &self.settings_dialog
        {
            if dialog.theme_dropdown {
                return match key.code {
                    KeyCode::Esc => Action::CloseSettingsTheme,
                    KeyCode::Up => Action::MoveSettingsTheme(-1),
                    KeyCode::Down => Action::MoveSettingsTheme(1),
                    KeyCode::Enter => Action::ChooseSettingsTheme(dialog.theme_selected),
                    _ => Action::None,
                };
            }
            if dialog.focus == SettingsControl::More {
                match key.code {
                    KeyCode::Up => return Action::ScrollSettingsDetails(-1),
                    KeyCode::Down => return Action::ScrollSettingsDetails(1),
                    _ => {}
                }
            }
        }
        if self.is_text_editing()
            && self.editor_completion.is_none()
            && matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat)
            && key.modifiers.is_empty()
        {
            match key.code {
                KeyCode::Left => return Action::TextMoveLeft,
                KeyCode::Right => return Action::TextMoveRight,
                KeyCode::Up => return Action::TextMoveUp,
                KeyCode::Down => return Action::TextMoveDown,
                _ => {}
            }
        }
        key_to_action(key, self.focus)
    }

    fn active_text_snapshot(&self) -> Option<(TextTarget, String, EditPolicy)> {
        let target = self.active_text_target()?;
        let (value, max_bytes, multiline) = match self.focus {
            Focus::SearchEditor
            | Focus::AdvancedEditor
            | Focus::EnrichmentEditor
            | Focus::GroupingEditor => {
                let state = self.view_state()?;
                let value = match self.focus {
                    Focus::SearchEditor => &state.search.draft,
                    Focus::AdvancedEditor => &state.advanced.draft,
                    Focus::EnrichmentEditor => &state.enrichment.draft,
                    Focus::GroupingEditor => &state.grouping.draft,
                    _ => unreachable!(),
                };
                (
                    value.clone(),
                    MAX_EDITOR_BYTES,
                    self.focus != Focus::SearchEditor,
                )
            }
            Focus::CommandEnrichment => {
                let dialog = self.command_enrichment_dialog.as_ref()?;
                let value = match dialog.selected_field {
                    CommandEnrichmentField::Program => &dialog.program,
                    CommandEnrichmentField::Arguments => &dialog.arguments,
                    CommandEnrichmentField::Cwd => &dialog.cwd,
                    CommandEnrichmentField::Environment => &dialog.environment,
                };
                (
                    value.clone(),
                    MAX_COMMAND_FIELD_BYTES,
                    matches!(
                        dialog.selected_field,
                        CommandEnrichmentField::Arguments | CommandEnrichmentField::Environment
                    ),
                )
            }
            Focus::SourceDialog => {
                let dialog = self.source_dialog.as_ref()?;
                match dialog.mode {
                    SourceDialogMode::Manual => (dialog.draft.clone(), MAX_EDITOR_BYTES, false),
                    SourceDialogMode::Discovery => {
                        (dialog.discovery.query.clone(), MAX_EDITOR_BYTES, false)
                    }
                    SourceDialogMode::Ai => {
                        (dialog.ai.instruction.clone(), MAX_AI_PROMPT_BYTES, false)
                    }
                }
            }
            Focus::ViewDialog => (self.view_dialog.as_ref()?.draft.clone(), 128, false),
            Focus::AskAi => (
                self.ask_ai_dialog.as_ref()?.prompt.clone(),
                MAX_AI_PROMPT_BYTES,
                true,
            ),
            Focus::Investigation => (
                self.investigation_dialog.as_ref()?.input.clone(),
                MAX_AI_PROMPT_BYTES,
                true,
            ),
            Focus::Recipes => (
                self.recipe_dialog.as_ref()?.name.clone(),
                MAX_EDITOR_BYTES,
                false,
            ),
            Focus::Bookmarks => (
                self.bookmark_dialog.as_ref()?.draft.clone(),
                MAX_BOOKMARK_NOTE_BYTES,
                false,
            ),
            Focus::Settings => {
                let dialog = self.settings_dialog.as_ref()?;
                let value = setting_field(dialog)?;
                (value.clone(), 256, false)
            }
            Focus::TimeEditor => {
                let dialog = self.time_dialog.as_ref()?;
                (
                    dialog_time_segment(dialog, dialog.focus).to_owned(),
                    32,
                    false,
                )
            }
            _ => return None,
        };
        Some((
            target,
            value,
            EditPolicy {
                max_bytes,
                multiline,
            },
        ))
    }

    /// Returns the active scalar cursor, initializing a newly opened concrete
    /// field at its end. Rendering calls this to keep the visible cursor honest.
    pub fn active_text_cursor(&mut self) -> Option<usize> {
        let (target, value, _) = self.active_text_snapshot()?;
        if self.focus == Focus::TimeEditor {
            let dialog = self.time_dialog.as_mut()?;
            dialog.segment_cursor = dialog.segment_cursor.min(value.chars().count());
            return Some(dialog.segment_cursor);
        }
        Some(self.text_cursors.get_or_end(target, &value).char_index)
    }

    fn apply_text_command(&mut self, command: EditCommand<'_>) -> bool {
        let Some((target, mut value, policy)) = self.active_text_snapshot() else {
            return false;
        };
        let mut cursor = if self.focus == Focus::TimeEditor {
            TextCursor {
                char_index: self
                    .time_dialog
                    .as_ref()
                    .map_or(0, |dialog| dialog.segment_cursor),
            }
        } else {
            self.text_cursors.get_or_end(target.clone(), &value)
        };
        let outcome = edit(&mut value, &mut cursor, command, policy);
        if self.focus == Focus::TimeEditor {
            if let Some(dialog) = &mut self.time_dialog {
                dialog.segment_cursor = cursor.char_index;
            }
        } else {
            self.text_cursors.store(target, cursor);
        }
        if outcome.changed {
            self.replace_active_text(value);
        }
        outcome.changed || outcome.moved
    }

    fn replace_active_text(&mut self, value: String) {
        match self.focus {
            Focus::SearchEditor
            | Focus::AdvancedEditor
            | Focus::EnrichmentEditor
            | Focus::GroupingEditor => {
                let Some(id) = self.active_view_id().map(str::to_owned) else {
                    return;
                };
                let purpose = self.editor_purpose().expect("active editor");
                let state = self.view_states.get_mut(&id).expect("view state");
                match purpose {
                    QueryPurpose::Search => state.search.draft = value,
                    QueryPurpose::Advanced => state.advanced.draft = value,
                    QueryPurpose::Enrichment => state.enrichment.draft = value,
                    QueryPurpose::Grouping => state.grouping.draft = value,
                }
                state.user_interaction_revision = state.user_interaction_revision.saturating_add(1);
                state.ai_definition_revision = state.ai_definition_revision.saturating_add(1);
                self.editor_completion = None;
                self.schedule_search();
            }
            Focus::CommandEnrichment => {
                let Some(dialog) = &mut self.command_enrichment_dialog else {
                    return;
                };
                *command_draft_field_mut(dialog) = value;
                dialog.error = None;
                dialog.review = None;
                dialog.run_state = CommandEnrichmentRunState::Unrun;
                dialog.run_status = "Draft changed · save before reviewing a run".into();
                if let Some(state) = self.view_states.get_mut(&dialog.view_id) {
                    state.user_interaction_revision =
                        state.user_interaction_revision.saturating_add(1);
                }
            }
            Focus::SourceDialog => {
                if let Some(dialog) = &mut self.source_dialog {
                    match dialog.mode {
                        SourceDialogMode::Manual => {
                            dialog.draft = value;
                            clear_path_completion(dialog);
                        }
                        SourceDialogMode::Discovery => {
                            dialog.discovery.query = value;
                            dialog.discovery.selected = 0;
                        }
                        SourceDialogMode::Ai => {
                            dialog.ai.instruction = value;
                            dialog.ai.stage = SourceAiStage::Input;
                        }
                    }
                    dialog.error = None;
                }
            }
            Focus::ViewDialog => {
                if let Some(dialog) = &mut self.view_dialog {
                    dialog.draft = value;
                    dialog.error = None;
                }
            }
            Focus::AskAi => {
                if let Some(dialog) = &mut self.ask_ai_dialog {
                    dialog.prompt = value;
                    dialog.stage = AskAiStage::Input;
                }
            }
            Focus::Investigation => {
                if let Some(dialog) = &mut self.investigation_dialog {
                    dialog.input = value;
                }
            }
            Focus::Recipes => {
                if let Some(dialog) = &mut self.recipe_dialog {
                    dialog.name = value;
                    dialog.interaction_revision = dialog.interaction_revision.saturating_add(1);
                }
            }
            Focus::Bookmarks => {
                if let Some(dialog) = &mut self.bookmark_dialog {
                    dialog.draft = value;
                    if let Some(state) = self.view_states.get_mut(&dialog.view_id) {
                        state.user_interaction_revision =
                            state.user_interaction_revision.saturating_add(1);
                    }
                }
            }
            Focus::Settings => edit_setting(self.settings_dialog.as_mut(), |field| *field = value),
            Focus::TimeEditor => {
                if let Some(dialog) = &mut self.time_dialog {
                    match dialog.focus {
                        TimeControl::StartDate => dialog.start_date = value,
                        TimeControl::StartClock => dialog.start_clock = value,
                        TimeControl::StartZone => dialog.start_zone = value,
                        TimeControl::EndDate => dialog.end_date = value,
                        TimeControl::EndClock => dialog.end_clock = value,
                        TimeControl::EndZone => dialog.end_zone = value,
                        _ => return,
                    }
                    dialog.window = TimeWindowChoice::Absolute;
                }
                let drafts = self.time_dialog.as_ref().map(dialog_time_drafts);
                if let Some(state) = self.view_state_mut()
                    && let Some((start, end, parts)) = drafts
                {
                    store_time_drafts(state, start, end, parts);
                    state.time_window_draft = TimeWindowChoice::Absolute;
                    mark_time_edit(state);
                    state.time_error = None;
                    state.time_draft_touched = true;
                }
            }
            _ => {}
        }
    }

    pub fn view_state(&self) -> Option<&ViewState> {
        self.active_view_id()
            .and_then(|id| self.view_states.get(id))
    }

    pub fn time_update_pending(&self) -> bool {
        self.view_state()
            .is_some_and(|state| state.pending_time.is_some())
    }

    fn view_state_mut(&mut self) -> Option<&mut ViewState> {
        let id = self.active_view_id()?.to_owned();
        self.view_states.get_mut(&id)
    }

    pub fn search_state(&self) -> Option<&EditorState> {
        self.view_state().map(|state| &state.search)
    }

    pub fn bookmarks_for_view(&self, view_id: &str) -> &[Bookmark] {
        self.view_states
            .get(view_id)
            .map_or(&[], |state| state.bookmarks.as_slice())
    }

    pub fn advanced_state(&self) -> Option<&EditorState> {
        self.view_state().map(|state| &state.advanced)
    }

    pub fn set_details_viewport(&mut self, row: Option<RowId>, scroll_limit: usize) -> usize {
        let Some(state) = self.view_state_mut() else {
            return 0;
        };
        if state.details_row != row {
            state.details_row = row;
            state.details_scroll = 0;
        }
        state.details_scroll_limit = scroll_limit;
        state.details_scroll = state.details_scroll.min(scroll_limit);
        state.details_scroll
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
            source_ids: self.view_source_ids(view_id),
            view_name: name,
            applied_search: state.search.applied.clone(),
            search_draft: state.search.draft.clone(),
            search_error: state.search.error.clone(),
            applied_advanced: state.advanced.applied.clone(),
            advanced_draft: state.advanced.draft.clone(),
            advanced_error: state.advanced.error.clone(),
            applied_enrichment: state.enrichment.applied.clone(),
            applied_enrichments: state.enrichments.clone(),
            enrichment_draft: state.enrichment.draft.clone(),
            enrichment_error: state.enrichment.error.clone(),
            enrichment_editing: state.enrichment_editing.clone(),
            enrichment_selected: state.enrichment_selected,
            command_enrichment: state.command_enrichment.clone(),
            command_enrichment_revision: state.command_enrichment_revision,
            command_publication: state.command_publication.clone(),
            applied_grouping: state.grouping.applied.clone(),
            grouping_draft: state.grouping.draft.clone(),
            grouping_error: state.grouping.error.clone(),
            applied_capture_time: match state.applied_capture_time_policy {
                Some(CaptureTimePolicy::Recent { .. }) => None,
                _ => state.applied_capture_time,
            },
            applied_capture_time_policy: state.applied_capture_time_policy,
            applied_time_basis: state.applied_time_basis,
            time_start_draft: state.time_start_draft.clone(),
            time_end_draft: state.time_end_draft.clone(),
            time_recent_draft: state.time_recent_draft.clone(),
            time_error: state.time_error.clone(),
            time_draft_touched: state.time_draft_touched,
            time_window_draft: state.time_window_draft,
            time_basis_draft: state.time_basis_draft,
            time_start_date_draft: state.time_start_date_draft.clone(),
            time_start_clock_draft: state.time_start_clock_draft.clone(),
            time_start_zone_draft: state.time_start_zone_draft.clone(),
            time_end_date_draft: state.time_end_date_draft.clone(),
            time_end_clock_draft: state.time_end_clock_draft.clone(),
            time_end_zone_draft: state.time_end_zone_draft.clone(),
            time_structured_draft_present: state.time_structured_draft_present,
            bookmarks: state.bookmarks.clone(),
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

    pub fn configure_settings(&mut self, context: SettingsContext) {
        self.settings_context = Some(context);
    }

    pub fn take_settings_requests(&mut self) -> Vec<SettingsRequest> {
        self.settings_requests.drain(..).collect()
    }

    pub fn take_command_enrichment_requests(&mut self) -> Vec<CommandEnrichmentRequest> {
        self.command_enrichment_requests.drain(..).collect()
    }

    pub(crate) fn command_work_pending(&self) -> bool {
        !self.pending_command_enrichment_saves.is_empty()
            || !self.pending_command_enrichment_runs.is_empty()
            || self
                .command_enrichment_dialog
                .as_ref()
                .is_some_and(|dialog| {
                    matches!(
                        dialog.run_state,
                        CommandEnrichmentRunState::Saving
                            | CommandEnrichmentRunState::Preparing
                            | CommandEnrichmentRunState::Running
                            | CommandEnrichmentRunState::SavingResults
                    )
                })
    }

    fn command_request_count(&self) -> usize {
        let mut generations = self
            .pending_command_enrichment_saves
            .keys()
            .copied()
            .collect::<HashSet<_>>();
        generations.extend(self.pending_command_enrichment_runs.keys().copied());
        generations.extend(
            self.command_enrichment_requests
                .iter()
                .map(|request| match request {
                    CommandEnrichmentRequest::Save { generation, .. }
                    | CommandEnrichmentRequest::PrepareRun { generation, .. }
                    | CommandEnrichmentRequest::Execute { generation, .. }
                    | CommandEnrichmentRequest::Cancel { generation, .. } => *generation,
                }),
        );
        generations.len()
    }

    pub fn finish_command_enrichment_save(
        &mut self,
        generation: u64,
        view_id: &str,
        definition_revision: u64,
        result: Result<Option<CommandEnrichmentStage>, String>,
    ) -> bool {
        let Some((pending_view, base_revision)) = self
            .pending_command_enrichment_saves
            .get(&generation)
            .cloned()
        else {
            return false;
        };
        if pending_view != view_id {
            return false;
        }
        self.pending_command_enrichment_saves.remove(&generation);
        if self
            .view_states
            .get(view_id)
            .is_none_or(|state| state.command_enrichment_revision != base_revision)
            || (result.is_ok() && base_revision >= definition_revision)
        {
            return false;
        }
        match result {
            Ok(stage) => {
                if let Some(state) = self.view_states.get_mut(view_id) {
                    state.command_enrichment = stage.clone();
                    state.command_enrichment_revision = definition_revision;
                }
                if let Some(dialog) = self.command_enrichment_dialog.as_mut()
                    && dialog.generation == generation
                    && dialog.view_id == view_id
                {
                    dialog.accepted = stage;
                    dialog.base_definition_revision = definition_revision;
                    dialog.error = None;
                    dialog.run_state = CommandEnrichmentRunState::Unrun;
                    dialog.run_status =
                        "Saved · Unrun; new records wait for an explicit run".into();
                    dialog.review = None;
                } else {
                    self.action_notice =
                        Some("command enrichment definition saved; it was not run".into());
                }
            }
            Err(error) => {
                if let Some(dialog) = self.command_enrichment_dialog.as_mut()
                    && dialog.generation == generation
                    && dialog.view_id == view_id
                {
                    dialog.error = Some(error);
                    dialog.run_state = CommandEnrichmentRunState::Error;
                } else {
                    self.action_notice = Some(format!("command enrichment unchanged: {error}"));
                }
            }
        }
        true
    }

    pub fn finish_command_enrichment_review(
        &mut self,
        generation: u64,
        view_id: &str,
        definition_revision: u64,
        result: Result<CommandEnrichmentReview, String>,
    ) -> bool {
        let Some(dialog) = self.command_enrichment_dialog.as_mut() else {
            return false;
        };
        if dialog.generation != generation
            || dialog.view_id != view_id
            || dialog.base_definition_revision != definition_revision
            || dialog.run_state != CommandEnrichmentRunState::Preparing
        {
            return false;
        }
        match result {
            Ok(review) => {
                dialog.review = Some(review);
                dialog.run_state = CommandEnrichmentRunState::Ready;
                dialog.run_status =
                    "Ready for review · confirmation runs exactly this bounded set".into();
            }
            Err(error) => {
                dialog.run_state = CommandEnrichmentRunState::Error;
                dialog.run_status = error;
            }
        }
        true
    }

    pub fn finish_command_enrichment_run(
        &mut self,
        generation: u64,
        view_id: &str,
        definition_revision: u64,
        result: Result<String, String>,
    ) -> bool {
        let Some((pending_view, pending_revision)) = self
            .pending_command_enrichment_runs
            .get(&generation)
            .cloned()
        else {
            return false;
        };
        if pending_view != view_id || pending_revision != definition_revision {
            return false;
        }
        self.pending_command_enrichment_runs.remove(&generation);
        if self
            .view_states
            .get(view_id)
            .is_none_or(|state| state.command_enrichment_revision != definition_revision)
        {
            return false;
        }
        let matching_dialog = self.command_enrichment_dialog.as_mut().filter(|dialog| {
            dialog.generation == generation
                && dialog.view_id == view_id
                && dialog.base_definition_revision == definition_revision
                && matches!(
                    dialog.run_state,
                    CommandEnrichmentRunState::Running | CommandEnrichmentRunState::SavingResults
                )
        });
        if let Some(dialog) = matching_dialog {
            dialog.review = None;
            match result {
                Ok(status) => {
                    dialog.run_state = CommandEnrichmentRunState::Complete;
                    dialog.run_status = status;
                }
                Err(error) => {
                    dialog.run_state = CommandEnrichmentRunState::Error;
                    dialog.run_status = error;
                }
            }
        } else {
            self.action_notice = Some(match result {
                Ok(status) => format!("command enrichment results saved: {status}"),
                Err(error) => format!("command enrichment results unchanged: {error}"),
            });
        }
        true
    }

    pub fn begin_command_result_save(
        &mut self,
        generation: u64,
        view_id: &str,
        definition_revision: u64,
    ) -> bool {
        if self.pending_command_enrichment_runs.get(&generation)
            != Some(&(view_id.to_owned(), definition_revision))
        {
            return false;
        }
        let Some(dialog) = self.command_enrichment_dialog.as_mut() else {
            return false;
        };
        if dialog.generation != generation
            || dialog.view_id != view_id
            || dialog.base_definition_revision != definition_revision
            || dialog.run_state != CommandEnrichmentRunState::Running
        {
            return false;
        }
        dialog.run_state = CommandEnrichmentRunState::SavingResults;
        dialog.run_status = "Saving results…".into();
        dialog.error = None;
        true
    }

    pub fn commit_command_publication(
        &mut self,
        view_id: &str,
        expected_command_revision: u64,
        publication: String,
    ) -> bool {
        let Some(state) = self.view_states.get_mut(view_id) else {
            return false;
        };
        if state.command_enrichment_revision != expected_command_revision
            || state.command_enrichment.is_none()
        {
            return false;
        }
        state.command_publication = Some(publication);
        true
    }

    pub fn complete_settings_save(
        &mut self,
        generation: u64,
        result: Result<SettingsContext, String>,
    ) -> bool {
        match result {
            Ok(context) => {
                self.ai_provider = context.effective_provider.clone();
                self.ai_mode = context.effective_mode.clone();
                self.ai_thinking = context.effective_thinking.clone();
                let preview = if let Some(dialog) = &mut self.settings_dialog {
                    if dialog.generation == generation {
                        dialog.saving = false;
                        dialog.draft = context.saved.clone();
                        dialog.context = context.clone();
                        dialog.status_kind = SettingsStatus::Saved;
                        dialog.status = settings_restart_status(&context);
                        dialog.details_scroll = 0;
                        None
                    } else {
                        // A newer dialog may already be previewing appearance changes.
                        // Advance its rollback baseline to the successful save without
                        // replacing that newer draft or briefly exposing stale globals.
                        dialog.context = context.clone();
                        self.source_notice = Some(settings_restart_status(&context));
                        Some((
                            dialog.draft.theme,
                            dialog.draft.delight_enabled,
                            dialog.draft.reduced_motion,
                            dialog.draft.ascii,
                        ))
                    }
                } else {
                    self.source_notice = Some(settings_restart_status(&context));
                    None
                };
                let (theme, delight, reduced_motion, ascii) = preview.unwrap_or((
                    context.effective_theme,
                    context.effective_delight_enabled,
                    context.effective_reduced_motion,
                    context.effective_ascii,
                ));
                self.theme_id = theme;
                self.delight_enabled = delight;
                self.reduced_motion = reduced_motion;
                self.ascii = ascii;
                self.settings_context = Some(context);
                true
            }
            Err(error) => {
                if let Some(dialog) = &mut self.settings_dialog
                    && dialog.generation == generation
                {
                    dialog.saving = false;
                    dialog.status_kind = SettingsStatus::Error;
                    dialog.status = format!("save failed: {error}");
                    dialog.details_scroll = 0;
                    true
                } else {
                    self.source_notice = Some(format!("settings save failed: {error}"));
                    false
                }
            }
        }
    }

    pub fn configure_appearance(
        &mut self,
        theme_id: ThemeId,
        delight_enabled: bool,
        reduced_motion: bool,
        ascii: bool,
    ) {
        self.theme_id = theme_id;
        self.delight_enabled = delight_enabled;
        self.reduced_motion = reduced_motion;
        self.ascii = ascii;
    }

    /// Hide an untouched startup placeholder until all persisted sources are open.
    pub fn defer_view_restore(&mut self, view_id: &str) {
        let selected = self.active_view_id().map(str::to_owned);
        self.views.retain(|view| view.id != view_id);
        self.view_states.remove(view_id);
        self.text_cursors.prune_identity(view_id);
        self.text_cursors.prune_identity(&format!("time:{view_id}"));
        self.text_cursors
            .prune_where_identity_contains(&format!(":{view_id}:"));
        self.selected_view = selected
            .and_then(|id| self.views.iter().position(|view| view.id == id))
            .unwrap_or_else(|| self.selected_view.min(self.views.len().saturating_sub(1)));
    }

    pub fn view_source_ids(&self, view_id: &str) -> Vec<String> {
        self.view_states
            .get(view_id)
            .filter(|state| !state.source_ids.is_empty())
            .map(|state| state.source_ids.clone())
            .unwrap_or_else(|| {
                self.views
                    .iter()
                    .find(|view| view.id == view_id)
                    .map(|view| vec![view.source_id.clone()])
                    .unwrap_or_default()
            })
    }

    pub fn begin_source_change(
        &mut self,
        view_id: &str,
        sources: Vec<String>,
    ) -> Result<QueryRequest, String> {
        let primary = self
            .views
            .iter()
            .find(|view| view.id == view_id)
            .ok_or("view no longer exists")?
            .source_id
            .clone();
        let mut seen = HashSet::new();
        if sources.is_empty()
            || sources.len() > 32
            || !sources.contains(&primary)
            || sources
                .iter()
                .any(|id| !seen.insert(id) || !self.sources.iter().any(|source| &source.id == id))
        {
            return Err("select up to 32 open sources, including this view's owning source".into());
        }
        let state = self
            .view_states
            .get_mut(view_id)
            .ok_or("view no longer exists")?;
        if state_has_pending_query(state) {
            return Err("wait for the current query before editing sources".into());
        }
        if state
            .bookmarks
            .iter()
            .any(|bookmark| !sources.contains(&bookmark.id.source_id))
        {
            return Err(
                "remove bookmarks for an excluded source before removing it from this view".into(),
            );
        }
        let generation = self.next_query_generation;
        self.next_query_generation = self.next_query_generation.saturating_add(1);
        state.desired_query_revision = state.desired_query_revision.saturating_add(1);
        state.user_interaction_revision = state.user_interaction_revision.saturating_add(1);
        state.ai_definition_revision = state.ai_definition_revision.saturating_add(1);
        let revision = state.desired_query_revision;
        let constraints = applied_constraints(state);
        state.desired_constraints = constraints.clone();
        state.pending_source_change = Some((revision, generation, sources));
        Ok(QueryRequest {
            view_id: view_id.into(),
            generation,
            revision,
            base_revision: state.applied_query_revision,
            base_constraints: constraints.clone(),
            constraints,
            purpose: QueryPurpose::Advanced,
        })
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
        let mut source_ids = HashSet::new();
        let primary = self
            .views
            .iter()
            .find(|view| view.id == view_id)
            .map(|view| &view.source_id);
        if !restored.source_ids.is_empty()
            && (restored.source_ids.len() > 32
                || primary.is_none_or(|id| !restored.source_ids.contains(id))
                || restored
                    .source_ids
                    .iter()
                    .any(|id| id.is_empty() || id.len() > 128 || !source_ids.insert(id)))
        {
            return false;
        }
        let mut bookmark_ids = HashSet::new();
        if restored.bookmarks.len() > MAX_BOOKMARKS
            || restored.bookmarks.iter().any(|bookmark| {
                (primary.is_none_or(|id| id != &bookmark.id.source_id)
                    && !restored.source_ids.contains(&bookmark.id.source_id))
                    || bookmark.id.source_id.is_empty()
                    || bookmark.id.source_id.len() > 128
                    || bookmark.note.len() > MAX_BOOKMARK_NOTE_BYTES
                    || bookmark.note.chars().any(char::is_control)
                    || !bookmark_ids.insert(bookmark.id.clone())
            })
        {
            return false;
        }
        self.view_states
            .get_mut(view_id)
            .expect("checked view")
            .source_ids = restored.source_ids.clone();
        if !valid_enrichments(&restored.applied_enrichments) {
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
        state.enrichment_editing = restored.enrichment_editing;
        state.enrichment_selected = restored.enrichment_selected;
        state.command_enrichment_revision = restored.command_enrichment_revision;
        state.command_enrichment = restored.command_enrichment;
        state.command_publication = restored.command_publication;
        state.grouping.draft = restored.grouping_draft;
        state.grouping.error = restored.grouping_error;
        state.time_start_draft = restored.time_start_draft;
        state.time_end_draft = restored.time_end_draft;
        state.time_recent_draft = restored.time_recent_draft;
        state.time_error = restored.time_error;
        state.time_draft_touched = restored.time_draft_touched
            || !state.time_start_draft.is_empty()
            || !state.time_end_draft.is_empty();
        state.time_window_draft = restored.time_window_draft;
        state.time_start_date_draft = restored.time_start_date_draft;
        state.time_start_clock_draft = restored.time_start_clock_draft;
        state.time_start_zone_draft = restored.time_start_zone_draft;
        state.time_end_date_draft = restored.time_end_date_draft;
        state.time_end_clock_draft = restored.time_end_clock_draft;
        state.time_end_zone_draft = restored.time_end_zone_draft;
        state.time_structured_draft_present = restored.time_structured_draft_present;
        if !state.time_structured_draft_present
            && (!state.time_start_draft.is_empty() || !state.time_end_draft.is_empty())
        {
            let start = split_time_draft(&state.time_start_draft);
            let end = split_time_draft(&state.time_end_draft);
            state.time_start_date_draft = start.0;
            state.time_start_clock_draft = start.1;
            state.time_start_zone_draft = start.2;
            state.time_end_date_draft = end.0;
            state.time_end_clock_draft = end.1;
            state.time_end_zone_draft = end.2;
            state.time_structured_draft_present = true;
        }
        state.bookmarks = restored.bookmarks;
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
            enrichments: if restored.applied_enrichments.is_empty() {
                legacy_enrichment(&restored.applied_enrichment)
            } else {
                restored.applied_enrichments
            },
            enrichment: None,
            capture_time: resolved_capture_time,
            time_basis: restored.applied_time_basis,
            grouping: nonempty(&restored.applied_grouping),
        };
        let purpose = if !constraints.enrichments.is_empty() {
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
        state.desired_time_basis = restored.applied_time_basis;
        state.time_basis_draft = if restored.time_draft_touched {
            restored.time_basis_draft
        } else {
            restored.applied_time_basis
        };
        state.search.pending_generation = Some(generation);
        state.search.pending_revision = Some(revision);
        state.search.pending_value = Some(restored.applied_search);
        state.advanced.pending_generation = Some(generation);
        state.advanced.pending_revision = Some(revision);
        state.advanced.pending_value = Some(restored.applied_advanced);
        state.enrichment.pending_generation = Some(generation);
        state.enrichment.pending_revision = Some(revision);
        state.enrichment.pending_value = Some(restored.applied_enrichment);
        state.grouping.pending_generation = Some(generation);
        state.grouping.pending_revision = Some(revision);
        state.grouping.pending_value = Some(restored.applied_grouping);
        state.pending_time = Some(PendingTime {
            generation,
            revision,
            value: constraints.capture_time,
            policy: restored_policy,
            basis: restored.applied_time_basis,
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
        if !valid_enrichments(&config.enrichments) {
            if let Some(state) = self.view_state_mut() {
                state.enrichment.error = Some(
                    "recipe enrichment stages have duplicate, oversized, or invalid IDs".into(),
                );
            }
            return false;
        }
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
        state.enrichment.draft = config
            .enrichments
            .last()
            .map_or_else(|| config.enrichment.clone(), |stage| stage.source.clone());
        state.enrichment_editing = config.enrichments.last().map(|stage| stage.id.clone());
        state.enrichment_selected = config.enrichments.len().saturating_sub(1);
        state.grouping.draft = config.grouping.clone();
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
        state.grouping.error = None;
        state.time_error = None;
        let pins = config.pinned_columns;
        let color = config.color_field;
        let constraints = QueryConstraints {
            text: nonempty_text(&config.search),
            advanced_polars: nonempty(&config.advanced),
            enrichments: if config.enrichments.is_empty() {
                legacy_enrichment(&config.enrichment)
            } else {
                config.enrichments.clone()
            },
            enrichment: None,
            capture_time: resolved_capture_time,
            time_basis: config.time_basis,
            grouping: nonempty(&config.grouping),
        };
        state.desired_constraints = constraints;
        state.desired_capture_time_policy = policy;
        state.desired_time_basis = config.time_basis;
        let Some(revision) = self.enqueue_query(&view_id, QueryPurpose::Advanced) else {
            let state = self.view_states.get_mut(&view_id).expect("view state");
            state.desired_constraints = applied_constraints(state);
            state.desired_capture_time_policy = state.applied_capture_time_policy;
            state.desired_time_basis = state.applied_time_basis;
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
            basis: config.time_basis,
        });
        state.pending_recipe = Some(PendingRecipe {
            revision,
            interaction_revision: state.user_interaction_revision,
            pinned_columns: pins,
            color_field: color,
            capture_time_policy: policy,
            time_basis: config.time_basis,
            suggestion: None,
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
            Focus::GroupingEditor => self.view_state().map(|state| &state.grouping),
            Focus::Selector
            | Focus::Logs
            | Focus::Details
            | Focus::SourceDialog
            | Focus::Help
            | Focus::ViewDialog
            | Focus::FieldPicker
            | Focus::AskAi
            | Focus::Investigation
            | Focus::CommandEnrichment => None,
            Focus::Recipes
            | Focus::TimeEditor
            | Focus::Storage
            | Focus::Settings
            | Focus::Context
            | Focus::Bookmarks => None,
        }
    }

    pub fn take_source_controls(&mut self) -> Vec<SourceControlRequest> {
        self.source_controls.drain(..).collect()
    }

    pub fn take_source_requests(&mut self) -> Vec<SourceLaunchRequest> {
        self.source_requests.drain(..).collect()
    }

    pub fn take_storage_requests(&mut self) -> Vec<StorageRequest> {
        self.storage_requests.drain(..).collect()
    }

    pub fn update_storage(
        &mut self,
        generation: u64,
        snapshot: StorageSnapshot,
        status: String,
        complete: bool,
    ) -> bool {
        let Some(dialog) = &mut self.storage_dialog else {
            return false;
        };
        if dialog.generation != generation {
            return false;
        }
        dialog.snapshot = snapshot;
        dialog.selected = dialog
            .selected
            .min(dialog.snapshot.entries.len().saturating_sub(1));
        dialog.scanning = !complete;
        dialog.status = status;
        if complete {
            dialog.confirm_clear = false;
        }
        true
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
        dialog.discovery.status_scroll = 0;
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
        self.set_recipes_with_suggestions(meta, items, Vec::new(), error);
    }
    pub fn set_recipes_with_suggestions(
        &mut self,
        meta: RecipeRequestMeta,
        items: Vec<RecipeItem>,
        suggestions: Vec<RecipeSuggestion>,
        error: Option<String>,
    ) {
        if let Some(dialog) = &mut self.recipe_dialog
            && dialog.id == meta.dialog_id
            && dialog.interaction_revision == meta.dialog_revision
            && dialog.pending_request_id == Some(meta.request_id)
        {
            dialog.items = items.into_iter().take(128).collect();
            dialog.suggestions = suggestions.into_iter().take(16).collect();
            dialog.selected = dialog.selected.min(dialog.items.len().saturating_sub(1));
            dialog.loading = false;
            dialog.pending_request_id = None;
            dialog.status = error.unwrap_or_else(|| {
                if dialog.mode == RecipeDialogMode::History {
                    format!(
                        "{} revisions (newest first; at most 100)",
                        dialog.items.len()
                    )
                } else {
                    format!("{} saved recipes", dialog.items.len())
                }
            });
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
    pub fn recipe_exported(&mut self, meta: RecipeRequestMeta, message: String) {
        if let Some(dialog) = &mut self.recipe_dialog
            && dialog.id == meta.dialog_id
            && dialog.interaction_revision == meta.dialog_revision
            && dialog.pending_request_id == Some(meta.request_id)
        {
            dialog.loading = false;
            dialog.pending_request_id = None;
            dialog.status = message;
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
                ai.progress = "Review only — explicit confirmation starts this source".into();
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
            self.source_notice = Some("reviewed agent source started".into());
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
                dialog.progress = "turn complete; type a follow-up to continue".into();
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
                dialog.progress = "proposal ready; applying uses native validation".into();
            }
            Err(message) => {
                dialog.stage = AskAiStage::Error;
                dialog.progress = message;
            }
        }
        true
    }

    pub fn finish_recipe_ai(
        &mut self,
        generation: u64,
        view_id: &str,
        revision: u64,
        result: Result<(String, String, Option<Vec<EnrichmentDefinition>>), String>,
    ) -> bool {
        let result = result.and_then(|(expression, explanation, chain)| {
            if chain
                .as_ref()
                .is_some_and(|stages| !valid_enrichments(stages))
            {
                Err("invalid or oversized enrichment chain; working view preserved".into())
            } else {
                Ok((expression, explanation, chain))
            }
        });
        let (expression, chain) = match result {
            Ok((expression, explanation, chain)) => (Ok((expression, explanation)), chain),
            Err(error) => (Err(error), None),
        };
        let accepted = self.finish_ask_ai(generation, view_id, revision, expression);
        if accepted
            && let Some(dialog) = &mut self.ask_ai_dialog
            && dialog.kind == AskAiKind::Recipe
            && dialog.stage == AskAiStage::Proposal
            && let Some(chain) = chain
            && let Some(config) = &mut dialog.recipe
        {
            config.enrichments = chain;
            config.enrichment.clear();
        }
        accepted
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
                    controls_focused: false,
                    control: SourceControl::Input,
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
                // A display-only grouping provider maps every constituent ID
                // to its leading visible row. Canonicalize selection to that
                // stable visible ID so highlighting and fold state agree.
                state.selected = provider
                    .page(
                        &view_id,
                        ViewportRequest {
                            start: index,
                            len: 1,
                        },
                    )
                    .rows
                    .first()
                    .map(|row| row.id.clone())
                    .or(state.selected.take());
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
            if self.enqueue_query(view_id, QueryPurpose::Search).is_some() {
                self.view_states
                    .get_mut(view_id)
                    .expect("view state")
                    .search
                    .search_due = None;
            } else {
                // Backpressure must not consume the final (possibly empty) draft.
                self.view_states
                    .get_mut(view_id)
                    .expect("view state")
                    .search
                    .search_due = Some(now + SEARCH_DEBOUNCE);
            }
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
        if let Some((revision, generation, _)) = &state.pending_source_change {
            if *revision == completion.revision && *generation == completion.generation {
                let (_, _, sources) = state
                    .pending_source_change
                    .take()
                    .expect("checked source change");
                match completion.result {
                    Ok(()) => {
                        if state
                            .selected
                            .as_ref()
                            .is_some_and(|id| !sources.contains(&id.source_id))
                        {
                            state.selected = None;
                        }
                        state.source_ids = sources;
                        state.applied_query_revision = completion.revision;
                        self.action_notice =
                            Some("view sources updated; source order, then record sequence".into());
                    }
                    Err(failure) => {
                        state.desired_constraints = applied_constraints(state);
                        self.action_notice =
                            Some(format!("view sources unchanged: {}", failure.message));
                    }
                }
                return true;
            }
            if *revision < completion.revision {
                state.pending_source_change = None;
            }
        }
        let request_is_pending = [
            &state.search,
            &state.advanced,
            &state.enrichment,
            &state.grouping,
        ]
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
                let accepted_time_basis = state
                    .pending_time
                    .as_ref()
                    .filter(|pending| pending.revision <= completion.revision)
                    .map(|pending| pending.basis);
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
                let enrichment_mutation = state.pending_enrichment_mutation.take();
                let accepted_grouping = pending_at_or_before(&state.grouping, completion.revision);
                state.search.applied = constraint_text(&constraints);
                state.advanced.applied = constraints.advanced_polars.clone().unwrap_or_default();
                let appended_enrichment = constraints.enrichments.len() > state.enrichments.len();
                state.enrichments = constraints.enrichments.clone();
                if appended_enrichment {
                    state.enrichment_selected = state.enrichments.len().saturating_sub(1);
                }
                state.enrichment_selected = state
                    .enrichment_selected
                    .min(state.enrichments.len().saturating_sub(1));
                state.enrichment.applied = constraints
                    .enrichments
                    .last()
                    .map_or_else(String::new, |stage| stage.source.clone());
                state.grouping.applied = constraints.grouping.clone().unwrap_or_default();
                state.applied_capture_time = constraints.capture_time;
                if let Some(policy) = accepted_time_policy {
                    state.applied_capture_time_policy = policy;
                }
                if let Some(basis) = accepted_time_basis {
                    state.applied_time_basis = basis;
                }
                state.applied_query_revision = completion.revision;
                if state
                    .pending_recipe
                    .as_ref()
                    .is_some_and(|pending| pending.revision == completion.revision)
                    && let Some(pending) = state.pending_recipe.take()
                {
                    if let Some(outcome) = pending.suggestion {
                        self.recipe_requests
                            .push_back(RecipeRequest::Outcome(outcome));
                    }
                    state.applied_capture_time_policy = pending.capture_time_policy;
                    state.applied_time_basis = pending.time_basis;
                    if pending.interaction_revision == state.user_interaction_revision {
                        state.pinned_columns = pending.pinned_columns;
                        state.color_field = pending.color_field;
                    }
                }
                clear_accepted_pending(&mut state.search, completion.revision);
                clear_accepted_pending(&mut state.advanced, completion.revision);
                clear_accepted_pending(&mut state.enrichment, completion.revision);
                clear_accepted_pending(&mut state.grouping, completion.revision);
                if accepted_search && state.search.draft == state.search.applied {
                    state.search.error = None;
                }
                if accepted_advanced && state.advanced.draft == state.advanced.applied {
                    state.advanced.error = None;
                }
                if accepted_enrichment_draft && enrichment_mutation.is_some() {
                    if enrichment_mutation != Some(PendingEnrichmentMutation::Reaffirm) {
                        state.enrichment.error = None;
                    }
                    if matches!(
                        enrichment_mutation,
                        Some(PendingEnrichmentMutation::Add | PendingEnrichmentMutation::Edit)
                    ) {
                        state.enrichment.draft.clear();
                    }
                    state.enrichment_editing = None;
                }
                if accepted_grouping {
                    state.grouping.error = None;
                }
            }
            Err(failure) => {
                if failure.purpose == QueryPurpose::Search
                    && state.pending_recipe.is_none()
                    && (failure.message.contains("queue is full")
                        || failure.message.contains("capacity is full"))
                {
                    state.search.error = Some(failure.message);
                    state.search.search_due = Some(Instant::now() + SEARCH_DEBOUNCE);
                    return true;
                }
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
                    clear_accepted_pending(&mut state.grouping, completion.revision);
                    state.desired_constraints = applied_constraints(state);
                    state.desired_capture_time_policy = state.applied_capture_time_policy;
                    state.desired_time_basis = state.applied_time_basis;
                    editor_mut(state, failed_purpose).error = Some(failure_message.clone());
                    let accepted = match failed_purpose {
                        QueryPurpose::Search => state.search.applied.clone(),
                        QueryPurpose::Advanced => state.advanced.applied.clone(),
                        QueryPurpose::Enrichment => state.enrichment.applied.clone(),
                        QueryPurpose::Grouping => state.grouping.applied.clone(),
                    };
                    if failed_purpose == QueryPurpose::Enrichment {
                        let stages = self
                            .view_states
                            .get(&completion.view_id)
                            .map_or_else(Vec::new, |state| state.enrichments.clone());
                        self.enqueue_enrichment_chain(
                            &completion.view_id,
                            stages,
                            accepted,
                            PendingEnrichmentMutation::Reaffirm,
                        );
                    } else {
                        self.enqueue_query_value(
                            &completion.view_id,
                            failed_purpose,
                            Some(accepted),
                        );
                    }
                    self.editor_mut(&completion.view_id, failed_purpose).error =
                        Some(failure_message);
                    return true;
                }
                let failed_purpose = failure.purpose;
                let failure_message = failure.message;
                if failed_purpose == QueryPurpose::Enrichment {
                    state.pending_enrichment_mutation = None;
                }
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
                .then(|| state.desired_constraints.enrichments.clone());
                let pending_enrichment_value = pending_enrichment
                    .as_ref()
                    .and_then(|_| state.enrichment.pending_value.clone())
                    .unwrap_or_default();
                let pending_enrichment_mutation = state.pending_enrichment_mutation;
                let pending_grouping = (failed_purpose != QueryPurpose::Grouping
                    && pending_at_or_before(&state.grouping, completion.revision))
                .then(|| state.grouping.pending_value.clone())
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
                let pending_time_basis = state
                    .pending_time
                    .as_ref()
                    .filter(|pending| pending.revision <= completion.revision)
                    .map(|pending| pending.basis);
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
                state.desired_time_basis = state.applied_time_basis;
                if let Some(value) = &pending_search {
                    state.desired_constraints.text = nonempty_text(value);
                }
                if let Some(value) = &pending_advanced {
                    state.desired_constraints.advanced_polars = nonempty(value);
                }
                if let Some(value) = &pending_enrichment {
                    state.desired_constraints.enrichments = value.clone();
                }
                if let Some(value) = &pending_grouping {
                    state.desired_constraints.grouping = nonempty(value);
                }
                if let Some(value) = pending_time {
                    state.desired_constraints.capture_time = value;
                }
                if let Some(policy) = pending_time_policy {
                    state.desired_capture_time_policy = policy;
                }
                if let Some(basis) = pending_time_basis {
                    state.desired_time_basis = basis;
                    state.desired_constraints.time_basis = basis;
                }
                let counterpart = pending_grouping
                    .map(|value| (QueryPurpose::Grouping, value))
                    .or_else(|| pending_advanced.map(|value| (QueryPurpose::Advanced, value)))
                    .or_else(|| pending_search.map(|value| (QueryPurpose::Search, value)));
                let restore_enrichment = counterpart.is_none()
                    && pending_enrichment.is_none()
                    && failure.purpose == QueryPurpose::Enrichment
                    && !state.enrichments.is_empty();
                let restore_applied = counterpart
                    .is_none()
                    .then(|| match failure.purpose {
                        QueryPurpose::Advanced if !state.search.applied.is_empty() => {
                            Some((QueryPurpose::Search, state.search.applied.clone()))
                        }
                        QueryPurpose::Enrichment => None,
                        QueryPurpose::Grouping if !state.grouping.applied.is_empty() => {
                            Some((QueryPurpose::Grouping, state.grouping.applied.clone()))
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
                let rebase = if let Some(value) = pending_enrichment {
                    self.enqueue_enrichment_chain(
                        &completion.view_id,
                        value,
                        pending_enrichment_value,
                        pending_enrichment_mutation.unwrap_or(PendingEnrichmentMutation::Edit),
                    )
                } else if let Some((purpose, value)) = counterpart {
                    // The older counterpart was never allowed to publish. Rebase it
                    // on the last accepted constraint and give it a fresh revision.
                    self.enqueue_query_value(&completion.view_id, purpose, Some(value))
                } else if restore_enrichment {
                    let stages = self
                        .view_states
                        .get(&completion.view_id)
                        .map_or_else(Vec::new, |state| state.enrichments.clone());
                    self.enqueue_enrichment_chain(
                        &completion.view_id,
                        stages,
                        String::new(),
                        PendingEnrichmentMutation::Reaffirm,
                    )
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

    fn handle_bookmark(&mut self, action: Action) {
        if action == Action::ToggleBookmark && matches!(self.focus, Focus::Logs | Focus::Selector) {
            let message = if let Some(state) = self.view_state_mut()
                && let Some(id) = state.selected.clone()
            {
                if let Some(index) = state
                    .bookmarks
                    .iter()
                    .position(|bookmark| bookmark.id == id)
                {
                    state.bookmarks.remove(index);
                    state.user_interaction_revision =
                        state.user_interaction_revision.saturating_add(1);
                    "bookmark removed"
                } else if state.bookmarks.len() < MAX_BOOKMARKS {
                    state.bookmarks.push(Bookmark {
                        id,
                        note: String::new(),
                    });
                    state.user_interaction_revision =
                        state.user_interaction_revision.saturating_add(1);
                    "bookmarked; B opens bookmarks and notes"
                } else {
                    "bookmark limit reached (128 per view)"
                }
            } else {
                "select a record to bookmark"
            };
            self.action_notice = Some(message.into());
            return;
        }
        if action == Action::OpenBookmarks && matches!(self.focus, Focus::Logs | Focus::Selector) {
            if let Some(view_id) = self.active_view_id() {
                self.bookmark_dialog = Some(BookmarkDialogState {
                    view_id: view_id.to_owned(),
                    selected: 0,
                    editing: None,
                    draft: String::new(),
                    status: String::new(),
                });
                self.focus = Focus::Bookmarks;
            }
            return;
        }
        if self.focus != Focus::Bookmarks {
            return;
        }
        let Some(dialog) = &mut self.bookmark_dialog else {
            return;
        };
        let Some(state) = self.view_states.get_mut(&dialog.view_id) else {
            return;
        };
        dialog.selected = dialog.selected.min(state.bookmarks.len().saturating_sub(1));
        match action {
            Action::MoveBookmark(delta) if dialog.editing.is_none() => {
                dialog.selected = dialog
                    .selected
                    .saturating_add_signed(delta as isize)
                    .min(state.bookmarks.len().saturating_sub(1));
            }
            Action::SelectBookmark(index) if dialog.editing.is_none() => {
                dialog.selected = index.min(state.bookmarks.len().saturating_sub(1));
            }
            Action::EditBookmarkNote if dialog.editing.is_none() => {
                if let Some(bookmark) = state.bookmarks.get(dialog.selected) {
                    dialog.editing = Some(bookmark.id.clone());
                    dialog.draft = bookmark.note.clone();
                    dialog.status.clear();
                    state.user_interaction_revision =
                        state.user_interaction_revision.saturating_add(1);
                }
            }
            Action::BookmarkInput(ch) if dialog.editing.is_some() => {
                if !ch.is_control()
                    && dialog.draft.len().saturating_add(ch.len_utf8()) <= MAX_BOOKMARK_NOTE_BYTES
                {
                    dialog.draft.push(ch);
                    state.user_interaction_revision =
                        state.user_interaction_revision.saturating_add(1);
                } else {
                    dialog.status = "note limit: 1024 bytes, single line".into();
                }
            }
            Action::BookmarkBackspace if dialog.editing.is_some() => {
                dialog.draft.pop();
                state.user_interaction_revision = state.user_interaction_revision.saturating_add(1);
            }
            Action::SubmitBookmark => {
                if let Some(id) = dialog.editing.take() {
                    if let Some(bookmark) = state
                        .bookmarks
                        .iter_mut()
                        .find(|bookmark| bookmark.id == id)
                    {
                        bookmark.note = std::mem::take(&mut dialog.draft);
                        state.user_interaction_revision =
                            state.user_interaction_revision.saturating_add(1);
                        dialog.status = "note updated; workspace autosave pending".into();
                    }
                } else if let Some(bookmark) = state.bookmarks.get(dialog.selected) {
                    self.context_dialog = Some(ContextDialogState {
                        view_id: dialog.view_id.clone(),
                        anchor: bookmark.id.clone(),
                        offset: -5,
                        return_focus: Focus::Bookmarks,
                    });
                    self.focus = Focus::Context;
                }
            }
            Action::DeleteBookmark
                if dialog.editing.is_none() && dialog.selected < state.bookmarks.len() =>
            {
                state.bookmarks.remove(dialog.selected);
                dialog.selected = dialog.selected.min(state.bookmarks.len().saturating_sub(1));
                state.user_interaction_revision = state.user_interaction_revision.saturating_add(1);
                dialog.status = "bookmark removed".into();
            }
            _ => {}
        }
    }

    pub fn handle<P: RowProvider>(&mut self, action: Action, provider: &P) {
        if !matches!(action, Action::Resize(..)) {
            self.action_notice = None;
        }
        if self.is_text_editing() {
            // Character actions need owned storage for the borrowed edit command.
            let character = match &action {
                Action::EditorInput(ch)
                | Action::CommandEnrichmentInput(ch)
                | Action::SettingsInput(ch)
                | Action::RecipeInput(ch)
                | Action::ViewInput(ch)
                | Action::SourceInput(ch)
                | Action::BookmarkInput(ch)
                | Action::TimeInput(ch) => Some(ch.to_string()),
                _ => None,
            };
            let edit_command = character
                .as_deref()
                .map(EditCommand::Insert)
                .or(match &action {
                    Action::EditorBackspace
                    | Action::CommandEnrichmentBackspace
                    | Action::SettingsBackspace
                    | Action::RecipeBackspace
                    | Action::ViewBackspace
                    | Action::SourceBackspace
                    | Action::BookmarkBackspace
                    | Action::TimeBackspace => Some(EditCommand::Backspace),
                    Action::EditorPaste(text) if self.focus != Focus::TimeEditor => {
                        Some(EditCommand::Insert(text))
                    }
                    Action::TextStartOfLine => Some(EditCommand::StartOfLine),
                    Action::TextEndOfLine => Some(EditCommand::EndOfLine),
                    Action::TextKillToEndOfLine => Some(EditCommand::KillToEndOfLine),
                    Action::TextMoveLeft => Some(EditCommand::MoveLeft),
                    Action::TextMoveRight => Some(EditCommand::MoveRight),
                    Action::TextMoveUp => Some(EditCommand::MoveUp),
                    Action::TextMoveDown => Some(EditCommand::MoveDown),
                    _ => None,
                });
            if let Some(command) = edit_command {
                self.apply_text_command(command);
                return;
            }
        } else if matches!(
            action,
            Action::TextStartOfLine
                | Action::TextEndOfLine
                | Action::TextKillToEndOfLine
                | Action::TextMoveLeft
                | Action::TextMoveRight
                | Action::TextMoveUp
                | Action::TextMoveDown
        ) {
            return;
        }
        match action {
            Action::Quit => self.should_quit = true,
            Action::CycleFocus => {
                self.focus = match self.focus {
                    Focus::Selector => Focus::Logs,
                    Focus::Logs if self.show_details => Focus::Details,
                    Focus::Details if !self.views.is_empty() => Focus::Selector,
                    Focus::Logs if !self.views.is_empty() => Focus::Selector,
                    Focus::Logs
                    | Focus::Details
                    | Focus::SearchEditor
                    | Focus::AdvancedEditor
                    | Focus::EnrichmentEditor
                    | Focus::CommandEnrichment
                    | Focus::GroupingEditor
                    | Focus::SourceDialog
                    | Focus::Help
                    | Focus::ViewDialog
                    | Focus::FieldPicker
                    | Focus::AskAi
                    | Focus::Investigation
                    | Focus::Storage
                    | Focus::Settings => Focus::Logs,
                    Focus::Recipes | Focus::TimeEditor | Focus::Context | Focus::Bookmarks => {
                        Focus::Logs
                    }
                }
            }
            Action::NextView | Action::SelectSidebar(1) => self.switch_view(1, provider),
            Action::PreviousView | Action::SelectSidebar(-1) => self.switch_view(-1, provider),
            Action::SelectSidebar(_) => {}
            Action::MoveLine(delta) => self.move_selection(delta, provider),
            Action::MoveHorizontal(delta) => {
                if let Some(state) = self.view_state_mut() {
                    state.horizontal_offset = state
                        .horizontal_offset
                        .saturating_add_signed(delta as isize)
                        .min(64 * 1024);
                }
            }
            Action::ResetHorizontal => {
                if let Some(state) = self.view_state_mut() {
                    state.horizontal_offset = 0;
                }
            }
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
            Action::ToggleBookmark
            | Action::OpenBookmarks
            | Action::MoveBookmark(_)
            | Action::SelectBookmark(_)
            | Action::EditBookmarkNote
            | Action::BookmarkInput(_)
            | Action::BookmarkBackspace
            | Action::SubmitBookmark
            | Action::DeleteBookmark => self.handle_bookmark(action),
            Action::OpenContext
                if matches!(
                    self.focus,
                    Focus::Logs | Focus::Selector | Focus::FieldPicker
                ) =>
            {
                let return_focus = if self.focus == Focus::FieldPicker {
                    Focus::FieldPicker
                } else {
                    Focus::Logs
                };
                let anchor = self.view_state().and_then(|state| {
                    if self.focus == Focus::FieldPicker {
                        state.field_picker_row.clone()
                    } else {
                        state.selected.clone()
                    }
                });
                if let Some((view_id, anchor)) = self.active_view_id().zip(anchor) {
                    self.context_dialog = Some(ContextDialogState {
                        view_id: view_id.to_owned(),
                        anchor,
                        offset: -5,
                        return_focus,
                    });
                    self.focus = Focus::Context;
                }
            }
            Action::OpenContext => {}
            Action::MoveContext(delta) if self.focus == Focus::Context => {
                if let Some(dialog) = &mut self.context_dialog {
                    if delta == 0 {
                        dialog.offset = -5;
                    } else {
                        dialog.offset = dialog
                            .offset
                            .saturating_add(delta)
                            .clamp(-1_000_000, 1_000_000);
                    }
                }
            }
            Action::MoveContext(_) => {}
            Action::ToggleDetails => {
                self.show_details = !self.show_details;
                if self.show_details {
                    self.focus = Focus::Details;
                } else if self.focus == Focus::Details {
                    self.focus = Focus::Logs;
                }
            }
            Action::ScrollDetails(delta) => {
                if let Some(state) = self.view_state_mut() {
                    state.details_scroll = if delta == i32::MIN {
                        0
                    } else if delta == i32::MAX {
                        state.details_scroll_limit
                    } else {
                        state
                            .details_scroll
                            .saturating_add_signed(delta as isize)
                            .min(state.details_scroll_limit)
                    };
                }
            }
            Action::ResetDetails => {
                if let Some(state) = self.view_state_mut() {
                    state.details_scroll = 0;
                }
            }
            Action::ToggleHelp => {
                if self.show_help {
                    self.show_help = false;
                    self.help_scroll = 0;
                    self.focus = self.help_return_focus;
                } else {
                    self.help_return_focus = self.focus;
                    self.help_scroll = 0;
                    self.show_help = true;
                    self.focus = Focus::Help;
                }
            }
            Action::ScrollHelp(delta) if self.show_help => {
                self.help_scroll = if delta == i32::MIN {
                    0
                } else if delta == i32::MAX {
                    self.help_scroll_limit
                } else {
                    self.help_scroll
                        .saturating_add_signed(delta as isize)
                        .min(self.help_scroll_limit)
                };
            }
            Action::ScrollHelp(_) => {}
            Action::ScrollDialog(delta) => {
                self.dialog_scroll = self
                    .dialog_scroll
                    .saturating_add_signed(delta as isize)
                    .min(self.dialog_scroll_limit);
            }
            Action::ScrollDiscoveryStatus(delta) if self.focus == Focus::SourceDialog => {
                if let Some(dialog) = &mut self.source_dialog {
                    dialog.discovery.status_scroll = dialog
                        .discovery
                        .status_scroll
                        .saturating_add_signed(delta as isize)
                        .min(dialog.discovery.status_scroll_limit);
                }
            }
            Action::ScrollDiscoveryStatus(_) => {}
            Action::ToggleDialogScrollFocus => {
                self.dialog_scroll_focused = !self.dialog_scroll_focused;
            }
            Action::ModalVertical(delta) => match self.focus {
                Focus::CommandEnrichment => {
                    let editing = self
                        .command_enrichment_dialog
                        .as_ref()
                        .is_some_and(|dialog| {
                            matches!(
                                dialog.run_state,
                                CommandEnrichmentRunState::Unrun | CommandEnrichmentRunState::Error
                            )
                        });
                    if !editing || self.dialog_scroll_focused {
                        self.handle(Action::ScrollDialog(delta), provider);
                    }
                }
                Focus::AskAi => {
                    let editing = self.ask_ai_dialog.as_ref().is_some_and(|dialog| {
                        matches!(dialog.stage, AskAiStage::Input | AskAiStage::Error)
                    });
                    if !editing || self.dialog_scroll_focused {
                        self.handle(Action::ScrollAskAi(delta), provider);
                    }
                }
                Focus::Storage => {
                    if self.dialog_scroll_focused {
                        self.handle(Action::ScrollDialog(delta), provider);
                    } else {
                        self.handle(Action::MoveStorage(delta), provider);
                    }
                }
                Focus::SourceDialog => {
                    match self.source_dialog.as_ref().map(|dialog| dialog.mode) {
                        Some(SourceDialogMode::Discovery) if self.dialog_scroll_focused => {
                            self.handle(Action::ScrollDiscoveryStatus(delta), provider);
                        }
                        Some(SourceDialogMode::Discovery) => {
                            self.handle(Action::MoveDiscovery(delta), provider);
                        }
                        Some(SourceDialogMode::Manual) => {
                            self.handle(Action::MovePathCompletion(delta), provider);
                        }
                        Some(SourceDialogMode::Ai) | None => {}
                    }
                }
                Focus::SearchEditor
                | Focus::AdvancedEditor
                | Focus::EnrichmentEditor
                | Focus::GroupingEditor => {
                    if self.dialog_scroll_focused {
                        self.handle(Action::ScrollDialog(delta), provider);
                    } else if self.editor_completion.is_some() {
                        self.handle(Action::MoveEditorCompletion(delta), provider);
                    }
                }
                _ => self.handle(Action::ScrollDialog(delta), provider),
            },
            Action::ScrollHoveredDialog(delta) => match self.focus {
                Focus::AskAi => self.handle(Action::ScrollAskAi(delta), provider),
                Focus::SourceDialog => self.handle(Action::ScrollDiscoveryStatus(delta), provider),
                _ => self.handle(Action::ScrollDialog(delta), provider),
            },
            Action::ToggleFollow => self.toggle_follow(provider),
            Action::OpenSearch => {
                if self.active_view_id().is_some() {
                    self.dialog_scroll = 0;
                    self.dialog_scroll_focused = false;
                    self.focus = Focus::SearchEditor;
                }
            }
            Action::OpenAdvanced => {
                if self.active_view_id().is_some() {
                    self.dialog_scroll = 0;
                    self.dialog_scroll_focused = false;
                    self.focus = Focus::AdvancedEditor;
                }
            }
            Action::OpenEnrichment => {
                if let Some(state) = self.view_state_mut() {
                    if state.enrichment.draft.is_empty() {
                        state.enrichment_editing = None;
                    }
                    self.focus = Focus::EnrichmentEditor;
                }
            }
            Action::OpenCommandEnrichment => {
                if let Some(view_id) = self.active_view_id().map(str::to_owned) {
                    let (accepted, revision) =
                        self.view_states.get(&view_id).map_or((None, 0), |state| {
                            (
                                state.command_enrichment.clone(),
                                state.command_enrichment_revision,
                            )
                        });
                    let (program, arguments, cwd, environment) = accepted.as_ref().map_or_else(
                        || (String::new(), String::new(), String::new(), String::new()),
                        command_stage_draft,
                    );
                    let generation = self.next_command_enrichment_generation;
                    self.next_command_enrichment_generation = generation.saturating_add(1);
                    self.command_enrichment_dialog = Some(CommandEnrichmentDialogState {
                        generation,
                        view_id,
                        base_definition_revision: revision,
                        selected_field: CommandEnrichmentField::Program,
                        selected_control: CommandEnrichmentControl::Field,
                        program,
                        arguments,
                        cwd,
                        environment,
                        accepted,
                        error: None,
                        run_state: CommandEnrichmentRunState::Unrun,
                        run_status: "Unrun · new records wait for an explicit run".into(),
                        review: None,
                    });
                    self.dialog_scroll = 0;
                    self.dialog_scroll_focused = false;
                    self.focus = Focus::CommandEnrichment;
                }
            }
            Action::CommandEnrichmentNextField if self.focus == Focus::CommandEnrichment => {
                if let Some(dialog) = &mut self.command_enrichment_dialog {
                    match dialog.selected_control {
                        CommandEnrichmentControl::Field => {
                            let index = CommandEnrichmentField::ALL
                                .iter()
                                .position(|field| *field == dialog.selected_field)
                                .unwrap_or(0);
                            if index + 1 < CommandEnrichmentField::ALL.len() {
                                dialog.selected_field = CommandEnrichmentField::ALL[index + 1];
                            } else {
                                dialog.selected_control = CommandEnrichmentControl::NewLine;
                            }
                        }
                        CommandEnrichmentControl::NewLine => {
                            dialog.selected_control = CommandEnrichmentControl::Save;
                        }
                        CommandEnrichmentControl::Save => {
                            dialog.selected_control = CommandEnrichmentControl::Review;
                        }
                        CommandEnrichmentControl::Review => {
                            dialog.selected_control = CommandEnrichmentControl::Remove;
                        }
                        CommandEnrichmentControl::Remove => {
                            dialog.selected_control = CommandEnrichmentControl::Field;
                            dialog.selected_field = CommandEnrichmentField::Program;
                        }
                    }
                }
            }
            Action::FocusCommandEnrichmentControl(control)
                if self.focus == Focus::CommandEnrichment =>
            {
                if let Some(dialog) = &mut self.command_enrichment_dialog {
                    dialog.selected_control = control;
                }
            }
            Action::ActivateCommandEnrichmentControl if self.focus == Focus::CommandEnrichment => {
                match self
                    .command_enrichment_dialog
                    .as_ref()
                    .map(|dialog| dialog.selected_control)
                {
                    Some(CommandEnrichmentControl::Field) => {
                        self.handle(Action::ConfirmCommandEnrichmentRun, provider)
                    }
                    Some(CommandEnrichmentControl::NewLine) => {
                        if let Some(dialog) = &mut self.command_enrichment_dialog {
                            dialog.selected_control = CommandEnrichmentControl::Field;
                        }
                        self.handle(Action::CommandEnrichmentInput('\n'), provider);
                        if let Some(dialog) = &mut self.command_enrichment_dialog {
                            dialog.selected_control = CommandEnrichmentControl::NewLine;
                        }
                    }
                    Some(CommandEnrichmentControl::Save) => {
                        self.handle(Action::SaveCommandEnrichment, provider)
                    }
                    Some(CommandEnrichmentControl::Review) => {
                        self.handle(Action::PrepareCommandEnrichmentRun, provider)
                    }
                    Some(CommandEnrichmentControl::Remove) => {
                        self.handle(Action::RemoveCommandEnrichment, provider)
                    }
                    None => {}
                }
            }
            Action::CommandEnrichmentInput(ch) if self.focus == Focus::CommandEnrichment => {
                if self.dialog_scroll_focused {
                    return;
                }
                let mut edited_view = None;
                if (!ch.is_control() || ch == '\n')
                    && let Some(dialog) = &mut self.command_enrichment_dialog
                {
                    if dialog.run_state == CommandEnrichmentRunState::SavingResults {
                        return;
                    }
                    if matches!(
                        dialog.run_state,
                        CommandEnrichmentRunState::Saving
                            | CommandEnrichmentRunState::Preparing
                            | CommandEnrichmentRunState::Running
                    ) {
                        dialog.error = Some("Wait for the current operation before editing".into());
                        return;
                    }
                    if ch == '\n'
                        && !matches!(
                            dialog.selected_field,
                            CommandEnrichmentField::Arguments | CommandEnrichmentField::Environment
                        )
                    {
                        return;
                    }
                    let field = command_draft_field_mut(dialog);
                    if field.len().saturating_add(ch.len_utf8()) <= MAX_COMMAND_FIELD_BYTES {
                        field.push(ch);
                        dialog.error = None;
                        dialog.review = None;
                        dialog.run_state = CommandEnrichmentRunState::Unrun;
                        dialog.run_status = "Draft changed · save before reviewing a run".into();
                        edited_view = Some(dialog.view_id.clone());
                    }
                }
                if let Some(view_id) = edited_view
                    && let Some(state) = self.view_states.get_mut(&view_id)
                {
                    state.user_interaction_revision =
                        state.user_interaction_revision.saturating_add(1);
                }
            }
            Action::CommandEnrichmentBackspace if self.focus == Focus::CommandEnrichment => {
                if self.dialog_scroll_focused {
                    return;
                }
                let mut edited_view = None;
                if let Some(dialog) = &mut self.command_enrichment_dialog {
                    if dialog.run_state == CommandEnrichmentRunState::SavingResults {
                        return;
                    }
                    if matches!(
                        dialog.run_state,
                        CommandEnrichmentRunState::Saving
                            | CommandEnrichmentRunState::Preparing
                            | CommandEnrichmentRunState::Running
                    ) {
                        dialog.error = Some("Wait for the current operation before editing".into());
                        return;
                    }
                    if command_draft_field_mut(dialog).pop().is_some() {
                        edited_view = Some(dialog.view_id.clone());
                    }
                    dialog.error = None;
                    dialog.review = None;
                    dialog.run_state = CommandEnrichmentRunState::Unrun;
                    dialog.run_status = "Draft changed · save before reviewing a run".into();
                }
                if let Some(view_id) = edited_view
                    && let Some(state) = self.view_states.get_mut(&view_id)
                {
                    state.user_interaction_revision =
                        state.user_interaction_revision.saturating_add(1);
                }
            }
            Action::SaveCommandEnrichment if self.focus == Focus::CommandEnrichment => {
                let Some(mut dialog) = self.command_enrichment_dialog.take() else {
                    return;
                };
                if dialog.run_state == CommandEnrichmentRunState::SavingResults {
                    self.command_enrichment_dialog = Some(dialog);
                    return;
                }
                if matches!(
                    dialog.run_state,
                    CommandEnrichmentRunState::Saving
                        | CommandEnrichmentRunState::Preparing
                        | CommandEnrichmentRunState::Running
                ) {
                    dialog.error = Some("Wait for the current operation before editing".into());
                    self.command_enrichment_dialog = Some(dialog);
                    return;
                }
                if self.command_request_count() >= MAX_COMMAND_REQUESTS {
                    dialog.error = Some(
                        "Command request queue is full; wait for the current operation".into(),
                    );
                    self.command_enrichment_dialog = Some(dialog);
                    return;
                }
                match command_candidate(&dialog) {
                    Ok(candidate) => {
                        let generation = self.next_command_enrichment_generation;
                        self.next_command_enrichment_generation = generation.saturating_add(1);
                        dialog.generation = generation;
                        self.pending_command_enrichment_saves.insert(
                            generation,
                            (dialog.view_id.clone(), dialog.base_definition_revision),
                        );
                        self.command_enrichment_requests.push_back(
                            CommandEnrichmentRequest::Save {
                                generation,
                                view_id: dialog.view_id.clone(),
                                base_definition_revision: dialog.base_definition_revision,
                                candidate: Some(candidate),
                            },
                        );
                        dialog.error = None;
                        dialog.run_status = "Saving definition…".into();
                        dialog.run_state = CommandEnrichmentRunState::Saving;
                        dialog.review = None;
                        if let Some(state) = self.view_states.get_mut(&dialog.view_id) {
                            state.user_interaction_revision =
                                state.user_interaction_revision.saturating_add(1);
                        }
                    }
                    Err(error) => dialog.error = Some(error),
                }
                self.command_enrichment_dialog = Some(dialog);
            }
            Action::RemoveCommandEnrichment if self.focus == Focus::CommandEnrichment => {
                let request_queue_full = self.command_request_count() >= MAX_COMMAND_REQUESTS;
                if let Some(dialog) = &mut self.command_enrichment_dialog {
                    if dialog.run_state == CommandEnrichmentRunState::SavingResults {
                        return;
                    }
                    if matches!(
                        dialog.run_state,
                        CommandEnrichmentRunState::Saving
                            | CommandEnrichmentRunState::Preparing
                            | CommandEnrichmentRunState::Running
                    ) {
                        dialog.error = Some("Wait for the current operation before editing".into());
                        return;
                    }
                    if request_queue_full {
                        dialog.error = Some(
                            "Command request queue is full; wait for the current operation".into(),
                        );
                        return;
                    }
                    let generation = self.next_command_enrichment_generation;
                    self.next_command_enrichment_generation = generation.saturating_add(1);
                    dialog.generation = generation;
                    self.pending_command_enrichment_saves.insert(
                        generation,
                        (dialog.view_id.clone(), dialog.base_definition_revision),
                    );
                    self.command_enrichment_requests
                        .push_back(CommandEnrichmentRequest::Save {
                            generation,
                            view_id: dialog.view_id.clone(),
                            base_definition_revision: dialog.base_definition_revision,
                            candidate: None,
                        });
                    dialog.error = None;
                    dialog.run_status = "Removing definition…".into();
                    dialog.run_state = CommandEnrichmentRunState::Saving;
                    dialog.review = None;
                    if let Some(state) = self.view_states.get_mut(&dialog.view_id) {
                        state.user_interaction_revision =
                            state.user_interaction_revision.saturating_add(1);
                    }
                }
            }
            Action::PrepareCommandEnrichmentRun if self.focus == Focus::CommandEnrichment => {
                let request_queue_full = self.command_request_count() >= MAX_COMMAND_REQUESTS;
                if let Some(dialog) = &mut self.command_enrichment_dialog {
                    if dialog.run_state == CommandEnrichmentRunState::SavingResults {
                        return;
                    }
                    if matches!(
                        dialog.run_state,
                        CommandEnrichmentRunState::Saving
                            | CommandEnrichmentRunState::Preparing
                            | CommandEnrichmentRunState::Running
                    ) {
                        dialog.error = Some("Wait for the current operation before editing".into());
                        return;
                    }
                    if request_queue_full {
                        dialog.error = Some(
                            "Command request queue is full; wait for the current operation".into(),
                        );
                        return;
                    }
                    let candidate = match command_candidate(dialog) {
                        Ok(candidate) => candidate,
                        Err(error) => {
                            dialog.error = Some(error);
                            return;
                        }
                    };
                    if dialog.accepted.as_ref() != Some(&candidate) {
                        dialog.review = None;
                        dialog.run_state = CommandEnrichmentRunState::Unrun;
                        dialog.error = Some(
                            "Draft differs from the saved command; save it before reviewing a run"
                                .into(),
                        );
                        return;
                    }
                    if let Some(stage) = &dialog.accepted {
                        let generation = self.next_command_enrichment_generation;
                        self.next_command_enrichment_generation = generation.saturating_add(1);
                        dialog.generation = generation;
                        dialog.run_state = CommandEnrichmentRunState::Preparing;
                        dialog.run_status = "Preparing bounded review…".into();
                        dialog.review = None;
                        self.command_enrichment_requests.push_back(
                            CommandEnrichmentRequest::PrepareRun {
                                generation,
                                view_id: dialog.view_id.clone(),
                                definition_revision: dialog.base_definition_revision,
                                stage_id: stage.id.clone(),
                            },
                        );
                    } else {
                        dialog.error =
                            Some("Save a valid command step before preparing a run".into());
                    }
                }
            }
            Action::ConfirmCommandEnrichmentRun if self.focus == Focus::CommandEnrichment => {
                let request_queue_full = self.command_request_count() >= MAX_COMMAND_REQUESTS;
                if let Some(dialog) = &mut self.command_enrichment_dialog
                    && dialog.run_state == CommandEnrichmentRunState::Ready
                    && let Some(review) = dialog.review.take()
                {
                    if request_queue_full {
                        dialog.review = Some(review);
                        dialog.error = Some(
                            "Command request queue is full; reviewed run was not started".into(),
                        );
                        return;
                    }
                    dialog.run_state = CommandEnrichmentRunState::Running;
                    dialog.run_status = "Running reviewed records…".into();
                    self.pending_command_enrichment_runs.insert(
                        dialog.generation,
                        (dialog.view_id.clone(), dialog.base_definition_revision),
                    );
                    self.command_enrichment_requests
                        .push_back(CommandEnrichmentRequest::Execute {
                            generation: dialog.generation,
                            view_id: dialog.view_id.clone(),
                            definition_revision: dialog.base_definition_revision,
                            review_token: review.review_token,
                        });
                }
            }
            Action::AddEnrichment if self.focus == Focus::EnrichmentEditor => {
                let view_id = self.active_view_id().map(str::to_owned);
                if let Some(state) = self.view_state_mut() {
                    state.enrichment_editing = None;
                    state.enrichment.draft.clear();
                    state.enrichment.error = None;
                    state.enrichment_control = EnrichmentControl::Editor;
                }
                if let Some(view_id) = view_id {
                    self.text_cursors.reset(
                        TextTarget {
                            identity: view_id,
                            field: "enrichment",
                        },
                        "",
                    );
                }
            }
            Action::EditEnrichment if self.focus == Focus::EnrichmentEditor => {
                let view_id = self.active_view_id().map(str::to_owned);
                let mut replacement = None;
                if let Some(state) = self.view_state_mut()
                    && let Some(stage) = state.enrichments.get(state.enrichment_selected).cloned()
                {
                    state.enrichment_editing = Some(stage.id);
                    state.enrichment.draft = stage.source;
                    state.enrichment.error = None;
                    state.enrichment_control = EnrichmentControl::Editor;
                    replacement = Some(state.enrichment.draft.clone());
                }
                if let (Some(view_id), Some(replacement)) = (view_id, replacement) {
                    self.text_cursors.reset(
                        TextTarget {
                            identity: view_id,
                            field: "enrichment",
                        },
                        &replacement,
                    );
                }
            }
            Action::RemoveEnrichment if self.focus == Focus::EnrichmentEditor => {
                self.remove_selected_enrichment();
            }
            Action::MoveEnrichment(delta) if self.focus == Focus::EnrichmentEditor => {
                if let Some(state) = self.view_state_mut()
                    && !state.enrichments.is_empty()
                    && state.enrichment_control == EnrichmentControl::Steps
                {
                    state.enrichment_selected = (state.enrichment_selected as i32 + delta)
                        .rem_euclid(state.enrichments.len() as i32)
                        as usize;
                }
            }
            Action::MoveEnrichmentControl(delta) if self.focus == Focus::EnrichmentEditor => {
                if let Some(state) = self.view_state_mut() {
                    let index = EnrichmentControl::ALL
                        .iter()
                        .position(|control| *control == state.enrichment_control)
                        .unwrap_or(0);
                    state.enrichment_control = EnrichmentControl::ALL[(index as i32 + delta)
                        .rem_euclid(EnrichmentControl::ALL.len() as i32)
                        as usize];
                }
            }
            Action::FocusEnrichmentControl(control) if self.focus == Focus::EnrichmentEditor => {
                if let Some(state) = self.view_state_mut() {
                    state.enrichment_control = control;
                }
                match control {
                    EnrichmentControl::Add => self.handle(Action::AddEnrichment, provider),
                    EnrichmentControl::Edit => self.handle(Action::EditEnrichment, provider),
                    EnrichmentControl::Remove => self.handle(Action::RemoveEnrichment, provider),
                    EnrichmentControl::ExternalCommand => {
                        self.handle(Action::OpenCommandEnrichment, provider)
                    }
                    EnrichmentControl::Steps | EnrichmentControl::Editor => {}
                }
            }
            Action::ActivateEnrichmentControl if self.focus == Focus::EnrichmentEditor => {
                match self.view_state().map(|state| state.enrichment_control) {
                    Some(EnrichmentControl::Editor) => self.handle(Action::SubmitDraft, provider),
                    Some(EnrichmentControl::Steps) => {}
                    Some(control) => self.handle(Action::FocusEnrichmentControl(control), provider),
                    None => {}
                }
            }
            Action::OpenGrouping => {
                if let Some(state) = self.view_state_mut() {
                    if state.grouping.draft.is_empty() && state.grouping.applied.is_empty() {
                        state.grouping.draft = r"^(\s+|Caused by:)".into();
                    }
                    self.focus = Focus::GroupingEditor;
                }
            }
            Action::ToggleExpandedGroup => {
                let selected = self.view_state().and_then(|state| state.selected.clone());
                if let (Some(id), Some(state)) = (selected, self.view_state_mut()) {
                    if !state.expanded_groups.remove(&id) {
                        state.expanded_groups.insert(id);
                    }
                    state.user_interaction_revision =
                        state.user_interaction_revision.saturating_add(1);
                }
            }
            Action::OpenSettings => {
                if let Some(context) = self.settings_context.clone() {
                    let generation = self.next_settings_generation;
                    self.next_settings_generation = generation.saturating_add(1);
                    self.settings_dialog = Some(SettingsDialogState {
                        generation,
                        selected: 0,
                        focus: SettingsControl::Field(SettingsField::Provider),
                        draft: context.saved.clone(),
                        context,
                        saving: false,
                        status_kind: SettingsStatus::Saved,
                        status: "Saved settings loaded; cache-limit changes apply after restart"
                            .into(),
                        theme_dropdown: false,
                        theme_selected: 0,
                        details_scroll: 0,
                        details_scroll_limit: 0,
                    });
                    self.focus = Focus::Settings;
                } else {
                    self.source_notice = Some("settings are unavailable in this build".into());
                }
            }
            Action::MoveSettings(delta) if self.focus == Focus::Settings => {
                if let Some(dialog) = &mut self.settings_dialog {
                    let controls = settings_controls(dialog);
                    let index = controls
                        .iter()
                        .position(|control| *control == dialog.focus)
                        .unwrap_or(0);
                    dialog.focus =
                        controls[(index as i32 + delta).rem_euclid(controls.len() as i32) as usize];
                    if let SettingsControl::Field(field) = dialog.focus {
                        dialog.selected = SettingsField::ALL
                            .iter()
                            .position(|candidate| *candidate == field)
                            .unwrap_or(0);
                    }
                }
            }
            Action::FocusSettings(control) if self.focus == Focus::Settings => {
                if let Some(dialog) = &mut self.settings_dialog
                    && (control != SettingsControl::More || dialog.details_scroll_limit > 0)
                {
                    dialog.focus = control;
                    if let SettingsControl::Field(field) = control {
                        dialog.selected = SettingsField::ALL
                            .iter()
                            .position(|candidate| *candidate == field)
                            .unwrap_or(0);
                    }
                }
            }
            Action::CycleSetting if self.focus == Focus::Settings => {
                if let Some(dialog) = &mut self.settings_dialog {
                    let SettingsControl::Field(field) = dialog.focus else {
                        return;
                    };
                    match field {
                        SettingsField::Theme => {
                            dialog.theme_selected = ThemeId::ALL
                                .iter()
                                .position(|theme| *theme == dialog.draft.theme)
                                .unwrap_or(0);
                            dialog.theme_dropdown = true;
                            return;
                        }
                        SettingsField::Delight => {
                            dialog.draft.delight_enabled = !dialog.draft.delight_enabled
                        }
                        SettingsField::ReducedMotion => {
                            dialog.draft.reduced_motion = !dialog.draft.reduced_motion
                        }
                        SettingsField::Ascii => dialog.draft.ascii = !dialog.draft.ascii,
                        _ => {}
                    }
                    mark_settings_pending(dialog);
                    self.theme_id = dialog.draft.theme;
                    self.delight_enabled = dialog.draft.delight_enabled;
                    self.reduced_motion = dialog.draft.reduced_motion;
                    self.ascii = dialog.draft.ascii;
                }
            }
            Action::ActivateSettings if self.focus == Focus::Settings => {
                match self.settings_dialog.as_ref().map(|dialog| dialog.focus) {
                    Some(SettingsControl::Field(SettingsField::Theme)) => {
                        self.handle(Action::CycleSetting, provider)
                    }
                    Some(SettingsControl::Field(
                        SettingsField::Delight
                        | SettingsField::ReducedMotion
                        | SettingsField::Ascii,
                    )) => self.handle(Action::CycleSetting, provider),
                    Some(SettingsControl::Save) => self.handle(Action::SaveSettings, provider),
                    Some(SettingsControl::More) => {}
                    Some(SettingsControl::Field(_)) | None => {}
                }
            }
            Action::MoveSettingsTheme(delta) if self.focus == Focus::Settings => {
                if let Some(dialog) = &mut self.settings_dialog
                    && dialog.theme_dropdown
                {
                    dialog.theme_selected = (dialog.theme_selected as i32 + delta)
                        .rem_euclid(ThemeId::ALL.len() as i32)
                        as usize;
                }
            }
            Action::ChooseSettingsTheme(index) if self.focus == Focus::Settings => {
                if let Some(dialog) = &mut self.settings_dialog
                    && dialog.theme_dropdown
                    && let Some(theme) = ThemeId::ALL.get(index).copied()
                {
                    dialog.draft.theme = theme;
                    dialog.theme_selected = index;
                    dialog.theme_dropdown = false;
                    mark_settings_pending(dialog);
                    self.theme_id = theme;
                }
            }
            Action::CloseSettingsTheme if self.focus == Focus::Settings => {
                if let Some(dialog) = &mut self.settings_dialog {
                    dialog.theme_dropdown = false;
                }
            }
            Action::ScrollSettingsDetails(delta) if self.focus == Focus::Settings => {
                if let Some(dialog) = &mut self.settings_dialog
                    && dialog.focus == SettingsControl::More
                {
                    dialog.details_scroll = dialog
                        .details_scroll
                        .saturating_add_signed(delta as isize)
                        .min(dialog.details_scroll_limit);
                }
            }
            Action::SettingsInput(character) if self.focus == Focus::Settings => {
                edit_setting(self.settings_dialog.as_mut(), |value| {
                    if value.len() + character.len_utf8() <= 256 {
                        value.push(character);
                    }
                });
            }
            Action::SettingsBackspace if self.focus == Focus::Settings => {
                edit_setting(self.settings_dialog.as_mut(), |value| {
                    value.pop();
                });
            }
            Action::SaveSettings if self.focus == Focus::Settings => {
                if let Some(dialog) = &mut self.settings_dialog {
                    if dialog.saving {
                        dialog.status_kind = SettingsStatus::Pending;
                        dialog.status = "settings save already pending".into();
                    } else if self.settings_requests.len() >= 2 {
                        dialog.status_kind = SettingsStatus::Error;
                        dialog.status = "settings save queue is full; retry shortly".into();
                    } else {
                        dialog.saving = true;
                        dialog.status_kind = SettingsStatus::Pending;
                        dialog.status = "saving global settings…".into();
                        self.settings_requests.push_back(SettingsRequest {
                            generation: dialog.generation,
                            values: dialog.draft.clone(),
                        });
                    }
                }
            }
            Action::OpenStorage => {
                self.dialog_scroll = 0;
                self.dialog_scroll_focused = false;
                if let Some(previous) = &self.storage_dialog
                    && previous.scanning
                {
                    self.storage_requests.push_back(StorageRequest {
                        generation: previous.generation,
                        kind: StorageRequestKind::Cancel,
                    });
                }
                let generation = self.next_storage_generation;
                self.next_storage_generation = generation.saturating_add(1);
                self.storage_dialog = Some(StorageDialogState {
                    generation,
                    snapshot: StorageSnapshot::default(),
                    selected: 0,
                    scanning: true,
                    confirm_clear: false,
                    status: "scanning application-owned storage…".into(),
                });
                self.storage_requests.push_back(StorageRequest {
                    generation,
                    kind: StorageRequestKind::Scan,
                });
                self.focus = Focus::Storage;
            }
            Action::RefreshStorage if self.focus == Focus::Storage => {
                if let Some(dialog) = &mut self.storage_dialog {
                    if dialog.scanning {
                        self.storage_requests.push_back(StorageRequest {
                            generation: dialog.generation,
                            kind: StorageRequestKind::Cancel,
                        });
                    }
                    let generation = self.next_storage_generation;
                    self.next_storage_generation = generation.saturating_add(1);
                    dialog.generation = generation;
                    dialog.scanning = true;
                    dialog.confirm_clear = false;
                    dialog.status = "refreshing storage usage…".into();
                    self.storage_requests.push_back(StorageRequest {
                        generation,
                        kind: StorageRequestKind::Scan,
                    });
                }
            }
            Action::ClearStorage if self.focus == Focus::Storage => {
                if let Some(dialog) = &mut self.storage_dialog {
                    if dialog.scanning || dialog.snapshot.reclaimable_bytes == 0 {
                        dialog.status = if dialog.scanning {
                            "wait for the current storage scan".into()
                        } else {
                            "no unused derived indexes are reclaimable".into()
                        };
                    } else if !dialog.confirm_clear {
                        dialog.confirm_clear = true;
                        dialog.status = format!(
                            "clear {} of unused recomputable derived indexes? press c again",
                            format_storage_bytes(dialog.snapshot.reclaimable_bytes)
                        );
                    } else {
                        dialog.scanning = true;
                        dialog.confirm_clear = false;
                        dialog.status = "clearing unused derived indexes…".into();
                        self.storage_requests.push_back(StorageRequest {
                            generation: dialog.generation,
                            kind: StorageRequestKind::ClearUnusedDerived,
                        });
                    }
                }
            }
            Action::MoveStorage(delta) if self.focus == Focus::Storage => {
                if let Some(dialog) = &mut self.storage_dialog
                    && !dialog.snapshot.entries.is_empty()
                {
                    dialog.selected = (dialog.selected as i32 + delta)
                        .rem_euclid(dialog.snapshot.entries.len() as i32)
                        as usize;
                    dialog.confirm_clear = false;
                }
            }
            Action::OpenTimestampAssistant => {
                if self.focus == Focus::AskAi
                    && self.ask_ai_dialog.as_ref().is_some_and(|dialog| {
                        !matches!(dialog.stage, AskAiStage::Input | AskAiStage::Error)
                    })
                {
                    return;
                }
                self.handle(Action::OpenAskAi, provider);
                if let Some(dialog) = &mut self.ask_ai_dialog {
                    dialog.kind = AskAiKind::Enrichment;
                    dialog.prompt = TIMESTAMP_PROMPT.into();
                    dialog.progress = "Timestamp → UTC RFC3339; edit instructions, then Enter to request a proposal".into();
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
                        recipe: None,
                        review_scroll: 0,
                        review_scroll_limit: 0,
                        recipe_outcome: None,
                    });
                    self.focus = Focus::AskAi;
                    self.dialog_scroll_focused = false;
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
                            "type a new question, or leave blank to resume the selected investigation"
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
                        AskAiKind::Recipe => "Describe how to adapt the suggested recipe",
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
                        dialog.progress = "agent request queue is full".into();
                    } else {
                        let instruction = if let Some(recipe) = &dialog.recipe {
                            let stages = recipe
                                .enrichments
                                .iter()
                                .map(|stage| {
                                    format!("id={:?} source={:?}", stage.id.0, stage.source)
                                })
                                .collect::<Vec<_>>()
                                .join("\n");
                            format!(
                                "{}\nReviewed advanced filter: {:?}\nReviewed ordered enrichment chain:\n{}\nLegacy enrichment: {:?}\nAdapt the advanced filter and, if needed, the complete ordered enrichment chain. Preserve all other settings. Return empty recipe_stage_revisions.",
                                dialog.prompt, recipe.advanced, stages, recipe.enrichment
                            )
                        } else {
                            dialog.prompt.clone()
                        };
                        if instruction.len() > 131_072 {
                            dialog.stage = AskAiStage::Error;
                            dialog.progress =
                                "recipe context exceeds the 128 KiB proposal limit".into();
                            return;
                        }
                        dialog.review_scroll = 0;
                        dialog.review_scroll_limit = 0;
                        dialog.stage = AskAiStage::Snapshot;
                        dialog.progress = "freezing applied view snapshot".into();
                        dialog.expression = None;
                        dialog.explanation = None;
                        self.ask_ai_requests.push_back(AskAiRequest::Start {
                            generation: dialog.generation,
                            view_id: dialog.view_id.clone(),
                            definition_revision: dialog.definition_revision,
                            kind: dialog.kind,
                            instruction,
                            provider: dialog.provider.clone(),
                            mode: dialog.mode.clone(),
                            thinking: dialog.thinking.clone(),
                        });
                    }
                }
            }
            Action::ScrollAskAi(delta) if self.focus == Focus::AskAi => {
                if let Some(dialog) = &mut self.ask_ai_dialog
                    && dialog.stage == AskAiStage::Proposal
                {
                    dialog.review_scroll = (i32::from(dialog.review_scroll) + delta)
                        .clamp(0, i32::from(dialog.review_scroll_limit))
                        as u16;
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
                        if kind == AskAiKind::Recipe {
                            let mut config = self
                                .ask_ai_dialog
                                .as_ref()
                                .and_then(|dialog| dialog.recipe.clone())
                                .unwrap_or_default();
                            config.advanced = expression;
                            let outcome = self
                                .ask_ai_dialog
                                .as_ref()
                                .and_then(|dialog| dialog.recipe_outcome.clone());
                            if self.apply_recipe_to_active_view(config) {
                                if let Some(state) = self.view_state_mut()
                                    && let Some(pending) = &mut state.pending_recipe
                                {
                                    pending.suggestion = outcome;
                                }
                                self.ask_ai_dialog = None;
                                self.focus = Focus::Logs;
                            } else if let Some(dialog) = &mut self.ask_ai_dialog {
                                dialog.stage = AskAiStage::Error;
                                dialog.progress =
                                    "query queue is full; working view was preserved".into();
                            }
                        } else {
                            self.focus = match kind {
                                AskAiKind::Filter => Focus::AdvancedEditor,
                                AskAiKind::Enrichment => Focus::EnrichmentEditor,
                                AskAiKind::Recipe => unreachable!(),
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
            }
            Action::OpenSource => {
                self.source_dialog.get_or_insert_with(Default::default);
                self.dialog_scroll_focused = false;
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
                let anchored_row = self.view_state().and_then(|state| state.selected.clone());
                let anchored = self
                    .active_view_id()
                    .zip(anchored_row.as_ref())
                    .and_then(|(view, id)| provider.row_by_id(view, id));
                let anchored_capture_nanos =
                    anchored.as_ref().and_then(|row| row.captured_at_unix_nanos);
                let anchored_event_nanos = anchored.as_ref().and_then(|row| {
                    row.details
                        .iter()
                        .find(|(name, _)| name == "event_time_utc_nanos")
                        .and_then(|(_, value)| value.parse().ok())
                });
                let anchored_extracted_nanos = anchored.as_ref().and_then(|row| {
                    row.details
                        .iter()
                        .find(|(name, _)| name == "derived.timestamp_utc")
                        .and_then(|(_, value)| parse_utc_nanos(value).ok())
                });
                let (basis, policy, applied, may_seed) =
                    self.view_state()
                        .map_or((TimeBasis::Capture, None, None, true), |state| {
                            (
                                if state.time_draft_touched {
                                    state.time_basis_draft
                                } else {
                                    state.applied_time_basis
                                },
                                state.applied_capture_time_policy,
                                state.applied_capture_time,
                                !state.time_draft_touched,
                            )
                        });
                if may_seed {
                    let seed = applied
                        .or_else(|| {
                            let center = match basis {
                                TimeBasis::Capture => anchored_capture_nanos,
                                TimeBasis::Event => anchored_event_nanos,
                                TimeBasis::Extracted => anchored_extracted_nanos,
                            };
                            center.map(|center| CaptureTimeRange {
                                start_unix_nanos: center.saturating_sub(30_000_000_000),
                                end_unix_nanos: center.saturating_add(30_000_000_000),
                            })
                        })
                        .unwrap_or(CaptureTimeRange {
                            start_unix_nanos: self
                                .clock_now_unix_nanos
                                .saturating_sub(30_000_000_000),
                            end_unix_nanos: self.clock_now_unix_nanos,
                        });
                    if let Some(state) = self.view_state_mut() {
                        state.time_start_draft = format_utc_nanos(seed.start_unix_nanos);
                        state.time_end_draft = format_utc_nanos(seed.end_unix_nanos);
                    }
                }
                if let Some(state) = self.view_state_mut()
                    && (may_seed || !state.time_structured_draft_present)
                {
                    let start = split_time_draft(&state.time_start_draft);
                    let end = split_time_draft(&state.time_end_draft);
                    state.time_start_date_draft = start.0;
                    state.time_start_clock_draft = start.1;
                    state.time_start_zone_draft = start.2;
                    state.time_end_date_draft = end.0;
                    state.time_end_clock_draft = end.1;
                    state.time_end_zone_draft = end.2;
                    state.time_structured_draft_present = true;
                }
                let (start_date, start_clock, start_zone, end_date, end_clock, end_zone) = self
                    .view_state()
                    .map(|state| {
                        if state.time_structured_draft_present {
                            return (
                                state.time_start_date_draft.clone(),
                                state.time_start_clock_draft.clone(),
                                state.time_start_zone_draft.clone(),
                                state.time_end_date_draft.clone(),
                                state.time_end_clock_draft.clone(),
                                state.time_end_zone_draft.clone(),
                            );
                        }
                        let start = split_time_draft(&state.time_start_draft);
                        let end = split_time_draft(&state.time_end_draft);
                        (start.0, start.1, start.2, end.0, end.1, end.2)
                    })
                    .unwrap_or_default();
                let window = self.view_state().map_or(TimeWindowChoice::All, |state| {
                    if state.time_draft_touched {
                        state.time_window_draft
                    } else {
                        match policy {
                            None => TimeWindowChoice::All,
                            Some(CaptureTimePolicy::Absolute(_)) => TimeWindowChoice::Absolute,
                            Some(CaptureTimePolicy::Recent { seconds }) => {
                                TimeWindowChoice::Recent(seconds)
                            }
                        }
                    }
                });
                self.time_dialog = Some(TimeDialogState {
                    focus: TimeControl::Basis,
                    dropdown: None,
                    window,
                    scroll: 0,
                    reveal_focus: true,
                    has_overflow: false,
                    dropdown_scroll: 0,
                    segment_cursor: usize::MAX,
                    start_date,
                    start_clock,
                    start_zone_custom: !is_time_zone_preset(&start_zone),
                    start_zone,
                    end_date,
                    end_clock,
                    end_zone_custom: !is_time_zone_preset(&end_zone),
                    end_zone,
                    highlighted: 0,
                    window_choices: time_window_choices(window),
                    anchored_row,
                    anchored_capture_nanos,
                    anchored_event_nanos,
                    anchored_extracted_nanos,
                    basis,
                });
                self.focus = Focus::TimeEditor;
            }
            Action::SwitchTimeField if self.focus == Focus::TimeEditor => {
                if let Some(dialog) = &mut self.time_dialog {
                    dialog.focus = if matches!(
                        dialog.focus,
                        TimeControl::EndDate
                            | TimeControl::EndClock
                            | TimeControl::EndZone
                            | TimeControl::EndZoneMenu
                    ) {
                        TimeControl::StartDate
                    } else {
                        TimeControl::EndDate
                    };
                    dialog.segment_cursor = usize::MAX;
                    dialog.reveal_focus = true;
                }
            }
            Action::TimeMoveFocus(delta) if self.focus == Focus::TimeEditor => {
                if let Some(dialog) = &mut self.time_dialog {
                    dialog.dropdown = None;
                    let controls = TimeControl::focusable(
                        dialog.has_overflow,
                        dialog.start_zone_custom,
                        dialog.end_zone_custom,
                    );
                    let at = controls
                        .iter()
                        .position(|item| *item == dialog.focus)
                        .unwrap_or(0);
                    dialog.focus = controls[(at as isize + delta as isize)
                        .rem_euclid(controls.len() as isize)
                        as usize];
                    dialog.segment_cursor = usize::MAX;
                    dialog.reveal_focus = true;
                }
            }
            Action::TimeFocus(control) if self.focus == Focus::TimeEditor => {
                if let Some(dialog) = &mut self.time_dialog {
                    dialog.focus = control;
                    dialog.dropdown = None;
                    dialog.segment_cursor = usize::MAX;
                    dialog.reveal_focus = true;
                }
            }
            Action::TimeOpenFocused if self.focus == Focus::TimeEditor => {
                if self
                    .time_dialog
                    .as_ref()
                    .is_some_and(|d| d.dropdown.is_some())
                {
                    self.handle(Action::TimeChoose, provider);
                    return;
                }
                let action = self.time_dialog.as_ref().map(|d| match d.focus {
                    TimeControl::Basis => None,
                    TimeControl::Window => None,
                    TimeControl::StartZoneMenu | TimeControl::EndZoneMenu => None,
                    TimeControl::Apply => Some(Action::SubmitTime),
                    TimeControl::Clear => Some(Action::ClearTime),
                    TimeControl::Recognize => Some(Action::OpenTimestampAssistant),
                    TimeControl::ScrollUp => Some(Action::TimeScroll(-1)),
                    TimeControl::ScrollDown => Some(Action::TimeScroll(1)),
                    _ => Some(Action::None),
                });
                if let Some(dialog) = &mut self.time_dialog {
                    dialog.dropdown = match dialog.focus {
                        TimeControl::Basis => Some(TimeDropdown::Basis),
                        TimeControl::Window => Some(TimeDropdown::Window),
                        TimeControl::StartZoneMenu => Some(TimeDropdown::StartZone),
                        TimeControl::EndZoneMenu => Some(TimeDropdown::EndZone),
                        _ => dialog.dropdown,
                    };
                    dialog.highlighted = match dialog.dropdown {
                        Some(TimeDropdown::Basis) => {
                            [TimeBasis::Capture, TimeBasis::Event, TimeBasis::Extracted]
                                .iter()
                                .position(|v| *v == dialog.basis)
                                .unwrap_or(0)
                        }
                        Some(TimeDropdown::Window) => dialog
                            .window_choices
                            .iter()
                            .position(|v| *v == dialog.window)
                            .unwrap_or(0),
                        Some(TimeDropdown::StartZone) => time_zone_choices()
                            .iter()
                            .position(|(_, value)| *value == dialog.start_zone)
                            .unwrap_or(time_zone_choices().len()),
                        Some(TimeDropdown::EndZone) => time_zone_choices()
                            .iter()
                            .position(|(_, value)| *value == dialog.end_zone)
                            .unwrap_or(time_zone_choices().len()),
                        None => 0,
                    };
                }
                if let Some(Some(action)) = action {
                    self.handle(action, provider);
                }
            }
            Action::SetTimeBasis(basis) if self.focus == Focus::TimeEditor => {
                if let Some(dialog) = &mut self.time_dialog {
                    dialog.basis = basis;
                }
                if let Some(state) = self.view_state_mut() {
                    mark_time_edit(state);
                    state.time_error = None;
                    state.time_basis_draft = basis;
                    state.time_draft_touched = true;
                }
            }
            Action::TimeMoveChoice(delta) if self.focus == Focus::TimeEditor => {
                if let Some(dialog) = &mut self.time_dialog {
                    match dialog.dropdown {
                        Some(TimeDropdown::Basis) => {
                            dialog.highlighted = (dialog.highlighted as isize + delta as isize)
                                .rem_euclid(3)
                                as usize;
                        }
                        Some(TimeDropdown::Window) => {
                            dialog.highlighted = (dialog.highlighted as isize + delta as isize)
                                .rem_euclid(dialog.window_choices.len() as isize)
                                as usize;
                        }
                        Some(TimeDropdown::StartZone) | Some(TimeDropdown::EndZone) => {
                            let choices = time_zone_choices().len() + 1;
                            dialog.highlighted = (dialog.highlighted as isize + delta as isize)
                                .rem_euclid(choices as isize)
                                as usize;
                        }
                        None if matches!(
                            dialog.focus,
                            TimeControl::ScrollUp | TimeControl::ScrollDown
                        ) =>
                        {
                            dialog.scroll = dialog.scroll.saturating_add_signed(delta as isize);
                            dialog.reveal_focus = false;
                        }
                        None => {}
                    }
                }
            }
            Action::TimeChoose if self.focus == Focus::TimeEditor => {
                let selected = self
                    .time_dialog
                    .as_ref()
                    .and_then(|dialog| match dialog.dropdown {
                        Some(TimeDropdown::Basis) => {
                            [TimeBasis::Capture, TimeBasis::Event, TimeBasis::Extracted]
                                .get(dialog.highlighted)
                                .copied()
                                .map(EitherTimeChoice::Basis)
                        }
                        Some(TimeDropdown::Window) => dialog
                            .window_choices
                            .get(dialog.highlighted)
                            .copied()
                            .map(EitherTimeChoice::Window),
                        Some(TimeDropdown::StartZone) | Some(TimeDropdown::EndZone) => {
                            time_zone_choices()
                                .get(dialog.highlighted)
                                .map(|(_, value)| EitherTimeChoice::Zone((*value).into()))
                                .or(Some(EitherTimeChoice::CustomZone))
                        }
                        None => None,
                    });
                if let Some(dialog) = &mut self.time_dialog {
                    dialog.dropdown = None;
                }
                match selected {
                    Some(EitherTimeChoice::Basis(basis)) => {
                        self.handle(Action::SetTimeBasis(basis), provider)
                    }
                    Some(EitherTimeChoice::Window(window)) => {
                        let unavailable = window == TimeWindowChoice::AroundSelected
                            && self
                                .time_dialog
                                .as_ref()
                                .is_none_or(|dialog| match dialog.basis {
                                    TimeBasis::Capture => dialog.anchored_capture_nanos.is_none(),
                                    TimeBasis::Event => dialog.anchored_event_nanos.is_none(),
                                    TimeBasis::Extracted => {
                                        dialog.anchored_extracted_nanos.is_none()
                                    }
                                });
                        if unavailable {
                            if let Some(state) = self.view_state_mut() {
                                state.time_error = Some("Around selected is unavailable: the opening record has no timestamp in this basis".into());
                            }
                            return;
                        }
                        if window == TimeWindowChoice::AroundSelected {
                            self.handle(Action::AroundSelected, provider);
                            return;
                        }
                        if let Some(dialog) = &mut self.time_dialog {
                            dialog.window = window;
                        }
                        if let Some(state) = self.view_state_mut() {
                            mark_time_edit(state);
                            state.time_error = None;
                            state.time_window_draft = window;
                            state.time_draft_touched = true;
                        }
                    }
                    Some(EitherTimeChoice::Zone(zone)) => {
                        let snapshot = if let Some(dialog) = &mut self.time_dialog {
                            match dialog.focus {
                                TimeControl::StartZone | TimeControl::StartZoneMenu => {
                                    dialog.start_zone = zone;
                                    dialog.start_zone_custom = false;
                                }
                                TimeControl::EndZone | TimeControl::EndZoneMenu => {
                                    dialog.end_zone = zone;
                                    dialog.end_zone_custom = false;
                                }
                                _ => return,
                            }
                            dialog.window = TimeWindowChoice::Absolute;
                            Some(dialog_time_drafts(dialog))
                        } else {
                            None
                        };
                        if let Some((start, end, parts)) = snapshot
                            && let Some(state) = self.view_state_mut()
                        {
                            store_time_drafts(state, start, end, parts);
                            state.time_window_draft = TimeWindowChoice::Absolute;
                            state.time_draft_touched = true;
                            state.time_error = None;
                            mark_time_edit(state);
                        }
                    }
                    Some(EitherTimeChoice::CustomZone) => {
                        if let Some(dialog) = &mut self.time_dialog {
                            match dialog.focus {
                                TimeControl::StartZone | TimeControl::StartZoneMenu => {
                                    dialog.start_zone_custom = true;
                                    dialog.focus = TimeControl::StartZone;
                                }
                                TimeControl::EndZone | TimeControl::EndZoneMenu => {
                                    dialog.end_zone_custom = true;
                                    dialog.focus = TimeControl::EndZone;
                                }
                                _ => {}
                            }
                            dialog.segment_cursor = usize::MAX;
                        }
                    }
                    None => {}
                }
            }
            Action::TimeChooseIndex(index) if self.focus == Focus::TimeEditor => {
                if let Some(dialog) = &mut self.time_dialog {
                    dialog.highlighted = index;
                }
                self.handle(Action::TimeChoose, provider);
            }
            Action::TimeScroll(delta) if self.focus == Focus::TimeEditor => {
                if let Some(dialog) = &mut self.time_dialog {
                    dialog.scroll = dialog.scroll.saturating_add_signed(delta as isize);
                    dialog.reveal_focus = false;
                }
            }
            Action::TimeMoveCursor(delta) if self.focus == Focus::TimeEditor => {
                let Some((cursor, len)) = self.time_dialog.as_ref().map(|d| {
                    (
                        d.segment_cursor,
                        dialog_time_segment(d, d.focus).chars().count(),
                    )
                }) else {
                    return;
                };
                if let Some(dialog) = &mut self.time_dialog {
                    let at = cursor.min(len);
                    dialog.segment_cursor = at.saturating_add_signed(delta as isize).min(len);
                }
            }
            Action::TimeInput(ch) if self.focus == Focus::TimeEditor => {
                let changed = self
                    .time_dialog
                    .as_mut()
                    .is_some_and(|dialog| edit_dialog_time_segment(dialog, Some(ch)));
                if !changed {
                    return;
                }
                let drafts = self.time_dialog.as_ref().map(dialog_time_drafts);
                if let Some(state) = self.view_state_mut()
                    && let Some((start, end, parts)) = drafts
                {
                    store_time_drafts(state, start, end, parts);
                    state.time_window_draft = TimeWindowChoice::Absolute;
                    mark_time_edit(state);
                    state.time_error = None;
                    state.time_draft_touched = true;
                }
            }
            Action::TimeBackspace if self.focus == Focus::TimeEditor => {
                let changed = self
                    .time_dialog
                    .as_mut()
                    .is_some_and(|dialog| edit_dialog_time_segment(dialog, None));
                if !changed {
                    return;
                }
                let drafts = self.time_dialog.as_ref().map(dialog_time_drafts);
                if let Some(state) = self.view_state_mut()
                    && let Some((start, end, parts)) = drafts
                {
                    store_time_drafts(state, start, end, parts);
                    state.time_window_draft = TimeWindowChoice::Absolute;
                    mark_time_edit(state);
                    state.time_error = None;
                    state.time_draft_touched = true;
                }
            }
            Action::ClearTime if self.focus == Focus::TimeEditor => {
                if let Some(state) = self.view_state_mut() {
                    state.time_start_draft.clear();
                    state.time_end_draft.clear();
                    state.time_recent_draft.clear();
                    state.time_start_date_draft.clear();
                    state.time_start_clock_draft.clear();
                    state.time_start_zone_draft.clear();
                    state.time_end_date_draft.clear();
                    state.time_end_clock_draft.clear();
                    state.time_end_zone_draft.clear();
                    state.time_structured_draft_present = true;
                    mark_time_edit(state);
                    state.time_draft_touched = true;
                    state.time_window_draft = TimeWindowChoice::All;
                }
                if let Some(dialog) = &mut self.time_dialog {
                    dialog.start_date.clear();
                    dialog.start_clock.clear();
                    dialog.start_zone.clear();
                    dialog.end_date.clear();
                    dialog.end_clock.clear();
                    dialog.end_zone.clear();
                    dialog.start_zone_custom = true;
                    dialog.end_zone_custom = true;
                    dialog.window = TimeWindowChoice::All;
                }
                let basis = self
                    .time_dialog
                    .as_ref()
                    .map_or(TimeBasis::Capture, |dialog| dialog.basis);
                self.submit_capture_time(None, None, basis);
            }
            Action::AroundSelected if self.focus == Focus::TimeEditor => {
                if let Some(dialog) = &mut self.time_dialog {
                    dialog.window = TimeWindowChoice::AroundSelected;
                }
                if let Some(state) = self.view_state_mut() {
                    mark_time_edit(state);
                    state.time_window_draft = TimeWindowChoice::AroundSelected;
                    state.time_draft_touched = true;
                }
                if let Some(dialog) = self.time_dialog.as_ref() {
                    let basis = dialog.basis;
                    let center = match basis {
                        TimeBasis::Capture => dialog.anchored_capture_nanos,
                        TimeBasis::Extracted => dialog.anchored_extracted_nanos,
                        TimeBasis::Event => dialog.anchored_event_nanos,
                    };
                    if let Some(center) = center {
                        let start = center.saturating_sub(30_000_000_000);
                        let end = center.saturating_add(30_000_000_000);
                        let start_text = format_utc_nanos(start);
                        let end_text = format_utc_nanos(end);
                        let start_parts = split_time_draft(&start_text);
                        let end_parts = split_time_draft(&end_text);
                        if let Some(dialog) = &mut self.time_dialog {
                            (dialog.start_date, dialog.start_clock, dialog.start_zone) =
                                start_parts;
                            (dialog.end_date, dialog.end_clock, dialog.end_zone) = end_parts;
                            dialog.start_zone_custom = !is_time_zone_preset(&dialog.start_zone);
                            dialog.end_zone_custom = !is_time_zone_preset(&dialog.end_zone);
                        }
                        let snapshot = self.time_dialog.as_ref().map(dialog_time_drafts);
                        if let Some(state) = self.view_state_mut() {
                            if let Some((start, end, parts)) = snapshot {
                                store_time_drafts(state, start, end, parts);
                            }
                            state.time_error = None;
                            state.time_draft_touched = true;
                        }
                    } else if let Some(state) = self.view_state_mut() {
                        state.time_error = Some(match basis {
                            TimeBasis::Capture => "selected record has no capture timestamp".into(),
                            TimeBasis::Extracted => {
                                "selected record has no valid extracted timestamp_utc".into()
                            }
                            TimeBasis::Event => {
                                "selected record has no recognized event timestamp".into()
                            }
                        });
                    }
                } else if let Some(state) = self.view_state_mut() {
                    state.time_error = Some("select a timestamped record first".into());
                }
            }
            Action::SubmitTime if self.focus == Focus::TimeEditor => {
                if let Some(state) = self.view_state_mut() {
                    mark_time_edit(state);
                }
                let choice = self
                    .time_dialog
                    .as_ref()
                    .map_or(TimeWindowChoice::Absolute, |d| d.window);
                if choice == TimeWindowChoice::All {
                    self.handle(Action::ClearTime, provider);
                    return;
                }
                if let TimeWindowChoice::Recent(seconds) = choice {
                    self.handle(Action::SetRecentTime(seconds), provider);
                    return;
                }
                if choice == TimeWindowChoice::AroundSelected {
                    let available = self.time_dialog.as_ref().is_some_and(|d| match d.basis {
                        TimeBasis::Capture => d.anchored_capture_nanos.is_some(),
                        TimeBasis::Event => d.anchored_event_nanos.is_some(),
                        TimeBasis::Extracted => d.anchored_extracted_nanos.is_some(),
                    });
                    if !available {
                        if let Some(state) = self.view_state_mut() {
                            state.time_error =
                                Some("opening record has no timestamp in the chosen basis".into());
                        }
                        return;
                    }
                    self.handle(Action::AroundSelected, provider);
                }
                let parsed = self.view_state().map(|state| {
                    parse_capture_range(&state.time_start_draft, &state.time_end_draft)
                });
                match parsed {
                    Some(Ok(window)) => {
                        let basis = self
                            .time_dialog
                            .as_ref()
                            .map_or(TimeBasis::Capture, |dialog| dialog.basis);
                        self.submit_capture_time(
                            Some(window),
                            Some(CaptureTimePolicy::Absolute(window)),
                            basis,
                        )
                    }
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
                        state.time_draft_touched = true;
                        state.time_window_draft = TimeWindowChoice::Recent(seconds);
                        mark_time_edit(state);
                    }
                    self.submit_capture_time(
                        Some(window),
                        Some(CaptureTimePolicy::Recent { seconds }),
                        self.time_dialog
                            .as_ref()
                            .map_or(TimeBasis::Capture, |dialog| dialog.basis),
                    );
                }
            }
            Action::SelectRecipeMode(mode) if self.focus == Focus::Recipes => {
                if self.recipe_requests.len() >= 8 {
                    if let Some(dialog) = &mut self.recipe_dialog {
                        dialog.status = "recipe request queue is full".into();
                    }
                    return;
                }
                let mut refresh = None;
                if let Some(dialog) = &mut self.recipe_dialog {
                    if matches!(mode, RecipeDialogMode::History | RecipeDialogMode::Update)
                        && (dialog.loading || dialog.items.get(dialog.selected).is_none())
                    {
                        dialog.status = "select a loaded recipe first".into();
                        return;
                    }
                    if mode == RecipeDialogMode::Update {
                        dialog.name = dialog.items[dialog.selected].name.clone();
                    }
                    if mode == RecipeDialogMode::Export && dialog.mode != mode {
                        dialog.name.clear();
                    }
                    dialog.mode = mode;
                    dialog.loading = false;
                    dialog.pending_request_id = None;
                    dialog.status.clear();
                    dialog.interaction_revision = dialog.interaction_revision.saturating_add(1);
                    if matches!(mode, RecipeDialogMode::Browse | RecipeDialogMode::History)
                        && self.recipe_requests.len() < 8
                    {
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
                    let request = if mode == RecipeDialogMode::History {
                        let dialog = self.recipe_dialog.as_mut().expect("recipe dialog");
                        let recipe_id = dialog.items[dialog.selected].id.clone();
                        dialog.selected = 0;
                        dialog.items.clear();
                        dialog.suggestions.clear();
                        RecipeRequest::History { meta, recipe_id }
                    } else {
                        RecipeRequest::List { meta }
                    };
                    self.recipe_requests.push_back(request);
                }
            }
            Action::MoveRecipe(delta) if self.focus == Focus::Recipes => {
                if let Some(dialog) = &mut self.recipe_dialog {
                    dialog.selected = move_index(dialog.selected, dialog.items.len(), delta);
                    dialog.interaction_revision = dialog.interaction_revision.saturating_add(1);
                }
            }
            Action::RefreshRecipeSuggestions
                if self.focus == Focus::Recipes
                    && self
                        .recipe_dialog
                        .as_ref()
                        .is_some_and(|dialog| dialog.mode == RecipeDialogMode::Browse) =>
            {
                let mut request = None;
                if let Some(dialog) = &mut self.recipe_dialog
                    && self.recipe_requests.len() < 8
                {
                    dialog.interaction_revision = dialog.interaction_revision.saturating_add(1);
                    dialog.loading = true;
                    dialog.status = "refreshing similar-source suggestions…".into();
                    request = Some((dialog.id, dialog.interaction_revision));
                }
                if let Some((dialog_id, revision)) = request {
                    let meta = self.next_recipe_request_meta(dialog_id, revision);
                    if let Some(dialog) = &mut self.recipe_dialog {
                        dialog.pending_request_id = Some(meta.request_id);
                    }
                    self.recipe_requests.push_back(RecipeRequest::List { meta });
                }
            }
            Action::RejectRecipeSuggestion if self.focus == Focus::Recipes => {
                let outcome = self.recipe_dialog.as_ref().and_then(|dialog| {
                    let item = dialog.items.get(dialog.selected)?;
                    dialog
                        .suggestions
                        .iter()
                        .find(|value| value.recipe_id == item.id)?;
                    Some(RecipeOutcome {
                        source_id: self.views.get(self.selected_view)?.source_id.clone(),
                        recipe_id: item.id.clone(),
                        revision: item.revision.clone(),
                        accepted: false,
                    })
                });
                if let Some(outcome) = outcome {
                    self.recipe_requests
                        .push_back(RecipeRequest::Outcome(outcome.clone()));
                    if let Some(dialog) = &mut self.recipe_dialog {
                        dialog
                            .suggestions
                            .retain(|value| value.recipe_id != outcome.recipe_id);
                        dialog.status = "suggestion rejected; recipe remains available".into();
                    }
                }
            }
            Action::AdaptRecipeSuggestion if self.focus == Focus::Recipes => {
                let selected = self.recipe_dialog.as_ref().and_then(|dialog| {
                    let item = dialog.items.get(dialog.selected)?.clone();
                    let suggestion = dialog
                        .suggestions
                        .iter()
                        .find(|value| value.recipe_id == item.id)?
                        .clone();
                    Some((item, suggestion))
                });
                if let Some((item, suggestion)) = selected
                    && item.incompatibility.is_none()
                    && let Some(view_id) = self.active_view_id().map(str::to_owned)
                {
                    let mut config = item.config;
                    if let Some(current) = self.persistent_view_state(&view_id) {
                        config.capture_time = current.applied_capture_time;
                        config.capture_time_policy = current.applied_capture_time_policy;
                        config.time_basis = current.applied_time_basis;
                        config.grouping = current.applied_grouping;
                    }
                    let generation = self.next_ask_ai_generation;
                    self.next_ask_ai_generation = generation.saturating_add(1);
                    let source_id = self
                        .views
                        .get(self.selected_view)
                        .map_or("", |view| view.source_id.as_str());
                    self.ask_ai_dialog = Some(AskAiDialogState {
                        generation,
                        definition_revision: self
                            .view_definition_revision(&view_id)
                            .unwrap_or_default(),
                        view_id,
                        kind: AskAiKind::Recipe,
                        prompt: format!(
                            "Adapt recipe {:?} for this source. source-id={source_id} Evidence: {}. Missing required fields: {}. Preserve unsupported presentation/time/grouping settings.",
                            item.name,
                            suggestion.evidence.join(", "),
                            suggestion.missing_fields.join(", ")
                        ),
                        provider: self.ai_provider.clone(),
                        mode: self.ai_mode.clone(),
                        thinking: self.ai_thinking.clone(),
                        stage: AskAiStage::Input,
                        progress: "Review the adaptation request before applying".into(),
                        expression: None,
                        explanation: None,
                        session_id: None,
                        snapshot_dir: None,
                        recipe: Some(config),
                        review_scroll: 0,
                        review_scroll_limit: 0,
                        recipe_outcome: Some(RecipeOutcome {
                            source_id: source_id.to_owned(),
                            recipe_id: item.id,
                            revision: item.revision,
                            accepted: true,
                        }),
                    });
                    self.focus = Focus::AskAi;
                    self.recipe_dialog = None;
                }
            }
            Action::RecipeInput(ch) if self.focus == Focus::Recipes => {
                if let Some(dialog) = &mut self.recipe_dialog
                    && dialog.mode.is_editable()
                    && dialog.name.len() < MAX_EDITOR_BYTES
                {
                    dialog.name.push(ch);
                    dialog.interaction_revision = dialog.interaction_revision.saturating_add(1);
                }
            }
            Action::RecipeBackspace if self.focus == Focus::Recipes => {
                if let Some(dialog) = &mut self.recipe_dialog
                    && dialog.mode.is_editable()
                {
                    dialog.name.pop();
                    dialog.interaction_revision = dialog.interaction_revision.saturating_add(1);
                }
            }
            Action::SubmitRecipe if self.focus == Focus::Recipes => {
                let apply = self.recipe_dialog.as_ref().and_then(|dialog| {
                    (dialog.mode.is_list())
                        .then(|| dialog.items.get(dialog.selected).cloned())
                        .flatten()
                });
                if let Some(item) = apply {
                    if let Some(error) = item.incompatibility {
                        if let Some(dialog) = &mut self.recipe_dialog {
                            dialog.status = error;
                        }
                    } else {
                        let outcome = self.recipe_dialog.as_ref().and_then(|dialog| {
                            dialog
                                .suggestions
                                .iter()
                                .find(|value| value.recipe_id == item.id)
                                .map(|_| RecipeOutcome {
                                    source_id: self
                                        .views
                                        .get(self.selected_view)
                                        .map(|view| view.source_id.clone())
                                        .unwrap_or_default(),
                                    recipe_id: item.id.clone(),
                                    revision: item.revision.clone(),
                                    accepted: true,
                                })
                        });
                        if self.apply_recipe_to_active_view(item.config) {
                            if let Some(state) = self.view_state_mut()
                                && let Some(pending) = &mut state.pending_recipe
                            {
                                pending.suggestion = outcome;
                            }
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
                    } else if mode == RecipeDialogMode::Export
                        && !name.is_empty()
                        && self.recipe_requests.len() < 8
                    {
                        if let Some(item) = self
                            .recipe_dialog
                            .as_ref()
                            .and_then(|dialog| dialog.items.get(dialog.selected))
                            .cloned()
                        {
                            let meta = self.next_recipe_request_meta(dialog_id, dialog_revision);
                            self.recipe_requests.push_back(RecipeRequest::Export {
                                meta,
                                path: name,
                                recipe_id: item.id,
                                revision: item.revision,
                            });
                            if let Some(dialog) = &mut self.recipe_dialog {
                                dialog.loading = true;
                                dialog.pending_request_id = Some(meta.request_id);
                                dialog.status = "exporting reviewed revision…".into();
                            }
                        } else if let Some(dialog) = &mut self.recipe_dialog {
                            dialog.status = "select a saved recipe before exporting".into();
                        }
                    } else if matches!(mode, RecipeDialogMode::Save | RecipeDialogMode::Update)
                        && !name.is_empty()
                        && self.recipe_requests.len() < 8
                    {
                        let config = self
                            .persistent_view_state(&view_id)
                            .map(|state| RecipeConfig {
                                search: state.applied_search,
                                advanced: state.applied_advanced,
                                enrichment: state.applied_enrichment,
                                enrichments: state.applied_enrichments,
                                pinned_columns: state.pinned_columns,
                                color_field: state.color_field,
                                capture_time: state.applied_capture_time,
                                capture_time_policy: state.applied_capture_time_policy,
                                time_basis: state.applied_time_basis,
                                grouping: state.applied_grouping,
                            })
                            .unwrap_or_default();
                        let meta = self.next_recipe_request_meta(dialog_id, dialog_revision);
                        let update = if mode == RecipeDialogMode::Update {
                            self.recipe_dialog
                                .as_ref()
                                .and_then(|dialog| dialog.items.get(dialog.selected))
                                .map(|item| (item.id.clone(), item.revision.clone()))
                        } else {
                            None
                        };
                        self.recipe_requests.push_back(RecipeRequest::Save {
                            update,
                            meta,
                            name,
                            view_id,
                            config: Box::new(config),
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
            Action::StopCapture | Action::RestartCapture => {
                if matches!(self.focus, Focus::Logs | Focus::Selector)
                    && let Some(view) = self.views.get(self.selected_view)
                {
                    if self.source_controls.len() < 8 {
                        if !self
                            .source_controls
                            .iter()
                            .any(|request| request.source_id == view.source_id)
                        {
                            self.source_controls.push_back(SourceControlRequest {
                                source_id: view.source_id.clone(),
                                restart: action == Action::RestartCapture,
                            });
                        }
                    } else {
                        self.action_notice = Some(
                            "source control queue full; retry after pending work settles".into(),
                        );
                    }
                }
            }
            Action::OpenViewDialog => {
                if let Some(view) = self.views.get(self.selected_view) {
                    self.view_dialog = Some(ViewDialogState {
                        source_ids: self.view_source_ids(&view.id),
                        selected_source: 0,
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
                        ViewDialogMode::Rename | ViewDialogMode::Sources => view.name.clone(),
                    };
                }
            }
            Action::MoveViewSource(delta) if self.focus == Focus::ViewDialog => {
                if let Some(dialog) = &mut self.view_dialog
                    && dialog.mode == ViewDialogMode::Sources
                    && !self.sources.is_empty()
                {
                    dialog.selected_source = (dialog.selected_source as i32 + delta)
                        .clamp(0, self.sources.len() as i32 - 1)
                        as usize;
                }
            }
            Action::ToggleViewSource if self.focus == Focus::ViewDialog => {
                if let Some(dialog) = &mut self.view_dialog
                    && dialog.mode == ViewDialogMode::Sources
                    && let Some(source) = self.sources.get(dialog.selected_source)
                {
                    if self
                        .views
                        .get(self.selected_view)
                        .is_some_and(|view| view.source_id == source.id)
                    {
                        dialog.error = Some("the owning source stays in this view".into());
                    } else if let Some(index) =
                        dialog.source_ids.iter().position(|id| id == &source.id)
                    {
                        dialog.source_ids.remove(index);
                        dialog.error = None;
                    } else if dialog.source_ids.len() < 32 {
                        dialog.source_ids.push(source.id.clone());
                        dialog.error = None;
                    }
                }
            }
            Action::ReorderViewSource(delta) if self.focus == Focus::ViewDialog => {
                if let Some(dialog) = &mut self.view_dialog
                    && dialog.mode == ViewDialogMode::Sources
                    && let Some(source) = self.sources.get(dialog.selected_source)
                    && let Some(index) = dialog.source_ids.iter().position(|id| id == &source.id)
                {
                    let target = (index as i32 + delta).clamp(0, dialog.source_ids.len() as i32 - 1)
                        as usize;
                    dialog.source_ids.swap(index, target);
                }
            }
            Action::ViewInput(character) if self.focus == Focus::ViewDialog => {
                if character == ' '
                    && self
                        .view_dialog
                        .as_ref()
                        .is_some_and(|dialog| dialog.mode == ViewDialogMode::Sources)
                {
                    self.handle(Action::ToggleViewSource, provider);
                    return;
                }
                if let Some(dialog) = &mut self.view_dialog
                    && dialog.draft.len() < 128
                {
                    if dialog.mode == ViewDialogMode::Sources {
                        return;
                    }
                    dialog.draft.push(character);
                    dialog.error = None;
                }
            }
            Action::ViewBackspace if self.focus == Focus::ViewDialog => {
                if let Some(dialog) = &mut self.view_dialog
                    && dialog.mode != ViewDialogMode::Sources
                {
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
                        source_ids: dialog.source_ids.clone(),
                        mode: dialog.mode,
                        source_id: view.source_id.clone(),
                        view_id: view.id.clone(),
                        name: name.to_owned(),
                    });
                }
            }
            Action::OpenFieldPicker => {
                if let Some(state) = self.view_state_mut() {
                    // Freeze the identity, not the current row projection. A cache
                    // miss is pending work and must not prevent the dialog opening.
                    state.field_picker_row = state.selected.clone();
                    state.field_picker_selected = 0;
                    state.field_picker_top = 0;
                }
                self.focus = Focus::FieldPicker;
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
                dialog.control = SourceControl::Input;
                dialog.controls_focused = false;
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
                dialog.control = SourceControl::Input;
                dialog.controls_focused = false;
                clear_path_completion(dialog);
            }
            Action::ToggleSourceControlFocus if self.focus == Focus::SourceDialog => {
                if let Some(dialog) = &mut self.source_dialog {
                    let controls = SourceControl::visible(dialog.mode);
                    let index = controls
                        .iter()
                        .position(|control| *control == dialog.control)
                        .unwrap_or(0);
                    dialog.control = controls[(index + 1) % controls.len()];
                    dialog.controls_focused = dialog.control != SourceControl::Input;
                }
            }
            Action::FocusSourceControl(control) if self.focus == Focus::SourceDialog => {
                if let Some(dialog) = &mut self.source_dialog {
                    dialog.control = control;
                    dialog.controls_focused = control != SourceControl::Input;
                }
                self.handle(Action::ActivateSourceControl, provider);
            }
            Action::MoveSourceMode(delta) if self.focus == Focus::SourceDialog => {
                let modes = [
                    SourceDialogMode::Manual,
                    SourceDialogMode::Discovery,
                    SourceDialogMode::Ai,
                ];
                let Some(dialog) = &mut self.source_dialog else {
                    return;
                };
                if !dialog.controls_focused {
                    return;
                }
                let index = modes
                    .iter()
                    .position(|mode| *mode == dialog.mode)
                    .unwrap_or(0);
                dialog.mode = modes[(index as i32 + delta).rem_euclid(modes.len() as i32) as usize];
                dialog.control = match dialog.mode {
                    SourceDialogMode::Manual => SourceControl::Manual,
                    SourceDialogMode::Discovery => SourceControl::Discovery,
                    SourceDialogMode::Ai => SourceControl::Agent,
                };
                clear_path_completion(dialog);
                if dialog.mode == SourceDialogMode::Discovery && dialog.discovery.generation == 0 {
                    self.start_discovery_scan();
                }
            }
            Action::ActivateSourceControl if self.focus == Focus::SourceDialog => {
                match self.source_dialog.as_ref().map(|dialog| dialog.control) {
                    Some(SourceControl::Input) => self.handle(Action::SubmitSource, provider),
                    Some(SourceControl::Manual) => {
                        if let Some(dialog) = &mut self.source_dialog {
                            dialog.mode = SourceDialogMode::Manual;
                            dialog.control = SourceControl::Input;
                            dialog.controls_focused = false;
                            clear_path_completion(dialog);
                        }
                    }
                    Some(SourceControl::Discovery) => {
                        if let Some(dialog) = &mut self.source_dialog {
                            dialog.mode = SourceDialogMode::Discovery;
                            dialog.control = SourceControl::Input;
                            dialog.controls_focused = false;
                            clear_path_completion(dialog);
                        }
                        if self
                            .source_dialog
                            .as_ref()
                            .is_some_and(|dialog| dialog.discovery.generation == 0)
                        {
                            self.start_discovery_scan();
                        }
                    }
                    Some(SourceControl::Agent) => {
                        if let Some(dialog) = &mut self.source_dialog {
                            dialog.mode = SourceDialogMode::Ai;
                            dialog.control = SourceControl::Input;
                            dialog.controls_focused = false;
                            clear_path_completion(dialog);
                        }
                    }
                    Some(SourceControl::File) => {
                        self.handle(Action::SelectSourceKind(SourceKind::File), provider)
                    }
                    Some(SourceControl::Command) => {
                        self.handle(Action::SelectSourceKind(SourceKind::Command), provider)
                    }
                    Some(SourceControl::Open) => self.handle(Action::SubmitSource, provider),
                    Some(SourceControl::Refresh) => self.handle(Action::RefreshDiscovery, provider),
                    None => {}
                }
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
                if self
                    .source_dialog
                    .as_ref()
                    .is_some_and(|dialog| dialog.mode == SourceDialogMode::Discovery)
                {
                    self.dialog_scroll_focused = !self.dialog_scroll_focused;
                } else {
                    self.complete_source_path();
                }
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
            Action::ToggleEditorCompletion => self.toggle_editor_completion(provider),
            Action::MoveEditorCompletion(delta) => {
                if let Some(completion) = &mut self.editor_completion
                    && !completion.items.is_empty()
                {
                    completion.selected = (completion.selected as i32 + delta)
                        .rem_euclid(completion.items.len() as i32)
                        as usize;
                    if completion.selected < completion.top {
                        completion.top = completion.selected;
                    } else if completion.selected >= completion.top + 8 {
                        completion.top = completion.selected + 1 - 8;
                    }
                }
            }
            Action::AcceptEditorCompletion => self.accept_editor_completion(),
            Action::EditorInput(character) if self.editor_open() && self.is_text_editing() => {
                if self.dialog_scroll_focused {
                    return;
                }
                self.editor_completion = None;
                self.append_editor(&character.to_string())
            }
            Action::EditorInput(character) if self.focus == Focus::AskAi => {
                if self.dialog_scroll_focused {
                    return;
                }
                self.append_ask_ai(&character.to_string())
            }
            Action::EditorInput(character) if self.focus == Focus::Investigation => {
                self.append_investigation(&character.to_string())
            }
            Action::EditorBackspace if self.editor_open() && self.is_text_editing() => {
                if self.dialog_scroll_focused {
                    return;
                }
                self.editor_completion = None;
                self.edit_active(|editor| {
                    editor.draft.pop();
                });
                self.schedule_search();
            }
            Action::EditorBackspace if self.focus == Focus::AskAi => {
                if self.dialog_scroll_focused {
                    return;
                }
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
            Action::EditorPaste(text) if self.focus == Focus::CommandEnrichment => {
                let mut edited_view = None;
                if let Some(dialog) = &mut self.command_enrichment_dialog {
                    if dialog.run_state == CommandEnrichmentRunState::SavingResults {
                        return;
                    }
                    if matches!(
                        dialog.run_state,
                        CommandEnrichmentRunState::Saving
                            | CommandEnrichmentRunState::Preparing
                            | CommandEnrichmentRunState::Running
                    ) {
                        dialog.error = Some("Wait for the current operation before editing".into());
                        return;
                    }
                    let multiline = matches!(
                        dialog.selected_field,
                        CommandEnrichmentField::Arguments | CommandEnrichmentField::Environment
                    );
                    let field = command_draft_field_mut(dialog);
                    let remaining = MAX_COMMAND_FIELD_BYTES.saturating_sub(field.len());
                    let mut accepted = text
                        .chars()
                        .take_while(|ch| {
                            field.len().saturating_add(ch.len_utf8()) <= MAX_COMMAND_FIELD_BYTES
                        })
                        .collect::<String>();
                    if !multiline {
                        accepted = accepted.replace(['\r', '\n'], " ");
                    }
                    if accepted.len() <= remaining {
                        field.push_str(&accepted);
                        edited_view = Some(dialog.view_id.clone());
                    }
                    dialog.error = None;
                    dialog.review = None;
                    dialog.run_state = CommandEnrichmentRunState::Unrun;
                    dialog.run_status = "Draft changed · save before reviewing a run".into();
                }
                if let Some(view_id) = edited_view
                    && let Some(state) = self.view_states.get_mut(&view_id)
                {
                    state.user_interaction_revision =
                        state.user_interaction_revision.saturating_add(1);
                }
            }
            Action::EditorPaste(text) if self.focus == Focus::Bookmarks => {
                if let Some(dialog) = &mut self.bookmark_dialog
                    && dialog.editing.is_some()
                {
                    if dialog.draft.len().saturating_add(text.len()) <= MAX_BOOKMARK_NOTE_BYTES
                        && !text.chars().any(char::is_control)
                    {
                        dialog.draft.push_str(&text);
                        if let Some(state) = self.view_states.get_mut(&dialog.view_id) {
                            state.user_interaction_revision =
                                state.user_interaction_revision.saturating_add(1);
                        }
                    } else {
                        dialog.status = "note must be a single line, at most 1024 bytes".into();
                    }
                }
            }
            Action::EditorPaste(text) if self.focus == Focus::Recipes => {
                if let Some(dialog) = &mut self.recipe_dialog
                    && dialog.mode.is_editable()
                {
                    if text.chars().any(char::is_control)
                        || dialog.name.len().saturating_add(text.len()) > MAX_EDITOR_BYTES
                    {
                        dialog.status = "recipe name/path must be a bounded single line".into();
                    } else {
                        dialog.name.push_str(&text);
                        dialog.interaction_revision = dialog.interaction_revision.saturating_add(1);
                    }
                }
            }
            Action::EditorPaste(text) if self.focus == Focus::ViewDialog => {
                if self
                    .view_dialog
                    .as_ref()
                    .is_some_and(|dialog| dialog.mode == ViewDialogMode::Sources)
                {
                    return;
                }
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
                let whole_bound = text.contains('T');
                let segment_length = self
                    .time_dialog
                    .as_ref()
                    .map_or(0, |dialog| dialog_time_segment(dialog, dialog.focus).len());
                if text.chars().any(char::is_control)
                    || text.len() > 64
                    || (!whole_bound && segment_length.saturating_add(text.len()) > 32)
                {
                    if let Some(state) = self.view_state_mut() {
                        state.time_error = Some("Pasted time exceeds the field limit or contains control characters; draft retained".into());
                    }
                    return;
                }
                if let Some(dialog) = &mut self.time_dialog {
                    if whole_bound {
                        let parts = split_time_draft(&text);
                        if matches!(
                            dialog.focus,
                            TimeControl::EndDate
                                | TimeControl::EndClock
                                | TimeControl::EndZone
                                | TimeControl::EndZoneMenu
                        ) {
                            (dialog.end_date, dialog.end_clock, dialog.end_zone) = parts;
                            dialog.end_zone_custom = !is_time_zone_preset(&dialog.end_zone);
                        } else {
                            (dialog.start_date, dialog.start_clock, dialog.start_zone) = parts;
                            dialog.start_zone_custom = !is_time_zone_preset(&dialog.start_zone);
                            dialog.focus = TimeControl::StartDate;
                        }
                        dialog.segment_cursor = usize::MAX;
                    } else {
                        if !matches!(
                            dialog.focus,
                            TimeControl::StartDate
                                | TimeControl::StartClock
                                | TimeControl::StartZone
                                | TimeControl::EndDate
                                | TimeControl::EndClock
                                | TimeControl::EndZone
                        ) {
                            return;
                        }
                        for ch in text.chars() {
                            edit_dialog_time_segment(dialog, Some(ch));
                        }
                    }
                    dialog.window = TimeWindowChoice::Absolute;
                }
                let drafts = self.time_dialog.as_ref().map(dialog_time_drafts);
                if let Some(state) = self.view_state_mut()
                    && let Some((start, end, parts)) = drafts
                {
                    store_time_drafts(state, start, end, parts);
                    state.time_window_draft = TimeWindowChoice::Absolute;
                    state.time_draft_touched = true;
                    state.time_error = None;
                    mark_time_edit(state);
                }
            }
            Action::EditorPaste(text) if self.editor_open() && self.is_text_editing() => {
                self.editor_completion = None;
                self.append_editor(&text)
            }
            Action::SubmitDraft if self.editor_open() => {
                if self.editor_completion.is_some() {
                    self.accept_editor_completion();
                } else {
                    self.submit_draft();
                }
            }
            Action::CancelEditor => {
                if self.focus == Focus::CommandEnrichment {
                    if let Some(target) = self.active_text_target() {
                        self.text_cursors.prune_identity(&target.identity);
                    }
                    if let Some(dialog) = self.command_enrichment_dialog.take()
                        && matches!(
                            dialog.run_state,
                            CommandEnrichmentRunState::Preparing
                                | CommandEnrichmentRunState::Running
                        )
                    {
                        self.command_enrichment_requests.push_back(
                            CommandEnrichmentRequest::Cancel {
                                generation: dialog.generation,
                                view_id: dialog.view_id,
                            },
                        );
                    }
                    self.focus = Focus::Logs;
                    return;
                }
                if self.focus == Focus::Context {
                    self.focus = self
                        .context_dialog
                        .take()
                        .map_or(Focus::Logs, |dialog| dialog.return_focus);
                    return;
                }
                if self.focus == Focus::Bookmarks {
                    if let Some(target) = self.active_text_target() {
                        self.text_cursors.prune_identity(&target.identity);
                    }
                    if let Some(dialog) = &mut self.bookmark_dialog
                        && dialog.editing.take().is_some()
                    {
                        dialog.draft.clear();
                    } else {
                        self.bookmark_dialog = None;
                        self.focus = Focus::Logs;
                    }
                    return;
                }
                if self.editor_completion.take().is_some() {
                    return;
                }
                if self.focus == Focus::Storage {
                    if let Some(dialog) = self.storage_dialog.take()
                        && dialog.scanning
                    {
                        self.storage_requests.push_back(StorageRequest {
                            generation: dialog.generation,
                            kind: StorageRequestKind::Cancel,
                        });
                    }
                    self.focus = Focus::Logs;
                    return;
                }
                if self.focus == Focus::Settings {
                    if let Some(target) = self.active_text_target() {
                        self.text_cursors.prune_identity(&target.identity);
                    }
                    if let Some(dialog) = self.settings_dialog.take() {
                        self.theme_id = dialog.context.effective_theme;
                        self.delight_enabled = dialog.context.effective_delight_enabled;
                        self.reduced_motion = dialog.context.effective_reduced_motion;
                        self.ascii = dialog.context.effective_ascii;
                    }
                    self.focus = Focus::Logs;
                    return;
                }
                if self.focus == Focus::FieldPicker {
                    self.focus = Focus::Logs;
                    return;
                }
                if self.focus == Focus::Recipes {
                    if let Some(target) = self.active_text_target() {
                        self.text_cursors.prune_identity(&target.identity);
                    }
                    self.recipe_dialog = None;
                    self.focus = Focus::Logs;
                    return;
                }
                if self.focus == Focus::TimeEditor {
                    if let Some(dialog) = &mut self.time_dialog
                        && dialog.dropdown.take().is_some()
                    {
                        return;
                    }
                    self.time_dialog = None;
                    self.focus = Focus::Logs;
                    return;
                }
                if self.focus == Focus::SourceDialog {
                    self.text_cursors.prune_identity("source-dialog");
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
                    if let Some(target) = self.active_text_target() {
                        self.text_cursors.prune_identity(&target.identity);
                    }
                    self.view_dialog = None;
                }
                if self.focus == Focus::AskAi
                    && {
                        if let Some(target) = self.active_text_target() {
                            self.text_cursors.prune_identity(&target.identity);
                        }
                        true
                    }
                    && let Some(dialog) = self.ask_ai_dialog.take()
                    && !matches!(dialog.stage, AskAiStage::Input | AskAiStage::Error)
                    && self.ask_ai_requests.len() < MAX_AI_REQUESTS
                {
                    self.ask_ai_requests.push_back(AskAiRequest::Cancel {
                        generation: dialog.generation,
                    });
                }
                if self.focus == Focus::Investigation
                    && {
                        if let Some(target) = self.active_text_target() {
                            self.text_cursors.prune_identity(&target.identity);
                        }
                        true
                    }
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
            | Action::SubmitInvestigation
            | Action::RefreshStorage
            | Action::ClearStorage
            | Action::MoveStorage(_) => {}
            Action::MoveSettings(_)
            | Action::CycleSetting
            | Action::SettingsInput(_)
            | Action::SettingsBackspace
            | Action::SaveSettings => {}
            Action::MoveFieldPicker(_)
            | Action::AddEnrichment
            | Action::EditEnrichment
            | Action::RemoveEnrichment
            | Action::MoveEnrichment(_)
            | Action::MoveEnrichmentControl(_)
            | Action::FocusEnrichmentControl(_)
            | Action::ActivateEnrichmentControl
            | Action::TogglePinnedField
            | Action::ToggleColorField
            | Action::FocusSettings(_)
            | Action::ActivateSettings
            | Action::MoveSettingsTheme(_)
            | Action::ChooseSettingsTheme(_)
            | Action::CloseSettingsTheme
            | Action::ScrollSettingsDetails(_)
            | Action::ToggleSourceKind
            | Action::SelectSourceKind(_)
            | Action::CompleteSourcePath
            | Action::MovePathCompletion(_)
            | Action::ToggleDiscovery
            | Action::ToggleSourceAi
            | Action::ToggleSourceControlFocus
            | Action::FocusSourceControl(_)
            | Action::MoveSourceMode(_)
            | Action::ActivateSourceControl
            | Action::RefreshDiscovery
            | Action::MoveDiscovery(_)
            | Action::SourceInput(_)
            | Action::SourceBackspace
            | Action::SubmitSource
            | Action::SelectViewDialogMode(_)
            | Action::SubmitViewDialog
            | Action::ViewInput(_)
            | Action::ViewBackspace
            | Action::MoveViewSource(_)
            | Action::ReorderViewSource(_)
            | Action::ToggleViewSource
            | Action::SelectAskAiKind(_)
            | Action::SubmitAskAi
            | Action::ApplyAskAi => {}
            Action::ScrollAskAi(_)
            | Action::SelectRecipeMode(_)
            | Action::MoveRecipe(_)
            | Action::RecipeInput(_)
            | Action::RecipeBackspace
            | Action::SubmitRecipe
            | Action::RefreshRecipeSuggestions
            | Action::RejectRecipeSuggestion
            | Action::AdaptRecipeSuggestion
            | Action::TimeInput(_)
            | Action::TimeBackspace
            | Action::SwitchTimeField
            | Action::SubmitTime
            | Action::ClearTime
            | Action::AroundSelected
            | Action::SetTimeBasis(_)
            | Action::SetRecentTime(_)
            | Action::TimeMoveFocus(_)
            | Action::TimeOpenFocused
            | Action::TimeMoveChoice(_)
            | Action::TimeChoose
            | Action::TimeFocus(_)
            | Action::TimeChooseIndex(_)
            | Action::TimeScroll(_)
            | Action::TimeMoveCursor(_)
            | Action::CommandEnrichmentNextField
            | Action::FocusCommandEnrichmentControl(_)
            | Action::ActivateCommandEnrichmentControl
            | Action::CommandEnrichmentInput(_)
            | Action::CommandEnrichmentBackspace
            | Action::TextStartOfLine
            | Action::TextEndOfLine
            | Action::TextKillToEndOfLine
            | Action::TextMoveLeft
            | Action::TextMoveRight
            | Action::TextMoveUp
            | Action::TextMoveDown
            | Action::SaveCommandEnrichment
            | Action::RemoveCommandEnrichment
            | Action::PrepareCommandEnrichmentRun
            | Action::ConfirmCommandEnrichmentRun => {}
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
                dialog.progress = "resuming selected local agent session".into();
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

    pub fn field_picker_row_id(&self) -> Option<&RowId> {
        self.view_state()?.field_picker_row.as_ref()
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
                dialog.ai.progress = "source agent request queue is full".into();
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
            dialog.ai.progress = "source agent request queue is full".into();
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
            QueryPurpose::Grouping => &mut state.grouping.draft,
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

    fn remove_selected_enrichment(&mut self) {
        let Some(view_id) = self.active_view_id().map(str::to_owned) else {
            return;
        };
        let Some(state) = self.view_states.get(&view_id) else {
            return;
        };
        if state.enrichments.is_empty() {
            return;
        }
        let pending_draft = state.enrichment.draft.clone();
        let mut stages = state.enrichments.clone();
        stages.remove(state.enrichment_selected.min(stages.len() - 1));
        if self
            .enqueue_enrichment_chain(
                &view_id,
                stages,
                pending_draft,
                PendingEnrichmentMutation::Remove,
            )
            .is_some()
        {
            let state = self.view_states.get_mut(&view_id).expect("view state");
            state.enrichment_editing = None;
            state.enrichment_selected = state
                .enrichment_selected
                .min(state.enrichments.len().saturating_sub(1));
            if state
                .enrichment_editing
                .as_ref()
                .is_some_and(|editing| !state.enrichments.iter().any(|stage| &stage.id == editing))
            {
                state.enrichment_editing = None;
            }
        }
    }

    fn enqueue_enrichment_chain(
        &mut self,
        view_id: &str,
        enrichments: Vec<EnrichmentDefinition>,
        pending_value: String,
        mutation: PendingEnrichmentMutation,
    ) -> Option<u64> {
        let key = (view_id.to_owned(), QueryPurpose::Enrichment);
        if !self.query_requests.contains_key(&key)
            && self.query_requests.len() >= MAX_PENDING_QUERY_REQUESTS
        {
            self.editor_mut(view_id, QueryPurpose::Enrichment).error =
                Some("query submission queue is full; stages were preserved".into());
            return None;
        }
        let generation = self.next_query_generation;
        self.next_query_generation = self.next_query_generation.saturating_add(1);
        let state = self.view_states.get_mut(view_id).expect("view state");
        let base_revision = state.applied_query_revision;
        let base_constraints = applied_constraints(state);
        let mut constraints = state.desired_constraints.clone();
        constraints.enrichments = enrichments;
        state.desired_query_revision = state.desired_query_revision.saturating_add(1);
        let revision = state.desired_query_revision;
        state.desired_constraints = constraints.clone();
        state.enrichment.pending_generation = Some(generation);
        state.enrichment.pending_revision = Some(revision);
        state.enrichment.pending_value = Some(pending_value);
        state.pending_enrichment_mutation = Some(mutation);
        state.enrichment.error = None;
        self.query_requests.insert(
            key,
            QueryRequest {
                view_id: view_id.to_owned(),
                generation,
                revision,
                base_revision,
                base_constraints,
                purpose: QueryPurpose::Enrichment,
                constraints,
            },
        );
        Some(revision)
    }

    fn submit_capture_time(
        &mut self,
        window: Option<CaptureTimeRange>,
        policy: Option<CaptureTimePolicy>,
        basis: TimeBasis,
    ) {
        let Some(view_id) = self.active_view_id().map(str::to_owned) else {
            return;
        };
        let state = self.view_states.get_mut(&view_id).expect("view state");
        state.desired_constraints.capture_time = window;
        state.desired_capture_time_policy = policy;
        state.desired_time_basis = basis;
        state.desired_constraints.time_basis = basis;
        state.time_error = None;
        if self.enqueue_time_query(&view_id).is_some() {
            self.time_dialog = None;
            self.focus = Focus::Logs;
        } else {
            let state = self.view_states.get_mut(&view_id).expect("view state");
            state.desired_constraints = applied_constraints(state);
            state.desired_capture_time_policy = state.applied_capture_time_policy;
            state.desired_time_basis = state.applied_time_basis;
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
            basis: state.desired_time_basis,
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
        let basis = self
            .view_states
            .get(view_id)
            .map_or(TimeBasis::Capture, |state| state.desired_time_basis);
        self.view_states
            .get_mut(view_id)
            .expect("view state")
            .pending_time = Some(PendingTime {
            generation: request.generation,
            revision: request.revision,
            value,
            policy,
            basis,
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
        let mut enrichment_mutation = None;
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
                if value.trim().is_empty() {
                    state.enrichment.error = Some(
                        "Provide a regex or named expression; Alt-R removes the selected stage"
                            .into(),
                    );
                    return None;
                } else if let Some(id) = &state.enrichment_editing {
                    if let Some(stage) = constraints
                        .enrichments
                        .iter_mut()
                        .find(|stage| &stage.id == id)
                    {
                        stage.source = value.clone();
                    }
                    enrichment_mutation = Some(PendingEnrichmentMutation::Edit);
                } else if constraints.enrichments.len() < 32 {
                    let mut candidate = generation;
                    let id = loop {
                        let id = EnrichmentStageId(format!("stage-{candidate}"));
                        if !constraints.enrichments.iter().any(|stage| stage.id == id) {
                            break id;
                        }
                        candidate = candidate.saturating_add(1);
                    };
                    constraints.enrichments.push(EnrichmentDefinition {
                        id,
                        source: value.clone(),
                    });
                    enrichment_mutation = Some(PendingEnrichmentMutation::Add);
                } else {
                    state.enrichment.error = Some("at most 32 enrichment stages".into());
                    return None;
                }
                value
            }
            QueryPurpose::Grouping => {
                let value = value.unwrap_or_else(|| state.grouping.draft.clone());
                constraints.grouping = nonempty(&value);
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
            QueryPurpose::Grouping => &mut state.grouping,
        };
        editor.pending_generation = Some(generation);
        editor.pending_revision = Some(revision);
        editor.pending_value = Some(pending_value);
        editor.error = None;
        if let Some(mutation) = enrichment_mutation {
            state.pending_enrichment_mutation = Some(mutation);
        }
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
            Focus::SearchEditor
                | Focus::AdvancedEditor
                | Focus::EnrichmentEditor
                | Focus::GroupingEditor
        )
    }

    fn toggle_editor_completion<P: RowProvider>(&mut self, provider: &P) {
        if self.dialog_scroll_focused {
            self.dialog_scroll_focused = false;
            self.editor_completion = None;
            return;
        }
        if matches!(self.focus, Focus::SearchEditor | Focus::GroupingEditor) {
            self.dialog_scroll_focused = true;
            return;
        }
        let Some(purpose @ (QueryPurpose::Advanced | QueryPurpose::Enrichment)) =
            self.editor_purpose()
        else {
            return;
        };
        let Some(view_id) = self.active_view_id().map(str::to_owned) else {
            return;
        };
        let draft = self.editor_mut(&view_id, purpose).draft.clone();
        let Some(target) = self.active_text_target() else {
            return;
        };
        let cursor = self
            .text_cursors
            .get_or_end(target.clone(), &draft)
            .char_index;
        let current_kind = self
            .editor_completion
            .as_ref()
            .filter(|state| {
                state.view_id == view_id
                    && state.purpose == purpose
                    && state.draft == draft
                    && state.target == target
                    && state.cursor == cursor
            })
            .map(|state| state.kind);
        if current_kind == Some(EditorCompletionKind::SampledValue) {
            self.editor_completion = None;
            self.dialog_scroll_focused = true;
            return;
        }
        let kind = current_kind.map_or(EditorCompletionKind::Field, |_| {
            EditorCompletionKind::SampledValue
        });
        let state = self.view_states.get(&view_id).expect("active view state");
        let page = provider.page(
            &view_id,
            ViewportRequest {
                start: state.top,
                len: state.viewport_height.clamp(1, MAX_COMPLETION_ROWS),
            },
        );
        let mut fields = std::collections::BTreeSet::new();
        // `raw` is the authoritative original record column and exists even
        // when a source has no recognized JSON/logfmt fields.
        fields.insert("raw".to_owned());
        let mut values = std::collections::BTreeSet::new();
        for row in page.rows {
            for (field, value) in row.fields.into_iter().take(MAX_COMPLETION_FIELDS) {
                if field.len() <= MAX_COMPLETION_TEXT_BYTES && fields.len() < MAX_COMPLETION_FIELDS
                {
                    fields.insert(field.clone());
                }
                if value.len() <= MAX_COMPLETION_TEXT_BYTES && values.len() < MAX_COMPLETION_VALUES
                {
                    values.insert((field, value));
                }
            }
        }
        let items: Vec<EditorCompletionItem> = match kind {
            EditorCompletionKind::Field => fields
                .into_iter()
                .map(|field| EditorCompletionItem {
                    label: python_string_literal(&field),
                    insertion: format!("pl.col({})", python_string_literal(&field)),
                })
                .collect(),
            EditorCompletionKind::SampledValue => values
                .into_iter()
                .map(|(field, value)| EditorCompletionItem {
                    label: format!(
                        "{} = {} (sampled lexical string)",
                        python_string_literal(&field),
                        python_string_literal(&value)
                    ),
                    insertion: python_string_literal(&value),
                })
                .collect(),
        };
        let generation = self.next_editor_completion_generation;
        self.next_editor_completion_generation = generation.saturating_add(1);
        let status = if items.is_empty() {
            "no fields or values in the sampled visible rows".into()
        } else {
            match kind {
                EditorCompletionKind::Field => {
                    "Fields insert pl.col(...); static sampled literals are available separately"
                        .into()
                }
                EditorCompletionKind::SampledValue => {
                    "Static quoted lexical literals from sampled rows; they do not vary per row"
                        .into()
                }
            }
        };
        self.editor_completion = Some(EditorCompletionState {
            generation,
            view_id,
            purpose,
            draft,
            target,
            cursor,
            kind,
            items,
            selected: 0,
            top: 0,
            status,
        });
    }

    fn accept_editor_completion(&mut self) {
        let Some(completion) = self.editor_completion.take() else {
            return;
        };
        if self.active_view_id() != Some(completion.view_id.as_str())
            || self.editor_purpose() != Some(completion.purpose)
            || self.active_text_target().as_ref() != Some(&completion.target)
        {
            return;
        }
        let current = self
            .editor_mut(&completion.view_id, completion.purpose)
            .draft
            .clone();
        let cursor = self
            .text_cursors
            .get_or_end(completion.target.clone(), &current)
            .char_index;
        if current != completion.draft || cursor != completion.cursor {
            return;
        }
        if let Some(item) = completion.items.get(completion.selected) {
            self.apply_text_command(EditCommand::Insert(&item.insertion));
            self.editor_mut(&completion.view_id, completion.purpose)
                .error = None;
        }
    }

    fn editor_purpose(&self) -> Option<QueryPurpose> {
        match self.focus {
            Focus::SearchEditor => Some(QueryPurpose::Search),
            Focus::AdvancedEditor => Some(QueryPurpose::Advanced),
            Focus::EnrichmentEditor => Some(QueryPurpose::Enrichment),
            Focus::GroupingEditor => Some(QueryPurpose::Grouping),
            Focus::Selector
            | Focus::Logs
            | Focus::Details
            | Focus::SourceDialog
            | Focus::Help
            | Focus::ViewDialog
            | Focus::FieldPicker
            | Focus::AskAi
            | Focus::Investigation
            | Focus::CommandEnrichment => None,
            Focus::Recipes
            | Focus::TimeEditor
            | Focus::Storage
            | Focus::Settings
            | Focus::Context
            | Focus::Bookmarks => None,
        }
    }

    fn editor_mut(&mut self, view_id: &str, purpose: QueryPurpose) -> &mut EditorState {
        let state = self.view_states.get_mut(view_id).expect("view state");
        match purpose {
            QueryPurpose::Search => &mut state.search,
            QueryPurpose::Advanced => &mut state.advanced,
            QueryPurpose::Enrichment => &mut state.enrichment,
            QueryPurpose::Grouping => &mut state.grouping,
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
            QueryPurpose::Grouping => &mut state.grouping,
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
        if self.focus == Focus::TimeEditor {
            let point = (event.column, event.row);
            match event.kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    if let Some(index) = self
                        .hit_regions
                        .time_choices
                        .iter()
                        .find_map(|(area, index)| contains(*area, point).then_some(*index))
                    {
                        self.handle(Action::TimeChooseIndex(index), provider);
                    } else if let Some(control) = self
                        .hit_regions
                        .time_controls
                        .iter()
                        .find_map(|(area, control)| contains(*area, point).then_some(*control))
                    {
                        self.handle(Action::TimeFocus(control), provider);
                        self.handle(Action::TimeOpenFocused, provider);
                    }
                }
                MouseEventKind::ScrollUp => self.handle(Action::TimeScroll(-1), provider),
                MouseEventKind::ScrollDown => self.handle(Action::TimeScroll(1), provider),
                _ => {}
            }
            return;
        }
        if self.focus == Focus::Settings {
            let point = (event.column, event.row);
            match event.kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    if let Some(index) = self
                        .hit_regions
                        .settings_theme_choices
                        .iter()
                        .find_map(|(area, index)| contains(*area, point).then_some(*index))
                    {
                        self.handle(Action::ChooseSettingsTheme(index), provider);
                    } else if let Some(control) = self
                        .hit_regions
                        .settings_controls
                        .iter()
                        .find_map(|(area, control)| contains(*area, point).then_some(*control))
                    {
                        self.handle(Action::FocusSettings(control), provider);
                        if matches!(
                            control,
                            SettingsControl::Field(
                                SettingsField::Theme
                                    | SettingsField::Delight
                                    | SettingsField::ReducedMotion
                                    | SettingsField::Ascii
                            ) | SettingsControl::Save
                        ) {
                            self.handle(Action::ActivateSettings, provider);
                        }
                    }
                }
                MouseEventKind::ScrollUp => {
                    self.handle(Action::ScrollSettingsDetails(-1), provider)
                }
                MouseEventKind::ScrollDown => {
                    self.handle(Action::ScrollSettingsDetails(1), provider)
                }
                _ => {}
            }
            return;
        }
        let point = (event.column, event.row);
        if let Some(area) = self
            .hit_regions
            .dialog_scroll
            .filter(|area| contains(*area, point))
        {
            match event.kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    self.dialog_scroll_focused = true;
                }
                MouseEventKind::ScrollUp => self.handle(Action::ScrollHoveredDialog(-1), provider),
                MouseEventKind::ScrollDown => self.handle(Action::ScrollHoveredDialog(1), provider),
                _ => {}
            }
            let _ = area;
            return;
        }
        if matches!(event.kind, MouseEventKind::Down(MouseButton::Left)) {
            self.dialog_scroll_focused = false;
        }
        if self.focus == Focus::ViewDialog {
            match event.kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    if let Some(index) =
                        self.hit_regions
                            .view_source_rows
                            .iter()
                            .find_map(|(area, index)| {
                                contains(*area, (event.column, event.row)).then_some(*index)
                            })
                        && let Some(dialog) = &mut self.view_dialog
                    {
                        dialog.selected_source = index;
                    }
                }
                MouseEventKind::ScrollUp => self.handle(Action::MoveViewSource(-1), provider),
                MouseEventKind::ScrollDown => self.handle(Action::MoveViewSource(1), provider),
                _ => {}
            }
            return;
        }
        if self.focus == Focus::Bookmarks {
            match event.kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    if let Some(index) =
                        self.hit_regions
                            .bookmark_rows
                            .iter()
                            .find_map(|(area, index)| {
                                contains(*area, (event.column, event.row)).then_some(*index)
                            })
                    {
                        self.handle(Action::SelectBookmark(index), provider);
                    }
                }
                MouseEventKind::ScrollUp => self.handle(Action::MoveBookmark(-1), provider),
                MouseEventKind::ScrollDown => self.handle(Action::MoveBookmark(1), provider),
                _ => {}
            }
            return;
        }
        if self.focus == Focus::Context {
            match event.kind {
                MouseEventKind::ScrollUp => self.handle(Action::MoveContext(-3), provider),
                MouseEventKind::ScrollDown => self.handle(Action::MoveContext(3), provider),
                _ => {}
            }
            return;
        }
        if self.editor_completion.is_some() {
            let point = (event.column, event.row);
            if matches!(event.kind, MouseEventKind::Down(MouseButton::Left))
                && let Some(index) = self
                    .hit_regions
                    .editor_completion_rows
                    .iter()
                    .find_map(|(area, index)| contains(*area, point).then_some(*index))
                && let Some(completion) = &mut self.editor_completion
            {
                completion.selected = index;
            }
            match event.kind {
                MouseEventKind::ScrollUp => self.handle(Action::MoveEditorCompletion(-1), provider),
                MouseEventKind::ScrollDown => {
                    self.handle(Action::MoveEditorCompletion(1), provider)
                }
                _ => {}
            }
            return;
        }
        if self.focus == Focus::EnrichmentEditor {
            let point = (event.column, event.row);
            if matches!(event.kind, MouseEventKind::Down(MouseButton::Left))
                && let Some(control) = self
                    .hit_regions
                    .enrichment_controls
                    .iter()
                    .find_map(|(area, control)| contains(*area, point).then_some(*control))
            {
                self.handle(Action::FocusEnrichmentControl(control), provider);
                return;
            }
            if matches!(event.kind, MouseEventKind::Down(MouseButton::Left))
                && let Some(index) = self
                    .hit_regions
                    .enrichment_rows
                    .iter()
                    .find_map(|(area, index)| contains(*area, point).then_some(*index))
                && let Some(state) = self.view_state_mut()
            {
                state.enrichment_selected = index;
            }
            return;
        }
        if self.focus == Focus::CommandEnrichment
            && matches!(event.kind, MouseEventKind::Down(MouseButton::Left))
        {
            let point = (event.column, event.row);
            if let Some(control) = self
                .hit_regions
                .command_enrichment_controls
                .iter()
                .find_map(|(area, control)| contains(*area, point).then_some(*control))
            {
                self.handle(Action::FocusCommandEnrichmentControl(control), provider);
                self.handle(Action::ActivateCommandEnrichmentControl, provider);
            }
            return;
        }
        if self.focus == Focus::Storage {
            let point = (event.column, event.row);
            if matches!(event.kind, MouseEventKind::Down(MouseButton::Left))
                && let Some(index) = self
                    .hit_regions
                    .storage_rows
                    .iter()
                    .find_map(|(area, index)| contains(*area, point).then_some(*index))
                && let Some(dialog) = &mut self.storage_dialog
            {
                dialog.selected = index;
                dialog.confirm_clear = false;
            }
            match event.kind {
                MouseEventKind::ScrollUp => self.handle(Action::MoveStorage(-1), provider),
                MouseEventKind::ScrollDown => self.handle(Action::MoveStorage(1), provider),
                _ => {}
            }
            return;
        }
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
        if self.focus == Focus::SourceDialog
            && matches!(event.kind, MouseEventKind::Down(MouseButton::Left))
        {
            let point = (event.column, event.row);
            if let Some(control) = self
                .hit_regions
                .source_controls
                .iter()
                .find_map(|(area, control)| contains(*area, point).then_some(*control))
            {
                self.handle(Action::FocusSourceControl(control), provider);
                return;
            }
        }
        if self.focus == Focus::SourceDialog
            && self
                .source_dialog
                .as_ref()
                .is_some_and(|dialog| dialog.mode == SourceDialogMode::Discovery)
        {
            let point = (event.column, event.row);
            if matches!(event.kind, MouseEventKind::Down(MouseButton::Left))
                && let Some(index) = self
                    .hit_regions
                    .discovery_rows
                    .iter()
                    .find_map(|(area, index)| contains(*area, point).then_some(*index))
                && let Some(dialog) = &mut self.source_dialog
            {
                dialog.discovery.selected = index;
            }
            match event.kind {
                MouseEventKind::ScrollUp => self.handle(Action::MoveDiscovery(-1), provider),
                MouseEventKind::ScrollDown => self.handle(Action::MoveDiscovery(1), provider),
                _ => {}
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
            match event.kind {
                MouseEventKind::ScrollUp => self.handle(Action::ScrollHelp(-1), provider),
                MouseEventKind::ScrollDown => self.handle(Action::ScrollHelp(1), provider),
                MouseEventKind::Down(MouseButton::Left) => {
                    self.handle(Action::ToggleHelp, provider)
                }
                _ => {}
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
                .log_row_indices
                .iter()
                .find_map(|(area, index)| contains(*area, point).then_some(*index))
            {
                let was_selected = self
                    .active_view_id()
                    .zip(self.view_state().and_then(|state| state.selected.as_ref()))
                    .and_then(|(view_id, id)| provider.index_of_id(view_id, id))
                    == Some(rows);
                self.focus = Focus::Logs;
                self.select_index(rows, provider);
                if was_selected {
                    self.handle(Action::ToggleExpandedGroup, provider);
                }
                return;
            }
            if self
                .hit_regions
                .details
                .is_some_and(|area| contains(area, point))
            {
                self.focus = Focus::Details;
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
        let over_details = self
            .hit_regions
            .details
            .is_some_and(|area| contains(area, point));
        let over_sidebar = self
            .hit_regions
            .sidebar
            .is_some_and(|area| contains(area, point));
        match event.kind {
            MouseEventKind::ScrollUp if over_details => {
                self.handle(Action::ScrollDetails(-3), provider)
            }
            MouseEventKind::ScrollDown if over_details => {
                self.handle(Action::ScrollDetails(3), provider)
            }
            MouseEventKind::ScrollUp if over_log => self.move_selection(-3, provider),
            MouseEventKind::ScrollLeft if over_log => {
                self.handle(Action::MoveHorizontal(-8), provider)
            }
            MouseEventKind::ScrollRight if over_log => {
                self.handle(Action::MoveHorizontal(8), provider)
            }
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

fn python_string_literal(value: &str) -> String {
    let mut result = String::with_capacity(value.len() + 2);
    result.push('\'');
    for character in value.chars() {
        match character {
            '\\' => result.push_str("\\\\"),
            '\'' => result.push_str("\\'"),
            '\n' => result.push_str("\\n"),
            '\r' => result.push_str("\\r"),
            '\t' => result.push_str("\\t"),
            value if value.is_control() => {
                use std::fmt::Write as _;
                let _ = write!(result, "\\u{:04x}", value as u32);
            }
            value => result.push(value),
        }
    }
    result.push('\'');
    result
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

fn time_window_choices(current: TimeWindowChoice) -> Vec<TimeWindowChoice> {
    let mut values = vec![
        TimeWindowChoice::All,
        TimeWindowChoice::Absolute,
        TimeWindowChoice::Recent(300),
        TimeWindowChoice::Recent(900),
        TimeWindowChoice::Recent(3600),
        TimeWindowChoice::AroundSelected,
    ];
    if let TimeWindowChoice::Recent(seconds) = current
        && !matches!(seconds, 300 | 900 | 3600)
    {
        values.insert(5, current);
    }
    values
}

const TIME_ZONE_CHOICES: [(&str, &str); 16] = [
    ("UTC", "Z"),
    ("UTC−12:00", "-12:00"),
    ("UTC−08:00", "-08:00"),
    ("UTC−05:00", "-05:00"),
    ("UTC−04:00", "-04:00"),
    ("UTC−03:00", "-03:00"),
    ("UTC−01:00", "-01:00"),
    ("UTC+01:00", "+01:00"),
    ("UTC+02:00", "+02:00"),
    ("UTC+03:00", "+03:00"),
    ("UTC+05:30", "+05:30"),
    ("UTC+05:45", "+05:45"),
    ("UTC+08:00", "+08:00"),
    ("UTC+09:00", "+09:00"),
    ("UTC+10:00", "+10:00"),
    ("UTC+14:00", "+14:00"),
];

pub(crate) fn time_zone_choices() -> &'static [(&'static str, &'static str)] {
    &TIME_ZONE_CHOICES
}

fn is_time_zone_preset(value: &str) -> bool {
    time_zone_choices()
        .iter()
        .any(|(_, preset)| *preset == value)
}

fn dialog_time_segment(dialog: &TimeDialogState, control: TimeControl) -> &str {
    match control {
        TimeControl::StartDate => &dialog.start_date,
        TimeControl::StartClock => &dialog.start_clock,
        TimeControl::StartZone => &dialog.start_zone,
        TimeControl::EndDate => &dialog.end_date,
        TimeControl::EndClock => &dialog.end_clock,
        TimeControl::EndZone => &dialog.end_zone,
        _ => "",
    }
}

fn edit_dialog_time_segment(dialog: &mut TimeDialogState, input: Option<char>) -> bool {
    let control = dialog.focus;
    if matches!(control, TimeControl::StartZone) && !dialog.start_zone_custom
        || matches!(control, TimeControl::EndZone) && !dialog.end_zone_custom
    {
        return false;
    }
    let cursor = dialog.segment_cursor;
    let part = match control {
        TimeControl::StartDate => &mut dialog.start_date,
        TimeControl::StartClock => &mut dialog.start_clock,
        TimeControl::StartZone => &mut dialog.start_zone,
        TimeControl::EndDate => &mut dialog.end_date,
        TimeControl::EndClock => &mut dialog.end_clock,
        TimeControl::EndZone => &mut dialog.end_zone,
        _ => return false,
    };
    let char_len = part.chars().count();
    let at = cursor.min(char_len);
    let byte_at = part
        .char_indices()
        .nth(at)
        .map_or(part.len(), |(index, _)| index);
    match input {
        Some(ch) if !ch.is_control() && part.len().saturating_add(ch.len_utf8()) <= 32 => {
            part.insert(byte_at, ch);
            dialog.segment_cursor = at + 1;
            dialog.window = TimeWindowChoice::Absolute;
            return true;
        }
        None if at > 0 => {
            let previous = part
                .char_indices()
                .nth(at - 1)
                .map_or(0, |(index, _)| index);
            part.replace_range(previous..byte_at, "");
            dialog.segment_cursor = at - 1;
            dialog.window = TimeWindowChoice::Absolute;
            return true;
        }
        _ => {}
    }
    false
}

fn dialog_time_drafts(dialog: &TimeDialogState) -> (String, String, [String; 6]) {
    (
        format!(
            "{}T{}{}",
            dialog.start_date, dialog.start_clock, dialog.start_zone
        ),
        format!(
            "{}T{}{}",
            dialog.end_date, dialog.end_clock, dialog.end_zone
        ),
        [
            dialog.start_date.clone(),
            dialog.start_clock.clone(),
            dialog.start_zone.clone(),
            dialog.end_date.clone(),
            dialog.end_clock.clone(),
            dialog.end_zone.clone(),
        ],
    )
}

fn store_time_drafts(state: &mut ViewState, start: String, end: String, parts: [String; 6]) {
    state.time_start_draft = start;
    state.time_end_draft = end;
    let [sd, sc, sz, ed, ec, ez] = parts;
    state.time_start_date_draft = sd;
    state.time_start_clock_draft = sc;
    state.time_start_zone_draft = sz;
    state.time_end_date_draft = ed;
    state.time_end_clock_draft = ec;
    state.time_end_zone_draft = ez;
    state.time_structured_draft_present = true;
}

pub fn split_time_draft(value: &str) -> (String, String, String) {
    let (date, rest) = value.split_once('T').unwrap_or((value, ""));
    let zone_at = rest
        .char_indices()
        .skip(1)
        .find(|(index, ch)| {
            matches!(ch, 'Z' | '+' | '-') || (*index >= 8 && rest[*index..].starts_with("UTC"))
        })
        .map(|(i, _)| i);
    let (clock, zone) = zone_at.map_or((rest, ""), |at| rest.split_at(at));
    (date.to_owned(), clock.to_owned(), zone.to_owned())
}

pub fn parse_utc_nanos(value: &str) -> Result<i64, String> {
    let value = value.trim();
    let (body, offset_seconds) = if let Some(body) = value.strip_suffix('Z') {
        (body, 0i64)
    } else if let Some(body) = value.strip_suffix("UTC") {
        (body.trim_end(), 0)
    } else {
        let at = value
            .char_indices()
            .skip(10)
            .find(|(_, ch)| matches!(ch, '+' | '-'))
            .map(|(i, _)| i)
            .ok_or_else(|| "timezone must be UTC, Z, or numeric ±HH:MM".to_owned())?;
        let (body, offset) = value.split_at(at);
        let bytes = offset.as_bytes();
        if bytes.len() != 6
            || bytes[3] != b':'
            || !bytes[1..3]
                .iter()
                .chain(&bytes[4..6])
                .all(u8::is_ascii_digit)
        {
            return Err("numeric timezone must use ±HH:MM".into());
        }
        let hours: i64 = offset[1..3]
            .parse()
            .map_err(|_| "invalid timezone offset")?;
        let minutes: i64 = offset[4..6]
            .parse()
            .map_err(|_| "invalid timezone offset")?;
        if hours > 23 || minutes > 59 {
            return Err("timezone offset is out of range".into());
        }
        let seconds = (hours * 60 + minutes)
            * 60
            * if bytes[0] == b'-' {
                -1
            } else if bytes[0] == b'+' {
                1
            } else {
                return Err("numeric timezone must use ±HH:MM".into());
            };
        (body, seconds)
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
        .checked_sub(offset_seconds)
        .ok_or_else(|| "UTC value overflows capture range".to_owned())?
        .checked_mul(1_000_000_000)
        .and_then(|v| v.checked_add(nanos))
        .ok_or_else(|| "UTC value overflows capture range".to_owned())
}

fn utc_syntax_error() -> String {
    "use UTC syntax YYYY-MM-DDTHH:MM:SS[.nnnnnnnnn]Z".into()
}

pub fn format_utc_nanos(value: i64) -> String {
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
        enrichments: state.enrichments.clone(),
        enrichment: None,
        capture_time: state.applied_capture_time,
        time_basis: state.applied_time_basis,
        grouping: nonempty(&state.grouping.applied),
    }
}

fn legacy_enrichment(source: &str) -> Vec<EnrichmentDefinition> {
    nonempty(source).map_or_else(Vec::new, |source| {
        vec![EnrichmentDefinition {
            id: EnrichmentStageId("legacy-stage-1".into()),
            source,
        }]
    })
}

fn valid_enrichments(stages: &[EnrichmentDefinition]) -> bool {
    if stages.len() > 32 {
        return false;
    }
    let mut ids = HashSet::with_capacity(stages.len());
    stages.iter().all(|stage| {
        !stage.id.0.is_empty()
            && stage.id.0.len() <= 128
            && !stage.source.is_empty()
            && stage.source.len() <= MAX_EDITOR_BYTES
            && ids.insert(stage.id.0.as_str())
    })
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

pub fn format_storage_bytes(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = 1024 * KIB;
    const GIB: u64 = 1024 * MIB;
    if bytes >= GIB {
        format!("{:.1} GiB", bytes as f64 / GIB as f64)
    } else if bytes >= MIB {
        format!("{:.1} MiB", bytes as f64 / MIB as f64)
    } else if bytes >= KIB {
        format!("{:.1} KiB", bytes as f64 / KIB as f64)
    } else {
        format!("{bytes} B")
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
        QueryPurpose::Grouping => &mut state.grouping,
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
        || state.grouping.pending_generation.is_some()
        || state.pending_time.is_some()
        || state.pending_recipe.is_some()
        || state.pending_source_change.is_some()
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

fn edit_setting(dialog: Option<&mut SettingsDialogState>, edit: impl FnOnce(&mut String)) {
    let Some(dialog) = dialog else {
        return;
    };
    let SettingsControl::Field(field) = dialog.focus else {
        return;
    };
    let value = match field {
        SettingsField::Provider => &mut dialog.draft.provider,
        SettingsField::Mode => &mut dialog.draft.mode,
        SettingsField::Thinking => &mut dialog.draft.thinking,
        SettingsField::RowCache => &mut dialog.draft.rows_mib,
        SettingsField::Membership => &mut dialog.draft.membership_mib,
        SettingsField::DiskTotal => &mut dialog.draft.disk_total_mib,
        SettingsField::IndexPerSource => &mut dialog.draft.index_per_source_mib,
        SettingsField::Theme
        | SettingsField::Delight
        | SettingsField::ReducedMotion
        | SettingsField::Ascii => return,
    };
    edit(value);
    mark_settings_pending(dialog);
}

fn setting_field(dialog: &SettingsDialogState) -> Option<&String> {
    let SettingsControl::Field(field) = dialog.focus else {
        return None;
    };
    Some(match field {
        SettingsField::Provider => &dialog.draft.provider,
        SettingsField::Mode => &dialog.draft.mode,
        SettingsField::Thinking => &dialog.draft.thinking,
        SettingsField::RowCache => &dialog.draft.rows_mib,
        SettingsField::Membership => &dialog.draft.membership_mib,
        SettingsField::DiskTotal => &dialog.draft.disk_total_mib,
        SettingsField::IndexPerSource => &dialog.draft.index_per_source_mib,
        SettingsField::Theme
        | SettingsField::Delight
        | SettingsField::ReducedMotion
        | SettingsField::Ascii => return None,
    })
}

fn mark_settings_pending(dialog: &mut SettingsDialogState) {
    dialog.status_kind = SettingsStatus::Pending;
    dialog.status = "Changes are not saved".into();
}

fn settings_controls(dialog: &SettingsDialogState) -> Vec<SettingsControl> {
    let mut controls = SettingsField::ALL
        .iter()
        .copied()
        .map(SettingsControl::Field)
        .collect::<Vec<_>>();
    controls.push(SettingsControl::Save);
    if dialog.details_scroll_limit > 0 {
        controls.push(SettingsControl::More);
    }
    controls
}

fn command_draft_field_mut(dialog: &mut CommandEnrichmentDialogState) -> &mut String {
    match dialog.selected_field {
        CommandEnrichmentField::Program => &mut dialog.program,
        CommandEnrichmentField::Arguments => &mut dialog.arguments,
        CommandEnrichmentField::Cwd => &mut dialog.cwd,
        CommandEnrichmentField::Environment => &mut dialog.environment,
    }
}

fn command_stage_draft(stage: &CommandEnrichmentStage) -> (String, String, String, String) {
    let CommandProgram::Exec { executable, args } = &stage.definition.program else {
        return (String::new(), String::new(), String::new(), String::new());
    };
    (
        executable.to_string_lossy().into_owned(),
        args.join("\n"),
        stage
            .definition
            .cwd
            .as_ref()
            .map_or_else(String::new, |path| path.to_string_lossy().into_owned()),
        stage
            .definition
            .environment
            .iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

fn command_candidate(
    dialog: &CommandEnrichmentDialogState,
) -> Result<CommandEnrichmentStage, String> {
    let executable = dialog.program.trim();
    if executable.is_empty() {
        return Err("Program is required".into());
    }
    if executable.len() > MAX_COMMAND_PROGRAM_BYTES {
        return Err("Program is too long".into());
    }
    let arguments = if dialog.arguments.is_empty() {
        Vec::new()
    } else {
        dialog
            .arguments
            .split('\n')
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    if arguments.len() > MAX_COMMAND_ARGUMENTS {
        return Err(format!(
            "Arguments are limited to {MAX_COMMAND_ARGUMENTS} lines"
        ));
    }
    let mut environment = BTreeMap::new();
    for line in dialog.environment.lines() {
        if line.is_empty() {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            return Err("Environment lines use KEY=value".into());
        };
        if key.is_empty() || key.contains('\0') || key.contains('=') {
            return Err("Environment keys must be non-empty and cannot contain =".into());
        }
        if environment
            .insert(key.to_owned(), value.to_owned())
            .is_some()
        {
            return Err(format!("Environment key {key} is duplicated"));
        }
    }
    if environment.len() > MAX_COMMAND_ENVIRONMENT {
        return Err(format!(
            "Environment is limited to {MAX_COMMAND_ENVIRONMENT} keys"
        ));
    }
    Ok(CommandEnrichmentStage {
        id: dialog.accepted.as_ref().map_or_else(
            || CommandEnrichmentStageId("command".into()),
            |stage| stage.id.clone(),
        ),
        definition: CommandDefinition {
            program: CommandProgram::Exec {
                executable: PathBuf::from(executable),
                args: arguments,
            },
            cwd: (!dialog.cwd.trim().is_empty()).then(|| PathBuf::from(dialog.cwd.trim())),
            environment,
            restart: RestartPolicy::Never,
        },
    })
}

fn settings_restart_status(context: &SettingsContext) -> String {
    let saved = &context.saved;
    let changed = saved.rows_mib.parse::<u64>().ok() != Some(context.applied_rows_mib)
        || saved.membership_mib.parse::<u64>().ok() != Some(context.applied_membership_mib)
        || saved.disk_total_mib.parse::<u64>().ok() != Some(context.applied_disk_total_mib)
        || saved.index_per_source_mib.parse::<u64>().ok()
            != Some(context.applied_index_per_source_mib);
    if changed {
        "saved; cache limits require restart (no raw data was evicted). Other active lvu processes with a different global disk cap can refuse index growth until all restart".into()
    } else {
        "saved and applied; existing investigations retain their agent session model".into()
    }
}

pub fn key_to_action(key: KeyEvent, focus: Focus) -> Action {
    if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
        return Action::None;
    }
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        return Action::Quit;
    }
    if key.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(
            focus,
            Focus::SearchEditor
                | Focus::AdvancedEditor
                | Focus::EnrichmentEditor
                | Focus::GroupingEditor
                | Focus::CommandEnrichment
                | Focus::SourceDialog
                | Focus::ViewDialog
                | Focus::AskAi
                | Focus::Investigation
                | Focus::Recipes
                | Focus::Bookmarks
                | Focus::Settings
                | Focus::TimeEditor
        )
    {
        match key.code {
            KeyCode::Char('a') => return Action::TextStartOfLine,
            KeyCode::Char('e') => return Action::TextEndOfLine,
            KeyCode::Char('k') => return Action::TextKillToEndOfLine,
            _ => {}
        }
    }
    if focus == Focus::Help {
        return match key.code {
            KeyCode::Esc | KeyCode::Char('?') => Action::ToggleHelp,
            KeyCode::Up | KeyCode::Char('k') => Action::ScrollHelp(-1),
            KeyCode::Down | KeyCode::Char('j') => Action::ScrollHelp(1),
            _ => Action::None,
        };
    }
    if focus == Focus::CommandEnrichment {
        return match key.code {
            KeyCode::Esc => Action::CancelEditor,
            KeyCode::Tab => Action::CommandEnrichmentNextField,
            KeyCode::Backspace => Action::CommandEnrichmentBackspace,
            KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                Action::SaveCommandEnrichment
            }
            KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                Action::PrepareCommandEnrichmentRun
            }
            KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::ALT) => {
                Action::CommandEnrichmentInput('\n')
            }
            KeyCode::Enter if key.modifiers.contains(KeyModifiers::CONTROL) => {
                Action::SaveCommandEnrichment
            }
            KeyCode::Enter if key.modifiers.contains(KeyModifiers::ALT) => {
                Action::PrepareCommandEnrichmentRun
            }
            KeyCode::Enter if key.modifiers.contains(KeyModifiers::SHIFT) => {
                Action::CommandEnrichmentInput('\n')
            }
            KeyCode::Enter => Action::ActivateCommandEnrichmentControl,
            KeyCode::Delete if key.modifiers.contains(KeyModifiers::ALT) => {
                Action::RemoveCommandEnrichment
            }
            KeyCode::Up => Action::ModalVertical(-1),
            KeyCode::Down => Action::ModalVertical(1),
            KeyCode::Char(character) => Action::CommandEnrichmentInput(character),
            _ => Action::None,
        };
    }
    if matches!(
        focus,
        Focus::SearchEditor
            | Focus::AdvancedEditor
            | Focus::EnrichmentEditor
            | Focus::GroupingEditor
    ) {
        if focus == Focus::EnrichmentEditor {
            return match key.code {
                KeyCode::Esc => Action::CancelEditor,
                KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::ALT) => {
                    Action::AddEnrichment
                }
                KeyCode::Char('e') if key.modifiers.contains(KeyModifiers::ALT) => {
                    Action::EditEnrichment
                }
                KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::ALT) => {
                    Action::RemoveEnrichment
                }
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::ALT) => {
                    Action::OpenCommandEnrichment
                }
                KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::ALT) => {
                    Action::EditorInput('\n')
                }
                KeyCode::Tab | KeyCode::BackTab => Action::MoveEnrichmentControl(
                    if matches!(key.code, KeyCode::BackTab)
                        || key.modifiers.contains(KeyModifiers::SHIFT)
                    {
                        -1
                    } else {
                        1
                    },
                ),
                KeyCode::Up => Action::MoveEnrichment(-1),
                KeyCode::Down => Action::MoveEnrichment(1),
                KeyCode::Enter => Action::ActivateEnrichmentControl,
                KeyCode::Char(' ') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    Action::ToggleEditorCompletion
                }
                KeyCode::Backspace => Action::EditorBackspace,
                KeyCode::Char(character) => Action::EditorInput(character),
                _ => Action::None,
            };
        }
        return match key.code {
            KeyCode::Esc => Action::CancelEditor,
            KeyCode::Char('a')
                if focus == Focus::EnrichmentEditor
                    && key.modifiers.contains(KeyModifiers::ALT) =>
            {
                Action::AddEnrichment
            }
            KeyCode::Char('e')
                if focus == Focus::EnrichmentEditor
                    && key.modifiers.contains(KeyModifiers::ALT) =>
            {
                Action::EditEnrichment
            }
            KeyCode::Char('r')
                if focus == Focus::EnrichmentEditor
                    && key.modifiers.contains(KeyModifiers::ALT) =>
            {
                Action::RemoveEnrichment
            }
            KeyCode::Char('c')
                if focus == Focus::EnrichmentEditor
                    && key.modifiers.contains(KeyModifiers::ALT) =>
            {
                Action::OpenCommandEnrichment
            }
            KeyCode::Char('j')
                if focus == Focus::EnrichmentEditor
                    && key.modifiers.contains(KeyModifiers::ALT) =>
            {
                Action::MoveEnrichment(1)
            }
            KeyCode::Char('k')
                if focus == Focus::EnrichmentEditor
                    && key.modifiers.contains(KeyModifiers::ALT) =>
            {
                Action::MoveEnrichment(-1)
            }
            KeyCode::Tab => Action::ToggleEditorCompletion,
            KeyCode::Up => Action::ModalVertical(-1),
            KeyCode::Down => Action::ModalVertical(1),
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
            KeyCode::Char('o') => Action::OpenContext,
            _ => Action::None,
        };
    }
    if focus == Focus::Recipes {
        return match key.code {
            KeyCode::Esc => Action::CancelEditor,
            KeyCode::Up => Action::MoveRecipe(-1),
            KeyCode::Down => Action::MoveRecipe(1),
            KeyCode::Enter => Action::SubmitRecipe,
            KeyCode::Char('x') => Action::RejectRecipeSuggestion,
            KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::ALT) => {
                Action::AdaptRecipeSuggestion
            }
            KeyCode::Char('g') if key.modifiers.contains(KeyModifiers::ALT) => {
                Action::RefreshRecipeSuggestions
            }
            KeyCode::Char('h') if key.modifiers.contains(KeyModifiers::ALT) => {
                Action::SelectRecipeMode(RecipeDialogMode::History)
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::ALT) => {
                Action::SelectRecipeMode(RecipeDialogMode::Update)
            }
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
            KeyCode::Char('e') if key.modifiers.contains(KeyModifiers::ALT) => {
                Action::SelectRecipeMode(RecipeDialogMode::Export)
            }
            KeyCode::Char(ch) => Action::RecipeInput(ch),
            _ => Action::None,
        };
    }
    if focus == Focus::TimeEditor {
        return match key.code {
            KeyCode::Char('t') if key.modifiers.contains(KeyModifiers::ALT) => {
                Action::OpenTimestampAssistant
            }
            KeyCode::Esc => Action::CancelEditor,
            KeyCode::Tab if key.modifiers.contains(KeyModifiers::SHIFT) => {
                Action::TimeMoveFocus(-1)
            }
            KeyCode::BackTab => Action::TimeMoveFocus(-1),
            KeyCode::Tab => Action::TimeMoveFocus(1),
            KeyCode::Up => Action::TimeMoveChoice(-1),
            KeyCode::Down => Action::TimeMoveChoice(1),
            KeyCode::Left => Action::TimeMoveCursor(-1),
            KeyCode::Right => Action::TimeMoveCursor(1),
            KeyCode::Enter => Action::TimeOpenFocused,
            KeyCode::Backspace => Action::TimeBackspace,
            KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::ALT) => {
                Action::AroundSelected
            }
            KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::ALT) => {
                Action::SetTimeBasis(TimeBasis::Capture)
            }
            KeyCode::Char('e') if key.modifiers.contains(KeyModifiers::ALT) => {
                Action::SetTimeBasis(TimeBasis::Event)
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::ALT) => {
                Action::SetTimeBasis(TimeBasis::Extracted)
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
            KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                Action::RefreshDiscovery
            }
            KeyCode::Char('f') if key.modifiers.contains(KeyModifiers::ALT) => {
                Action::SelectSourceKind(SourceKind::File)
            }
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::ALT) => {
                Action::SelectSourceKind(SourceKind::Command)
            }
            KeyCode::Down => Action::ModalVertical(1),
            KeyCode::Up => Action::ModalVertical(-1),
            KeyCode::Left => Action::MoveSourceMode(-1),
            KeyCode::Right => Action::MoveSourceMode(1),
            KeyCode::Char(' ') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                Action::CompleteSourcePath
            }
            KeyCode::Tab | KeyCode::BackTab => Action::ToggleSourceControlFocus,
            KeyCode::Enter => Action::ActivateSourceControl,
            KeyCode::Backspace => Action::SourceBackspace,
            KeyCode::Char(character) => Action::SourceInput(character),
            _ => Action::None,
        };
    }
    if focus == Focus::ViewDialog {
        return match key.code {
            KeyCode::Up if key.modifiers.contains(KeyModifiers::ALT) => {
                Action::ReorderViewSource(-1)
            }
            KeyCode::Down if key.modifiers.contains(KeyModifiers::ALT) => {
                Action::ReorderViewSource(1)
            }
            KeyCode::Up => Action::MoveViewSource(-1),
            KeyCode::Down => Action::MoveViewSource(1),
            KeyCode::Char('m') if key.modifiers.contains(KeyModifiers::ALT) => {
                Action::SelectViewDialogMode(ViewDialogMode::Sources)
            }
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
            KeyCode::Tab => Action::ToggleDialogScrollFocus,
            KeyCode::Down => Action::ModalVertical(1),
            KeyCode::Up => Action::ModalVertical(-1),
            KeyCode::Esc => Action::CancelEditor,
            KeyCode::Enter => Action::SubmitAskAi,
            KeyCode::Backspace => Action::EditorBackspace,
            KeyCode::Char('t') if key.modifiers.contains(KeyModifiers::ALT) => {
                Action::OpenTimestampAssistant
            }
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
    if focus == Focus::Storage {
        return match key.code {
            KeyCode::Esc => Action::CancelEditor,
            KeyCode::Tab => Action::ToggleDialogScrollFocus,
            KeyCode::Up => Action::ModalVertical(-1),
            KeyCode::Down => Action::ModalVertical(1),
            KeyCode::Char('k') => Action::MoveStorage(-1),
            KeyCode::Char('j') => Action::MoveStorage(1),
            KeyCode::Char('r') => Action::RefreshStorage,
            KeyCode::Char('c') => Action::ClearStorage,
            KeyCode::Char('q') => Action::Quit,
            _ => Action::None,
        };
    }
    if focus == Focus::Bookmarks {
        return match key.code {
            KeyCode::Esc => Action::CancelEditor,
            KeyCode::Up => Action::MoveBookmark(-1),
            KeyCode::Down => Action::MoveBookmark(1),
            KeyCode::Char('e') if key.modifiers.contains(KeyModifiers::ALT) => {
                Action::EditBookmarkNote
            }
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::ALT) => {
                Action::DeleteBookmark
            }
            KeyCode::Enter => Action::SubmitBookmark,
            KeyCode::Backspace => Action::BookmarkBackspace,
            KeyCode::Char(ch) => Action::BookmarkInput(ch),
            _ => Action::None,
        };
    }
    if focus == Focus::Context {
        return match key.code {
            KeyCode::Esc | KeyCode::Char('o') => Action::CancelEditor,
            KeyCode::Up | KeyCode::Char('k') => Action::MoveContext(-1),
            KeyCode::Down | KeyCode::Char('j') => Action::MoveContext(1),
            KeyCode::Char('g') => Action::MoveContext(0),
            KeyCode::Char('q') => Action::Quit,
            _ => Action::None,
        };
    }
    if focus == Focus::Settings {
        return match key.code {
            KeyCode::Esc => Action::CancelEditor,
            KeyCode::Up | KeyCode::BackTab => Action::MoveSettings(-1),
            KeyCode::Down | KeyCode::Tab => Action::MoveSettings(1),
            KeyCode::Char(' ') | KeyCode::Enter => Action::ActivateSettings,
            KeyCode::Backspace => Action::SettingsBackspace,
            KeyCode::Char(character) => Action::SettingsInput(character),
            _ => Action::None,
        };
    }
    if matches!(focus, Focus::Logs | Focus::Selector) && key.modifiers.contains(KeyModifiers::ALT) {
        match key.code {
            KeyCode::Char('s') => return Action::StopCapture,
            KeyCode::Char('r') => return Action::RestartCapture,
            _ => {}
        }
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
    if focus == Focus::Details {
        return match key.code {
            KeyCode::Up => Action::ScrollDetails(-1),
            KeyCode::Down => Action::ScrollDetails(1),
            KeyCode::Tab => Action::CycleFocus,
            KeyCode::Char('d') => Action::ToggleDetails,
            KeyCode::Char('?') => Action::ToggleHelp,
            KeyCode::Char('q') => Action::Quit,
            _ => Action::None,
        };
    }
    match key.code {
        KeyCode::Char('q') => Action::Quit,
        KeyCode::Tab => Action::CycleFocus,
        KeyCode::Char(']') => Action::NextView,
        KeyCode::Char('[') => Action::PreviousView,
        KeyCode::Down | KeyCode::Char('j') => Action::MoveLine(1),
        KeyCode::Left => Action::MoveHorizontal(-8),
        KeyCode::Right => Action::MoveHorizontal(8),
        KeyCode::Char('0') => Action::ResetHorizontal,
        KeyCode::Up | KeyCode::Char('k') => Action::MoveLine(-1),
        KeyCode::Char('g') => Action::Top,
        KeyCode::Char('G') => Action::End,
        KeyCode::Char('d') => Action::ToggleDetails,
        KeyCode::Char('o') => Action::OpenContext,
        KeyCode::Char('b') => Action::ToggleBookmark,
        KeyCode::Char('B') => Action::OpenBookmarks,
        KeyCode::Char('v') => Action::OpenViewDialog,
        KeyCode::Char('?') => Action::ToggleHelp,
        KeyCode::Char('f') => Action::ToggleFollow,
        KeyCode::Char('/') => Action::OpenSearch,
        KeyCode::Char('p') => Action::OpenAdvanced,
        KeyCode::Char('e') => Action::OpenEnrichment,
        KeyCode::Char('m') => Action::OpenGrouping,
        KeyCode::Char('S') => Action::OpenStorage,
        KeyCode::Char(',') => Action::OpenSettings,
        KeyCode::Enter => Action::ToggleExpandedGroup,
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

#[cfg(test)]
mod completion_literal_regression {
    #[test]
    fn control_characters_use_python_unicode_escape_syntax() {
        assert_eq!(
            super::python_string_literal("\0\u{1b}\u{85}"),
            "'\\u0000\\u001b\\u0085'"
        );
    }
}

#[cfg(test)]
mod time_form_tests {
    use super::*;

    #[test]
    fn numeric_offsets_normalize_without_losing_fractional_nanoseconds() {
        let utc = parse_utc_nanos("2024-02-29T12:34:56.123456789Z").unwrap();
        assert_eq!(
            parse_utc_nanos("2024-02-29T14:34:56.123456789+02:00"),
            Ok(utc)
        );
        assert_eq!(
            parse_utc_nanos("2024-02-29T07:04:56.123456789-05:30"),
            Ok(utc)
        );
        assert_eq!(format_utc_nanos(utc), "2024-02-29T12:34:56.123456789Z");
    }

    #[test]
    fn invalid_calendar_offsets_and_overflow_are_rejected() {
        assert!(parse_utc_nanos("2023-02-29T00:00:00Z").is_err());
        assert!(parse_utc_nanos("2024-01-01T00:00:00+24:00").is_err());
        assert!(parse_utc_nanos("9999-12-31T23:59:59.999999999-23:59").is_err());
    }

    #[test]
    fn time_focus_does_not_claim_global_navigation_keys() {
        for code in [
            KeyCode::PageUp,
            KeyCode::PageDown,
            KeyCode::Home,
            KeyCode::End,
        ] {
            assert_eq!(
                key_to_action(KeyEvent::new(code, KeyModifiers::NONE), Focus::TimeEditor),
                Action::None
            );
            assert_eq!(
                key_to_action(
                    KeyEvent::new(code, KeyModifiers::CONTROL),
                    Focus::TimeEditor
                ),
                Action::None
            );
        }
    }
}

#[cfg(test)]
mod command_activity_tests {
    use super::*;

    #[test]
    fn command_activity_includes_commit_after_dialog_closes_but_not_review() {
        let (_, sources, views) = crate::fixture::FixtureProvider::demo();
        let mut app = App::new(sources, views, true);
        let (provider, _, _) = crate::fixture::FixtureProvider::demo();
        app.handle(Action::OpenCommandEnrichment, &provider);
        for state in [
            CommandEnrichmentRunState::Saving,
            CommandEnrichmentRunState::Preparing,
            CommandEnrichmentRunState::Running,
            CommandEnrichmentRunState::SavingResults,
        ] {
            app.command_enrichment_dialog.as_mut().unwrap().run_state = state;
            assert!(app.command_work_pending());
        }
        app.command_enrichment_dialog.as_mut().unwrap().run_state =
            CommandEnrichmentRunState::Ready;
        assert!(!app.command_work_pending());
        app.pending_command_enrichment_runs
            .insert(1, ("view".into(), 1));
        app.command_enrichment_dialog = None;
        assert!(app.command_work_pending());
        app.pending_command_enrichment_runs.clear();
        assert!(!app.command_work_pending());
    }
}
