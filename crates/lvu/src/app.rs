use std::{
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    path::PathBuf,
    time::{Duration, Instant},
};

use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use lvu_core::{CommandDefinition, CommandProgram, FieldCorrelation, RestartPolicy};
use ratatui::layout::Rect;

use crate::component::{
    AgentDefaults, Appearance, Clock, Component, Ctx, Event as ComponentEvent, LayerId, NO_ROWS,
    Open, Outcome, RawEvent, Surface, ViewEvent,
};
use crate::components::Layers;
use crate::provider::{DisplayRow, GapDirection, RowId, RowProvider, ViewportRequest};
use crate::text_edit::{CursorBank, TextTarget};
use crate::theme::ThemeId;

pub const MAX_EDITOR_BYTES: usize = 16 * 1024;
pub const MAX_PENDING_QUERY_REQUESTS: usize = 32;
pub const TIMESTAMP_PROMPT: &str = "Use the prepared typed schema, sample values and coverage to derive exactly one field named timestamp_utc from an existing usable timestamp column. When that inline evidence is sufficient, do not read files or invoke tools. If no prepared context is supplied, inspect the fixed snapshot schema and bounded samples first. Do not extract a JSON field from raw when its value is available in a usable named column. Fall back to raw extraction only for unstructured timestamps or documented projection/type conflicts and explain why. Do not substitute capture time for an event timestamp. Return a Polars enrichment expression producing UTC RFC3339 strings in the exact format %Y-%m-%dT%H:%M:%S%.6fZ. Use str.extract when needed, str.to_datetime or str.strptime with an explicit input format and strict=False, then dt.convert_time_zone('UTC') and dt.strftime. Preserve raw and prior enrichment stages. Missing, malformed, or ambiguous timestamps must produce null. Never infer a missing year, day/month order, epoch unit, or timezone; explain what user-provided information is needed instead. Explicit numeric offsets must be normalized to UTC. Explain the detected source field/input format, timezone evidence, output format, and unmatched cases. Only propose the enrichment; do not modify files.";

pub const SEARCH_DEBOUNCE: Duration = Duration::from_millis(300);
pub(crate) const MAX_AI_PROMPT_BYTES: usize = 8 * 1024;
const MAX_INVESTIGATION_MESSAGES: usize = 64;
const MAX_INVESTIGATION_MESSAGE_BYTES: usize = 16 * 1024;
/// Opened structured-value paths a view remembers (§8.11).
pub const MAX_EXPANDED_PATHS: usize = 256;
const MAX_COMPLETION_ROWS: usize = 128;
/// How deep a nested path may be before the picker stops offering it.
const MAX_COMPLETION_PATH_DEPTH: usize = 4;
const MAX_COMPLETION_FIELDS: usize = 128;
const MAX_COMPLETION_VALUES: usize = 256;
const MAX_COMPLETION_TEXT_BYTES: usize = 512;
const MAX_COMMAND_PROGRAM_BYTES: usize = 4096;
const MAX_COMMAND_ARGUMENTS: usize = 128;
const MAX_COMMAND_ENVIRONMENT: usize = 128;
pub(crate) const MAX_COMMAND_FIELD_BYTES: usize = 16 * 1024;
pub(crate) const MAX_COMMAND_REQUESTS: usize = 8;
/// Smallest repeated run that collapses by default.
pub const DEFAULT_FOLD_MINIMUM_RUN: usize = 3;
/// Expanded runs remembered per view. Expansion is a user choice about a
/// handful of runs, not a second index.
pub const MAX_FOLD_EXPANDED: usize = 256;
/// Widest lookback window a view may ask for. A scope is a bounded amount of
/// interleaving to tolerate, not a reason to keep every run open forever.
pub const MAX_FOLD_LOOKBACK: usize = 4_096;

/// Lookback windows the Folding dialog offers, in the order it lists them.
/// Zero is `Adjacent`; the rest are how many rows of other keys may intervene.
pub const FOLD_LOOKBACK_CHOICES: [usize; 5] = [0, 2, 8, 32, 128];
/// Minimum runs the Folding dialog offers.
pub const FOLD_MINIMUM_RUN_CHOICES: [usize; 5] = [2, 3, 5, 10, 25];

/// The folding policy a view currently asks its provider for.
fn fold_request(state: &ViewState) -> crate::provider::FoldRequest {
    crate::provider::FoldRequest {
        enabled: state.fold_enabled,
        minimum_run: effective_fold_minimum_run(state.fold_minimum_run),
        key_column: state.fold_key_column.clone(),
        scope: match state.fold_lookback {
            0 => crate::provider::FoldScopeRequest::Adjacent,
            window => crate::provider::FoldScopeRequest::Lookback(window),
        },
        normalisation: state.fold_normalisation,
        expanded: state.fold_expanded.clone(),
    }
}

fn effective_fold_minimum_run(value: usize) -> usize {
    if value == 0 {
        DEFAULT_FOLD_MINIMUM_RUN
    } else {
        value.max(2)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Focus {
    Selector,
    Logs,
    Details,
    /// A converted component is on top; which one is `layers.stack.last()`
    /// (component-model.md §6.4). Checked at exactly three bridge sites: input
    /// dispatch (`terminal.rs`), render dispatch (`ui.rs`) and dismissal.
    Layer,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AskAiKind {
    Filter,
    Enrichment,
    Recipe,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AskControl {
    Kind,
    Prompt,
    Submit,
    Apply,
    /// Re-run the same request against the wider bounded sample. Offered once,
    /// and only when the first sample left rows out or the answer said it
    /// needed more (`docs/larger-ask-sample.md`).
    Widen,
    Cancel,
    /// The Proposal/Activity panes. Focusing them scrolls the body (§8.8); it
    /// is not a button, so it is never drawn in the action row.
    More,
}

/// A task Ask was opened *for*. The kind is then already decided and the
/// request is prepared, so the dialog explains the task instead of offering an
/// irrelevant choice (`docs/dialog-system.md` §3 header, §12.17).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AskTask {
    RecognizeTimestamp,
}

impl AskTask {
    /// Title suffix: `Ask 🧠 · Recognize timestamp` (§7.1 `Noun · object`).
    pub fn object(self) -> &'static str {
        match self {
            AskTask::RecognizeTimestamp => "Recognize timestamp",
        }
    }

    /// The header summary line: what submitting this will actually do.
    pub fn summary(self) -> &'static str {
        match self {
            AskTask::RecognizeTimestamp => {
                "Derives one timestamp_utc field in UTC RFC3339 from a timestamp already in the data"
            }
        }
    }

    /// The help row: the prefilled request is a starting point, not a fixed
    /// instruction the user is merely being shown.
    pub fn help(self) -> &'static str {
        match self {
            AskTask::RecognizeTimestamp => {
                "The request below is a prepared starting point; edit it before submitting. Nothing changes until you review and apply the proposal."
            }
        }
    }

    pub fn message(self) -> &'static str {
        match self {
            AskTask::RecognizeTimestamp => "review the prepared request, then submit",
        }
    }
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
    /// Set when Ask was opened for a prepared task rather than by `A`.
    pub task: Option<AskTask>,
    pub focus: AskControl,
    pub kind_dropdown: bool,
    pub kind_selected: usize,
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
    /// What the bounded preparation admitted for this request. `None` until
    /// preparation finishes.
    pub sample: Option<AskSample>,
    /// The sample the proposal on screen was built from, kept beside it so two
    /// answers to the same question can be told apart.
    pub answer_sample: Option<AskSample>,
    /// The one standard-tier answer retained while its wider retry is current.
    /// There are exactly two tiers, so one immutable slot is the complete
    /// bounded history rather than the start of an unbounded transcript.
    pub previous_answer: Option<AskAnswer>,
    /// The agent said the sample was not enough.
    pub needs_more: bool,
    pub review_scroll: u16,
    pub review_scroll_limit: u16,
    /// Internal scroll of the multi-line Request field, in visual rows.
    pub prompt_scroll: u16,
    /// Cells the Request field wraps at, recorded by the renderer so Up/Down
    /// move by the rows the user can actually see.
    pub prompt_width: u16,
}

/// Which bounded sample a request was prepared against. Mirrors
/// `lvu_view::export::assistance::SampleTier`, which the shell cannot name
/// here (§2.2): the dialog only needs to say which one an answer used.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum AskSampleTier {
    #[default]
    Standard,
    Wider,
}

impl AskSampleTier {
    pub fn label(self) -> &'static str {
        match self {
            AskSampleTier::Standard => "standard",
            AskSampleTier::Wider => "wider",
        }
    }
}

/// What the bounded preparation actually admitted, so the dialog can say it
/// and an answer can record it (`docs/larger-ask-sample.md`).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AskSample {
    pub used: u64,
    pub available: u64,
    pub sources: usize,
    pub tier: AskSampleTier,
}

/// Immutable presentation evidence for the answer immediately preceding a
/// wider retry. Applying always reads the live fields on `AskAiDialogState`,
/// never this snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AskAnswer {
    pub request: String,
    pub expression: String,
    pub explanation: String,
    pub sample: AskSample,
    pub recipe: Option<RecipeConfig>,
}

impl AskSample {
    /// Whether the preparation left rows out. Known before the prompt is sent
    /// and regardless of what comes back.
    pub fn omitted(self) -> bool {
        self.available > self.used
    }

    /// `128 of 4,201,993 rows · 3 sources · standard`
    pub fn summary(self) -> String {
        format!(
            "{} of {} rows · {} source{} · {}",
            self.used,
            self.available,
            self.sources,
            if self.sources == 1 { "" } else { "s" },
            self.tier.label()
        )
    }
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
        /// `true` re-runs the same request against the wider tier.
        wider: bool,
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

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum InvestigationControl {
    /// §8.6: the `Saved` cell of the `New │ Saved` segmented control.
    Saved,
    /// The `New` cell of the same control.
    ModeNew,
    #[default]
    Prompt,
    Submit,
    /// `[ Open ]`, the saved list's action.
    Open,
    New,
    /// The transcript pane, which takes focus so it can be scrolled.
    More,
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
    pub focus: InvestigationControl,
    pub input: String,
    pub progress: String,
    pub selected: usize,
    pub items: Vec<InvestigationItem>,
    pub investigation_id: Option<String>,
    pub session_id: Option<String>,
    pub snapshot_dir: Option<String>,
    pub manifest_path: Option<String>,
    pub messages: VecDeque<String>,
    pub review_scroll: u16,
    pub review_scroll_limit: u16,
    /// Which segment the body is showing: the transcript, or the saved list.
    pub saved_mode: bool,
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

/// Why a view exists.
///
/// Persisted per view and set once when the view is created. It is never
/// derived from the display name, so renaming a view cannot change whether its
/// definition may be edited.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ViewRole {
    /// The source's permanent unfiltered view. Its filter, enrichment, time
    /// window and source membership are fixed; everything about how it is
    /// presented is not.
    Canonical,
    /// An ordinary editable view.
    #[default]
    Derived,
}

/// A request to create the derived view an edit to a canonical view implies.
///
/// The candidate identity is chosen once per editing burst, so debounced typing
/// supersedes one candidate instead of proposing a view per keystroke.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ViewForkRequest {
    pub origin_view_id: String,
    pub candidate_view_id: String,
    pub source_id: String,
    pub source_ids: Vec<String>,
    pub name: String,
}

/// A derived view whose query succeeded and which is waiting to be persisted.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReadyFork {
    pub candidate_view_id: String,
    pub origin_view_id: String,
    pub source_id: String,
    pub name: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ForkStage {
    /// Waiting for the runtime to register the candidate view.
    Requested,
    /// A query for the candidate is in flight.
    Querying,
    /// The query succeeded; waiting for persistence to be accepted.
    Persisting,
}

/// The edit a fork will replay onto its candidate once the candidate exists.
///
/// Staging the edit rather than running it immediately is what makes the fork
/// safe: the candidate has to be registered with the query runtime before any
/// query for it can be submitted, and until then nothing about the origin has
/// changed.
#[derive(Clone, Debug)]
enum ForkEdit {
    Editor {
        purpose: QueryPurpose,
        draft: String,
        enrichment_editing: Option<EnrichmentStageId>,
    },
    Time {
        window: Option<CaptureTimeRange>,
        policy: Option<CaptureTimePolicy>,
        basis: TimeBasis,
    },
    /// The clock travels with the recipe: `apply_recipe_in_place` resolves a
    /// rolling capture-time policy against it, and the replay happens a round
    /// trip after the seam was called (§6.5).
    Recipe {
        config: Box<RecipeConfig>,
        now_nanos: i64,
    },
}

/// What `Views::install_fork` finished, so the shell can do its half: carry an
/// open dialog onto the new view and select it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct InstalledFork {
    pub(crate) origin_view_id: String,
    pub(crate) candidate_view_id: String,
}

#[derive(Clone, Debug)]
pub(crate) struct PendingFork {
    origin_view_id: String,
    candidate_view_id: String,
    source_id: String,
    name: String,
    /// The origin's interaction revision when the fork was proposed. A later
    /// interaction with the origin abandons the candidate rather than
    /// installing a view the user has moved on from.
    interaction_revision: u64,
    stage: ForkStage,
    purpose: QueryPurpose,
    edit: ForkEdit,
}

/// A bookmark jump waiting for its record to become locatable.
#[derive(Clone, Debug)]
struct PendingJump {
    view_id: String,
    row: RowId,
    /// Bounded: a record that never resolves stops being chased.
    attempts: usize,
}

/// Where `o` came from (docs/raw-context-as-jump.md): the view and record the
/// user was looking at, and the dialog they pressed it from, so `o` again can
/// put all three back. One deep, shell-owned, never persisted: a glance, not
/// state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RawContextOrigin {
    /// The filtered view the jump left.
    pub view_id: String,
    /// The anchor's source's All events view, where the jump landed.
    pub raw_view_id: String,
    /// The record that was selected, held by identity.
    pub anchor: RowId,
    /// The dialog to re-push on return, if `o` was pressed inside one.
    pub layer: Option<Open>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ViewDialogMode {
    Sources,
    Blank,
    Clone,
    Rename,
}
impl ViewDialogMode {
    pub const ALL: [Self; 4] = [Self::Blank, Self::Clone, Self::Rename, Self::Sources];
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
    /// A submission this editor made which forked instead of applying in
    /// place. The query belongs to the candidate view, so `pending_generation`
    /// stays empty here and there is nothing else on this state to say that a
    /// save is outstanding — which is how a step editor on All events came to
    /// report nothing at all and accept a second Save on the same draft.
    /// Cleared wherever the fork ends: installed, discarded or superseded.
    pub fork_pending: bool,
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

/// Layer one of the enrichment dialog: the ordered step list and its actions.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum EnrichmentControl {
    #[default]
    Steps,
    Add,
    Edit,
    Remove,
    ExternalCommand,
}

impl EnrichmentControl {
    pub(crate) const ALL: [Self; 5] = [
        Self::Steps,
        Self::Add,
        Self::Edit,
        Self::Remove,
        Self::ExternalCommand,
    ];
}

/// Layer two of the enrichment dialog: one step's expression, its previewed
/// input record, the resulting output, and the save/cancel transaction.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum EnrichmentStepControl {
    #[default]
    Expression,
    /// The record this step reads: a list whose selection is the preview.
    Input,
    /// The accepted chain's output for that record; scrollable, not editable.
    Output,
    Save,
    Remove,
}

impl EnrichmentStepControl {
    /// Remove only exists while an accepted step is being edited; a disabled
    /// control must not occupy traversal or space.
    pub(crate) fn traversal(editing: bool) -> &'static [Self] {
        if editing {
            &[
                Self::Expression,
                Self::Input,
                Self::Output,
                Self::Save,
                Self::Remove,
            ]
        } else {
            &[Self::Expression, Self::Input, Self::Output, Self::Save]
        }
    }
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
    pub selected: Option<RowId>,
    pub follow: bool,
    pub last_total: usize,
    pub provider_revision: u64,
    pub viewport_height: usize,
    pub search: EditorState,
    pub advanced: EditorState,
    pub enrichment: EditorState,
    pub enrichments: Vec<EnrichmentDefinition>,
    /// Run state per command step of `enrichments`, keyed by stage id. A
    /// step whose id is absent has never been saved for a run.
    pub command_steps: BTreeMap<String, CommandStepState>,
    pub enrichment_selected: usize,
    pub enrichment_editing: Option<EnrichmentStageId>,
    pub enrichment_control: EnrichmentControl,
    pub grouping: EditorState,
    pub applied_capture_time: Option<CaptureTimeRange>,
    /// User-authored policy. Rolling refreshes update the resolved range above
    /// without changing the definition revision.
    pub applied_capture_time_policy: Option<CaptureTimePolicy>,
    pub applied_time_basis: TimeBasis,
    /// Accepted `TimeFieldSelection` token backing `TimeBasis::Selected`.
    pub applied_time_field: Option<String>,
    pub time_start_draft: String,
    pub time_end_draft: String,
    pub time_recent_draft: String,
    pub time_error: Option<String>,
    pub time_draft_touched: bool,
    pub time_window_draft: TimeWindowChoice,
    /// The quiet period `{`/`}` treat as a gap, in seconds. Per view for the
    /// same reason: what counts as "quiet" is a property of the stream being
    /// read, not of the person reading it. Zero reads as
    /// [`DEFAULT_GAP_THRESHOLD_SECONDS`].
    pub time_gap_threshold_seconds: u64,
    /// What the last gap jump found, for the status line. Cleared by the next
    /// action, like every other transient notice.
    pub gap_notice: Option<String>,
    /// The bookmark the Bookmarks list last had selected (§7.3), so the list
    /// reopens on it after a Raw context jump and return.
    pub bookmark_selected: usize,
    pub time_basis_draft: TimeBasis,
    /// Token the Time dialog is proposing; promoted to `applied_time_field`
    /// only when the user applies the dialog.
    pub time_field_draft: Option<String>,
    pub time_start_date_draft: String,
    pub time_start_clock_draft: String,
    pub time_start_zone_draft: String,
    pub time_end_date_draft: String,
    pub time_end_clock_draft: String,
    pub time_end_zone_draft: String,
    pub time_structured_draft_present: bool,
    pub applied_query_revision: u64,
    pub desired_query_revision: u64,
    pub exact_field: Option<FieldCorrelation>,
    pub pinned_columns: Vec<String>,
    pub color_field: Option<String>,
    /// Accepted predicate colour rules: what the rows on screen were painted
    /// with. Display-only, so an invalid draft can never disturb it.
    pub color_rules: Vec<ColorRule>,
    /// The Colour rules dialog's working list, kept per view like every other
    /// draft so closing and reopening the dialog resumes the edit.
    pub color_rules_draft: Vec<ColorRule>,
    pub color_rules_error: Option<String>,
    /// The generation/revision of a candidate repaint. Kept separate from the
    /// Advanced editor even though both execute through the same native query.
    pub pending_color_rules: Option<(u64, u64)>,
    pub field_picker_selected: usize,
    /// Which of the Fields dialog's controls has focus (§8.8).
    pub field_picker_control: FieldPickerControl,
    pub field_picker_row: Option<RowId>,
    pub expanded_groups: HashSet<RowId>,
    /// Structured-value paths (`http.tags`) the user has opened in Details or
    /// Fields (§8.11). Per view and per path; session memory, like
    /// `expanded_groups`. Bounded by [`MAX_EXPANDED_PATHS`].
    pub expanded_paths: std::collections::BTreeSet<String>,
    /// Which row of the Details pane's structured view the cursor is on.
    pub details_cursor: usize,
    /// Set by a cursor move and consumed by the next frame, which scrolls
    /// only then to keep the cursor row on screen; a plain scroll clears it
    /// so the pane goes where the user sent it.
    pub details_reveal: bool,
    /// Repeated-pattern folding for this view. Off by default; reversible
    /// presentation only, so nothing here changes a record or a filter.
    pub fold_enabled: bool,
    pub fold_minimum_run: usize,
    /// The column whose value is the fold key. `None` is the derived `pattern`
    /// column, which is what folding used before a column could be chosen, so a
    /// view that never opened the Folding dialog behaves exactly as before.
    pub fold_key_column: Option<String>,
    /// Rows of other keys a run may span. Zero is adjacent-only.
    pub fold_lookback: usize,
    /// Only consulted for the derived `pattern` column.
    pub fold_normalisation: crate::provider::FoldNormalisation,
    /// Folded runs the user has expanded, named by their first member. Ordered
    /// so persistence round-trips deterministically.
    pub fold_expanded: Vec<RowId>,
    /// What folding is currently doing, as the provider reports it. Derived
    /// every frame; never persisted.
    pub fold_summary: Option<crate::provider::FoldSummary>,
    pub(crate) user_interaction_revision: u64,
    ai_definition_revision: u64,
    desired_constraints: QueryConstraints,
    desired_capture_time_policy: Option<CaptureTimePolicy>,
    desired_time_basis: TimeBasis,
    /// The recipe awaiting its query's completion. `pub(crate)` because the
    /// Recipes layer attaches the suggestion outcome to it after
    /// `Views::apply_recipe` accepts (§4.2 routes the acceptance back).
    pub(crate) pending_recipe: Option<PendingRecipe>,
    pending_time: Option<PendingTime>,
    pending_enrichment_mutation: Option<PendingEnrichmentMutation>,
    rolling_refresh_due: bool,
}

impl ViewState {
    /// The quiet period gap navigation looks for.
    pub fn gap_threshold_seconds(&self) -> u64 {
        match self.time_gap_threshold_seconds {
            0 => DEFAULT_GAP_THRESHOLD_SECONDS,
            value => value,
        }
    }
}

impl ViewState {
    /// The definition revision of a command step, `0` before its first save.
    pub fn command_revision(&self, stage_id: &str) -> u64 {
        self.command_steps
            .get(stage_id)
            .map_or(0, |state| state.revision)
    }

    /// Whether a submitted time window is still in flight. The Time layer shows
    /// `Updating` while it is, and keeps the last applied window active.
    pub fn time_update_pending(&self) -> bool {
        self.pending_time.is_some()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PendingEnrichmentMutation {
    Add,
    Edit,
    Remove,
    Reaffirm,
    /// A command step was inserted or replaced by the External command
    /// dialog; on acceptance the dialog bumps that step's run revision.
    CommandSave,
    /// Steps changed position; nothing to close.
    Reorder,
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
pub(crate) struct PendingRecipe {
    revision: u64,
    interaction_revision: u64,
    pinned_columns: Vec<String>,
    color_field: Option<String>,
    capture_time_policy: Option<CaptureTimePolicy>,
    time_basis: TimeBasis,
    /// The outcome to report once the query lands, attached by the Recipes
    /// layer so an accepted suggestion is only recorded on real acceptance.
    pub(crate) suggestion: Option<RecipeOutcome>,
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
    pub command_steps: BTreeMap<String, CommandStepState>,
    pub applied_grouping: String,
    pub grouping_draft: String,
    pub grouping_error: Option<String>,
    pub applied_capture_time: Option<CaptureTimeRange>,
    pub applied_capture_time_policy: Option<CaptureTimePolicy>,
    pub applied_time_basis: TimeBasis,
    /// Accepted `TimeFieldSelection` token backing `TimeBasis::Selected`.
    pub applied_time_field: Option<String>,
    pub time_start_draft: String,
    pub time_end_draft: String,
    pub time_recent_draft: String,
    pub time_error: Option<String>,
    pub time_draft_touched: bool,
    pub time_window_draft: TimeWindowChoice,
    /// Zero means "never set": the built-in default applies. Persisting the
    /// sentinel rather than the resolved value is what lets the default change
    /// later without rewriting every stored view.
    pub time_gap_threshold_seconds: u64,
    pub time_basis_draft: TimeBasis,
    /// Token the Time dialog is proposing; promoted to `applied_time_field`
    /// only when the user applies the dialog.
    pub time_field_draft: Option<String>,
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
    /// When this view was last selected, ordering the views of one source so a
    /// restart reopens the one that was in use. Zero means never chosen.
    pub selected_at: u64,
    pub pinned_columns: Vec<String>,
    pub color_field: Option<String>,
    /// Accepted predicate colour rules. Persisted per view like the pins and
    /// the colour field, because they are that view's presentation.
    pub color_rules: Vec<ColorRule>,
    pub fold_enabled: bool,
    pub fold_minimum_run: usize,
    pub fold_key_column: Option<String>,
    pub fold_lookback: usize,
    pub fold_normalisation: crate::provider::FoldNormalisation,
    pub fold_expanded: Vec<RowId>,
    pub exact_field: Option<FieldCorrelation>,
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

/// One step of a view's ordered enrichment chain (docs/dialog-system.md
/// §12.5). An expression step compiles and runs inside the query; a command
/// step names an external program whose durable results join the frame as
/// `<name>.<field>` columns for every step, filter and grouping after it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EnrichmentDefinition {
    pub id: EnrichmentStageId,
    /// Expression step: `/regex with (?P<name>...) groups/` or `name = Python
    /// Polars Expr`. Command step: the output prefix, a bare identifier.
    pub source: String,
    /// `Some` for a command step: the program that produces the outputs.
    pub command: Option<CommandDefinition>,
}

impl EnrichmentDefinition {
    pub fn expression(id: impl Into<String>, source: impl Into<String>) -> Self {
        Self {
            id: EnrichmentStageId(id.into()),
            source: source.into(),
            command: None,
        }
    }

    pub fn command(
        id: impl Into<String>,
        name: impl Into<String>,
        definition: CommandDefinition,
    ) -> Self {
        Self {
            id: EnrichmentStageId(id.into()),
            source: name.into(),
            command: Some(definition),
        }
    }

    pub fn is_command(&self) -> bool {
        self.command.is_some()
    }

    /// The prefix a command step's outputs carry: `geo` → `geo.city`,
    /// `geo.status`. Expression steps have no prefix.
    pub fn output_prefix(&self) -> Option<&str> {
        self.command.as_ref().map(|_| self.source.as_str())
    }

    /// The command stage a command step is, for the controller and the dialog.
    pub fn command_stage(&self) -> Option<CommandEnrichmentStage> {
        self.command
            .as_ref()
            .map(|definition| CommandEnrichmentStage {
                id: CommandEnrichmentStageId(self.id.0.clone()),
                definition: definition.clone(),
            })
    }
}

/// Per-step run state of a command step (§12.6): the revision of the saved
/// definition the dialog and the controller fence on, and the immutable
/// reference to the last published result set. The definition itself is the
/// chain's; this is what a run knows about it.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CommandStepState {
    pub revision: u64,
    pub publication: Option<String>,
}

/// The output prefix a command step defaults to, which is also what the one
/// pre-chain command step was called.
pub const DEFAULT_COMMAND_STEP_NAME: &str = "command";

/// A valid output prefix: an identifier that cannot collide with a protected
/// column or read as an expression.
pub fn valid_command_step_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name != "raw"
        && !name.starts_with("_lvu_")
        && name.chars().enumerate().all(|(index, ch)| {
            ch == '_' || ch.is_ascii_alphabetic() || (index > 0 && ch.is_ascii_digit())
        })
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
    /// The output prefix (§12.6): results land as `<name>.<field>`.
    Name,
    #[default]
    Program,
    Arguments,
    Cwd,
    Environment,
}

impl CommandEnrichmentField {
    pub(crate) const ALL: [Self; 5] = [
        Self::Name,
        Self::Program,
        Self::Arguments,
        Self::Cwd,
        Self::Environment,
    ];
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
    /// The chain step this dialog edits or will insert.
    pub stage_id: String,
    /// Where a new step goes in the chain; ignored once the step exists.
    pub insert_at: usize,
    pub name: String,
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
    /// A save is a chain mutation through the query seam, not a request here.
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
        stage_id: CommandEnrichmentStageId,
        review_token: String,
    },
    Cancel {
        generation: u64,
        view_id: String,
    },
}

/// What the Fields dialog wants counted over the whole view, and when to stop.
///
/// The dialog already describes the selected field over a bounded sample, which
/// is instant and stays on screen. This asks for the same figures over every
/// record the view holds; the answer replaces the sample when it arrives and
/// never blocks it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FieldStatsRequest {
    Resolve {
        generation: u64,
        view_id: String,
        /// The column to count. Nested paths arrive already extracted.
        path: String,
        /// What the app has decided this field is, from the sample it has in
        /// hand. The engine counts under this rule rather than classifying,
        /// so the two figures cannot disagree about what a number is.
        kind: crate::field_stats::ValueType,
    },
    /// The selection moved off the field, or the dialog closed.
    Cancel { generation: u64 },
}

/// Whole-view figures for one field, as the pane should show them.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WholeViewStats {
    pub generation: u64,
    pub view_id: String,
    pub path: String,
    /// Records counted, which is what the heading names.
    pub records: u64,
    pub present: u64,
    /// Present values of the type the app named.
    pub matching: u64,
    pub distinct: u64,
    /// `distinct` stopped being exact at the engine's cap.
    pub distinct_capped: bool,
    pub top: Vec<(String, u64)>,
    pub minimum: Option<String>,
    pub maximum: Option<String>,
}
pub use crate::components::correlation::{CorrelationRequest, CorrelationSourceChoice};

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct QueryConstraints {
    pub text: Option<TextConstraint>,
    pub exact_field: Option<FieldCorrelation>,
    pub advanced_polars: Option<String>,
    /// Ordered stages. Later definitions may reference fields from earlier ones.
    pub enrichments: Vec<EnrichmentDefinition>,
    /// Compatibility input for pre-chain adapters. New UI requests leave this unset.
    pub enrichment: Option<String>,
    /// Fixed time window for `time_basis`, half-open `[start_unix_nanos, end_unix_nanos)`.
    pub capture_time: Option<CaptureTimeRange>,
    pub time_basis: TimeBasis,
    /// `lvu_live::TimeFieldSelection::to_token()` for `TimeBasis::Selected`.
    /// Unset for every other basis.
    pub time_field: Option<String>,
    /// Display-only continuation prefix-regex. Physical membership is unchanged.
    pub grouping: Option<String>,
    /// Ordered predicate colour rules. Display-only: a rule decides how a row
    /// is painted, never whether it is in the view. The first rule that matches
    /// a row wins, so the order the user put them in is the precedence.
    pub color_rules: Vec<ColorRule>,
}

/// One "when <predicate> then <colour>" rule.
///
/// The predicate is written in the search box's own language — a literal,
/// `field: value`, `/regex/flags`, or a `pl.…` expression — and is compiled and
/// evaluated by the query engine, never by a second matcher in the terminal.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ColorRule {
    pub predicate: String,
    pub color: RuleColor,
}

/// At most this many rules per view. Each one is another predicate the engine
/// evaluates over every batch, so the list is bounded like every other queue.
pub const MAX_COLOR_RULES: usize = 16;

/// The colours a rule may paint with.
///
/// A closed set rather than free RGB: every entry is contrast-checked against
/// both themes and lifted into the xterm cube on a 256-colour terminal, so a
/// rule cannot produce something unreadable, and the token persists as a stable
/// name rather than as three numbers whose contrast nobody re-checks.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum RuleColor {
    #[default]
    Red,
    Orange,
    Yellow,
    Green,
    Cyan,
    Blue,
    Purple,
    Magenta,
}

impl RuleColor {
    pub const ALL: [Self; 8] = [
        Self::Red,
        Self::Orange,
        Self::Yellow,
        Self::Green,
        Self::Cyan,
        Self::Blue,
        Self::Purple,
        Self::Magenta,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Red => "red",
            Self::Orange => "orange",
            Self::Yellow => "yellow",
            Self::Green => "green",
            Self::Cyan => "cyan",
            Self::Blue => "blue",
            Self::Purple => "purple",
            Self::Magenta => "magenta",
        }
    }

    pub fn parse(token: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|color| color.label() == token)
    }

    /// The hue this colour names, in degrees. The theme turns it into
    /// something readable on its own background and colour depth.
    pub(crate) fn hue(self) -> f64 {
        match self {
            Self::Red => 0.0,
            Self::Orange => 30.0,
            Self::Yellow => 55.0,
            Self::Green => 130.0,
            Self::Cyan => 185.0,
            Self::Blue => 220.0,
            Self::Purple => 270.0,
            Self::Magenta => 310.0,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum TimeBasis {
    #[default]
    Capture,
    Event,
    /// UTC RFC3339 strings from the accepted timestamp_utc enrichment.
    Extracted,
    /// A field the user declared explicitly. The declaration itself travels in
    /// `QueryConstraints::time_field` as a `TimeFieldSelection` token, so this
    /// enum stays `Copy` and every existing basis path is untouched.
    Selected,
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

/// A timestamp field the recognizer offered, in the plain form the Time dialog
/// needs. `lvu-live` owns recognition and `lvu-query` owns validation, and this
/// crate can depend on neither, so `lvu-app` builds these and hands them over.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TimeFieldCandidate {
    /// `lvu_live::TimeFieldSelection::to_token()`. Opaque here.
    pub token: String,
    /// The field itself: `ts`, `raw prefix`, `column: latency_start`.
    pub label: String,
    /// How its values are read: `RFC3339`, `epoch milliseconds`, `auto`.
    pub reading: String,
    /// Validated share of the sampled records this reading resolves.
    pub coverage_percent: Option<u8>,
    /// `TimeFieldSelection::assumptions()`. A non-empty list means the reading
    /// rests on a guess, and the dialog must not accept it unasked.
    pub assumptions: Vec<String>,
    /// Why recognition or validation refused this reading. A blocked candidate
    /// is still shown, with its reason, but cannot be chosen.
    pub blocked: Option<String>,
    /// The chrono format a text reading is read with, so the dialog can show
    /// and edit it without knowing how a token is spelled.
    pub text_format: Option<String>,
    /// Other complete readings of the same field, offered as the override when
    /// this one needs an assumption. The dialog picks among them; it never
    /// edits a token, because token semantics belong to the recognizer.
    pub alternatives: Vec<TimeFieldCandidate>,
}

impl TimeFieldCandidate {
    /// Index of the first reading that could actually be applied, counting the
    /// candidate itself as 0. A candidate the recognizer blocked is usually
    /// blocked *because* it needs an assumption, and its alternatives are
    /// exactly the readings that supply one — so it stays choosable as long as
    /// one of them survives.
    pub fn first_usable(&self) -> Option<usize> {
        if self.blocked.is_none() {
            return Some(0);
        }
        self.alternatives
            .iter()
            .position(|reading| reading.blocked.is_none())
            .map(|index| index + 1)
    }
}

/// Recognizer output for the active view, fenced by `generation`.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TimeRecognition {
    pub sampled_records: usize,
    pub candidates: Vec<TimeFieldCandidate>,
    pub diagnostics: Vec<String>,
    /// Event time of the anchored row under the accepted selection, when there
    /// is one. `lvu` cannot read a token itself, so this is computed alongside.
    pub anchored_selected_nanos: Option<i64>,
    pub scanning: bool,
    /// What the edited format actually read, as a reading in its own right, so
    /// the confirmation step shows a measured coverage rather than the user's
    /// hope. `blocked` carries a format the query layer refused outright.
    pub probe: Option<TimeFieldCandidate>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TimeRecognitionRequest {
    pub generation: u64,
    pub view_id: String,
    /// Accepted token, so the anchored event time can be read in the same pass.
    pub token: Option<String>,
    pub anchored_row: Option<RowId>,
    /// A time format the user edited, to be measured against the same sample.
    /// `lvu` cannot parse a format itself, so the reading it would produce is
    /// asked for rather than assumed.
    pub text_format_probe: Option<TextFormatProbe>,
}

/// One edited text format, awaiting measurement.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TextFormatProbe {
    /// Enrichment column the format is for, without the display prefix.
    pub column: String,
    pub format: String,
}

/// Short name of a built-in basis, used in both the field and its dropdown.
pub fn time_basis_label(basis: TimeBasis) -> &'static str {
    match basis {
        TimeBasis::Capture => "Capture",
        TimeBasis::Event => "Recognized",
        TimeBasis::Extracted => "Extracted",
        TimeBasis::Selected => "Chosen field",
    }
}

/// What the Time dialog is offering to apply. Only `Absolute` and `Recent`
/// survive as an applied *policy*; the three data-relative choices resolve
/// against the dataset at the moment they are applied and become an absolute
/// window, which is why they fill the start and end fields rather than hiding
/// what they mean (§12.4).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum TimeWindowChoice {
    #[default]
    All,
    Absolute,
    /// The last `n` seconds by the *clock*: a rolling window that ages rows out
    /// even when nothing new arrives.
    Recent(u64),
    /// Every record the view holds: first event to last event, in the current
    /// basis. Distinct from `All`, which applies no window at all — this one
    /// writes the dataset's own bounds into the fields to be narrowed.
    DataFirstToLast,
    /// The last `n` seconds *of data*: measured back from the newest record
    /// rather than from now, so it is not empty on a dataset that stopped an
    /// hour ago.
    DataRecent(u64),
    /// A window centred on the record the dialog opened over, `n` seconds
    /// either side. The width travels in the choice because it *is* the
    /// choice: "around selected" without a width does not describe a window.
    AroundSelected(u64),
}

/// The one place a window choice becomes words. Rendering, the dropdown and
/// the status line all read it, so what a user picks and what they are told
/// they picked cannot drift apart.
pub fn time_window_label(choice: TimeWindowChoice) -> String {
    match choice {
        TimeWindowChoice::All => "All time".into(),
        TimeWindowChoice::Absolute => "Absolute".into(),
        TimeWindowChoice::Recent(seconds) if matches!(seconds, 300 | 900 | 3600) => {
            format!("Last {} by clock", format_capture_duration(seconds))
        }
        TimeWindowChoice::Recent(seconds) => {
            format!("Custom last {} by clock", format_capture_duration(seconds))
        }
        TimeWindowChoice::DataFirstToLast => "First → last event".into(),
        TimeWindowChoice::DataRecent(seconds) => {
            format!("Last {} of data", format_capture_duration(seconds))
        }
        TimeWindowChoice::AroundSelected(seconds) => {
            format!("± {} around selected", format_capture_duration(seconds))
        }
    }
}

/// The ± width `AroundSelected` uses until the user edits it. Thirty seconds
/// either side is a minute of context, which is what the hard-coded window was
/// before it became editable.
pub const DEFAULT_AROUND_SECONDS: u64 = 30;

/// The quiet period `{`/`}` treat as a gap until the user says otherwise.
///
/// A minute is long enough that an ordinary lull in a busy log is not a "gap"
/// worth jumping to, and short enough that a restart, a deploy or a stalled
/// producer shows up as one. It is editable in the Time dialog and reported in
/// the status line, so the number is never something the user has to guess.
pub const DEFAULT_GAP_THRESHOLD_SECONDS: u64 = 60;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecipeItem {
    pub id: String,
    pub revision: String,
    pub name: String,
    /// When this revision was saved. `None` for a recipe written before the
    /// stored document carried the field; §12.9's date column shows those as
    /// `—` rather than guessing.
    pub saved_at_unix_nanos: Option<i64>,
    pub config: RecipeConfig,
    pub incompatibility: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecipeSuggestion {
    pub recipe_id: String,
    pub evidence: Vec<String>,
    pub missing_fields: Vec<String>,
}

/// §12.11: the Fields dialog's focus order — the list, then its two actions.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum FieldPickerControl {
    #[default]
    List,
    Pin,
    Color,
    /// Follow this field's value into every source that carries it.
    Correlate,
    /// §8.12 one-key actions from the selected value.
    Filter,
    Exclude,
    Fold,
    /// Offered when the record's fields have not arrived: raw context is the
    /// one thing still worth doing with the record, so it is a button rather
    /// than a remembered key.
    Context,
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

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum RecipeDialogControl {
    /// Retained for the Alt shortcuts that switch mode directly. §12.9 retired
    /// the row of mode buttons, so nothing renders one any more.
    Mode(RecipeDialogMode),
    Input,
    #[default]
    List,
    Apply,
    Refresh,
    Adapt,
    Reject,
    /// `[ Save ]`, `[ Update ]`, `[ History ]`: the modes that survived as
    /// actions rather than as a mode bar.
    Save,
    Update,
    History,
    /// `[ More ▾ ]`: the rest, behind one anchored menu (§8.3, §10).
    More,
    /// Cancel an editable mode and return to the list.
    Cancel,
}

/// The entries `[ More ▾ ]` offers, in order.
pub const RECIPE_MORE_ITEMS: [(&str, RecipeDialogControl); 3] = [
    (
        "Import…",
        RecipeDialogControl::Mode(RecipeDialogMode::Import),
    ),
    (
        "Export…",
        RecipeDialogControl::Mode(RecipeDialogMode::Export),
    ),
    ("Refresh", RecipeDialogControl::Refresh),
];

impl RecipeDialogMode {
    pub const ALL: [Self; 6] = [
        Self::Browse,
        Self::Save,
        Self::Import,
        Self::Export,
        Self::History,
        Self::Update,
    ];
    pub fn is_list(self) -> bool {
        matches!(self, Self::Browse | Self::History)
    }
    pub fn is_editable(self) -> bool {
        matches!(self, Self::Save | Self::Import | Self::Export)
    }
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PathCompletionRequest {
    pub generation: u64,
    pub draft: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiscoveryItem {
    pub key: String,
    pub label: String,
    pub detail: String,
    pub status: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DiscoveryUiRequest {
    Scan { generation: u64 },
    Cancel { generation: u64 },
    Select { generation: u64, key: String },
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
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Action {
    /// Push a converted layer (component-model.md §4.1). One variant for every
    /// dialog, replacing the per-dialog `Open*` variants as each is converted.
    Open(Open),
    /// A palette command the top layer declared, delivered as
    /// `Event::Command` (§4.3).
    Command(LayerId, crate::command_palette::CommandId),
    /// Terminal input while `focus == Focus::Layer` (§6.4). It outlived the
    /// last legacy focus as the bridge every layer is reached through; it goes
    /// with `Outcome::Legacy`/`Defer` once those hand-offs become `ctx` calls.
    Raw(RawEvent),
    /// A converted layer's reviewed proposal, applied by the shell because
    /// every destination — the advanced-filter draft, the enrichment step
    /// editor, `apply_recipe_to_active_view` — is still legacy (§6.4). The Ask
    /// layer stays on the stack so a refusal lands in its message row.
    ApplyAskProposal {
        kind: AskAiKind,
        expression: String,
        recipe: Option<Box<RecipeConfig>>,
        outcome: Option<RecipeOutcome>,
    },
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
    /// §8.11: move the Details cursor over the structured rows; scrolls the
    /// pane instead when the record is not a tree.
    DetailsCursor(i32),
    /// Expand (`Some(true)`), collapse (`Some(false)`) or toggle (`None`) the
    /// container under the Details cursor.
    DetailsPath(Option<bool>),
    ResetDetails,
    /// `o`: jump to the selected record (or `anchor`) in its source's All
    /// events view, remembering where it came from; with an origin already
    /// held in that view, return instead (docs/raw-context-as-jump.md).
    /// `layer` is the dialog the jump closed, re-pushed on return.
    RawContext {
        anchor: Option<RowId>,
        layer: Option<Open>,
    },
    /// Return to the view, record and dialog `o` came from.
    ReturnFromRawContext,
    ToggleBookmark,
    /// Migration-only: selecting a bookmarked record in its canonical view is
    /// the shell's job — it switches view, moves the selection and chases the
    /// row. `fallback_view` is the layer's own view, used when the canonical
    /// one has not been restored yet.
    JumpToRecord {
        row: RowId,
        fallback_view: String,
    },
    ScrollDialog(i32),
    ToggleDialogScrollFocus,
    ModalVertical(i32),
    ScrollHoveredDialog(i32),
    ToggleFollow,
    StopCapture,
    RestartCapture,
    /// Jump to the next or previous quiet period longer than the view's gap
    /// threshold. Presentation-only navigation: it moves the selection and
    /// says what it found, and changes no constraint.
    JumpToGap(GapDirection),
    ToggleExpandedGroup,
    ToggleFolding,
    CollapseAllFolds,
    OpenSource,
    /// Migration-only (§6.4): the Recipes layer's `Alt-a` hands the selection
    /// to the agent dialog, which is converted last (§6.3 step 12). It becomes
    /// `Replace(Open::Ask { .. })` then, and the variant goes.
    AdaptRecipe {
        item: Box<RecipeItem>,
        suggestion: Box<RecipeSuggestion>,
    },
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SettingsValues {
    pub provider: String,
    pub mode: String,
    pub thinking: String,
    pub theme: ThemeId,
    /// Fixed UTC offset token the log viewport formats times in.
    pub display_zone: String,
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
    pub effective_display_zone: String,
    pub display_zone_source: &'static str,
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

pub const MAX_BOOKMARKS: usize = 128;
/// Provider ticks a bookmark jump waits for its record before giving up.
const MAX_JUMP_ATTEMPTS: usize = 240;
pub const MAX_BOOKMARK_NOTE_BYTES: usize = 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Bookmark {
    pub id: RowId,
    pub note: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BookmarkDialogControl {
    List,
    Input,
    /// §12.10 `[ Go to ]`: select the record in its source's canonical
    /// All events view. Honest since bookmarks became source-scoped.
    Goto,
    Context,
    Edit,
    Save,
    Delete,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SettingsRequest {
    pub generation: u64,
    pub values: SettingsValues,
}

/// Why the seam would not apply a recipe. A recipe is a whole definition, so
/// there is nothing partial to keep: the caller words the refusal and the
/// applied view is untouched.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecipeRejected {
    /// The recipe's enrichment stages have duplicate, oversized or invalid IDs.
    /// The offending view's enrichment error says so.
    InvalidStages,
    /// The query submission queue is full; the last applied definition stays.
    QueueFull,
}

/// Why the seam would not apply an edit. Either way the caller keeps its draft
/// and words its own message, rather than the seam guessing how to report it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SubmitRefused {
    /// The query submission queue is full; the last applied window stays.
    QueueFull,
}

/// The product's shared view state and the one query seam (component-model.md
/// §2.3). These fields moved verbatim out of `App`; the bodies that touch them
/// are unchanged except that `self.views.states` became `self.views.states`.
///
/// The public surface below is what a component may use through `Ctx`. The
/// `#[doc(hidden)]` fields under it are the legacy shell's, reachable only
/// inside this crate, and they shrink to nothing as dialogs convert. §2.5
/// records the compromise: `Ctx` hands out `&mut Views` wholesale, so review —
/// not the compiler — is what keeps a component off the by-id paths.
#[derive(Debug)]
pub struct Views {
    #[doc(hidden)]
    pub(crate) items: Vec<ViewItem>,
    #[doc(hidden)]
    pub(crate) selected: usize,
    #[doc(hidden)]
    pub(crate) states: HashMap<String, ViewState>,
    #[doc(hidden)]
    pub(crate) requests: HashMap<(String, QueryPurpose), QueryRequest>,
    #[doc(hidden)]
    pub(crate) next_generation: u64,
    /// Explicit per-view role. Absent means [`ViewRole::Derived`]: a view whose
    /// role is unknown is editable, never accidentally immutable. It lives here
    /// because the seam has to know whether an edit may be applied in place —
    /// and, since the forking step, because the seam is what stages the derived
    /// view an in-place refusal implies (§2.3).
    #[doc(hidden)]
    pub(crate) roles: HashMap<String, ViewRole>,
    /// The fork subsystem: one candidate per origin, plus the three queues the
    /// runtime drains. An edit that a canonical view refuses becomes a derived
    /// view, and every step of that — proposing the candidate, replaying the
    /// edit onto it, discarding it, installing it — is the seam's, because all
    /// of it is view lifecycle and none of it is shell surface (§2.3).
    #[doc(hidden)]
    pub(crate) pending_forks: HashMap<String, PendingFork>,
    #[doc(hidden)]
    pub(crate) fork_requests: VecDeque<ViewForkRequest>,
    #[doc(hidden)]
    pub(crate) ready_forks: VecDeque<ReadyFork>,
    #[doc(hidden)]
    pub(crate) fork_discards: VecDeque<String>,
    #[doc(hidden)]
    pub(crate) next_fork_sequence: u64,
    /// Bookmarks belong to the source whose records they mark, so every view of
    /// that source shows the same set and filtering one cannot hide or lose it.
    /// Resolving them for a view is a join with `states`, which is why they
    /// live beside it rather than on `App` (§2.3).
    #[doc(hidden)]
    pub(crate) bookmarks: HashMap<String, Vec<Bookmark>>,
}

impl Default for Views {
    fn default() -> Self {
        Self {
            items: Vec::new(),
            selected: 0,
            states: HashMap::new(),
            requests: HashMap::new(),
            // Generations and fork sequences are 1-based: zero is the "never
            // issued" value the completion fences compare against.
            next_generation: 1,
            roles: HashMap::new(),
            pending_forks: HashMap::new(),
            fork_requests: VecDeque::new(),
            ready_forks: VecDeque::new(),
            fork_discards: VecDeque::new(),
            next_fork_sequence: 1,
            bookmarks: HashMap::new(),
        }
    }
}

impl Views {
    /// Bookmarks a view shows: those of every source it contains, in source
    /// order. Kept as one list so the dialog can index it directly.
    pub fn bookmarks_for_view(&self, view_id: &str) -> Vec<Bookmark> {
        self.source_ids(view_id)
            .into_iter()
            .filter_map(|source_id| self.bookmarks.get(&source_id))
            .flat_map(|bookmarks| bookmarks.iter().cloned())
            .collect()
    }

    /// Replaces one source's bookmarks. Used when a workspace is loaded.
    pub fn set_source_bookmarks(&mut self, source_id: &str, bookmarks: Vec<Bookmark>) {
        self.bookmarks.insert(
            source_id.to_owned(),
            bookmarks.into_iter().take(MAX_BOOKMARKS).collect(),
        );
    }

    pub fn source_bookmarks(&self, source_id: &str) -> &[Bookmark] {
        self.bookmarks.get(source_id).map_or(&[], Vec::as_slice)
    }

    /// Bookmarks are shared by every view of a source, so changing them makes
    /// each of those views worth saving.
    pub fn note_bookmark_change(&mut self, source_id: &str) {
        let affected: Vec<String> = self
            .items
            .iter()
            .map(|view| view.id.clone())
            .filter(|view_id| self.source_ids(view_id).iter().any(|id| id == source_id))
            .collect();
        for view_id in affected {
            if let Some(state) = self.states.get_mut(&view_id) {
                state.user_interaction_revision = state.user_interaction_revision.saturating_add(1);
            }
        }
    }

    /// Adds or removes the record's bookmark, and says which happened.
    pub fn toggle_bookmark(&mut self, id: RowId) -> &'static str {
        let source_id = id.source_id.clone();
        let set = self.bookmarks.entry(source_id.clone()).or_default();
        let message = if let Some(index) = set.iter().position(|bookmark| bookmark.id == id) {
            set.remove(index);
            "bookmark removed"
        } else if set.len() < MAX_BOOKMARKS {
            set.push(Bookmark {
                id,
                note: String::new(),
            });
            "bookmarked; B opens bookmarks and notes"
        } else {
            "bookmark limit reached (128 per source)"
        };
        self.note_bookmark_change(&source_id);
        message
    }

    /// Replaces one bookmark's note. `false` when the bookmark is gone.
    pub fn set_bookmark_note(&mut self, id: &RowId, note: String) -> bool {
        let updated = self
            .bookmarks
            .get_mut(&id.source_id)
            .and_then(|set| set.iter_mut().find(|bookmark| &bookmark.id == id))
            .map(|bookmark| bookmark.note = note)
            .is_some();
        if updated {
            self.note_bookmark_change(&id.source_id);
        }
        updated
    }

    pub fn remove_bookmark(&mut self, id: &RowId) {
        if let Some(set) = self.bookmarks.get_mut(&id.source_id) {
            set.retain(|bookmark| &bookmark.id != id);
        }
        self.note_bookmark_change(&id.source_id);
    }

    pub fn items(&self) -> &[ViewItem] {
        &self.items
    }

    pub fn active_id(&self) -> Option<&str> {
        self.items.get(self.selected).map(|view| view.id.as_str())
    }

    pub fn active(&self) -> Option<&ViewState> {
        self.active_id().and_then(|id| self.states.get(id))
    }

    /// One named view's state. The enrichment layers hold a `view_id` across
    /// a request rather than assuming the active view has not changed (§7.3).
    pub fn state(&self, view_id: &str) -> Option<&ViewState> {
        self.states.get(view_id)
    }

    pub fn state_mut(&mut self, view_id: &str) -> Option<&mut ViewState> {
        self.states.get_mut(view_id)
    }

    /// The definition revision the assistance path fences proposals against.
    pub fn definition_revision(&self, view_id: &str) -> Option<u64> {
        self.states
            .get(view_id)
            .map(|state| state.ai_definition_revision)
    }

    pub fn active_mut(&mut self) -> Option<&mut ViewState> {
        let id = self.active_id()?.to_owned();
        self.states.get_mut(&id)
    }

    /// The selected view's identity, name and owning source. A component that
    /// edits the view list needs all three, and `active_id` alone cannot give
    /// them without a by-id lookup the seam is meant to prevent.
    pub fn active_item(&self) -> Option<&ViewItem> {
        self.items.get(self.selected)
    }

    /// Every source this view draws from. A view with no recorded membership
    /// draws from its owning source alone.
    pub fn source_ids(&self, view_id: &str) -> Vec<String> {
        self.states
            .get(view_id)
            .filter(|state| !state.source_ids.is_empty())
            .map(|state| state.source_ids.clone())
            .unwrap_or_else(|| {
                self.items
                    .iter()
                    .find(|view| view.id == view_id)
                    .map(|view| vec![view.source_id.clone()])
                    .unwrap_or_default()
            })
    }

    /// The capture-time seam. Records the desired window, policy and basis on
    /// the view and enqueues one query, leaving the applied view untouched if
    /// the queue is full so the last good window stays usable.
    pub fn submit_capture_time(
        &mut self,
        view_id: &str,
        window: Option<CaptureTimeRange>,
        policy: Option<CaptureTimePolicy>,
        basis: TimeBasis,
    ) -> Result<u64, SubmitRefused> {
        let Some(state) = self.states.get_mut(view_id) else {
            return Err(SubmitRefused::QueueFull);
        };
        state.desired_constraints.capture_time = window;
        state.desired_capture_time_policy = policy;
        state.desired_time_basis = basis;
        state.desired_constraints.time_basis = basis;
        // The token travels with the basis it belongs to and only with it.
        state.desired_constraints.time_field = (basis == TimeBasis::Selected)
            .then(|| state.time_field_draft.clone())
            .flatten();
        state.time_error = None;
        match self.apply_desired_time(view_id) {
            Some(revision) => Ok(revision),
            None => {
                let state = self.states.get_mut(view_id).expect("view state");
                state.desired_constraints = applied_constraints(state);
                state.desired_capture_time_policy = state.applied_capture_time_policy;
                state.desired_time_basis = state.applied_time_basis;
                Err(SubmitRefused::QueueFull)
            }
        }
    }

    pub fn role(&self, view_id: &str) -> ViewRole {
        self.roles.get(view_id).copied().unwrap_or_default()
    }

    /// True when the view's definition is fixed. Presentation is never fixed.
    pub fn definition_is_fixed(&self, view_id: &str) -> bool {
        self.role(view_id) == ViewRole::Canonical
    }

    /// The source the active view captures. A recipe outcome is recorded
    /// against the source it was judged on, not against the view.
    pub fn active_source_id(&self) -> Option<&str> {
        self.items
            .get(self.selected)
            .map(|view| view.source_id.as_str())
    }

    /// What saving a recipe from this view would store: its *accepted*
    /// definition and presentation, never a draft. This is deliberately not
    /// `App::persistent_view_state`, which also carries bookmarks and the
    /// selection stamp — both shell-owned and neither part of a recipe.
    pub fn applied_recipe_config(&self, view_id: &str) -> Option<RecipeConfig> {
        let state = self.states.get(view_id)?;
        Some(RecipeConfig {
            search: state.search.applied.clone(),
            advanced: state.advanced.applied.clone(),
            enrichment: state.enrichment.applied.clone(),
            enrichments: state.enrichments.clone(),
            pinned_columns: state.pinned_columns.clone(),
            color_field: state.color_field.clone(),
            capture_time: match state.applied_capture_time_policy {
                Some(CaptureTimePolicy::Recent { .. }) => None,
                _ => state.applied_capture_time,
            },
            capture_time_policy: state.applied_capture_time_policy,
            time_basis: state.applied_time_basis,
            grouping: state.grouping.applied.clone(),
        })
    }

    /// The recipe seam. A recipe is a whole definition, so this replaces every
    /// draft and the desired constraints at once and enqueues one query; an
    /// invalid or refused recipe leaves the applied view exactly as it was.
    ///
    /// Moved from `App::apply_recipe_to_view`, with the definition-fixed guard
    /// that `App::apply_recipe_to_active_view` did first: a canonical view
    /// becomes the view the recipe describes rather than being edited in place,
    /// and staging that fork is still the shell's (§2.3).
    pub fn apply_recipe(
        &mut self,
        view_id: &str,
        config: RecipeConfig,
        now_nanos: i64,
    ) -> Result<u64, RecipeRejected> {
        if !valid_enrichments(&config.enrichments) {
            if let Some(state) = self.states.get_mut(view_id) {
                state.enrichment.error = Some(
                    "recipe enrichment stages have duplicate, oversized, or invalid IDs".into(),
                );
            }
            return Err(RecipeRejected::InvalidStages);
        }
        if self.definition_is_fixed(view_id) {
            return if self.stage_recipe_fork(view_id, config, now_nanos) {
                Ok(self
                    .states
                    .get(view_id)
                    .map_or(0, |state| state.desired_query_revision))
            } else {
                Err(RecipeRejected::QueueFull)
            };
        }
        self.apply_recipe_in_place(view_id, config, now_nanos)
    }

    /// The in-place half, reached directly by the shell once a fork candidate
    /// exists: the candidate is derived, so the fixed-definition guard above
    /// would never fire for it and asking again would be misleading.
    pub(crate) fn apply_recipe_in_place(
        &mut self,
        view_id: &str,
        config: RecipeConfig,
        now_nanos: i64,
    ) -> Result<u64, RecipeRejected> {
        if !valid_enrichments(&config.enrichments) {
            if let Some(state) = self.states.get_mut(view_id) {
                state.enrichment.error = Some(
                    "recipe enrichment stages have duplicate, oversized, or invalid IDs".into(),
                );
            }
            return Err(RecipeRejected::InvalidStages);
        }
        let view_id = view_id.to_owned();
        let policy = config
            .capture_time_policy
            .or(config.capture_time.map(CaptureTimePolicy::Absolute));
        let resolved_capture_time =
            policy.and_then(|value| resolve_capture_time_policy(value, now_nanos));
        let Some(state) = self.states.get_mut(&view_id) else {
            return Err(RecipeRejected::QueueFull);
        };
        state.user_interaction_revision = state.user_interaction_revision.saturating_add(1);
        state.ai_definition_revision = state.ai_definition_revision.saturating_add(1);
        state.search.draft = config.search.clone();
        state.advanced.draft = config.advanced.clone();
        // The step editor's draft is the last expression step; a command
        // step's `source` is its output name, not something to edit there.
        let last_expression = config
            .enrichments
            .iter()
            .rev()
            .find(|stage| !stage.is_command());
        state.enrichment.draft =
            last_expression.map_or_else(|| config.enrichment.clone(), |stage| stage.source.clone());
        state.enrichment_editing = last_expression.map(|stage| stage.id.clone());
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
            exact_field: state.exact_field.clone(),
            advanced_polars: nonempty(&config.advanced),
            enrichments: if config.enrichments.is_empty() {
                legacy_enrichment(&config.enrichment)
            } else {
                config.enrichments.clone()
            },
            enrichment: None,
            time_field: None,
            capture_time: resolved_capture_time,
            // A recipe carries no field token, so a `Selected` basis it cannot
            // describe degrades to capture time rather than silently reading
            // an undeclared field.
            time_basis: recipe_time_basis(config.time_basis),
            grouping: nonempty(&config.grouping),
            // A recipe describes a definition, not a palette: the view keeps
            // the colour rules the user gave it.
            color_rules: state.color_rules.clone(),
        };
        state.desired_constraints = constraints;
        state.desired_capture_time_policy = policy;
        state.desired_time_basis = recipe_time_basis(config.time_basis);
        let Some(revision) = self.enqueue_value(&view_id, QueryPurpose::Advanced, None) else {
            let state = self.states.get_mut(&view_id).expect("view state");
            state.desired_constraints = applied_constraints(state);
            state.desired_capture_time_policy = state.applied_capture_time_policy;
            state.desired_time_basis = state.applied_time_basis;
            return Err(RecipeRejected::QueueFull);
        };
        let state = self.states.get_mut(&view_id).expect("view state");
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
        Ok(revision)
    }

    /// Records a draft against the view and enqueues one query for `purpose`.
    /// Moved verbatim out of `App::enqueue_query_value`, which keeps only the
    /// fixed-definition guard that forks instead (§2.3) and then delegates
    /// here. Step 7 takes the remaining editors through this seam.
    pub(crate) fn enqueue_value(
        &mut self,
        view_id: &str,
        purpose: QueryPurpose,
        value: Option<String>,
    ) -> Option<u64> {
        let key = (view_id.to_owned(), purpose);
        if !self.requests.contains_key(&key) && self.requests.len() >= MAX_PENDING_QUERY_REQUESTS {
            editor_mut(self.states.get_mut(view_id).expect("view state"), purpose).error =
                Some("query submission queue is full; draft was preserved".into());
            return None;
        }
        let generation = self.next_generation;
        self.next_generation = self.next_generation.saturating_add(1);
        let state = self.states.get_mut(view_id).expect("view state");
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
                        "Provide a named expression or /regex/ before saving this step".into(),
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
                        command: None,
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
        self.requests.insert(
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

    /// Enqueue a whole replacement enrichment chain (a removal or a reorder),
    /// as opposed to `enqueue_value`'s single-step add or edit. Moved verbatim
    /// out of `App` for step 13: the enrichment list is a component now and
    /// this is the seam it submits through (§2.3).
    pub(crate) fn enqueue_enrichment_chain(
        &mut self,
        view_id: &str,
        enrichments: Vec<EnrichmentDefinition>,
        pending_value: String,
        mutation: PendingEnrichmentMutation,
    ) -> Option<u64> {
        let key = (view_id.to_owned(), QueryPurpose::Enrichment);
        if !self.requests.contains_key(&key) && self.requests.len() >= MAX_PENDING_QUERY_REQUESTS {
            editor_mut(
                self.states.get_mut(view_id).expect("view state"),
                QueryPurpose::Enrichment,
            )
            .error = Some("query submission queue is full; stages were preserved".into());
            return None;
        }
        let generation = self.next_generation;
        self.next_generation = self.next_generation.saturating_add(1);
        let state = self.states.get_mut(view_id).expect("view state");
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
        self.requests.insert(
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

    /// Re-runs the applied chain unchanged (§12.5). A command step's results
    /// are read by later steps and by filters, so when a publication lands or
    /// is restored the chain is evaluated again over it. Nothing about the
    /// definition changes and no command runs; the query is one more chain
    /// submission, fenced and accepted like any other.
    pub fn reaffirm_enrichment_chain(&mut self, view_id: &str) -> Option<u64> {
        let state = self.states.get(view_id)?;
        let chain = state.enrichments.clone();
        let pending_draft = state.enrichment.draft.clone();
        self.enqueue_enrichment_chain(
            view_id,
            chain,
            pending_draft,
            PendingEnrichmentMutation::Reaffirm,
        )
    }

    /// The editor for one purpose on one view. Drafts, the last accepted
    /// value, the error and the pending fence are all per view (§7.3), so a
    /// component reads and writes them here rather than caching a copy.
    pub fn editor(&self, view_id: &str, purpose: QueryPurpose) -> Option<&EditorState> {
        self.states
            .get(view_id)
            .map(|state| editor_of(state, purpose))
    }

    pub fn editor_mut(&mut self, view_id: &str, purpose: QueryPurpose) -> Option<&mut EditorState> {
        self.states
            .get_mut(view_id)
            .map(|state| editor_mut(state, purpose))
    }

    /// Records that the user changed something about this view, which is what
    /// `lvu-app` polls to decide when to persist it.
    pub fn touch(&mut self, view_id: &str) {
        if let Some(state) = self.states.get_mut(view_id) {
            state.user_interaction_revision = state.user_interaction_revision.saturating_add(1);
            state.ai_definition_revision = state.ai_definition_revision.saturating_add(1);
        }
    }

    /// Arms the live-search debounce. The deadline is view state so that a
    /// keystroke in one view cannot cancel another view's pending search, and
    /// `App::flush_debounced_searches` is what fires it.
    pub fn schedule_search(&mut self, view_id: &str) {
        if let Some(state) = self.states.get_mut(view_id) {
            state.search.search_due = Some(Instant::now() + SEARCH_DEBOUNCE);
        }
    }

    /// Cancels an armed debounce, so an explicit apply is not followed by a
    /// second live query for the same draft.
    pub fn cancel_scheduled_search(&mut self, view_id: &str) {
        if let Some(state) = self.states.get_mut(view_id) {
            state.search.search_due = None;
        }
    }

    /// The editor seam a component submits through, and the third guard-and-
    /// refuse wrapper over one evaluator: `submit_capture_time` and
    /// `apply_recipe` have the same shape. It checks the definition, calls
    /// `enqueue_value`, and words the two refusals a component has to tell
    /// apart — a full queue keeps the draft and says so in the editor, while a
    /// fixed definition means the edit becomes a derived view and the shell
    /// stages it (§2.3).
    ///
    /// Callers that must *not* fork — the debounced live search, which would
    /// otherwise propose a view per pause in typing — check the role first and
    /// go straight to `enqueue_value`.
    pub fn enqueue(
        &mut self,
        view_id: &str,
        purpose: QueryPurpose,
        value: Option<String>,
    ) -> Result<u64, SubmitRefused> {
        if !self.states.contains_key(view_id) {
            return Err(SubmitRefused::QueueFull);
        }
        // Grouping is a display-only continuation rule, so it stays editable on
        // the canonical view along with the rest of its presentation.
        if purpose != QueryPurpose::Grouping && self.definition_is_fixed(view_id) {
            return self
                .stage_editor_fork(view_id, purpose, value)
                .ok_or(SubmitRefused::QueueFull);
        }
        self.enqueue_value(view_id, purpose, value)
            .ok_or(SubmitRefused::QueueFull)
    }

    /// Re-run the view's query for a change that is *presentation only*.
    ///
    /// A canonical view's definition is fixed; its presentation never is, which
    /// is what `definition_is_fixed` documents and what display-only grouping
    /// already relies on. Repainting All events must not create a derived view,
    /// so this goes straight to the evaluator with no fork guard.
    pub fn enqueue_presentation(
        &mut self,
        view_id: &str,
        rules: Vec<ColorRule>,
    ) -> Result<u64, SubmitRefused> {
        if !self.states.contains_key(view_id) {
            return Err(SubmitRefused::QueueFull);
        }
        let key = (view_id.to_owned(), QueryPurpose::Advanced);
        if !self.requests.contains_key(&key) && self.requests.len() >= MAX_PENDING_QUERY_REQUESTS {
            return Err(SubmitRefused::QueueFull);
        }
        let generation = self.next_generation;
        self.next_generation = self.next_generation.saturating_add(1);
        let state = self.states.get_mut(view_id).expect("view state");
        let base_revision = state.applied_query_revision;
        let base_constraints = applied_constraints(state);
        state.desired_constraints.color_rules = rules;
        state.color_rules_error = None;
        state.desired_query_revision = state.desired_query_revision.saturating_add(1);
        let revision = state.desired_query_revision;
        state.pending_color_rules = Some((generation, revision));
        self.requests.insert(
            key,
            QueryRequest {
                view_id: view_id.to_owned(),
                generation,
                revision,
                base_revision,
                base_constraints,
                purpose: QueryPurpose::Advanced,
                constraints: state.desired_constraints.clone(),
            },
        );
        Ok(revision)
    }
    /// The three queues the runtime drains. They are `pub(crate)` rather than
    /// `pub`: a component never registers or installs a view, and the shell
    /// forwards these to `lvu-app` unchanged (§2.3).
    pub(crate) fn take_view_fork_requests(&mut self) -> Vec<ViewForkRequest> {
        self.fork_requests.drain(..).collect()
    }

    pub(crate) fn take_ready_forks(&mut self) -> Vec<ReadyFork> {
        self.ready_forks.drain(..).collect()
    }

    pub(crate) fn take_fork_discards(&mut self) -> Vec<String> {
        self.fork_discards.drain(..).collect()
    }

    /// The candidate id, when this view *is* a fork candidate. The shell asks
    /// before it settles a completion, because a candidate's query decides
    /// whether the view it would create ever appears.
    pub(crate) fn fork_candidate(&self, view_id: &str) -> Option<String> {
        Some(self.fork_of_candidate(view_id)?.candidate_view_id.clone())
    }

    fn fork_of_candidate(&self, candidate: &str) -> Option<&PendingFork> {
        self.pending_forks
            .values()
            .find(|fork| fork.candidate_view_id == candidate)
    }

    /// Returns the candidate for an edit to a canonical view, creating it on
    /// the first edit of a burst and reusing it afterwards.
    ///
    /// Reuse is what keeps debounced typing from proposing a view per
    /// keystroke: the same candidate identity is restaged, and the queued query
    /// for it is replaced rather than added to.
    fn stage_fork(&mut self, origin: &str, purpose: QueryPurpose, edit: ForkEdit) -> Option<u64> {
        let source_id = self
            .items
            .iter()
            .find(|view| view.id == origin)?
            .source_id
            .clone();
        let unchanged = self
            .states
            .get(origin)
            .map(|state| state.desired_query_revision)
            .unwrap_or_default();
        // The origin never carries the edit. Returning it to its applied
        // definition here is what makes "All events cannot be filtered in
        // place" true even while the candidate is still being prepared.
        let base = {
            let state = self.states.get_mut(origin)?;
            state.desired_constraints = applied_constraints(state);
            state.desired_capture_time_policy = state.applied_capture_time_policy;
            state.desired_time_basis = state.applied_time_basis;
            state.clone()
        };
        // An edit that leaves the definition where it already is creates
        // nothing. Clearing the search box on an unfiltered view is the common
        // case, and it must cancel any candidate in flight rather than propose
        // a second unfiltered view.
        let applied = applied_constraints(&base);
        let edits_nothing = match &edit {
            ForkEdit::Editor { purpose, draft, .. } => match purpose {
                QueryPurpose::Search => nonempty_text(draft) == applied.text,
                QueryPurpose::Advanced => nonempty(draft) == applied.advanced_polars,
                QueryPurpose::Grouping => nonempty(draft) == applied.grouping,
                QueryPurpose::Enrichment => false,
            },
            ForkEdit::Time {
                window,
                policy,
                basis,
            } => {
                *window == applied.capture_time
                    && *policy == base.applied_capture_time_policy
                    && *basis == base.applied_time_basis
            }
            ForkEdit::Recipe { .. } => false,
        };
        if edits_nothing {
            self.cancel_fork_for_origin(origin);
            return Some(unchanged);
        }
        // The edit is going ahead, on a view the caller cannot see the query
        // of. Recording that here is what lets the editor which submitted it
        // say so and refuse a second submission of the same draft; without it
        // the only evidence a fork is in flight lives on the candidate.
        if let ForkEdit::Editor { purpose, .. } = &edit
            && let Some(state) = self.states.get_mut(origin)
        {
            let editor = editor_mut(state, *purpose);
            editor.fork_pending = true;
            editor.error = None;
        }
        let interaction_revision = base.user_interaction_revision;
        let existing = self.pending_forks.get(origin).cloned();
        let candidate_view_id = match &existing {
            Some(fork) => fork.candidate_view_id.clone(),
            None => {
                self.next_fork_sequence = self.next_fork_sequence.saturating_add(1);
                // A real view identity from the outset, so a candidate that is
                // installed needs no renaming and can be persisted as it is.
                lvu_core::ViewId::new().0.to_string()
            }
        };
        let name = match &existing {
            Some(fork) => fork.name.clone(),
            None => self.derived_view_name(&source_id, &edit),
        };
        if existing.is_none() {
            // Resolved here, because a view that predates ordered membership
            // carries an empty list and falls back to its own entry, which a
            // candidate does not have yet.
            let candidate_state = fork_candidate_state(&base, self.source_ids(origin));
            self.states
                .insert(candidate_view_id.clone(), candidate_state);
        }
        let stage = existing
            .as_ref()
            .map_or(ForkStage::Requested, |fork| fork.stage);
        self.pending_forks.insert(
            origin.to_owned(),
            PendingFork {
                origin_view_id: origin.to_owned(),
                candidate_view_id: candidate_view_id.clone(),
                source_id: source_id.clone(),
                name: name.clone(),
                interaction_revision,
                stage: match stage {
                    // A superseding edit restarts the candidate's query rather
                    // than racing the one already in flight.
                    ForkStage::Persisting => ForkStage::Persisting,
                    other => other,
                },
                purpose,
                edit,
            },
        );
        if existing.is_none() {
            let source_ids = self.source_ids(&candidate_view_id);
            self.fork_requests.push_back(ViewForkRequest {
                origin_view_id: origin.to_owned(),
                candidate_view_id,
                source_id,
                source_ids,
                name,
            });
        } else if matches!(stage, ForkStage::Querying) {
            // Already registered: restage immediately so the queued query for
            // this candidate is replaced by the newest draft.
            let candidate = self
                .pending_forks
                .get(origin)
                .map(|fork| fork.candidate_view_id.clone())?;
            self.replay_fork_edit(&candidate);
        }
        Some(unchanged)
    }

    fn derived_view_name(&self, source_id: &str, edit: &ForkEdit) -> String {
        let base = match edit {
            ForkEdit::Editor { purpose, draft, .. } => match purpose {
                // Named after what it filters, the way a user would name it,
                // rather than after the control they used.
                QueryPurpose::Search => {
                    let literal = draft.trim();
                    if literal.is_empty() {
                        "Filtered".to_owned()
                    } else {
                        literal.chars().take(32).collect()
                    }
                }
                QueryPurpose::Advanced => "Filtered".to_owned(),
                QueryPurpose::Enrichment => "Enriched".to_owned(),
                QueryPurpose::Grouping => "Grouped".to_owned(),
            },
            ForkEdit::Time { .. } => "Time window".to_owned(),
            ForkEdit::Recipe { .. } => "Recipe".to_owned(),
        };
        let taken = |name: &str, views: &Self| {
            views
                .items
                .iter()
                .any(|view| view.source_id == source_id && view.name == name)
                || views
                    .pending_forks
                    .values()
                    .any(|fork| fork.source_id == source_id && fork.name == name)
        };
        if !taken(&base, self) {
            return base;
        }
        for suffix in 2..=64u32 {
            let candidate = format!("{base} {suffix}");
            if !taken(&candidate, self) {
                return candidate;
            }
        }
        format!("{base} {}", self.next_fork_sequence)
    }

    /// Starts the candidate's query. Called by the runtime once the candidate
    /// view is registered, so a query can never be submitted for a view the
    /// runtime does not know.
    pub(crate) fn begin_fork_query(&mut self, candidate_view_id: &str) -> bool {
        let Some(origin) = self
            .fork_of_candidate(candidate_view_id)
            .map(|fork| fork.origin_view_id.clone())
        else {
            return false;
        };
        if let Some(fork) = self.pending_forks.get_mut(&origin) {
            fork.stage = ForkStage::Querying;
        }
        self.replay_fork_edit(candidate_view_id)
    }

    /// Applies the staged edit to the candidate and queues its query.
    fn replay_fork_edit(&mut self, candidate_view_id: &str) -> bool {
        let Some(fork) = self.fork_of_candidate(candidate_view_id).cloned() else {
            return false;
        };
        match fork.edit {
            ForkEdit::Editor {
                purpose,
                draft,
                enrichment_editing,
            } => {
                let Some(state) = self.states.get_mut(candidate_view_id) else {
                    return false;
                };
                state.enrichment_editing = enrichment_editing;
                match purpose {
                    QueryPurpose::Search => state.search.draft = draft.clone(),
                    QueryPurpose::Advanced => state.advanced.draft = draft.clone(),
                    QueryPurpose::Enrichment => state.enrichment.draft = draft.clone(),
                    QueryPurpose::Grouping => state.grouping.draft = draft.clone(),
                }
                // The candidate is derived by construction, so the guard
                // above it would never fire and asking again would be
                // misleading: this is the in-place half, as with recipes.
                self.enqueue_value(candidate_view_id, purpose, Some(draft))
                    .is_some()
            }
            ForkEdit::Time {
                window,
                policy,
                basis,
            } => {
                let Some(state) = self.states.get_mut(candidate_view_id) else {
                    return false;
                };
                state.desired_constraints.capture_time = window;
                state.desired_capture_time_policy = policy;
                state.desired_time_basis = basis;
                state.desired_constraints.time_basis = basis;
                state.time_error = None;
                let Some(revision) = self.enqueue_time_query(candidate_view_id) else {
                    return false;
                };
                self.track_time_request(candidate_view_id, revision, window);
                true
            }
            // The recipe carries the clock the seam was called with, so a
            // replay resolves a rolling window against the moment the user
            // applied it rather than the moment the runtime happened to
            // register the candidate (§6.5).
            ForkEdit::Recipe { config, now_nanos } => self
                .apply_recipe_in_place(candidate_view_id, *config, now_nanos)
                .is_ok(),
        }
    }

    /// Installs a candidate as a real view.
    ///
    /// Called only after the query succeeded and persistence was accepted, so a
    /// view never appears for an edit that did not work. Returns the origin and
    /// candidate ids, because selecting the new view and carrying an open
    /// dialog onto it are the shell's half — everything here is view lifecycle.
    pub(crate) fn install_fork(&mut self, candidate_view_id: &str) -> Option<InstalledFork> {
        let fork = self.fork_of_candidate(candidate_view_id).cloned()?;
        if self.items.iter().any(|view| view.id == candidate_view_id) {
            return None;
        }
        self.pending_forks.remove(&fork.origin_view_id);
        let item = ViewItem {
            id: fork.candidate_view_id.clone(),
            source_id: fork.source_id.clone(),
            name: fork.name.clone(),
        };
        // Directly after the view it came from. The sidebar groups views under
        // their source, so appending here would leave cycling order disagreeing
        // with what is on screen: the next view visually would not be the next
        // view `]` reaches.
        match self
            .items
            .iter()
            .position(|view| view.id == fork.origin_view_id)
        {
            Some(index) => self.items.insert(index + 1, item),
            None => self.items.push(item),
        }
        self.roles
            .insert(fork.candidate_view_id.clone(), ViewRole::Derived);
        // The origin returns to being unfiltered, including its editor drafts:
        // what the user typed now lives in the view it created.
        if let Some(state) = self.states.get_mut(&fork.origin_view_id) {
            state.search.draft = state.search.applied.clone();
            state.advanced.draft = state.advanced.applied.clone();
            state.enrichment.draft.clear();
            state.enrichment_editing = None;
            state.search.error = None;
            state.advanced.error = None;
            state.enrichment.error = None;
            state.search.fork_pending = false;
            state.advanced.fork_pending = false;
            state.enrichment.fork_pending = false;
            state.grouping.fork_pending = false;
            state.time_error = None;
        }
        Some(InstalledFork {
            origin_view_id: fork.origin_view_id,
            candidate_view_id: fork.candidate_view_id,
        })
    }

    /// The candidate's name, for the persistent state the shell assembles.
    pub(crate) fn fork_name(&self, candidate_view_id: &str) -> Option<String> {
        Some(self.fork_of_candidate(candidate_view_id)?.name.clone())
    }

    /// Abandons a candidate. Nothing was ever visible, so nothing is removed
    /// from the view list; the diagnostic goes back to the view the user is
    /// actually looking at.
    pub(crate) fn discard_fork(&mut self, candidate_view_id: &str, reason: String) -> bool {
        let Some(fork) = self.fork_of_candidate(candidate_view_id).cloned() else {
            return false;
        };
        self.pending_forks.remove(&fork.origin_view_id);
        self.states.remove(candidate_view_id);
        self.roles.remove(candidate_view_id);
        self.requests
            .retain(|(view_id, _), _| view_id != candidate_view_id);
        self.fork_discards.push_back(candidate_view_id.to_owned());
        if let Some(state) = self.states.get_mut(&fork.origin_view_id) {
            let editor = match fork.purpose {
                QueryPurpose::Search => &mut state.search,
                QueryPurpose::Advanced => &mut state.advanced,
                QueryPurpose::Enrichment => &mut state.enrichment,
                QueryPurpose::Grouping => &mut state.grouping,
            };
            // The outcome is known either way: a silent discard has to release
            // the editor as surely as a rejection does, or the draft could
            // never be submitted again.
            editor.fork_pending = false;
            if !reason.is_empty() {
                editor.error = Some(reason.clone());
                if matches!(fork.edit, ForkEdit::Time { .. }) {
                    state.time_error = Some(reason);
                }
            }
        }
        true
    }

    /// Cancels the candidate a view is proposing. Private, and deliberately:
    /// its one legitimate caller is `stage_fork`, when an edit turns out to
    /// change nothing. Escape is *not* an undo — dismissing an editor must
    /// leave a fork the user already applied alone (`7b002b5`).
    fn cancel_fork_for_origin(&mut self, origin_view_id: &str) -> bool {
        let Some(candidate) = self
            .pending_forks
            .get(origin_view_id)
            .map(|fork| fork.candidate_view_id.clone())
        else {
            return false;
        };
        self.discard_fork(&candidate, String::new())
    }

    /// Moves a candidate whose query succeeded to persistence.
    ///
    /// The candidate must still be the one its origin is proposing: a newer
    /// editing burst supersedes an older candidate rather than installing both.
    pub(crate) fn mark_fork_ready(&mut self, candidate_view_id: &str) {
        let Some(fork) = self.fork_of_candidate(candidate_view_id).cloned() else {
            return;
        };
        let current = self.pending_forks.get(&fork.origin_view_id);
        let superseded = current.is_none_or(|current| {
            current.candidate_view_id != fork.candidate_view_id
                || current.interaction_revision != fork.interaction_revision
        });
        if superseded
            || self.role(&fork.origin_view_id) != ViewRole::Canonical
            || self.items.iter().all(|view| view.id != fork.origin_view_id)
        {
            self.discard_fork(candidate_view_id, String::new());
            return;
        }
        if fork.stage == ForkStage::Persisting {
            return;
        }
        if let Some(pending) = self.pending_forks.get_mut(&fork.origin_view_id) {
            pending.stage = ForkStage::Persisting;
        }
        self.ready_forks.push_back(ReadyFork {
            candidate_view_id: fork.candidate_view_id,
            origin_view_id: fork.origin_view_id,
            source_id: fork.source_id,
            name: fork.name,
        });
    }

    fn track_time_request(
        &mut self,
        view_id: &str,
        revision: u64,
        value: Option<CaptureTimeRange>,
    ) {
        let Some(request) = self
            .requests
            .values()
            .find(|request| request.view_id == view_id && request.revision == revision)
        else {
            return;
        };
        let policy = self
            .states
            .get(view_id)
            .and_then(|state| state.desired_capture_time_policy);
        let basis = self
            .states
            .get(view_id)
            .map_or(TimeBasis::Capture, |state| state.desired_time_basis);
        self.states
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

    /// Applies the desired capture-time window a caller has already recorded,
    /// forking when the view's definition is fixed. The desired state is
    /// written before this is called precisely so `stage_time_fork` can read it
    /// straight back out. Both `submit_capture_time` and the rolling-policy
    /// refresh go through here, so there is one answer to "what does applying
    /// a time window to All events do".
    pub(crate) fn apply_desired_time(&mut self, view_id: &str) -> Option<u64> {
        if self.definition_is_fixed(view_id) {
            return self.stage_time_fork(view_id);
        }
        self.enqueue_time_query(view_id)
    }

    /// Turns the desired time window this seam has already recorded into a
    /// derived-view candidate.
    fn stage_time_fork(&mut self, view_id: &str) -> Option<u64> {
        let state = self.states.get(view_id)?;
        let edit = ForkEdit::Time {
            window: state.desired_constraints.capture_time,
            policy: state.desired_capture_time_policy,
            basis: state.desired_time_basis,
        };
        self.stage_fork(view_id, QueryPurpose::Advanced, edit)
    }

    /// Turns a recipe refused by a fixed definition into a derived-view
    /// candidate. The mirror of `stage_time_fork`.
    fn stage_recipe_fork(&mut self, view_id: &str, config: RecipeConfig, now_nanos: i64) -> bool {
        self.stage_fork(
            view_id,
            QueryPurpose::Advanced,
            ForkEdit::Recipe {
                config: Box::new(config),
                now_nanos,
            },
        )
        .is_some()
    }

    /// Turns a draft refused by a fixed definition into a derived-view
    /// candidate. The mirror of `stage_time_fork`, for the three editors.
    fn stage_editor_fork(
        &mut self,
        view_id: &str,
        purpose: QueryPurpose,
        value: Option<String>,
    ) -> Option<u64> {
        let (draft, enrichment_editing) = {
            let state = self.states.get(view_id)?;
            let draft = value.unwrap_or_else(|| editor_of(state, purpose).draft.clone());
            (draft, state.enrichment_editing.clone())
        };
        self.stage_fork(
            view_id,
            purpose,
            ForkEdit::Editor {
                purpose,
                draft,
                enrichment_editing,
            },
        )
    }

    pub(crate) fn enqueue_time_query(&mut self, view_id: &str) -> Option<u64> {
        let key = (view_id.to_owned(), QueryPurpose::Advanced);
        if !self.requests.contains_key(&key) && self.requests.len() >= MAX_PENDING_QUERY_REQUESTS {
            return None;
        }
        let generation = self.next_generation;
        self.next_generation = self.next_generation.saturating_add(1);
        let state = self.states.get_mut(view_id).expect("view state");
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
        self.requests.insert(
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
}

/// Shell-owned state that `Ctx`/`RenderCtx` wrap, plus the layer stack's
/// neighbours (component-model.md §2.5). Legacy `App` fields migrate in here
/// one conversion at a time; today it holds what the pilot's `Ctx` needs.
/// Keeping it a *separate field* of `App` from `layers` is the whole point: a
/// `Ctx` borrowed from `shell` and a `&mut` component borrowed from `layers`
/// are disjoint, so no component ever needs `&mut App`.
#[derive(Debug, Default)]
pub struct Shell {
    pub cursors: CursorBank,
    pub size: (u16, u16),
    pub clock_now_unix_nanos: i64,
}

impl Shell {
    pub fn clock(&self) -> Clock {
        Clock {
            now_unix_nanos: self.clock_now_unix_nanos,
        }
    }
}

pub struct App {
    pub shell: Shell,
    pub layers: Layers,
    pub title: String,
    pub demo_mode: bool,
    pub sources: Vec<SourceItem>,
    /// The product's shared view state and the query seam (§2.3). Components
    /// reach it through `Ctx`, never through `App`.
    pub views: Views,
    pub focus: Focus,
    /// The base focus that was current when the bottom layer was pushed, so a
    /// popped stack resumes exactly where it left off (§1). This is what the
    /// retired Raw context dialog's `return_focus` and `App::help_return_focus`
    /// were, hoisted into the stack where every layer inherits it.
    layer_return_focus: Focus,
    pub show_details: bool,
    /// Where `o` came from, while the user is looking at the raw stream.
    pub raw_context_origin: Option<RawContextOrigin>,
    pub dialog_scroll: usize,
    pub dialog_scroll_limit: usize,
    pub dialog_scroll_focused: bool,
    pub should_quit: bool,
    pub hit_regions: HitRegions,
    pub source_notice: Option<String>,
    pub action_notice: Option<String>,
    /// Theme, delight, reduced motion and ASCII fallback. Shell state with one
    /// writer (Settings, through `Ctx.appearance`) and many readers; a separate
    /// `App` field so `shell_ctx` can hand out `&mut` to it disjointly from
    /// `layers` (component-model.md §2.2, §2.5).
    pub appearance: Appearance,
    /// Whether an interactive source-less launch should show the startup modal.
    /// This is deliberately independent from the footer delight setting.
    pub show_startup_title: bool,
    /// When each view was last selected. A restart reopens the view the user
    /// was working in, which is only All events until they choose another.
    view_selection_stamps: HashMap<String, u64>,
    next_selection_stamp: u64,
    /// Sources whose remembered selection has already been applied this run.
    restored_selections: HashSet<String>,
    /// At most one candidate per origin view, keyed by the origin.
    /// Candidate views the runtime must unregister.
    pending_jump: Option<PendingJump>,
    source_controls: VecDeque<SourceControlRequest>,
    field_stats_requests: VecDeque<FieldStatsRequest>,
    /// The live whole-view answer, and the question it answers. Held on the app
    /// rather than in the dialog so a dialog that closes and reopens over the
    /// same field does not ask again.
    whole_view_stats: Option<WholeViewStats>,
    /// A pass is out for this generation and has not answered yet.
    field_stats_pending: Option<u64>,
    field_stats_generation: u64,
    /// Shell configuration a component may read through `Ctx.agent` (§6.5).
    agent: AgentDefaults,
    view_runtime_status: HashMap<String, String>,
    last_clock_unix_nanos: Option<i64>,
    next_rolling_refresh: Option<Instant>,
}

impl App {
    pub fn new(sources: Vec<SourceItem>, views: Vec<ViewItem>, demo_mode: bool) -> Self {
        let empty = views.is_empty();
        // An empty workspace opens on Add source, as it always has. The layer
        // is on the stack from construction rather than a focus the shell
        // names, which is what `Focus::SourceDialog` was.
        let mut layers = Layers::default();
        if empty {
            layers.source.open_at_startup();
            layers.stack.push(LayerId::Source);
        }
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
            shell: Shell {
                size: (80, 24),
                ..Shell::default()
            },
            title: "lvu log workspace".into(),
            demo_mode,
            sources,
            views: Views {
                items: views,
                states: view_states,
                ..Views::default()
            },
            layers,
            focus: if empty { Focus::Layer } else { Focus::Logs },
            layer_return_focus: Focus::Logs,
            show_details: false,
            raw_context_origin: None,
            dialog_scroll: 0,
            dialog_scroll_limit: 0,
            dialog_scroll_focused: false,
            should_quit: false,
            hit_regions: HitRegions::default(),
            source_notice: None,
            action_notice: None,
            appearance: Appearance::default(),
            show_startup_title: true,
            view_selection_stamps: HashMap::new(),
            next_selection_stamp: 1,
            restored_selections: HashSet::new(),
            pending_jump: None,
            source_controls: VecDeque::new(),
            field_stats_requests: VecDeque::new(),
            whole_view_stats: None,
            field_stats_pending: None,
            field_stats_generation: 0,
            agent: AgentDefaults {
                provider: "codex/gpt-5.6-sol".into(),
                mode: "full-access".into(),
                thinking: "medium".into(),
            },
            view_runtime_status: HashMap::new(),
            last_clock_unix_nanos: None,
            next_rolling_refresh: None,
        }
    }

    /// The sidebar list and its selection live in `Views` now (§2.3); these
    /// keep `lvu-app` and the base UI off its internals.
    pub fn views(&self) -> &[ViewItem] {
        &self.views.items
    }

    pub fn selected_view(&self) -> usize {
        self.views.selected
    }

    pub fn set_selected_view(&mut self, index: usize) {
        self.views.selected = index;
    }

    pub fn active_view_id(&self) -> Option<&str> {
        self.views.active_id()
    }

    fn dismissal_action(&self) -> Action {
        match self.focus {
            Focus::Selector | Focus::Logs => Action::Quit,
            Focus::Details => Action::ToggleDetails,
            Focus::Layer => Action::CancelEditor,
        }
    }

    pub fn key_to_action(&self, key: KeyEvent) -> Action {
        if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat)
            && key.modifiers.contains(KeyModifiers::CONTROL)
            && key.code == KeyCode::Char('c')
        {
            return Action::Quit;
        }
        if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat)
            && (key.code == KeyCode::Esc
                || (key.code == KeyCode::Char('q') && key.modifiers.is_empty()))
        {
            // Every dialog with a text field is a layer now, and a layer decides
            // for itself whether `q` types or dismisses (`Component::text_focus`).
            return self.dismissal_action();
        }
        key_to_action(key, self.focus)
    }

    pub fn view_state(&self) -> Option<&ViewState> {
        self.active_view_id()
            .and_then(|id| self.views.states.get(id))
    }

    fn view_state_mut(&mut self) -> Option<&mut ViewState> {
        let id = self.active_view_id()?.to_owned();
        self.views.states.get_mut(&id)
    }

    pub fn search_state(&self) -> Option<&EditorState> {
        self.view_state().map(|state| &state.search)
    }

    /// Bookmarks a view shows: those of every source it contains, in source
    /// order. Kept as one list so the dialog can index it directly.
    pub fn bookmarks_for_view(&self, view_id: &str) -> Vec<Bookmark> {
        self.views.bookmarks_for_view(view_id)
    }

    /// Replaces one source's bookmarks. Used when a workspace is loaded.
    pub fn set_source_bookmarks(&mut self, source_id: &str, bookmarks: Vec<Bookmark>) {
        self.views.set_source_bookmarks(source_id, bookmarks);
    }

    pub fn source_bookmarks(&self, source_id: &str) -> &[Bookmark] {
        self.views.source_bookmarks(source_id)
    }

    /// Adds a bookmark to a source's set without losing a note already there.
    fn merge_source_bookmark(&mut self, bookmark: Bookmark) {
        let bookmarks = self
            .views
            .bookmarks
            .entry(bookmark.id.source_id.clone())
            .or_default();
        if let Some(existing) = bookmarks
            .iter_mut()
            .find(|existing| existing.id == bookmark.id)
        {
            if existing.note.is_empty() {
                existing.note = bookmark.note;
            } else if !bookmark.note.is_empty() && existing.note != bookmark.note {
                let mut joined = format!("{} / {}", existing.note, bookmark.note);
                let mut limit = MAX_BOOKMARK_NOTE_BYTES.min(joined.len());
                while !joined.is_char_boundary(limit) {
                    limit -= 1;
                }
                joined.truncate(limit);
                existing.note = joined;
            }
        } else if bookmarks.len() < MAX_BOOKMARKS {
            bookmarks.push(bookmark);
        }
    }

    pub fn advanced_state(&self) -> Option<&EditorState> {
        self.view_state().map(|state| &state.advanced)
    }

    /// The structured rows the Details pane is showing for the selected
    /// record, in drawn order; empty when the record is not a JSON object.
    pub fn details_rows<P: RowProvider>(&self, provider: &P) -> Vec<crate::json_tree::TreeRow> {
        let Some(row) = self.selected_row(provider) else {
            return Vec::new();
        };
        let Some(state) = self.view_state() else {
            return Vec::new();
        };
        crate::json_tree::JsonTree::parse(&row.text)
            .filter(crate::json_tree::JsonTree::is_object)
            .map(|tree| tree.rows(&|path| state.expanded_paths.contains(path)))
            .unwrap_or_default()
    }

    /// Scrolls the Details pane only as far as it takes to show the line
    /// `above` wrapped lines down, and returns the offset to draw with.
    pub fn reveal_details_line(&mut self, above: usize, height: usize) -> usize {
        let Some(state) = self.view_state_mut() else {
            return 0;
        };
        if !state.details_reveal {
            return state.details_scroll.min(state.details_scroll_limit);
        }
        state.details_reveal = false;
        if above < state.details_scroll {
            state.details_scroll = above;
        } else if height > 0 && above >= state.details_scroll.saturating_add(height) {
            state.details_scroll = above.saturating_add(1).saturating_sub(height);
        }
        state.details_scroll = state.details_scroll.min(state.details_scroll_limit);
        state.details_scroll
    }

    pub fn set_details_viewport(&mut self, row: Option<RowId>, scroll_limit: usize) -> usize {
        let Some(state) = self.view_state_mut() else {
            return 0;
        };
        if state.details_row != row {
            // The cursor belongs to a record: a new record starts it at the
            // top. The first frame is not a new record, so moves made before
            // it are kept.
            if state.details_row.is_some() {
                state.details_cursor = 0;
            }
            state.details_row = row;
            state.details_scroll = 0;
        }
        state.details_scroll_limit = scroll_limit;
        state.details_scroll = state.details_scroll.min(scroll_limit);
        state.details_scroll
    }

    pub fn persistent_view_state(&self, view_id: &str) -> Option<PersistentViewState> {
        let name = self
            .views
            .items
            .iter()
            .find(|view| view.id == view_id)?
            .name
            .clone();
        self.persistent_view_state_named(view_id, name)
    }

    /// The same projection for a view that is not in the view list yet, which
    /// is how a fork candidate is persisted before it becomes visible.
    fn persistent_view_state_named(
        &self,
        view_id: &str,
        name: String,
    ) -> Option<PersistentViewState> {
        let state = self.views.states.get(view_id)?;
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
            command_steps: state.command_steps.clone(),
            applied_grouping: state.grouping.applied.clone(),
            grouping_draft: state.grouping.draft.clone(),
            grouping_error: state.grouping.error.clone(),
            applied_capture_time: match state.applied_capture_time_policy {
                Some(CaptureTimePolicy::Recent { .. }) => None,
                _ => state.applied_capture_time,
            },
            applied_capture_time_policy: state.applied_capture_time_policy,
            applied_time_basis: state.applied_time_basis,
            applied_time_field: state.applied_time_field.clone(),
            time_field_draft: state.time_field_draft.clone(),
            time_start_draft: state.time_start_draft.clone(),
            time_end_draft: state.time_end_draft.clone(),
            time_recent_draft: state.time_recent_draft.clone(),
            time_error: state.time_error.clone(),
            time_draft_touched: state.time_draft_touched,
            time_window_draft: state.time_window_draft,
            time_gap_threshold_seconds: state.time_gap_threshold_seconds,
            time_basis_draft: state.time_basis_draft,
            time_start_date_draft: state.time_start_date_draft.clone(),
            time_start_clock_draft: state.time_start_clock_draft.clone(),
            time_start_zone_draft: state.time_start_zone_draft.clone(),
            time_end_date_draft: state.time_end_date_draft.clone(),
            time_end_clock_draft: state.time_end_clock_draft.clone(),
            time_end_zone_draft: state.time_end_zone_draft.clone(),
            time_structured_draft_present: state.time_structured_draft_present,
            bookmarks: self.bookmarks_for_view(view_id),
            selected_at: self.view_selection_stamp(view_id),
            selected: state.selected.clone(),
            follow: state.follow,
            pinned_columns: state.pinned_columns.clone(),
            color_field: state.color_field.clone(),
            color_rules: state.color_rules.clone(),
            fold_enabled: state.fold_enabled,
            fold_minimum_run: state.fold_minimum_run,
            fold_key_column: state.fold_key_column.clone(),
            fold_lookback: state.fold_lookback,
            fold_normalisation: state.fold_normalisation,
            fold_expanded: state.fold_expanded.clone(),
            exact_field: state.exact_field.clone(),
        })
    }

    /// Changes only for direct user edits/navigation, so asynchronous restore
    /// work can be fenced without treating provider-driven row arrival as input.
    pub fn view_interaction_revision(&self, view_id: &str) -> Option<u64> {
        self.views
            .states
            .get(view_id)
            .map(|state| state.user_interaction_revision)
    }

    pub fn view_definition_revision(&self, view_id: &str) -> Option<u64> {
        self.views
            .states
            .get(view_id)
            .map(|state| state.ai_definition_revision)
    }

    pub fn configure_ai(&mut self, provider: String, mode: String, thinking: String) {
        self.agent.provider = provider.clone();
        self.agent.mode = mode.clone();
        self.agent.thinking = thinking.clone();
        // The Ask layer seeds each request from these, so it is told too; the
        // legacy dialog used to read `App::ai_*` at open (§2.2 keeps the
        // defaults in the shell, not in `Ctx`).
        self.layers.ask.configure(provider, mode, thinking);
    }

    /// The effective settings `lvu-app` resolved. The Settings component owns
    /// the snapshot (§2.4); this stays as the shell's one-line entry point.
    pub fn configure_settings(&mut self, context: SettingsContext) {
        self.layers.settings.configure(context);
    }

    pub fn take_command_enrichment_requests(&mut self) -> Vec<CommandEnrichmentRequest> {
        self.layers.external_command.take_requests()
    }

    pub fn take_correlation_requests(&mut self) -> Vec<CorrelationRequest> {
        self.layers.correlation.outbox.take()
    }

    pub fn take_field_stats_requests(&mut self) -> Vec<FieldStatsRequest> {
        self.field_stats_requests.drain(..).collect()
    }

    /// Ask for whole-view figures for `path`, superseding any pass still out.
    ///
    /// Called when the described field changes — a different path, a different
    /// view, or a new applied revision — which is the same moment the sampled
    /// figures are recomputed.
    pub fn request_field_stats(
        &mut self,
        view_id: &str,
        path: &str,
        kind: crate::field_stats::ValueType,
    ) {
        if let Some(generation) = self.field_stats_pending.take() {
            self.field_stats_requests
                .push_back(FieldStatsRequest::Cancel { generation });
        }
        self.field_stats_generation = self.field_stats_generation.saturating_add(1);
        let generation = self.field_stats_generation;
        self.whole_view_stats = None;
        self.field_stats_pending = Some(generation);
        self.field_stats_requests
            .push_back(FieldStatsRequest::Resolve {
                generation,
                view_id: view_id.to_owned(),
                path: path.to_owned(),
                kind,
            });
    }

    /// Stop asking: the dialog closed, or the field it described is gone.
    pub fn cancel_field_stats(&mut self) {
        if let Some(generation) = self.field_stats_pending.take() {
            self.field_stats_requests
                .push_back(FieldStatsRequest::Cancel { generation });
        }
        self.whole_view_stats = None;
    }

    /// A pass answered. A late answer to a superseded question is dropped.
    pub fn finish_field_stats(&mut self, stats: Result<WholeViewStats, (u64, String)>) {
        let generation = match &stats {
            Ok(stats) => stats.generation,
            Err((generation, _)) => *generation,
        };
        if self.field_stats_pending != Some(generation) {
            return;
        }
        self.field_stats_pending = None;
        // A failed pass leaves the sample showing and says nothing more: the
        // pane already has figures the user can act on.
        self.whole_view_stats = stats.ok();
    }

    /// The whole-view figures for `view_id` and `path`, when they describe
    /// exactly that field. Anything else is an answer to another question.
    pub fn whole_view_stats(&self, view_id: &str, path: &str) -> Option<&WholeViewStats> {
        self.whole_view_stats
            .as_ref()
            .filter(|stats| stats.view_id == view_id && stats.path == path)
    }

    /// The field the Fields dialog is describing right now, as `(view, path)`.
    ///
    /// Computed with the dialog's own `field_rows`, from state this owns, so
    /// the shell and the pane can never disagree about which field is being
    /// described. Cheap: one cached row lookup and one walk of that record's
    /// paths, no scan.
    pub fn described_field(
        &self,
        provider: &dyn crate::provider::RowProvider,
    ) -> Option<(String, String)> {
        let view_id = self.active_view_id()?.to_owned();
        let state = self.views.states.get(&view_id)?;
        let id = state.field_picker_row.as_ref()?;
        let row = provider.row_by_id(&view_id, id)?;
        let rows = crate::components::fields::field_rows(&row, &state.expanded_paths);
        let selected = rows.get(state.field_picker_selected)?;
        Some((view_id, selected.path.clone()))
    }

    /// The live whole-view figures, whatever field they describe. The caller
    /// checks that they describe the one it is drawing.
    pub fn whole_view_stats_any(&self) -> Option<&WholeViewStats> {
        self.whole_view_stats.as_ref()
    }

    /// A pass is out and has not answered yet.
    pub fn field_stats_pending(&self) -> bool {
        self.field_stats_pending.is_some()
    }

    pub fn is_correlation_current(&self, generation: u64, origin_view_id: &str) -> bool {
        self.layers
            .correlation
            .is_current(generation, origin_view_id, self.active_view_id())
    }

    /// The lookup ended without a mapping. A reason stays on the layer as its
    /// error state; a notice closes the layer and goes to the status line.
    /// Fenced by the component: a stale completion only releases capacity.
    pub fn finish_correlation(
        &mut self,
        generation: u64,
        origin_view_id: &str,
        result: Result<String, String>,
    ) -> bool {
        let active = self.active_view_id().map(str::to_owned);
        match self
            .layers
            .correlation
            .finish(generation, origin_view_id, active.as_deref(), result)
        {
            None => false,
            Some(Ok(notice)) => {
                self.close_layer(LayerId::Correlation);
                self.action_notice = Some(notice);
                true
            }
            Some(Err(_)) => true,
        }
    }

    /// The lookup resolved: the layer moves from its pending state to the
    /// mapping, where the user names the field in every other source.
    #[allow(clippy::too_many_arguments)]
    pub fn open_correlation_dialog(
        &mut self,
        generation: u64,
        origin_view_id: &str,
        field: String,
        value: lvu_core::ExactScalar,
        value_label: String,
        sources: Vec<CorrelationSourceChoice>,
    ) -> bool {
        let active = self.active_view_id().map(str::to_owned);
        self.layers.correlation.resolved(
            generation,
            origin_view_id,
            active.as_deref(),
            field,
            value,
            value_label,
            sources,
        )
    }

    /// The controller could not open the accepted view. The layer keeps the
    /// whole mapping so the user can adjust it; the origin view is untouched.
    pub fn correlation_accept_failed(&mut self, generation: u64, message: String) -> bool {
        self.layers.correlation.accept_failed(generation, message)
    }

    /// The correlated view exists. Close the mapping layer and say so.
    pub fn correlation_accepted(&mut self, generation: u64, notice: String) -> bool {
        if !self.layers.correlation.accepted(generation) {
            return false;
        }
        self.close_layer(LayerId::Correlation);
        self.action_notice = Some(notice);
        true
    }

    pub fn correlation_dialog_open(&self) -> bool {
        self.layers.correlation.mapping().is_some()
    }

    pub fn field_correlation_pending(&self) -> bool {
        self.layers.correlation.lookup_pending()
    }

    pub(crate) fn command_work_pending(&self) -> bool {
        self.layers.external_command.work_pending()
    }

    /// §8 completion paths. The dialog, its outbox and the two
    /// pending-generation maps are the layer's now, so these are forwarders:
    /// the fencing itself moved unchanged with the state it fences (§6.5).
    pub fn finish_command_enrichment_review(
        &mut self,
        generation: u64,
        view_id: &str,
        definition_revision: u64,
        result: Result<CommandEnrichmentReview, String>,
    ) -> bool {
        self.layers
            .external_command
            .finish_review(generation, view_id, definition_revision, result)
    }

    pub fn finish_command_enrichment_run(
        &mut self,
        generation: u64,
        view_id: &str,
        definition_revision: u64,
        result: Result<String, String>,
    ) -> bool {
        let App {
            layers,
            views,
            action_notice,
            ..
        } = self;
        layers.external_command.finish_run(
            generation,
            view_id,
            definition_revision,
            result,
            views,
            action_notice,
        )
    }

    pub fn begin_command_result_save(
        &mut self,
        generation: u64,
        view_id: &str,
        definition_revision: u64,
    ) -> bool {
        self.layers
            .external_command
            .begin_result_save(generation, view_id, definition_revision)
    }

    pub fn commit_command_publication(
        &mut self,
        view_id: &str,
        stage_id: &str,
        expected_command_revision: u64,
        publication: String,
    ) -> bool {
        let Some(state) = self.views.states.get_mut(view_id) else {
            return false;
        };
        if !state
            .enrichments
            .iter()
            .any(|step| step.is_command() && step.id.0 == stage_id)
        {
            return false;
        }
        let Some(step) = state.command_steps.get_mut(stage_id) else {
            return false;
        };
        if step.revision != expected_command_revision {
            return false;
        }
        step.publication = Some(publication);
        true
    }

    /// The worker's answer to a `SettingsRequest`. The dialog's half — the
    /// generation fence, the draft and the status row — belongs to the
    /// component; what stays here is the shell state a save also changes: the
    /// agent defaults, the live appearance and the status-line notice.
    pub fn complete_settings_save(
        &mut self,
        generation: u64,
        result: Result<SettingsContext, String>,
    ) -> bool {
        match result {
            Ok(context) => {
                self.agent.provider = context.effective_provider.clone();
                self.agent.mode = context.effective_mode.clone();
                self.agent.thinking = context.effective_thinking.clone();
                let completion = self.layers.settings.complete_save(generation, context);
                self.appearance = completion.appearance;
                if let Some(notice) = completion.notice {
                    self.source_notice = Some(notice);
                }
                true
            }
            Err(error) => {
                if self.layers.settings.fail_save(generation, &error) {
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
        self.appearance.theme_id = theme_id;
        self.appearance.delight_enabled = delight_enabled;
        self.appearance.reduced_motion = reduced_motion;
        self.appearance.ascii = ascii;
    }

    /// Hide an untouched startup placeholder until all persisted sources are open.
    pub fn defer_view_restore(&mut self, view_id: &str) {
        self.cancel_correlation_for_view(view_id);
        let selected = self.active_view_id().map(str::to_owned);
        self.views.items.retain(|view| view.id != view_id);
        self.views.states.remove(view_id);
        self.shell.cursors.prune_identity(view_id);
        self.shell
            .cursors
            .prune_identity(&format!("time:{view_id}"));
        self.shell
            .cursors
            .prune_where_identity_contains(&format!(":{view_id}:"));
        self.views.selected = selected
            .and_then(|id| self.views.items.iter().position(|view| view.id == id))
            .unwrap_or_else(|| {
                self.views
                    .selected
                    .min(self.views.items.len().saturating_sub(1))
            });
    }

    pub fn view_source_ids(&self, view_id: &str) -> Vec<String> {
        self.views.source_ids(view_id)
    }

    pub fn begin_source_change(
        &mut self,
        view_id: &str,
        sources: Vec<String>,
    ) -> Result<QueryRequest, String> {
        if self.view_definition_is_fixed(view_id) {
            return Err(
                "All events always shows exactly its own source; create a view to combine sources"
                    .into(),
            );
        }
        let primary = self
            .views
            .items
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
            .views
            .states
            .get_mut(view_id)
            .ok_or("view no longer exists")?;
        if state_has_pending_query(state) {
            return Err("wait for the current query before editing sources".into());
        }
        if self
            .bookmarks_for_view(view_id)
            .iter()
            .any(|bookmark| !sources.contains(&bookmark.id.source_id))
        {
            return Err(
                "remove bookmarks for an excluded source before removing it from this view".into(),
            );
        }
        let state = self
            .views
            .states
            .get_mut(view_id)
            .ok_or("view no longer exists")?;
        let generation = self.views.next_generation;
        self.views.next_generation = self.views.next_generation.saturating_add(1);
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
        self.views
            .states
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
        if !self.views.states.contains_key(view_id) {
            return false;
        }
        self.cancel_correlation_for_view(view_id);
        let mut source_ids = HashSet::new();
        let primary = self
            .views
            .items
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
        // A correlation whose mapping names a source this view does not carry
        // would silently search nothing; refuse it instead.
        if restored.exact_field.as_ref().is_some_and(|correlation| {
            correlation.validate().is_err()
                || correlation.source_ids().any(|id| {
                    !restored.source_ids.iter().any(|source| source == id)
                        && primary.is_none_or(|primary| primary != id)
                })
        }) {
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
        self.views
            .states
            .get_mut(view_id)
            .expect("checked view")
            .source_ids = restored.source_ids.clone();
        if !valid_enrichments(&restored.applied_enrichments) {
            return false;
        }
        if !restored.view_name.is_empty()
            && let Some(view) = self.views.items.iter_mut().find(|view| view.id == view_id)
        {
            // Restored names are part of the fenced snapshot, not user input.
            view.name = restored.view_name.clone();
        }
        let state = self
            .views
            .states
            .get_mut(view_id)
            .expect("view state checked above");
        state.ai_definition_revision = state.ai_definition_revision.saturating_add(1);
        // A correlation has no draft: it is accepted state or nothing. Making
        // it accepted at restore means a save taken before the first scan
        // completes cannot quietly write the view back without it. The base
        // snapshot below still has to describe what the adapter last applied,
        // so the previous value is what that request is fenced against.
        let previous_exact_field = state.exact_field.take();
        state.exact_field = restored.exact_field.clone();
        state.search.draft = restored.search_draft;
        state.search.error = restored.search_error;
        state.advanced.draft = restored.advanced_draft;
        state.advanced.error = restored.advanced_error;
        state.enrichment.draft = restored.enrichment_draft;
        state.enrichment.error = restored.enrichment_error;
        state.enrichment_editing = restored.enrichment_editing;
        state.enrichment_selected = restored.enrichment_selected;
        state.command_steps = restored.command_steps;
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
        state.time_gap_threshold_seconds = restored.time_gap_threshold_seconds;
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
        let bookmarks = restored.bookmarks.clone();
        let selected_at = restored.selected_at;
        state.selected = restored.selected;
        state.follow = restored.follow;
        state.pinned_columns = restored.pinned_columns.into_iter().take(8).collect();
        state.color_field = restored.color_field;
        state.fold_enabled = restored.fold_enabled;
        state.fold_minimum_run = effective_fold_minimum_run(restored.fold_minimum_run);
        state.fold_key_column = restored
            .fold_key_column
            .filter(|column| !column.trim().is_empty());
        state.fold_lookback = restored.fold_lookback.min(MAX_FOLD_LOOKBACK);
        state.fold_normalisation = restored.fold_normalisation;
        state.fold_expanded = restored
            .fold_expanded
            .into_iter()
            .take(MAX_FOLD_EXPANDED)
            .collect();
        let restored_policy = restored.applied_capture_time_policy.or(restored
            .applied_capture_time
            .map(CaptureTimePolicy::Absolute));
        let resolved_capture_time = restored_policy.and_then(|policy| {
            resolve_capture_time_policy(policy, self.shell.clock_now_unix_nanos)
        });
        let constraints = QueryConstraints {
            text: nonempty_text(&restored.applied_search),
            exact_field: restored.exact_field.clone(),
            advanced_polars: nonempty(&restored.applied_advanced),
            enrichments: if restored.applied_enrichments.is_empty() {
                legacy_enrichment(&restored.applied_enrichment)
            } else {
                restored.applied_enrichments
            },
            enrichment: None,
            time_field: restored.applied_time_field.clone(),
            capture_time: resolved_capture_time,
            time_basis: restored.applied_time_basis,
            grouping: nonempty(&restored.applied_grouping),
            color_rules: restored.color_rules.clone(),
        };
        let purpose = if !constraints.enrichments.is_empty() {
            QueryPurpose::Enrichment
        } else if constraints.advanced_polars.is_some() {
            QueryPurpose::Advanced
        } else {
            QueryPurpose::Search
        };
        let generation = self.views.next_generation;
        self.views.next_generation = self.views.next_generation.saturating_add(1);
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
        state.applied_time_field = restored.applied_time_field.clone();
        state.time_field_draft = if restored.time_draft_touched {
            restored.time_field_draft.clone()
        } else {
            restored.applied_time_field.clone()
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
        self.views.requests.insert(
            (view_id.to_owned(), purpose),
            QueryRequest {
                view_id: view_id.to_owned(),
                generation,
                revision,
                base_revision: state.applied_query_revision,
                base_constraints: QueryConstraints {
                    exact_field: previous_exact_field,
                    ..applied_constraints(state)
                },
                purpose,
                constraints,
            },
        );
        self.set_view_selection_stamp(view_id, selected_at);
        // Bookmarks restore into their source, not into this view. Merging
        // rather than replacing is what carries a workspace whose views each
        // held their own copy into one shared set without losing a note.
        for bookmark in bookmarks {
            self.merge_source_bookmark(bookmark);
        }
        true
    }

    /// The recipe seam applied to the active view. Kept for the Ask 🧠 recipe
    /// adaptation path, which is converted last (§6.3 step 12); a fixed
    /// definition forks inside `Views::apply_recipe` now.
    fn apply_recipe_to_active_view(&mut self, config: RecipeConfig) -> bool {
        let Some(view_id) = self.active_view_id().map(str::to_owned) else {
            return false;
        };
        let now = self.shell.clock_now_unix_nanos;
        self.views.apply_recipe(&view_id, config, now).is_ok()
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
            Focus::Selector | Focus::Logs | Focus::Details => None,
            // The enrichment editors are layers now, and a layer reads its own
            // editor state out of `Views` (§2.5).
            Focus::Layer => None,
        }
    }

    pub fn take_source_controls(&mut self) -> Vec<SourceControlRequest> {
        self.source_controls.drain(..).collect()
    }

    pub fn take_source_requests(&mut self) -> Vec<SourceLaunchRequest> {
        self.layers.source.take_launches()
    }

    pub fn take_discovery_requests(&mut self) -> Vec<DiscoveryUiRequest> {
        self.layers.source.take_discovery()
    }

    pub fn take_path_completion_requests(&mut self) -> Vec<PathCompletionRequest> {
        self.layers.source.take_path_completions()
    }

    /// The Source layer's four drains and completions. The dialog's half is the
    /// component's; these stay as `lvu-app`'s one-line entry points (§2.4).
    pub fn active_path_completion_generation(&self) -> Option<u64> {
        self.layers.source.active_path_completion_generation()
    }

    pub fn apply_path_completion_result(
        &mut self,
        generation: u64,
        original_draft: &str,
        _replacement: Option<String>,
        candidates: Vec<String>,
        error: Option<String>,
    ) -> bool {
        self.layers
            .source
            .complete_path(generation, original_draft, candidates, error)
    }

    pub fn apply_discovery_result(
        &mut self,
        generation: u64,
        items: Vec<DiscoveryItem>,
        status: String,
    ) -> bool {
        self.layers
            .source
            .complete_discovery(generation, items, status)
    }

    pub fn add_source_view(&mut self, source: SourceItem, view: ViewItem) {
        if self.sources.iter().all(|item| item.id != source.id) {
            self.sources.push(source);
        }
        if self.views.items.iter().all(|item| item.id != view.id) {
            self.views.states.insert(
                view.id.clone(),
                ViewState {
                    follow: true,
                    ..ViewState::default()
                },
            );
            self.views.items.push(view);
        }
        if self.views.items.len() == 1 {
            self.views.selected = 0;
        }
    }

    /// The persisted role of a view. Unknown views are editable.
    pub fn view_role(&self, view_id: &str) -> ViewRole {
        self.views.role(view_id)
    }

    /// Records a view's role. Only the runtime, reading persisted role
    /// metadata, may call this; nothing derives a role from a display name.
    pub fn set_view_role(&mut self, view_id: &str, role: ViewRole) {
        self.views.roles.insert(view_id.to_owned(), role);
    }

    /// The source's canonical view, by recorded role only.
    pub fn canonical_view_for_source(&self, source_id: &str) -> Option<&str> {
        self.views
            .items
            .iter()
            .find(|view| {
                view.source_id == source_id && self.view_role(&view.id) == ViewRole::Canonical
            })
            .map(|view| view.id.as_str())
    }

    /// True when the view's definition is fixed. Presentation is never fixed.
    pub fn view_definition_is_fixed(&self, view_id: &str) -> bool {
        self.views.definition_is_fixed(view_id)
    }

    /// Candidate views the runtime must register before their query can run.
    pub fn take_view_fork_requests(&mut self) -> Vec<ViewForkRequest> {
        self.views.take_view_fork_requests()
    }

    /// Derived views whose query succeeded and which must be persisted before
    /// they may be shown.
    pub fn take_ready_forks(&mut self) -> Vec<ReadyFork> {
        self.views.take_ready_forks()
    }

    /// Candidate views the runtime must unregister. A candidate reaches this
    /// list only after it has been removed from every visible structure.
    pub fn take_fork_discards(&mut self) -> Vec<String> {
        self.views.take_fork_discards()
    }

    /// Starts the candidate's query. Called by the runtime once the candidate
    /// view is registered, so a query can never be submitted for a view the
    /// runtime does not know.
    pub fn begin_fork_query(&mut self, candidate_view_id: &str) -> bool {
        self.views.begin_fork_query(candidate_view_id)
    }

    /// Abandons a candidate. Nothing was ever visible, so nothing is removed
    /// from the view list; the diagnostic goes back to the view the user is
    /// actually looking at.
    pub fn discard_fork(&mut self, candidate_view_id: &str, reason: String) -> bool {
        self.views.discard_fork(candidate_view_id, reason)
    }

    /// The candidate's definition, for persistence before it is installed.
    /// The name is the seam's; the bookmarks and the selection stamp the
    /// persistent state also carries are the shell's.
    pub fn fork_persistent_state(&self, candidate_view_id: &str) -> Option<PersistentViewState> {
        let name = self.views.fork_name(candidate_view_id)?;
        self.persistent_view_state_named(candidate_view_id, name)
    }

    /// Installs a candidate as a real view and selects it.
    ///
    /// The view lifecycle half — inserting the item after its origin, marking
    /// it derived, returning the origin to its applied definition — is the
    /// seam's. What is left here is the shell surface: an open dialog has to
    /// follow the fork, and the user has to end up on the new view.
    pub fn install_fork(&mut self, candidate_view_id: &str) -> bool {
        let Some(installed) = self.views.install_fork(candidate_view_id) else {
            return false;
        };
        // An open editor was working on the origin. Its context has to follow
        // the fork, or it would keep describing a view the edit no longer
        // belongs to: a step editor bound to All events shows no accepted
        // outputs, because All events has none.
        let carried =
            self.carry_dialogs_to_fork(&installed.origin_view_id, &installed.candidate_view_id);
        // Selecting a view normally returns to the log surface, but the user is
        // usually still typing: keep them in the editor they are working in, on
        // the view their edit just created.
        // `Focus::Layer` covers the three converted editors: `select_view`
        // resets focus but never touches the layer stack, so restoring the
        // focus is enough to leave the user in the editor they were typing in.
        let editing = matches!(self.focus, Focus::Layer).then_some(self.focus);
        self.select_view(&installed.candidate_view_id);
        if let Some(focus) = editing {
            self.focus = focus;
        }
        if carried {
            // A saved step closes its editor and returns to the step list, the
            // same as saving one on an ordinary view does. Popping the child
            // leaves the list underneath, which *is* the return (§5.3).
            self.close_layer(LayerId::EnrichmentStep);
        }
        // Deliberately silent: the view list already shows the new view
        // selected beside the one it came from, and a status-line notice here
        // would push the view's own query state off the end of the terminal at
        // ordinary widths, hiding the answer the user is actually waiting for.
        true
    }

    /// Re-points dialogs that were opened on the origin at the view its edit
    /// created. Returns true when a step editor was among them, because that
    /// one is finished rather than carried.
    fn carry_dialogs_to_fork(&mut self, origin: &str, candidate: &str) -> bool {
        let closed_step = self.layers.enrichment_step.retarget_view(origin, candidate);
        if let Some(held) = &mut self.raw_context_origin
            && held.view_id == origin
        {
            held.view_id = candidate.to_owned();
        }
        self.layers
            .external_command
            .retarget_view(origin, candidate);
        closed_step
    }

    /// Move the selection to the next quiet period longer than this view's gap
    /// threshold, and say what was found.
    ///
    /// Navigation only: nothing about the view's definition changes, so this
    /// works the same on All events as on a filtered view. The threshold is the
    /// view's own, editable in the Time dialog, and the status line names it
    /// alongside the gap so the answer is never a number the user has to guess
    /// the meaning of.
    fn jump_to_gap<P: RowProvider>(&mut self, direction: GapDirection, provider: &P) {
        let Some(view_id) = self.active_view_id().map(str::to_owned) else {
            return;
        };
        let Some(state) = self.views.states.get(&view_id) else {
            return;
        };
        let threshold_seconds = state.gap_threshold_seconds();
        let from = state.selected.clone();
        let threshold = i64::try_from(threshold_seconds)
            .unwrap_or(i64::MAX / 1_000_000_000)
            .saturating_mul(1_000_000_000);
        let basis = state.applied_time_basis;
        let hit = provider.find_gap(&view_id, from.as_ref(), direction, threshold, basis);
        let Some(hit) = hit else {
            let word = match direction {
                GapDirection::Forward => "after",
                GapDirection::Backward => "before",
            };
            // A view that cannot be read in the basis it is filtered on has no
            // gaps to report, and saying "none" would be a different claim from
            // "cannot tell". The message distinguishes them.
            let unavailable =
                basis != TimeBasis::Capture && provider.time_bounds(&view_id, basis).is_none();
            self.action_notice = Some(if unavailable {
                format!(
                    "gaps need {} times for this view; apply a window or a filter in that basis first",
                    time_basis_label(basis).to_lowercase()
                )
            } else {
                format!(
                    "no gap longer than {} {word} here",
                    format_capture_duration(threshold_seconds)
                )
            });
            return;
        };
        // Landing is a selection move, so it goes through the same path a
        // bookmark jump does and keeps the row addressed by identity.
        let Some(index) = provider.index_of_id(&view_id, &hit.row) else {
            self.action_notice = Some("the gap's record is no longer displayed".into());
            return;
        };
        let height = self
            .views
            .states
            .get(&view_id)
            .map_or(1, |state| state.viewport_height.max(1));
        let state = self.views.states.get_mut(&view_id).expect("view state");
        state.follow = false;
        state.selected = Some(hit.row.clone());
        state.top = index.saturating_sub(height / 2);
        state.gap_notice = Some(format!(
            "gap {} · quiet from {}",
            format_capture_duration(
                u64::try_from(hit.gap_nanos.max(0) / 1_000_000_000).unwrap_or(0)
            ),
            format_utc_nanos(hit.previous_unix_nanos)
        ));
    }

    /// Jumps to a record in its source's canonical view.
    ///
    /// The canonical view is the one place a record is always present, so a
    /// bookmark taken in a filtered view still leads somewhere when that
    /// filter no longer matches it.
    pub fn jump_to_record(&mut self, row: RowId, fallback_view: &str) -> bool {
        let Some(target) = self
            .canonical_view_for_source(&row.source_id)
            .map(str::to_owned)
            // A workspace whose canonical view has not been restored yet still
            // jumps somewhere useful rather than doing nothing.
            .or_else(|| (!fallback_view.is_empty()).then(|| fallback_view.to_owned()))
        else {
            return false;
        };
        if self.views.items.iter().all(|view| view.id != target) {
            return false;
        }
        self.select_view(&target);
        if let Some(state) = self.views.states.get_mut(&target) {
            state.follow = false;
            state.selected = Some(row.clone());
        }
        self.pending_jump = Some(PendingJump {
            view_id: target,
            row,
            attempts: 0,
        });
        self.focus = Focus::Logs;
        true
    }

    /// `o` (docs/raw-context-as-jump.md): with an origin held and the raw
    /// view active, return; otherwise jump to `anchor` (or the selected
    /// record) in its source's All events view, remembering the view, the
    /// record and `layer` — the dialog the jump closed — so `o` again puts
    /// them back. Nothing runs, nothing is filtered: it is a view switch and
    /// a selection, chased through `pending_jump` until the record resolves.
    fn raw_context<P: RowProvider>(
        &mut self,
        anchor: Option<RowId>,
        layer: Option<Open>,
        provider: &P,
    ) {
        if self
            .raw_context_origin
            .as_ref()
            .is_some_and(|held| self.active_view_id() == Some(held.raw_view_id.as_str()))
            && layer.is_none()
        {
            self.return_from_raw_context(provider);
            return;
        }
        let Some(from) = self.active_view_id().map(str::to_owned) else {
            return;
        };
        let anchor = anchor.or_else(|| self.view_state().and_then(|state| state.selected.clone()));
        let Some(anchor) = anchor else {
            self.action_notice = Some("select a log record first".into());
            if let Some(open) = layer {
                self.push_layer(open, provider);
            }
            return;
        };
        let Some(raw) = self
            .canonical_view_for_source(&anchor.source_id)
            .map(str::to_owned)
        else {
            self.action_notice = Some("this source has no All events view to show".into());
            if let Some(open) = layer {
                self.push_layer(open, provider);
            }
            return;
        };
        if raw == from {
            // Already the raw stream: nothing to jump to. The dialog that
            // asked comes back, because nothing else changed.
            self.action_notice = Some("this is the raw stream · o returns nowhere".into());
            if let Some(open) = layer {
                self.push_layer(open, provider);
            }
            return;
        }
        self.raw_context_origin = Some(RawContextOrigin {
            view_id: from,
            raw_view_id: raw.clone(),
            anchor: anchor.clone(),
            layer,
        });
        self.land_on(&raw, anchor);
    }

    /// The way back: the origin view, its anchor centred and selected, and
    /// the dialog `o` was pressed in, re-pushed from view-owned state.
    fn return_from_raw_context<P: RowProvider>(&mut self, provider: &P) {
        let Some(origin) = self.raw_context_origin.take() else {
            self.action_notice = Some("nothing to return to".into());
            return;
        };
        if self
            .views
            .items
            .iter()
            .all(|view| view.id != origin.view_id)
        {
            self.action_notice = Some("the view o came from was closed".into());
            return;
        }
        self.land_on(&origin.view_id, origin.anchor);
        if let Some(open) = origin.layer {
            self.push_layer(open, provider);
        }
    }

    /// Selects `view_id` with `row` selected and chased into the middle of
    /// the viewport, the way a bookmark's `Go to` lands.
    fn land_on(&mut self, view_id: &str, row: RowId) {
        self.select_view(view_id);
        if let Some(state) = self.views.states.get_mut(view_id) {
            state.follow = false;
            state.selected = Some(row.clone());
        }
        self.pending_jump = Some(PendingJump {
            view_id: view_id.to_owned(),
            row,
            attempts: 0,
        });
        self.focus = Focus::Logs;
    }

    /// An origin is a glance at the raw view; any navigation that leaves that
    /// view retires it. Checked before every action and by the status line.
    pub fn retire_left_raw_context(&mut self) {
        if self
            .raw_context_origin
            .as_ref()
            .is_some_and(|held| self.active_view_id() != Some(held.raw_view_id.as_str()))
        {
            self.raw_context_origin = None;
        }
    }

    /// The origin `o` would return to, while the raw view it landed in is
    /// still the active one; `None` otherwise.
    pub fn raw_context_origin(&self) -> Option<&RawContextOrigin> {
        self.raw_context_origin
            .as_ref()
            .filter(|held| self.active_view_id() == Some(held.raw_view_id.as_str()))
    }

    /// Whether a jump is still chasing its record into view.
    pub fn jump_pending(&self) -> bool {
        self.pending_jump.is_some()
    }

    /// Centres a pending jump once its record becomes locatable.
    ///
    /// Bounded: a record that never appears stops being chased and says so,
    /// rather than pinning the viewport forever.
    fn resolve_pending_jump<P: RowProvider>(&mut self, provider: &P, height: usize) -> bool {
        let Some(jump) = self.pending_jump.clone() else {
            return false;
        };
        if self.active_view_id() != Some(jump.view_id.as_str()) {
            self.pending_jump = None;
            return false;
        }
        if let Some(index) = provider.index_of_id(&jump.view_id, &jump.row) {
            let total = provider
                .page(&jump.view_id, ViewportRequest { start: 0, len: 0 })
                .total;
            if let Some(state) = self.views.states.get_mut(&jump.view_id) {
                state.follow = false;
                state.selected = Some(jump.row.clone());
                state.top = index
                    .saturating_sub(height / 2)
                    .min(total.saturating_sub(1));
            }
            self.pending_jump = None;
            return true;
        }
        let attempts = jump.attempts.saturating_add(1);
        if attempts >= MAX_JUMP_ATTEMPTS {
            self.pending_jump = None;
            self.action_notice = Some(format!(
                "record #{} is not addressable in this view yet",
                jump.row.sequence
            ));
            return true;
        }
        self.pending_jump = Some(PendingJump { attempts, ..jump });
        false
    }

    pub fn add_view(&mut self, view: ViewItem) {
        if self.views.items.iter().any(|item| item.id == view.id) {
            return;
        }
        self.views.roles.entry(view.id.clone()).or_default();
        self.views.states.insert(
            view.id.clone(),
            ViewState {
                follow: true,
                ..ViewState::default()
            },
        );
        self.views.items.push(view);
    }

    pub fn rename_view(&mut self, view_id: &str, name: String) -> bool {
        let Some(source_id) = self
            .views
            .items
            .iter()
            .find(|view| view.id == view_id)
            .map(|view| view.source_id.clone())
        else {
            return false;
        };
        if self
            .views
            .items
            .iter()
            .any(|view| view.id != view_id && view.source_id == source_id && view.name == name)
        {
            return false;
        }
        let view = self
            .views
            .items
            .iter_mut()
            .find(|view| view.id == view_id)
            .expect("view checked above");
        view.name = name;
        if let Some(state) = self.views.states.get_mut(view_id) {
            state.user_interaction_revision = state.user_interaction_revision.saturating_add(1);
        }
        true
    }

    /// Whether any assistance layer has a remote turn in flight, for the
    /// activity indicator. Stages belong to the components; this is the shell
    /// answering on their behalf.
    pub fn assistance_working(&self) -> bool {
        self.layers.ask.is_working() || self.layers.investigation.is_working()
    }

    pub fn take_ask_ai_requests(&mut self) -> Vec<AskAiRequest> {
        self.layers.ask.outbox.take()
    }

    pub fn take_source_ai_requests(&mut self) -> Vec<SourceAiRequest> {
        self.layers.source.take_ai()
    }
    pub fn take_recipe_requests(&mut self) -> Vec<RecipeRequest> {
        self.layers.recipes.outbox.take()
    }

    pub fn set_recipes(
        &mut self,
        meta: RecipeRequestMeta,
        items: Vec<RecipeItem>,
        error: Option<String>,
    ) {
        self.set_recipes_with_suggestions(meta, items, Vec::new(), error);
    }

    /// The worker's answer to a `List` or `History` request. The fence is the
    /// component's: one dialog spans both surfaces, so one generation counter
    /// decides whether an answer is still wanted.
    pub fn set_recipes_with_suggestions(
        &mut self,
        meta: RecipeRequestMeta,
        items: Vec<RecipeItem>,
        suggestions: Vec<RecipeSuggestion>,
        error: Option<String>,
    ) {
        self.layers
            .recipes
            .set_recipes(meta, items, suggestions, error);
    }

    pub fn recipe_saved(&mut self, meta: RecipeRequestMeta, message: String) {
        if !self.layers.recipes.saved(meta, message.clone()) {
            self.source_notice = Some(message);
        }
    }

    pub fn recipe_exported(&mut self, meta: RecipeRequestMeta, message: String) {
        if !self.layers.recipes.exported(meta, message.clone()) {
            self.source_notice = Some(message);
        }
    }

    pub fn recipe_failed(&mut self, meta: RecipeRequestMeta, message: String) {
        let _ = self.layers.recipes.failed(meta, &message);
        self.source_notice = Some(format!("recipe error: {message}"));
    }

    pub fn update_source_ai_progress(
        &mut self,
        generation: u64,
        stage: SourceAiStage,
        progress: String,
        session_id: Option<String>,
    ) -> bool {
        self.layers
            .source
            .update_ai_progress(generation, stage, progress, session_id)
    }

    pub fn finish_source_ai(
        &mut self,
        generation: u64,
        result: Result<SourceAiPreview, String>,
    ) -> bool {
        self.layers.source.finish_ai(generation, result)
    }

    pub fn source_ai_launch_succeeded(&mut self, generation: u64, view_id: &str) {
        if self.layers.source.ai_launch_matches(generation) {
            self.select_view(view_id);
            self.close_source_layer();
            self.source_notice = Some("reviewed agent source started".into());
        }
    }

    pub fn source_ai_launch_failed(&mut self, generation: u64, message: String) {
        self.finish_source_ai(generation, Err(message));
    }

    /// Pushes the Source layer from the shell: a refused launch reopens the
    /// dialog on the draft that failed, wherever the user was.
    fn push_source_layer(&mut self) {
        if self.layers.stack.is_empty() {
            self.layer_return_focus = match self.focus {
                Focus::Selector | Focus::Logs | Focus::Details => self.focus,
                _ => Focus::Logs,
            };
        }
        self.layers.stack.retain(|id| *id != LayerId::Source);
        self.layers.stack.push(LayerId::Source);
        self.focus = Focus::Layer;
    }

    /// Closes the Source layer from the shell: a reviewed source started, or
    /// `lvu-app` opened the workspace on sources that already exist.
    pub fn close_source_layer(&mut self) {
        self.layers.source.close();
        self.close_layer(LayerId::Source);
    }

    pub fn take_investigation_requests(&mut self) -> Vec<InvestigationRequest> {
        self.layers.investigation.outbox.take()
    }

    pub fn set_investigations(&mut self, items: Vec<InvestigationItem>) {
        self.layers.investigation.set_saved(items);
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
        self.layers.investigation.progress(
            generation,
            stage,
            progress,
            session_id,
            snapshot_dir,
            manifest_path,
        )
    }

    pub fn investigation_ready(&mut self, generation: u64, item: InvestigationItem) -> bool {
        self.layers.investigation.ready(generation, item)
    }

    pub fn push_investigation_event(
        &mut self,
        session_id: &str,
        message: String,
        terminal: Result<(), String>,
    ) -> bool {
        self.layers
            .investigation
            .push_event(session_id, message, terminal)
    }

    pub fn append_investigation_output(&mut self, session_id: &str, message: String) -> bool {
        self.layers.investigation.append_output(session_id, message)
    }

    /// The bounded sample `lvu-app` prepared for a request.
    pub fn record_ask_sample(&mut self, generation: u64, sample: AskSample) -> bool {
        self.layers.ask.sampled(generation, sample)
    }

    /// The agent reported that the sample it was given was insufficient.
    pub fn ask_needs_more_data(&mut self, generation: u64) -> bool {
        self.layers.ask.needs_more(generation)
    }

    pub fn update_ask_ai_progress(
        &mut self,
        generation: u64,
        stage: AskAiStage,
        progress: String,
        session_id: Option<String>,
        snapshot_dir: Option<String>,
    ) -> bool {
        self.layers
            .ask
            .progress(generation, stage, progress, session_id, snapshot_dir)
    }

    /// §2.4 keeps the dialog's half in the component; the revision check is
    /// the shell's, because only it holds `Views`.
    pub fn finish_ask_ai(
        &mut self,
        generation: u64,
        view_id: &str,
        definition_revision: u64,
        expression: Result<(String, String), String>,
    ) -> bool {
        let current = self.view_definition_revision(view_id) == Some(definition_revision);
        self.layers.ask.complete(
            generation,
            view_id,
            definition_revision,
            current,
            expression,
        )
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
        let current = self.view_definition_revision(view_id) == Some(revision);
        self.layers
            .ask
            .complete_recipe(generation, view_id, revision, current, expression, chain)
    }

    /// The worker accepted a view mutation. §4.2: the shell says what happened
    /// to the view and every open layer decides for itself; it does not reach
    /// into a dialog to close it.
    pub fn view_request_succeeded(&mut self, view_id: &str) {
        self.broadcast_view_event(ViewEvent::SourcesChanged {
            view_id: view_id.to_owned(),
        });
        self.select_view(view_id);
        self.source_notice = Some("view saved".into());
    }

    pub fn view_request_failed(&mut self, message: String) {
        self.layers.view.fail(message.clone());
        self.source_notice = Some(format!("view error: {message}"));
    }

    pub fn select_view(&mut self, view_id: &str) {
        if let Some(index) = self.views.items.iter().position(|view| view.id == view_id) {
            if self.active_view_id() != Some(view_id) {
                self.cancel_active_correlation();
            }
            self.views.selected = index;
            self.focus = Focus::Logs;
            self.record_view_selection(view_id);
        }
    }

    /// Stamps a view as the most recently used one of its source.
    fn record_view_selection(&mut self, view_id: &str) {
        let stamp = self.next_selection_stamp;
        self.next_selection_stamp = self.next_selection_stamp.saturating_add(1);
        self.view_selection_stamps.insert(view_id.to_owned(), stamp);
    }

    /// The persisted ordering value for a view, saved with its presentation.
    pub fn view_selection_stamp(&self, view_id: &str) -> u64 {
        self.view_selection_stamps
            .get(view_id)
            .copied()
            .unwrap_or_default()
    }

    /// Restores a view's remembered position in that ordering.
    ///
    /// Later stamps issued in this run must still win, so the counter is
    /// advanced past anything restored.
    pub fn set_view_selection_stamp(&mut self, view_id: &str, stamp: u64) {
        if stamp == 0 {
            return;
        }
        self.view_selection_stamps.insert(view_id.to_owned(), stamp);
        self.next_selection_stamp = self.next_selection_stamp.max(stamp.saturating_add(1));
    }

    /// Reselects the view a source was last used through.
    ///
    /// Only applies while the current selection belongs to that source and has
    /// not been chosen more recently than the remembered one, so a user who has
    /// already moved somewhere else is never pulled back. Runs once per source.
    pub fn restore_source_selection(&mut self, source_id: &str) -> bool {
        if !self.restored_selections.insert(source_id.to_owned()) {
            return false;
        }
        let Some((view_id, stamp)) = self
            .views
            .items
            .iter()
            .filter(|view| view.source_id == source_id)
            .map(|view| (view.id.clone(), self.view_selection_stamp(&view.id)))
            .max_by_key(|(_, stamp)| *stamp)
            .filter(|(_, stamp)| *stamp > 0)
        else {
            return false;
        };
        let Some(current) = self.active_view_id().map(str::to_owned) else {
            return false;
        };
        if current == view_id {
            return false;
        }
        let current_is_this_source = self
            .views
            .items
            .iter()
            .any(|view| view.id == current && view.source_id == source_id);
        if !current_is_this_source || self.view_selection_stamp(&current) > stamp {
            return false;
        }
        // Restoring is not a user action: it must not move focus out of
        // whatever they have already opened.
        let focus = self.focus;
        self.select_view(&view_id);
        self.view_selection_stamps.insert(view_id, stamp);
        self.next_selection_stamp = self.next_selection_stamp.max(stamp.saturating_add(1));
        self.focus = focus;
        true
    }

    pub fn source_request_succeeded(&mut self, request: &SourceLaunchRequest, view_id: &str) {
        self.source_notice = Some("source started".into());
        self.select_view(view_id);
        if self.layers.source.launch_matches(request) {
            self.close_source_layer();
        }
    }

    pub fn source_request_failed(&mut self, request: SourceLaunchRequest, message: String) {
        self.source_notice = Some(format!("source error: {message}"));
        if !self.layers.source.fail_launch(&request, &message) {
            self.layers.source.open_on_failure(request, message);
            self.push_source_layer();
        }
    }

    pub fn discovery_selection_succeeded(&mut self, generation: u64, view_id: &str) {
        self.source_notice = Some("discovered source started".into());
        self.select_view(view_id);
        if self.layers.source.discovery_matches(generation) {
            self.close_source_layer();
        }
    }

    pub fn discovery_selection_failed(&mut self, generation: u64, message: String) {
        self.source_notice = Some(format!("source error: {message}"));
        self.layers.source.fail_discovery(generation, message);
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
        // Folding is a property of the view, so the provider is told about it
        // before anything is measured; it ignores an unchanged policy. A view
        // that has never folded costs nothing here, which keeps the default
        // redraw path exactly as it was.
        let folding = self
            .views
            .states
            .get(&view_id)
            .is_some_and(|state| state.fold_enabled || state.fold_summary.is_some());
        if folding {
            if let Some(state) = self.views.states.get(&view_id) {
                provider.set_fold(&view_id, &fold_request(state));
            }
            let summary = provider.fold_summary(&view_id);
            if let Some(state) = self.views.states.get_mut(&view_id) {
                state.fold_summary = summary;
            }
        }
        let jumped = self.resolve_pending_jump(provider, viewport_height.max(1));
        let revision = provider.revision(&view_id);
        let total = provider
            .page(&view_id, ViewportRequest { start: 0, len: 0 })
            .total;
        let height = viewport_height.max(1);
        let state = self
            .views
            .states
            .get_mut(&view_id)
            .expect("view state exists");
        let changed = revision != state.provider_revision
            || total != state.last_total
            || height != state.viewport_height;
        if !changed {
            return jumped;
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
        self.views
            .requests
            .drain()
            .map(|(_, request)| request)
            .collect()
    }

    /// Enqueues due live searches. Tests pass a future instant to avoid sleeps.
    pub fn flush_debounced_searches(&mut self, now: Instant) -> bool {
        let due: Vec<String> = self
            .views
            .states
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
            if self
                .enqueue_live_query(view_id, QueryPurpose::Search)
                .is_some()
            {
                self.views
                    .states
                    .get_mut(view_id)
                    .expect("view state")
                    .search
                    .search_due = None;
            } else {
                // Backpressure must not consume the final (possibly empty) draft.
                self.views
                    .states
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
        self.shell.clock_now_unix_nanos = now_unix_nanos;
        self.last_clock_unix_nanos = Some(now_unix_nanos);
        let cadence_due = clock_moved_backward
            || self
                .next_rolling_refresh
                .is_none_or(|deadline| elapsed_now >= deadline);
        if cadence_due {
            self.next_rolling_refresh = elapsed_now.checked_add(Duration::from_secs(1));
            for state in self.views.states.values_mut() {
                if matches!(
                    state.desired_capture_time_policy,
                    Some(CaptureTimePolicy::Recent { .. })
                ) {
                    state.rolling_refresh_due = true;
                }
            }
        }
        let rolling: Vec<(String, CaptureTimePolicy, CaptureTimeRange)> = self
            .views
            .states
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
                .views
                .states
                .get(&view_id)
                .expect("collected view")
                .desired_constraints
                .capture_time;
            {
                let state = self.views.states.get_mut(&view_id).expect("collected view");
                state.desired_constraints.capture_time = Some(range);
                state.desired_capture_time_policy = Some(policy);
            }
            if self.views.apply_desired_time(&view_id).is_some() {
                self.views
                    .states
                    .get_mut(&view_id)
                    .expect("collected view")
                    .rolling_refresh_due = false;
                changed = true;
            } else {
                self.views
                    .states
                    .get_mut(&view_id)
                    .expect("collected view")
                    .desired_constraints
                    .capture_time = previous;
            }
        }
        changed
    }

    /// Accepts a completed query, and settles any fork the query belonged to.
    ///
    /// A candidate only becomes a real view here, and only when its own query
    /// succeeded: a failure or a superseded revision leaves the candidate
    /// unbuilt, which is why a rejected filter cannot leave a phantom view.
    pub fn apply_query_completion(&mut self, completion: QueryCompletion) -> bool {
        let candidate = self.views.fork_candidate(&completion.view_id);
        let failure = completion
            .result
            .as_ref()
            .err()
            .map(|failure| failure.message.clone());
        let accepted = self.apply_query_completion_inner(completion);
        let Some(candidate) = candidate else {
            return accepted;
        };
        if !accepted {
            // Stale or superseded: the candidate keeps waiting for its own
            // newest query rather than being installed or thrown away.
            return accepted;
        }
        match failure {
            None => self.views.mark_fork_ready(&candidate),
            Some(message) => {
                self.views.discard_fork(&candidate, message);
            }
        }
        accepted
    }

    fn apply_query_completion_inner(&mut self, completion: QueryCompletion) -> bool {
        let Some(state) = self.views.states.get_mut(&completion.view_id) else {
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
        let mut close_enrichment_step = false;
        // §12.5: a command step's save, removal or reorder is a chain change,
        // and the External command dialog learns its answer the way every
        // layer does (§4.2), as a view event.
        let mut command_chain_settled: Option<Result<(), String>> = None;
        let request_is_pending = [
            &state.search,
            &state.advanced,
            &state.enrichment,
            &state.grouping,
        ]
        .into_iter()
        .any(|editor| editor.pending_generation == Some(completion.generation))
            || state
                .pending_color_rules
                .is_some_and(|(generation, _)| generation == completion.generation)
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
                state.exact_field = constraints.exact_field.clone();
                state.advanced.applied = constraints.advanced_polars.clone().unwrap_or_default();
                let appended_enrichment = constraints.enrichments.len() > state.enrichments.len();
                let previous_chain =
                    std::mem::replace(&mut state.enrichments, constraints.enrichments.clone());
                // §12.5: every command step keeps its own run state. A step
                // whose definition changed gets a new revision (its last
                // publication stays readable until a run replaces it); a
                // step that left the chain takes its state with it. Saving
                // never runs anything.
                let mut saved_command = None;
                for step in state.enrichments.iter().filter(|step| step.is_command()) {
                    let previous = previous_chain
                        .iter()
                        .find(|candidate| candidate.id == step.id)
                        .and_then(EnrichmentDefinition::command_stage);
                    let run = state.command_steps.entry(step.id.0.clone()).or_default();
                    // A restored chain arrives with its revisions; only a
                    // fresh step or a changed definition earns a new one.
                    let changed = match previous {
                        Some(previous) => Some(previous) != step.command_stage(),
                        None => run.revision == 0,
                    };
                    if changed {
                        run.revision = run.revision.saturating_add(1);
                        saved_command = Some(step.source.clone());
                    }
                }
                let chain = &state.enrichments;
                state.command_steps.retain(|id, _| {
                    chain
                        .iter()
                        .any(|step| step.is_command() && step.id.0 == *id)
                });
                if enrichment_mutation == Some(PendingEnrichmentMutation::CommandSave)
                    && let Some(name) = saved_command
                {
                    self.action_notice = Some(format!(
                        "command step {name} saved · not run; new records wait for an explicit run"
                    ));
                }
                if matches!(
                    enrichment_mutation,
                    Some(
                        PendingEnrichmentMutation::CommandSave
                            | PendingEnrichmentMutation::Remove
                            | PendingEnrichmentMutation::Reorder
                    )
                ) {
                    command_chain_settled = Some(Ok(()));
                }
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
                // Accepted with the query that evaluated them, so the dialog's
                // "edited" state clears exactly when the rows repaint.
                state.color_rules = constraints.color_rules.clone();
                if state
                    .pending_color_rules
                    .is_some_and(|(_, revision)| revision <= completion.revision)
                {
                    state.pending_color_rules = None;
                    state.color_rules_error = None;
                }
                state.applied_capture_time = constraints.capture_time;
                if let Some(policy) = accepted_time_policy {
                    state.applied_capture_time_policy = policy;
                }
                if let Some(basis) = accepted_time_basis {
                    state.applied_time_basis = basis;
                    state.applied_time_field = constraints.time_field.clone();
                }
                state.applied_query_revision = completion.revision;
                if state
                    .pending_recipe
                    .as_ref()
                    .is_some_and(|pending| pending.revision == completion.revision)
                    && let Some(pending) = state.pending_recipe.take()
                {
                    if let Some(outcome) = pending.suggestion {
                        self.layers.recipes.record_outcome(outcome);
                    }
                    // §12.5: a recipe's command steps arrive saved and unrun.
                    // One whose program this machine lacks is still applied,
                    // and the notice says so now rather than at the first run.
                    let missing = missing_programs(&state.enrichments);
                    if !missing.is_empty() {
                        let list = missing
                            .iter()
                            .map(|(name, program)| format!("{name} needs {program}"))
                            .collect::<Vec<_>>()
                            .join(", ");
                        self.action_notice = Some(format!(
                            "recipe applied · not on this machine: {list} · the step is saved unrun"
                        ));
                    }
                    state.applied_capture_time_policy = pending.capture_time_policy;
                    state.applied_time_basis = pending.time_basis;
                    state.applied_time_field = None;
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
                        // A saved step returns the user to the layer-one list.
                        close_enrichment_step = true;
                    }
                    state.enrichment_editing = None;
                }
                if accepted_grouping {
                    state.grouping.error = None;
                }
            }
            Err(failure) => {
                if state.pending_color_rules == Some((completion.generation, completion.revision)) {
                    state.pending_color_rules = None;
                    state.desired_constraints = applied_constraints(state);
                    state.color_rules_error = Some(failure.message);
                    return true;
                }
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
                            .views
                            .states
                            .get(&completion.view_id)
                            .map_or_else(Vec::new, |state| state.enrichments.clone());
                        self.views.enqueue_enrichment_chain(
                            &completion.view_id,
                            stages,
                            accepted,
                            PendingEnrichmentMutation::Reaffirm,
                        );
                    } else {
                        self.views
                            .enqueue(&completion.view_id, failed_purpose, Some(accepted))
                            .ok();
                    }
                    self.editor_mut(&completion.view_id, failed_purpose).error =
                        Some(failure_message);
                    return true;
                }
                let failed_purpose = failure.purpose;
                let failure_message = failure.message;
                if failed_purpose == QueryPurpose::Enrichment {
                    if matches!(
                        state.pending_enrichment_mutation,
                        Some(
                            PendingEnrichmentMutation::CommandSave
                                | PendingEnrichmentMutation::Remove
                                | PendingEnrichmentMutation::Reorder
                        )
                    ) {
                        command_chain_settled = Some(Err(failure_message.clone()));
                    }
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
                    self.views.enqueue_enrichment_chain(
                        &completion.view_id,
                        value,
                        pending_enrichment_value,
                        pending_enrichment_mutation.unwrap_or(PendingEnrichmentMutation::Edit),
                    )
                } else if let Some((purpose, value)) = counterpart {
                    // The older counterpart was never allowed to publish. Rebase it
                    // on the last accepted constraint and give it a fresh revision.
                    self.views
                        .enqueue(&completion.view_id, purpose, Some(value))
                        .ok()
                } else if restore_enrichment {
                    let stages = self
                        .views
                        .states
                        .get(&completion.view_id)
                        .map_or_else(Vec::new, |state| state.enrichments.clone());
                    self.views.enqueue_enrichment_chain(
                        &completion.view_id,
                        stages,
                        String::new(),
                        PendingEnrichmentMutation::Reaffirm,
                    )
                } else if let Some((purpose, value)) = restore_applied {
                    // Dispatchers advance desired composite revisions before
                    // compilation. Reaffirm the accepted snapshot so arrivals
                    // cannot remain fenced by the rejected candidate.
                    self.views
                        .enqueue(&completion.view_id, purpose, Some(value))
                        .ok()
                } else if pending_time.is_some() {
                    self.views.enqueue_time_query(&completion.view_id)
                } else {
                    None
                };
                if let Some(revision) = rebase
                    && pending_time.is_some()
                    && self
                        .views
                        .states
                        .get(&completion.view_id)
                        .is_some_and(|state| state.pending_time.is_none())
                {
                    self.views.track_time_request(
                        &completion.view_id,
                        revision,
                        pending_time.flatten(),
                    );
                }
                // Internal rebase submissions must not erase the diagnostic for
                // the user's rejected draft, even when the failed constraint is
                // also the only accepted constraint available to reaffirm.
                self.editor_mut(&completion.view_id, failed_purpose).error = Some(failure_message);
            }
        }
        // §4.2: what used to be a shell flag reaching into the step dialog is
        // now the event the step layer decides for itself on. It is emitted
        // exactly where `close_enrichment_step` was set — an accepted Add or
        // Edit of the draft the step editor holds — so a Reaffirm or a Remove
        // still leaves an open editor alone (§6.5).
        if close_enrichment_step || command_chain_settled == Some(Ok(())) {
            self.broadcast_view_event(ViewEvent::QueryAccepted {
                view_id: completion.view_id.clone(),
                purpose: QueryPurpose::Enrichment,
                revision: completion.revision,
            });
        }
        if let Some(Err(message)) = command_chain_settled {
            self.broadcast_view_event(ViewEvent::QueryRejected {
                view_id: completion.view_id.clone(),
                purpose: QueryPurpose::Enrichment,
                message,
            });
        }
        true
    }

    // ---- component bridge (component-model.md §6.4) ----------------------
    //
    // `Ctx` is never produced by a method on `&mut self`: each of these
    // destructures `App` once so that the shell's shared state and the
    // component are provably different fields (§2.5).

    fn push_layer<P: RowProvider>(&mut self, open: Open, provider: &P) {
        // Settings has nothing to show without the effective-settings snapshot
        // `lvu-app` supplies. A component cannot decline its own `open`, so the
        // refusal — and the notice the legacy `Action::OpenSettings` arm wrote —
        // stays here.
        if open == Open::Settings && !self.layers.settings.is_configured() {
            self.source_notice = Some("settings are unavailable in this build".into());
            return;
        }
        // A layer that edits the active view opens nothing without one, which
        // is the precondition each legacy `Open*` arm carried itself (§6.4).
        if open.needs_active_view() && self.views.active_item().is_none() {
            return;
        }
        // Legacy shell scroll state that the unconverted dialogs still share.
        // Every legacy `Open*` arm zeroes it; keeping that here means opening a
        // layer leaves exactly the same state behind as it used to.
        self.dialog_scroll = 0;
        self.dialog_scroll_focused = false;
        let layer = open.layer();
        let App {
            shell,
            layers,
            views,
            sources,
            action_notice,
            appearance,
            agent,
            ..
        } = self;
        let mut ctx = shell_ctx(
            views,
            sources,
            appearance,
            agent,
            shell,
            action_notice,
            provider,
        );
        match open {
            Open::Storage => layers.storage.open((), &mut ctx),
            Open::Time => layers.time.open((), &mut ctx),
            Open::Help => layers.help.open((), &mut ctx),
            Open::Settings => layers.settings.open((), &mut ctx),
            Open::Fields => layers.fields.open(None, &mut ctx),
            Open::FieldColumn { column } => layers.fields.open(Some(column), &mut ctx),
            Open::View => layers.view.open(ViewDialogMode::Clone, &mut ctx),
            Open::ViewMembership => layers.view.open(ViewDialogMode::Sources, &mut ctx),
            Open::Source => layers.source.open((), &mut ctx),
            Open::Folding => layers.folding.open((), &mut ctx),
            Open::ViewSummary => layers.view_summary.open((), &mut ctx),
            Open::Recipes { mode } => layers.recipes.open(
                if mode == RecipeDialogMode::Browse {
                    crate::components::recipes::RecipesOpen::Browse
                } else {
                    crate::components::recipes::RecipesOpen::Mode(mode)
                },
                &mut ctx,
            ),
            Open::RecipeHistory {
                recipe_id,
                recipe_name,
            } => layers.recipes.open(
                crate::components::recipes::RecipesOpen::History {
                    recipe_id,
                    recipe_name,
                },
                &mut ctx,
            ),
            Open::Search => layers.filter.open(Some(QueryPurpose::Search), &mut ctx),
            Open::Advanced => layers.filter.open(Some(QueryPurpose::Advanced), &mut ctx),
            Open::Grouping => layers.grouping.open(None, &mut ctx),
            Open::ColorRules => layers.color_rules.open((), &mut ctx),
            Open::Enrichment => layers.enrichment.open((), &mut ctx),
            Open::EnrichmentStep { editing, prefill } => layers.enrichment_step.open(
                crate::components::enrichment_step::StepOpen { editing, prefill },
                &mut ctx,
            ),
            Open::ExternalCommand { stage, insert_at } => layers.external_command.open(
                crate::components::external_command::CommandOpen { stage, insert_at },
                &mut ctx,
            ),
            Open::Bookmarks => layers.bookmarks.open((), &mut ctx),
            Open::Ask(params) => layers.ask.open(params, &mut ctx),
            Open::Investigation => layers.investigation.open((), &mut ctx),
            Open::Correlation(params) => layers.correlation.open(params, &mut ctx),
        }
        let first = layers.stack.is_empty();
        layers.stack.retain(|id| *id != layer);
        layers.stack.push(layer);
        if first {
            // §1: "base focus resumes exactly as before the first push". Only a
            // base focus is restorable; `Focus::Layer` itself is not a state
            // the stack may return to.
            self.layer_return_focus = match self.focus {
                Focus::Selector | Focus::Logs | Focus::Details => self.focus,
                _ => Focus::Logs,
            };
        }
        self.focus = Focus::Layer;
    }

    fn pop_layer(&mut self) {
        self.layers.stack.pop();
        if self.layers.stack.is_empty() {
            self.focus = self.layer_return_focus;
        }
    }

    fn apply_outcome<P: RowProvider>(&mut self, outcome: Outcome, provider: &P) {
        match outcome {
            Outcome::Ignored | Outcome::Consumed => {}
            Outcome::Close => self.pop_layer(),
            Outcome::Replace(open) => {
                self.pop_layer();
                self.push_layer(open, provider);
            }
            Outcome::OpenChild(open) => self.push_layer(open, provider),
            Outcome::Legacy(action) => {
                self.pop_layer();
                self.handle(action, provider);
            }
            // The layer stays underneath: the shell operation runs and the
            // layer that asked is still on top to show its result.
            Outcome::Defer(action) => self.handle(action, provider),
        }
    }

    fn handle_event<P: RowProvider>(&mut self, event: RawEvent, provider: &P) {
        // Ctrl-C is intercepted by the shell before dispatch; quitting is not a
        // component's business (§1).
        if let RawEvent::Key(key) = &event
            && matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat)
            && key.modifiers.contains(KeyModifiers::CONTROL)
            && key.code == KeyCode::Char('c')
        {
            self.should_quit = true;
            return;
        }
        let Some(top) = self.layers.top() else {
            return;
        };
        let App {
            shell,
            layers,
            views,
            sources,
            action_notice,
            appearance,
            agent,
            ..
        } = self;
        let mut ctx = shell_ctx(
            views,
            sources,
            appearance,
            agent,
            shell,
            action_notice,
            provider,
        );
        let outcome = match top {
            LayerId::Storage => dispatch_raw(&mut layers.storage, event, &mut ctx),
            LayerId::Time => dispatch_raw(&mut layers.time, event, &mut ctx),
            LayerId::Help => dispatch_raw(&mut layers.help, event, &mut ctx),
            LayerId::Settings => dispatch_raw(&mut layers.settings, event, &mut ctx),
            LayerId::Fields => dispatch_raw(&mut layers.fields, event, &mut ctx),
            LayerId::View => dispatch_raw(&mut layers.view, event, &mut ctx),
            LayerId::Source => dispatch_raw(&mut layers.source, event, &mut ctx),
            LayerId::Folding => dispatch_raw(&mut layers.folding, event, &mut ctx),
            LayerId::Recipes | LayerId::RecipeHistory => {
                dispatch_raw(&mut layers.recipes, event, &mut ctx)
            }
            LayerId::Filter => dispatch_raw(&mut layers.filter, event, &mut ctx),
            LayerId::Grouping => dispatch_raw(&mut layers.grouping, event, &mut ctx),
            LayerId::ColorRules => dispatch_raw(&mut layers.color_rules, event, &mut ctx),
            LayerId::Enrichment => dispatch_raw(&mut layers.enrichment, event, &mut ctx),
            LayerId::EnrichmentStep => dispatch_raw(&mut layers.enrichment_step, event, &mut ctx),
            LayerId::ExternalCommand => dispatch_raw(&mut layers.external_command, event, &mut ctx),
            LayerId::Bookmarks => dispatch_raw(&mut layers.bookmarks, event, &mut ctx),
            LayerId::Ask => dispatch_raw(&mut layers.ask, event, &mut ctx),
            LayerId::Investigation => dispatch_raw(&mut layers.investigation, event, &mut ctx),
            LayerId::Correlation => dispatch_raw(&mut layers.correlation, event, &mut ctx),
            LayerId::ViewSummary => dispatch_raw(&mut layers.view_summary, event, &mut ctx),
        };
        self.apply_outcome(outcome, provider);
    }

    /// §8.10: the action row the top layer is showing, as `dispatch_raw` reads
    /// it to resolve a mnemonic. Public because the audit that every underlined
    /// letter is a live key has to be able to enumerate them.
    pub fn top_layer_action_labels<P: RowProvider>(&mut self, provider: &P) -> Vec<&'static str> {
        let Some(top) = self.layers.top() else {
            return Vec::new();
        };
        let App {
            shell,
            layers,
            views,
            sources,
            action_notice,
            appearance,
            agent,
            ..
        } = self;
        let ctx = shell_ctx(
            views,
            sources,
            appearance,
            agent,
            shell,
            action_notice,
            provider,
        );
        match top {
            LayerId::Storage => layers.storage.action_labels(&ctx),
            LayerId::Time => layers.time.action_labels(&ctx),
            LayerId::Help => layers.help.action_labels(&ctx),
            LayerId::Settings => layers.settings.action_labels(&ctx),
            LayerId::Fields => layers.fields.action_labels(&ctx),
            LayerId::View => layers.view.action_labels(&ctx),
            LayerId::Source => layers.source.action_labels(&ctx),
            LayerId::Folding => layers.folding.action_labels(&ctx),
            LayerId::ColorRules => layers.color_rules.action_labels(&ctx),
            LayerId::Recipes | LayerId::RecipeHistory => layers.recipes.action_labels(&ctx),
            LayerId::Filter => layers.filter.action_labels(&ctx),
            LayerId::Grouping => layers.grouping.action_labels(&ctx),
            LayerId::Enrichment => layers.enrichment.action_labels(&ctx),
            LayerId::EnrichmentStep => layers.enrichment_step.action_labels(&ctx),
            LayerId::ExternalCommand => layers.external_command.action_labels(&ctx),
            LayerId::Bookmarks => layers.bookmarks.action_labels(&ctx),
            LayerId::Ask => layers.ask.action_labels(&ctx),
            LayerId::Investigation => layers.investigation.action_labels(&ctx),
            LayerId::Correlation => layers.correlation.action_labels(&ctx),
            LayerId::ViewSummary => layers.view_summary.action_labels(&ctx),
        }
    }

    /// §8.10: whether the top layer has a text field focused right now, which
    /// is what decides whether a bare letter is a mnemonic or a character.
    /// Read by the audit that walks every layer's action row.
    pub fn top_layer_text_focus(&self) -> bool {
        let Some(top) = self.layers.top() else {
            return false;
        };
        let layers = &self.layers;
        match top {
            LayerId::Storage => layers.storage.text_focus(),
            LayerId::Time => layers.time.text_focus(),
            LayerId::Help => layers.help.text_focus(),
            LayerId::Settings => layers.settings.text_focus(),
            LayerId::Fields => layers.fields.text_focus(),
            LayerId::View => layers.view.text_focus(),
            LayerId::Source => layers.source.text_focus(),
            LayerId::Folding => layers.folding.text_focus(),
            LayerId::ColorRules => layers.color_rules.text_focus(),
            LayerId::Recipes | LayerId::RecipeHistory => layers.recipes.text_focus(),
            LayerId::Filter => layers.filter.text_focus(),
            LayerId::Grouping => layers.grouping.text_focus(),
            LayerId::Enrichment => layers.enrichment.text_focus(),
            LayerId::EnrichmentStep => layers.enrichment_step.text_focus(),
            LayerId::ExternalCommand => layers.external_command.text_focus(),
            LayerId::Bookmarks => layers.bookmarks.text_focus(),
            LayerId::Ask => layers.ask.text_focus(),
            LayerId::Investigation => layers.investigation.text_focus(),
            LayerId::Correlation => layers.correlation.text_focus(),
            LayerId::ViewSummary => layers.view_summary.text_focus(),
        }
    }

    fn deliver_command<P: RowProvider>(
        &mut self,
        layer: LayerId,
        id: crate::command_palette::CommandId,
        provider: &P,
    ) {
        if self.layers.top() != Some(layer) {
            return;
        }
        let App {
            shell,
            layers,
            views,
            sources,
            action_notice,
            appearance,
            agent,
            ..
        } = self;
        let mut ctx = shell_ctx(
            views,
            sources,
            appearance,
            agent,
            shell,
            action_notice,
            provider,
        );
        let outcome = match layer {
            LayerId::Storage => layers.storage.handle(ComponentEvent::Command(id), &mut ctx),
            LayerId::Time => layers.time.handle(ComponentEvent::Command(id), &mut ctx),
            LayerId::Help => layers.help.handle(ComponentEvent::Command(id), &mut ctx),
            LayerId::Settings => layers
                .settings
                .handle(ComponentEvent::Command(id), &mut ctx),
            LayerId::Fields => layers.fields.handle(ComponentEvent::Command(id), &mut ctx),
            LayerId::View => layers.view.handle(ComponentEvent::Command(id), &mut ctx),
            LayerId::Source => layers.source.handle(ComponentEvent::Command(id), &mut ctx),
            LayerId::Folding => layers.folding.handle(ComponentEvent::Command(id), &mut ctx),
            LayerId::Recipes | LayerId::RecipeHistory => {
                layers.recipes.handle(ComponentEvent::Command(id), &mut ctx)
            }
            LayerId::Filter => layers.filter.handle(ComponentEvent::Command(id), &mut ctx),
            LayerId::Grouping => layers
                .grouping
                .handle(ComponentEvent::Command(id), &mut ctx),
            LayerId::ColorRules => layers
                .color_rules
                .handle(ComponentEvent::Command(id), &mut ctx),
            LayerId::Enrichment => layers
                .enrichment
                .handle(ComponentEvent::Command(id), &mut ctx),
            LayerId::EnrichmentStep => layers
                .enrichment_step
                .handle(ComponentEvent::Command(id), &mut ctx),
            LayerId::ExternalCommand => layers
                .external_command
                .handle(ComponentEvent::Command(id), &mut ctx),
            LayerId::Bookmarks => layers
                .bookmarks
                .handle(ComponentEvent::Command(id), &mut ctx),
            LayerId::Ask => layers.ask.handle(ComponentEvent::Command(id), &mut ctx),
            LayerId::Investigation => layers
                .investigation
                .handle(ComponentEvent::Command(id), &mut ctx),
            LayerId::Correlation => layers
                .correlation
                .handle(ComponentEvent::Command(id), &mut ctx),
            LayerId::ViewSummary => layers
                .view_summary
                .handle(ComponentEvent::Command(id), &mut ctx),
        };
        self.apply_outcome(outcome, provider);
    }

    /// §4.2: something changed in the shared view state, so every open layer
    /// hears about it, top first, and decides for itself. The shell never
    /// reaches into a layer to close it; a layer that has nothing left to edit
    /// returns `Close` and is popped here.
    fn broadcast_view_event(&mut self, event: ViewEvent) {
        for id in self.layers.stack.iter().rev().copied().collect::<Vec<_>>() {
            let App {
                shell,
                layers,
                views,
                sources,
                action_notice,
                appearance,
                agent,
                ..
            } = self;
            let mut ctx = shell_ctx(
                views,
                sources,
                appearance,
                agent,
                shell,
                action_notice,
                &NO_ROWS,
            );
            let outcome = match id {
                LayerId::Storage => layers
                    .storage
                    .handle(ComponentEvent::View(event.clone()), &mut ctx),
                LayerId::Time => layers
                    .time
                    .handle(ComponentEvent::View(event.clone()), &mut ctx),
                LayerId::Help => layers
                    .help
                    .handle(ComponentEvent::View(event.clone()), &mut ctx),
                LayerId::Settings => layers
                    .settings
                    .handle(ComponentEvent::View(event.clone()), &mut ctx),
                LayerId::Bookmarks => layers
                    .bookmarks
                    .handle(ComponentEvent::View(event.clone()), &mut ctx),
                LayerId::Fields => layers
                    .fields
                    .handle(ComponentEvent::View(event.clone()), &mut ctx),
                LayerId::Ask => layers
                    .ask
                    .handle(ComponentEvent::View(event.clone()), &mut ctx),
                LayerId::Investigation => layers
                    .investigation
                    .handle(ComponentEvent::View(event.clone()), &mut ctx),
                LayerId::View => layers
                    .view
                    .handle(ComponentEvent::View(event.clone()), &mut ctx),
                LayerId::Source => layers
                    .source
                    .handle(ComponentEvent::View(event.clone()), &mut ctx),
                LayerId::Folding => layers
                    .folding
                    .handle(ComponentEvent::View(event.clone()), &mut ctx),
                LayerId::Recipes | LayerId::RecipeHistory => layers
                    .recipes
                    .handle(ComponentEvent::View(event.clone()), &mut ctx),
                LayerId::Filter => layers
                    .filter
                    .handle(ComponentEvent::View(event.clone()), &mut ctx),
                LayerId::Grouping => layers
                    .grouping
                    .handle(ComponentEvent::View(event.clone()), &mut ctx),
                LayerId::ColorRules => layers
                    .color_rules
                    .handle(ComponentEvent::View(event.clone()), &mut ctx),
                LayerId::Enrichment => layers
                    .enrichment
                    .handle(ComponentEvent::View(event.clone()), &mut ctx),
                LayerId::EnrichmentStep => layers
                    .enrichment_step
                    .handle(ComponentEvent::View(event.clone()), &mut ctx),
                LayerId::ExternalCommand => layers
                    .external_command
                    .handle(ComponentEvent::View(event.clone()), &mut ctx),
                LayerId::Correlation => layers
                    .correlation
                    .handle(ComponentEvent::View(event.clone()), &mut ctx),
                LayerId::ViewSummary => layers
                    .view_summary
                    .handle(ComponentEvent::View(event.clone()), &mut ctx),
            };
            debug_assert!(
                matches!(
                    outcome,
                    Outcome::Ignored | Outcome::Consumed | Outcome::Close
                ),
                "a layer may only consume or close on a view event"
            );
            if outcome == Outcome::Close {
                self.close_layer(id);
            }
        }
    }

    /// Pop a named layer wherever it sits, because a view event reaches layers
    /// that are not on top. `pop_layer` is the top-of-stack case.
    fn close_layer(&mut self, id: LayerId) {
        self.layers.stack.retain(|open| *open != id);
        if self.layers.stack.is_empty() && self.focus == Focus::Layer {
            self.focus = self.layer_return_focus;
        }
    }

    /// §4.3: palette entries contributed by components rather than computed
    /// from a peek at their private state. Every slot contributes, whether or
    /// not it is on the stack, so an entry that used to be listed-but-muted
    /// from the base focus still is.
    pub fn layer_commands(&self) -> Vec<(LayerId, crate::component::CommandEntry)> {
        let mut entries: Vec<(LayerId, crate::component::CommandEntry)> = self
            .layers
            .storage
            .commands(&self.views)
            .into_iter()
            .map(|entry| (LayerId::Storage, entry))
            .collect();
        entries.extend(
            self.layers
                .time
                .commands(&self.views)
                .into_iter()
                .map(|entry| (LayerId::Time, entry)),
        );
        entries.extend(
            self.layers
                .help
                .commands(&self.views)
                .into_iter()
                .map(|entry| (LayerId::Help, entry)),
        );
        entries.extend(
            self.layers
                .fields
                .commands(&self.views)
                .into_iter()
                .map(|entry| (LayerId::Fields, entry)),
        );
        entries.extend(
            self.layers
                .view
                .commands(&self.views)
                .into_iter()
                .map(|entry| (LayerId::View, entry)),
        );
        entries.extend(
            self.layers
                .recipes
                .commands(&self.views)
                .into_iter()
                .map(|entry| (LayerId::Recipes, entry)),
        );
        entries.extend(
            self.layers
                .filter
                .commands(&self.views)
                .into_iter()
                .map(|entry| (LayerId::Filter, entry)),
        );
        entries.extend(
            self.layers
                .color_rules
                .commands(&self.views)
                .into_iter()
                .map(|entry| (LayerId::ColorRules, entry)),
        );
        entries.extend(
            self.layers
                .source
                .commands(&self.views)
                .into_iter()
                .map(|entry| (LayerId::Source, entry)),
        );
        entries.extend(
            self.layers
                .enrichment
                .commands(&self.views)
                .into_iter()
                .map(|entry| (LayerId::Enrichment, entry)),
        );
        entries.extend(
            self.layers
                .enrichment_step
                .commands(&self.views)
                .into_iter()
                .map(|entry| (LayerId::EnrichmentStep, entry)),
        );
        entries.extend(
            self.layers
                .external_command
                .commands(&self.views)
                .into_iter()
                .map(|entry| (LayerId::ExternalCommand, entry)),
        );
        entries.extend(
            self.layers
                .bookmarks
                .commands(&self.views)
                .into_iter()
                .map(|entry| (LayerId::Bookmarks, entry)),
        );
        entries.extend(
            self.layers
                .ask
                .commands(&self.views)
                .into_iter()
                .map(|entry| (LayerId::Ask, entry)),
        );
        entries.extend(
            self.layers
                .investigation
                .commands(&self.views)
                .into_iter()
                .map(|entry| (LayerId::Investigation, entry)),
        );
        entries.extend(
            self.layers
                .correlation
                .commands(&self.views)
                .into_iter()
                .map(|entry| (LayerId::Correlation, entry)),
        );
        entries
    }

    pub fn handle<P: RowProvider>(&mut self, action: Action, provider: &P) {
        self.retire_left_raw_context();
        if !matches!(action, Action::Resize(..)) {
            self.action_notice = None;
            // The gap report describes where the last jump landed, so the next
            // deliberate action retires it exactly as it retires a notice.
            if !matches!(action, Action::JumpToGap(_))
                && let Some(state) = self.view_state_mut()
            {
                state.gap_notice = None;
            }
        }
        // The shared text plumbing is gone with the last legacy text dialog:
        // every field belongs to a layer, which edits it from its own keymap.
        // The `Action::Text*` and `Action::Editor*` variants survive only as
        // no-ops until the last legacy producer of them is retired.
        if matches!(
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
                    Focus::Details if !self.views.items.is_empty() => Focus::Selector,
                    Focus::Logs if !self.views.items.is_empty() => Focus::Selector,
                    Focus::Logs | Focus::Details | Focus::Layer => Focus::Logs,
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
            Action::ToggleBookmark if matches!(self.focus, Focus::Logs | Focus::Selector) => {
                let selected = self.view_state().and_then(|state| state.selected.clone());
                let message = match selected {
                    Some(id) => self.views.toggle_bookmark(id),
                    None => "select a record to bookmark",
                };
                self.action_notice = Some(message.into());
            }
            Action::ToggleBookmark => {}
            Action::JumpToRecord { row, fallback_view } => {
                self.jump_to_record(row, &fallback_view);
            }
            Action::RawContext { anchor, layer } => self.raw_context(anchor, layer, provider),
            Action::ReturnFromRawContext => self.return_from_raw_context(provider),
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
                    state.details_reveal = false;
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
                    state.details_cursor = 0;
                }
            }
            Action::DetailsCursor(delta) => {
                let rows = self.details_rows(provider);
                let at_edge = self.view_state().is_some_and(|state| {
                    (delta < 0 && state.details_cursor == 0)
                        || (delta > 0 && state.details_cursor + 1 >= rows.len())
                });
                // No tree, or the cursor is already at the end it is being
                // pushed past: the rows above the tree and the enrichment
                // rows below it are reached by scrolling.
                if rows.is_empty() || at_edge {
                    self.handle(Action::ScrollDetails(delta), provider);
                } else if let Some(state) = self.view_state_mut() {
                    state.details_cursor = (state.details_cursor as i64 + i64::from(delta))
                        .clamp(0, rows.len() as i64 - 1)
                        as usize;
                    state.details_reveal = true;
                }
            }
            Action::DetailsPath(expand) => {
                let rows = self.details_rows(provider);
                let Some(state) = self.view_state_mut() else {
                    return;
                };
                let cursor = state.details_cursor.min(rows.len().saturating_sub(1));
                let Some(row) = rows.get(cursor) else {
                    return;
                };
                match (&row.shape, expand) {
                    (crate::json_tree::RowShape::Container { expanded, .. }, want) => {
                        let open = want.unwrap_or(!expanded);
                        if open {
                            if state.expanded_paths.len() < MAX_EXPANDED_PATHS
                                || state.expanded_paths.contains(&row.path)
                            {
                                state.expanded_paths.insert(row.path.clone());
                            }
                        } else {
                            state.expanded_paths.remove(&row.path);
                        }
                    }
                    // Left on a leaf climbs to the container it sits in.
                    (crate::json_tree::RowShape::Scalar(_), Some(false)) => {
                        if let Some(parent) = rows[..cursor]
                            .iter()
                            .rposition(|candidate| candidate.depth + 1 == row.depth)
                        {
                            state.details_cursor = parent;
                            state.details_reveal = true;
                        }
                    }
                    _ => {}
                }
            }
            Action::ScrollDialog(delta) => {
                self.dialog_scroll = self
                    .dialog_scroll
                    .saturating_add_signed(delta as isize)
                    .min(self.dialog_scroll_limit);
            }
            Action::ToggleDialogScrollFocus => {
                self.dialog_scroll_focused = !self.dialog_scroll_focused;
            }
            // Every dialog these two used to special-case is now a layer that
            // scrolls itself, so both fall through to the shared scroll.
            Action::ModalVertical(delta) | Action::ScrollHoveredDialog(delta) => {
                self.handle(Action::ScrollDialog(delta), provider)
            }
            Action::ToggleFollow => self.toggle_follow(provider),
            Action::JumpToGap(direction) => self.jump_to_gap(direction, provider),
            Action::ToggleExpandedGroup => {
                let selected = self.view_state().and_then(|state| state.selected.clone());
                let Some(id) = selected else {
                    return;
                };
                // A collapsed run answers Enter first: expanding it restores its
                // constituent events in their original order, individually
                // selectable. Otherwise Enter keeps its multiline-group meaning.
                let folded = self
                    .view_state()
                    .is_some_and(|state| state.fold_enabled)
                    .then(|| {
                        self.active_view_id()
                            .map(|view_id| provider.fold_members(view_id, &id))
                            .unwrap_or_default()
                    })
                    .filter(|members| members.len() > 1)
                    .is_some();
                if let Some(state) = self.view_state_mut() {
                    if folded {
                        if let Some(index) =
                            state.fold_expanded.iter().position(|entry| entry == &id)
                        {
                            state.fold_expanded.remove(index);
                        } else if state.fold_expanded.len() < MAX_FOLD_EXPANDED {
                            state.fold_expanded.push(id);
                        } else {
                            self.action_notice = Some(format!(
                                "at most {MAX_FOLD_EXPANDED} expanded runs per view; collapse one first"
                            ));
                            return;
                        }
                    } else if !state.expanded_groups.remove(&id) {
                        state.expanded_groups.insert(id);
                    }
                    let state = self.view_state_mut().expect("view state");
                    state.user_interaction_revision =
                        state.user_interaction_revision.saturating_add(1);
                }
            }
            Action::ToggleFolding => {
                if let Some(state) = self.view_state_mut() {
                    state.fold_enabled = !state.fold_enabled;
                    if state.fold_minimum_run == 0 {
                        state.fold_minimum_run = DEFAULT_FOLD_MINIMUM_RUN;
                    }
                    if !state.fold_enabled {
                        state.fold_expanded.clear();
                    }
                    let enabled = state.fold_enabled;
                    let run = state.fold_minimum_run;
                    state.user_interaction_revision =
                        state.user_interaction_revision.saturating_add(1);
                    self.action_notice = Some(if enabled {
                        format!("folding repeated events: runs of {run} or more collapse")
                    } else {
                        "folding off; every event is listed individually".into()
                    });
                }
            }
            Action::CollapseAllFolds => {
                if let Some(state) = self.view_state_mut() {
                    if !state.fold_enabled {
                        self.action_notice =
                            Some("folding is off for this view; nothing is collapsed".into());
                        return;
                    }
                    state.fold_expanded.clear();
                    state.user_interaction_revision =
                        state.user_interaction_revision.saturating_add(1);
                    self.action_notice = Some("every repeated run is collapsed again".into());
                }
            }
            Action::Open(open) => self.push_layer(open, provider),
            Action::Command(layer, id) => self.deliver_command(layer, id, provider),
            Action::Raw(event) if self.focus == Focus::Layer => self.handle_event(event, provider),
            Action::Raw(_) => {}
            Action::AdaptRecipe { item, suggestion } => {
                // Moved from `Action::AdaptRecipeSuggestion`; the selection now
                // arrives as plain data because the Recipes layer no longer has
                // a `RecipeDialogState` on `App` to read it back from. Ask is a
                // layer too, so this is a push rather than a dialog assignment.
                let (item, suggestion) = (*item, *suggestion);
                if let Some(view_id) = self.active_view_id().map(str::to_owned) {
                    let mut config = item.config;
                    if let Some(current) = self.persistent_view_state(&view_id) {
                        config.capture_time = current.applied_capture_time;
                        config.capture_time_policy = current.applied_capture_time_policy;
                        config.time_basis = current.applied_time_basis;
                        config.grouping = current.applied_grouping;
                    }
                    let source_id = self
                        .views
                        .items
                        .get(self.views.selected)
                        .map_or("", |view| view.source_id.as_str())
                        .to_owned();
                    let prompt = format!(
                        "Adapt recipe {:?} for this source. source-id={source_id} Evidence: {}. Missing required fields: {}. Preserve unsupported presentation/time/grouping settings.",
                        item.name,
                        suggestion.evidence.join(", "),
                        suggestion.missing_fields.join(", ")
                    );
                    let outcome = RecipeOutcome {
                        source_id,
                        recipe_id: item.id,
                        revision: item.revision,
                        accepted: true,
                    };
                    self.push_layer(
                        Open::Ask(crate::components::ask::AskOpen::Recipe {
                            config: Box::new(config),
                            outcome,
                            prompt,
                        }),
                        provider,
                    );
                }
            }
            // §6.4: the proposal's destinations are all still legacy, so the
            // shell writes them. The layer is still on the stack, which is what
            // lets a refusal be shown where the user is looking.
            Action::ApplyAskProposal {
                kind,
                expression,
                recipe,
                outcome,
            } => {
                if kind == AskAiKind::Recipe {
                    let mut config = recipe.map(|config| *config).unwrap_or_default();
                    config.advanced = expression;
                    if self.apply_recipe_to_active_view(config) {
                        if let Some(state) = self.view_state_mut()
                            && let Some(pending) = &mut state.pending_recipe
                        {
                            pending.suggestion = outcome;
                        }
                        self.layers.ask.finish_apply();
                        self.pop_layer();
                    } else {
                        self.layers
                            .ask
                            .fail_apply("query queue is full; working view was preserved");
                    }
                } else {
                    let Some(view_id) = self.active_view_id().map(str::to_owned) else {
                        return;
                    };
                    // The proposal is written into the view's draft, which is
                    // where both destinations read it from.
                    let purpose = match kind {
                        AskAiKind::Filter => QueryPurpose::Advanced,
                        AskAiKind::Enrichment => QueryPurpose::Enrichment,
                        AskAiKind::Recipe => unreachable!(),
                    };
                    if let Some(editor) = self.views.editor_mut(&view_id, purpose) {
                        editor.draft = expression;
                        editor.error = None;
                    }
                    self.views.touch(&view_id);
                    self.layers.ask.finish_apply();
                    self.pop_layer();
                    match kind {
                        // A filter proposal opens the Advanced layer on the
                        // draft it just wrote.
                        AskAiKind::Filter => self.push_layer(Open::Advanced, provider),
                        // A proposed enrichment lands in the step editor so its
                        // input and output stay inspectable.
                        // A proposed enrichment lands in the step editor so
                        // its input and output stay inspectable, with the step
                        // list underneath it as its parent.
                        AskAiKind::Enrichment => {
                            self.push_layer(Open::Enrichment, provider);
                            self.push_layer(
                                Open::EnrichmentStep {
                                    editing: None,
                                    prefill: None,
                                },
                                provider,
                            );
                        }
                        AskAiKind::Recipe => unreachable!(),
                    }
                    self.enqueue_query(&view_id, purpose);
                }
            }
            Action::StopCapture | Action::RestartCapture => {
                if matches!(self.focus, Focus::Logs | Focus::Selector)
                    && let Some(view) = self.views.items.get(self.views.selected)
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
            Action::CancelEditor => {
                // Dismissing an editor does *not* abandon a candidate: on the
                // canonical view a fork only exists once the user has applied
                // something. `enqueue_live_query` refuses to fork mid-word, so
                // there is no draft candidate to clean up here — only a view
                // the user asked for, still settling its query and its save.
                // Discarding it on Escape made an applied filter vanish for
                // anyone who closed the editor before the fork landed. A fork
                // that fails is still discarded with its reason, and a later
                // edit still supersedes it.
                self.focus = Focus::Logs;
            }
            Action::OpenSource => self.handle(Action::Open(Open::Source), provider),
            Action::Resize(width, height) => self.shell.size = (width, height),
            Action::Mouse(event) => self.handle_mouse(event, provider),
            // Reachable only while their dialog holds focus; the guarded arms
            // above handle them there.
            Action::FixtureAdvance | Action::None => {}
            Action::EditorInput(_) | Action::EditorBackspace | Action::EditorPaste(_) => {}
            Action::TextStartOfLine
            | Action::TextEndOfLine
            | Action::TextKillToEndOfLine
            | Action::TextMoveLeft
            | Action::TextMoveRight
            | Action::TextMoveUp
            | Action::TextMoveDown => {}
        }
    }

    /// A lookup whose origin is `view_id` is abandoned, and the layer that
    /// was waiting on it closes: the record it froze belongs to a view that
    /// is going away or is no longer the active one.
    fn cancel_correlation_for_view(&mut self, view_id: &str) {
        if self.layers.correlation.cancel_for_view(view_id) {
            self.close_layer(LayerId::Correlation);
        }
    }

    /// The lookup on screen, whichever view it came from.
    fn cancel_active_correlation(&mut self) {
        if self.layers.correlation.lookup_pending() {
            let origin = self.layers.correlation.origin_view_id().to_owned();
            self.cancel_correlation_for_view(&origin);
        }
    }

    fn enqueue_query(&mut self, view_id: &str, purpose: QueryPurpose) -> Option<u64> {
        self.views.enqueue(view_id, purpose, None).ok()
    }

    /// The live, debounced application of a draft.
    ///
    /// A canonical view does not accept one: while its editor is open the draft
    /// is only a draft, and All events keeps showing every record until the
    /// user applies it. Forking mid-word would hand them a view they did not
    /// ask for and accumulate one per pause in typing.
    fn enqueue_live_query(&mut self, view_id: &str, purpose: QueryPurpose) -> Option<u64> {
        if self.view_definition_is_fixed(view_id) {
            // Accepted, in the sense that the debounce is satisfied and must
            // not retry; nothing is applied and nothing is created.
            return self
                .views
                .states
                .get(view_id)
                .map(|state| state.desired_query_revision);
        }
        self.views.enqueue(view_id, purpose, None).ok()
    }

    fn editor_mut(&mut self, view_id: &str, purpose: QueryPurpose) -> &mut EditorState {
        let state = self.views.states.get_mut(view_id).expect("view state");
        match purpose {
            QueryPurpose::Search => &mut state.search,
            QueryPurpose::Advanced => &mut state.advanced,
            QueryPurpose::Enrichment => &mut state.enrichment,
            QueryPurpose::Grouping => &mut state.grouping,
        }
    }

    fn switch_view<P: RowProvider>(&mut self, delta: i32, provider: &P) {
        if self.views.items.is_empty() {
            return;
        }
        self.cancel_active_correlation();
        self.views.selected =
            (self.views.selected as i32 + delta).rem_euclid(self.views.items.len() as i32) as usize;
        // Cycling is how a view is usually chosen, so it is what a restart has
        // to remember; only recording explicit selection would reopen whichever
        // view happened to be created last instead.
        if let Some(view_id) = self.active_view_id().map(str::to_owned) {
            self.record_view_selection(&view_id);
        }
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
        let total = self.views.states[&view_id].last_total;
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
        let state = self.views.states.get_mut(&view_id).expect("view state");
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
        let follow = !self.views.states[&id].follow;
        let state = self.views.states.get_mut(&id).expect("view state");
        state.follow = follow;
        state.user_interaction_revision = state.user_interaction_revision.saturating_add(1);
        if follow {
            self.handle(Action::End, provider);
        }
    }

    fn handle_mouse<P: RowProvider>(&mut self, event: MouseEvent, provider: &P) {
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
        let point = (event.column, event.row);
        if matches!(event.kind, MouseEventKind::Down(MouseButton::Left)) {
            if let Some((_, index)) = self
                .hit_regions
                .sidebar_views
                .iter()
                .find(|(area, _)| contains(*area, point))
            {
                self.views.selected = *index;
                if let Some(view_id) = self.active_view_id().map(str::to_owned) {
                    self.record_view_selection(&view_id);
                }
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

/// The one place a `Ctx` is built. It takes the shell's state and the legacy
/// `App` fields it has not absorbed yet as separate arguments, which is what
/// makes the caller's destructure of `App` provably disjoint from `layers`
/// (§2.5). The spec sketches this as `shell.ctx(provider)`; during migration it
/// cannot be a method, because two of its members are still legacy fields.
#[allow(clippy::too_many_arguments)]
fn shell_ctx<'a, P: RowProvider>(
    views: &'a mut Views,
    sources: &'a [SourceItem],
    appearance: &'a mut Appearance,
    agent: &'a AgentDefaults,
    shell: &'a mut Shell,
    notices: &'a mut Option<String>,
    provider: &'a P,
) -> Ctx<'a> {
    let clock = shell.clock();
    let size = shell.size;
    Ctx::new(
        views,
        sources,
        appearance,
        agent,
        provider,
        &mut shell.cursors,
        notices,
        clock,
        size,
    )
}

/// The shell's half of §5.2: dismissal keys become `Event::Dismiss`, the
/// §8.10 accelerator becomes a button press, and a mouse event outside the
/// layer's popup is dropped so a modal cannot leak a click to the log behind
/// it. Routing only; the component decides meaning.
///
/// The mnemonic is resolved here, once, rather than in thirteen keymaps. That
/// is why `x`, `f` and `d` did nothing in Fields while `c` worked: the row was
/// drawn with underlines from `&`-marked labels, but the keys behind them were
/// hand-written per dialog, some Alt-only and some bare, and the shell drops an
/// `Ignored` key from a layer rather than passing it down to the base screen
/// (§7.5, and `key_to_action`'s `Focus::Layer` arm), so the letter reached
/// nothing at all.
fn dispatch_raw<C: Component>(component: &mut C, event: RawEvent, ctx: &mut Ctx<'_>) -> Outcome {
    let surface: Surface = component.surface();
    let event = match event {
        RawEvent::Key(key) => {
            if is_dismissal(key, !component.text_focus()) {
                ComponentEvent::Dismiss
            } else if let Some(index) = crate::dialog_controls::mnemonic_press(
                &component.action_labels(ctx),
                &key,
                component.text_focus(),
            ) {
                return component.press_action(index, ctx);
            } else {
                ComponentEvent::Key(key)
            }
        }
        RawEvent::Paste(text) => ComponentEvent::Paste(text),
        RawEvent::Resize => ComponentEvent::Resize,
        RawEvent::Mouse(mouse) => {
            let point = (mouse.column, mouse.row);
            if !contains(surface.popup, point) {
                return Outcome::Ignored;
            }
            let hit = component.hit(point);
            ComponentEvent::Mouse {
                kind: mouse.kind,
                point,
                hit,
            }
        }
    };
    component.handle(event, ctx)
}

/// Esc always, and bare `q` only when no text field has focus (§1).
fn is_dismissal(key: KeyEvent, no_text_focus: bool) -> bool {
    matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat)
        && (key.code == KeyCode::Esc
            || (key.code == KeyCode::Char('q') && key.modifiers.is_empty() && no_text_focus))
}

fn contains(area: Rect, point: (u16, u16)) -> bool {
    point.0 >= area.x && point.0 < area.right() && point.1 >= area.y && point.1 < area.bottom()
}

fn nonempty(value: &str) -> Option<String> {
    (!value.is_empty()).then(|| value.to_owned())
}

/// §8.13: the expression that reads a nested leaf. Nested values are JSON
/// text inside their top-level column on the query side, so the leaf is
/// addressed by JSON path; a key the path syntax cannot spell is extracted
/// lexically instead: the pair's own spelling, the value captured as
/// group 1 (a quoted string, or a bare number/literal).
pub(crate) fn nested_path_expression(path: &str) -> String {
    let column = crate::json_tree::top_level_key(path);
    if let Some(json_path) = crate::json_tree::json_path(path) {
        return format!(
            "pl.col({}).str.json_path_match({})",
            python_string_literal(column),
            python_string_literal(&json_path)
        );
    }
    let leaf = path
        .rsplit(['.', '[', ']'])
        .find(|part| !part.is_empty())
        .unwrap_or(path)
        .trim_start_matches('[');
    let mut escaped = String::new();
    for ch in leaf.chars() {
        if r"\.+*?()|[]{}^$".contains(ch) {
            escaped.push('\\');
        }
        escaped.push(ch);
    }
    let pattern = format!(r#""{escaped}"\s*:\s*("(?:[^"\\]|\\.)*"|[^,}}\]]+)"#);
    format!(
        "pl.col({}).str.extract({}, 1)",
        python_string_literal(column),
        python_string_literal(&pattern)
    )
}

pub(crate) fn python_string_literal(value: &str) -> String {
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

pub(crate) fn parse_capture_range(start: &str, end: &str) -> Result<CaptureTimeRange, String> {
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

pub fn time_zone_choices() -> &'static [(&'static str, &'static str)] {
    &TIME_ZONE_CHOICES
}

/// The offset token used when nothing is configured. UTC: unambiguous, and
/// what every log this tool has ever displayed already showed.
pub const DEFAULT_DISPLAY_ZONE: &str = "Z";

/// Minutes east of UTC for an offset token, or `None` when the token is not one
/// this build knows. An unknown token reads as UTC rather than as an error: a
/// settings file written by a newer version must not stop the log from being
/// legible.
pub fn time_zone_offset_minutes(token: &str) -> Option<i64> {
    if token == "Z" || token.eq_ignore_ascii_case("utc") {
        return Some(0);
    }
    let (sign, rest) = match token.as_bytes().first()? {
        b'+' => (1, &token[1..]),
        b'-' => (-1, &token[1..]),
        _ => return None,
    };
    let (hours, minutes) = rest.split_once(':')?;
    let hours: i64 = hours.parse().ok()?;
    let minutes: i64 = minutes.parse().ok()?;
    (hours <= 23 && minutes <= 59).then_some(sign * (hours * 60 + minutes))
}

/// The label the settings dropdown and the read-only rows show for a token.
pub fn time_zone_label(token: &str) -> String {
    time_zone_choices()
        .iter()
        .find(|(_, value)| *value == token)
        .map_or_else(|| token.to_owned(), |(label, _)| (*label).to_owned())
}

/// One row's clock, in the display zone.
///
/// The offset is fixed: this build carries no timezone database, so a named
/// zone and its daylight-saving transitions cannot be honoured. The suffix is
/// always shown for that reason — `14:30:00.000+02:00` says exactly what it
/// means, where a bare `14:30` would not.
pub fn format_display_time(unix_nanos: i64, zone: &str) -> String {
    let offset = time_zone_offset_minutes(zone).unwrap_or(0);
    let shifted = unix_nanos.saturating_add(offset.saturating_mul(60_000_000_000));
    let seconds = shifted.div_euclid(1_000_000_000);
    let millis = shifted.rem_euclid(1_000_000_000) / 1_000_000;
    let day_seconds = seconds.rem_euclid(86_400);
    let clock = format!(
        "{:02}:{:02}:{:02}.{millis:03}",
        day_seconds / 3_600,
        day_seconds % 3_600 / 60,
        day_seconds % 60
    );
    if offset == 0 {
        format!("{clock}Z")
    } else {
        let sign = if offset < 0 { '-' } else { '+' };
        let magnitude = offset.abs();
        format!("{clock}{sign}{:02}:{:02}", magnitude / 60, magnitude % 60)
    }
}

/// The width the log viewport's time column needs in a zone. UTC's `Z` is one
/// character; an offset is six.
pub fn display_time_width(zone: &str) -> u16 {
    if time_zone_offset_minutes(zone).unwrap_or(0) == 0 {
        13
    } else {
        18
    }
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

/// A `YYYY-MM-DD` date in the reader's display zone, for §12.9's date column.
///
/// The column is ten cells wide, so a date and not a timestamp: the moment a
/// revision was saved is a day, and the revision id is what names it exactly.
/// The zone matters even so — near midnight a fixed offset moves the date by a
/// day, and a date column that disagreed with the log's clock would be worse
/// than no date at all.
pub fn format_display_date(unix_nanos: i64, zone: &str) -> String {
    let shifted =
        unix_nanos.saturating_add(time_zone_offset_minutes(zone).unwrap_or(0) * 60_000_000_000);
    // `format_utc_nanos` is the calendar arithmetic; the date is its first ten
    // characters, and taking them here keeps one implementation of the calendar.
    format_utc_nanos(shifted).chars().take(10).collect()
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

/// `TimeBasis::Selected` is meaningless without the token that names the field,
/// and recipes do not carry one.
fn recipe_time_basis(basis: TimeBasis) -> TimeBasis {
    match basis {
        TimeBasis::Selected => TimeBasis::Capture,
        other => other,
    }
}

/// A readable name for a persisted token, without decoding its full grammar:
/// the field reference is the token's first `|`-separated part.
pub fn time_field_token_label(token: &str) -> String {
    let field = token.split('|').next().unwrap_or(token);
    match field.split_once(':') {
        Some(("structured", path)) => path.to_owned(),
        Some(("column", name)) => format!("column: {name}"),
        _ if field == "raw" => "raw prefix".to_owned(),
        _ => field.to_owned(),
    }
}

/// The state a fork candidate starts from.
///
/// Everything about how the origin is presented travels: pinned columns,
/// colours, folding, grouping, follow. Its *definition* does not need clearing,
/// because a canonical view has none — that is what makes it canonical — so
/// this only resets the query bookkeeping. The candidate is registered with the
/// runtime as a new raw view, and its first query has to be based on nothing
/// applied yet or the runtime rejects it.
///
/// Copying the whole state rather than a list of presentation fields is
/// deliberate: a presentation field added later travels without anyone
/// remembering to add it here.
fn fork_candidate_state(base: &ViewState, source_ids: Vec<String>) -> ViewState {
    let mut candidate = base.clone();
    candidate.source_ids = source_ids;
    // Grouping is display-only but is still applied by a query, so it moves
    // back to a draft for the candidate's own first query to apply.
    candidate.grouping.draft = base.grouping.applied.clone();
    candidate.grouping.applied.clear();
    candidate.applied_query_revision = 0;
    candidate.desired_query_revision = 0;
    candidate.provider_revision = 0;
    candidate.last_total = 0;
    candidate.top = 0;
    candidate.pending_source_change = None;
    candidate.pending_time = None;
    candidate.pending_recipe = None;
    candidate.pending_enrichment_mutation = None;
    candidate.rolling_refresh_due = false;
    for editor in [
        &mut candidate.search,
        &mut candidate.advanced,
        &mut candidate.enrichment,
        &mut candidate.grouping,
    ] {
        editor.pending_generation = None;
        editor.pending_revision = None;
        editor.pending_value = None;
        editor.error = None;
        editor.search_due = None;
        // The candidate is the fork; it is not itself waiting on one.
        editor.fork_pending = false;
    }
    candidate.desired_constraints = applied_constraints(&candidate);
    candidate.desired_constraints.grouping = nonempty(&candidate.grouping.draft);
    candidate.desired_capture_time_policy = candidate.applied_capture_time_policy;
    candidate.desired_time_basis = candidate.applied_time_basis;
    candidate
}

fn applied_constraints(state: &ViewState) -> QueryConstraints {
    QueryConstraints {
        text: nonempty_text(&state.search.applied),
        exact_field: state.exact_field.clone(),
        advanced_polars: nonempty(&state.advanced.applied),
        enrichments: state.enrichments.clone(),
        enrichment: None,
        time_field: state.applied_time_field.clone(),
        capture_time: state.applied_capture_time,
        time_basis: state.applied_time_basis,
        grouping: nonempty(&state.grouping.applied),
        color_rules: state.color_rules.clone(),
    }
}

fn legacy_enrichment(source: &str) -> Vec<EnrichmentDefinition> {
    nonempty(source).map_or_else(Vec::new, |source| {
        vec![EnrichmentDefinition {
            id: EnrichmentStageId("legacy-stage-1".into()),
            source,
            command: None,
        }]
    })
}

fn valid_enrichments(stages: &[EnrichmentDefinition]) -> bool {
    if stages.len() > 32 {
        return false;
    }
    let mut ids = HashSet::with_capacity(stages.len());
    let mut names = HashSet::new();
    stages.iter().all(|stage| {
        !stage.id.0.is_empty()
            && stage.id.0.len() <= 128
            && !stage.source.is_empty()
            && stage.source.len() <= MAX_EDITOR_BYTES
            && ids.insert(stage.id.0.as_str())
            && stage
                .output_prefix()
                .is_none_or(|name| valid_command_step_name(name) && names.insert(name.to_owned()))
    })
}

/// Whether `executable` can be started here: an absolute or directory-qualified
/// path that exists and is executable, or a bare name found on `PATH`. A
/// recipe that needs a program this machine lacks says so when it is applied
/// (§12.5) rather than failing at the first run.
pub fn program_available(executable: &std::path::Path) -> bool {
    fn runnable(path: &std::path::Path) -> bool {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::metadata(path)
                .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        }
        #[cfg(not(unix))]
        {
            std::fs::metadata(path).is_ok_and(|meta| meta.is_file())
        }
    }
    if executable.as_os_str().is_empty() {
        return false;
    }
    if executable.components().count() > 1 || executable.is_absolute() {
        return runnable(executable);
    }
    std::env::var_os("PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|dir| runnable(&dir.join(executable))))
}

/// The command steps of `chain` whose program is not available here, as
/// `(step name, program)`.
pub fn missing_programs(chain: &[EnrichmentDefinition]) -> Vec<(String, String)> {
    chain
        .iter()
        .filter_map(|stage| {
            let command = stage.command.as_ref()?;
            match &command.program {
                lvu_core::CommandProgram::Exec { executable, .. }
                    if !program_available(executable) =>
                {
                    Some((stage.source.clone(), executable.display().to_string()))
                }
                lvu_core::CommandProgram::Exec { .. } => None,
                lvu_core::CommandProgram::Shell { text } => {
                    Some((stage.source.clone(), text.clone()))
                }
            }
        })
        .collect()
}

pub(crate) fn mark_time_edit(state: &mut ViewState) {
    state.user_interaction_revision = state.user_interaction_revision.saturating_add(1);
    state.ai_definition_revision = state.ai_definition_revision.saturating_add(1);
}

pub(crate) fn resolve_capture_time_policy(
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

/// The rows a completion popup offers, and the sentence that qualifies them.
///
/// Shared by the converted Advanced layer and the still-legacy enrichment step
/// editor, so the two cannot drift about what a field or a sampled literal is.
/// Completion samples row values; folding is presentation and must not change
/// what it offers, which is why this reads `unfolded_page` (AGENTS.md).
pub(crate) fn sample_editor_completion(
    provider: &dyn RowProvider,
    view_id: &str,
    top: usize,
    viewport_height: usize,
    kind: EditorCompletionKind,
) -> (Vec<EditorCompletionItem>, String) {
    let page = provider.unfolded_page(
        view_id,
        ViewportRequest {
            start: top,
            len: viewport_height.clamp(1, MAX_COMPLETION_ROWS),
        },
    );
    let mut fields = std::collections::BTreeSet::new();
    // `raw` is the authoritative original record column and exists even when a
    // source has no recognized JSON/logfmt fields.
    fields.insert("raw".to_owned());
    // §8.13: nested paths the sampled JSON records carry, so a path is picked
    // rather than typed. Each is a leaf under its top-level column.
    let mut paths = std::collections::BTreeSet::new();
    let mut values = std::collections::BTreeSet::new();
    for row in page.rows {
        if let Some(tree) = crate::json_tree::JsonTree::parse(&row.text)
            && tree.is_object()
        {
            for tree_row in tree.rows(&|_| true) {
                if tree_row.depth == 0 || tree_row.depth > MAX_COMPLETION_PATH_DEPTH {
                    continue;
                }
                if matches!(tree_row.shape, crate::json_tree::RowShape::Scalar(_))
                    && tree_row.path.len() <= MAX_COMPLETION_TEXT_BYTES
                    && paths.len() < MAX_COMPLETION_FIELDS
                {
                    paths.insert(tree_row.path);
                }
            }
        }
        for (field, value) in row.fields.into_iter().take(MAX_COMPLETION_FIELDS) {
            if field.len() <= MAX_COMPLETION_TEXT_BYTES && fields.len() < MAX_COMPLETION_FIELDS {
                fields.insert(field.clone());
            }
            if value.len() <= MAX_COMPLETION_TEXT_BYTES && values.len() < MAX_COMPLETION_VALUES {
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
            .chain(paths.into_iter().map(|path| EditorCompletionItem {
                label: if crate::json_tree::json_path(&path).is_some() {
                    format!("  {path}  (nested · JSON path)")
                } else {
                    format!("  {path}  (nested · extracted lexically)")
                },
                insertion: nested_path_expression(&path),
            }))
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
    let status = if items.is_empty() {
        "no fields or values in the sampled visible rows".into()
    } else {
        match kind {
            EditorCompletionKind::Field => {
                "Fields insert pl.col(...); a nested path reads its leaf from the top-level column's JSON text".into()
            }
            EditorCompletionKind::SampledValue => {
                "Static quoted lexical literals from sampled rows; they do not vary per row".into()
            }
        }
    };
    (items, status)
}

fn editor_of(state: &ViewState, purpose: QueryPurpose) -> &EditorState {
    match purpose {
        QueryPurpose::Search => &state.search,
        QueryPurpose::Advanced => &state.advanced,
        QueryPurpose::Enrichment => &state.enrichment,
        QueryPurpose::Grouping => &state.grouping,
    }
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

pub(crate) fn move_index(current: usize, length: usize, delta: i32) -> usize {
    if length == 0 {
        return 0;
    }
    (current as i32 + delta).rem_euclid(length as i32) as usize
}

pub(crate) fn move_control<T: Copy + Eq>(current: T, controls: &[T], delta: i32) -> T {
    if controls.is_empty() {
        return current;
    }
    let index = controls
        .iter()
        .position(|control| *control == current)
        .unwrap_or(0);
    controls[move_index(index, controls.len(), delta)]
}

pub(crate) fn bookmark_controls(editing: bool, has_bookmarks: bool) -> Vec<BookmarkDialogControl> {
    if editing {
        vec![BookmarkDialogControl::Input, BookmarkDialogControl::Save]
    } else if has_bookmarks {
        vec![
            BookmarkDialogControl::List,
            BookmarkDialogControl::Context,
            BookmarkDialogControl::Edit,
            BookmarkDialogControl::Delete,
        ]
    } else {
        vec![BookmarkDialogControl::List]
    }
}

pub(crate) fn push_bounded_message(messages: &mut VecDeque<String>, message: String) {
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

pub(crate) fn bounded_message(mut message: String) -> String {
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

/// §8.8 focus order: the segments, the question, then the actions in the order
/// §12.18 draws them.
pub(crate) fn command_draft_field_mut(dialog: &mut CommandEnrichmentDialogState) -> &mut String {
    match dialog.selected_field {
        CommandEnrichmentField::Name => &mut dialog.name,
        CommandEnrichmentField::Program => &mut dialog.program,
        CommandEnrichmentField::Arguments => &mut dialog.arguments,
        CommandEnrichmentField::Cwd => &mut dialog.cwd,
        CommandEnrichmentField::Environment => &mut dialog.environment,
    }
}

pub(crate) fn command_stage_draft(
    stage: &CommandEnrichmentStage,
) -> (String, String, String, String) {
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

pub(crate) fn command_candidate(
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
        id: CommandEnrichmentStageId(dialog.stage_id.clone()),
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

pub fn key_to_action(key: KeyEvent, focus: Focus) -> Action {
    if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
        return Action::None;
    }
    // A converted layer owns its keymap: no key maps to an `Action` while one
    // is on top, so the base table cannot leak into it (§3).
    if focus == Focus::Layer {
        return Action::None;
    }
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        return Action::Quit;
    }
    // §8.10: the base screen takes no text, so every letter there is a key and
    // no base operation is bound to Alt alone. These two were, and on an xterm
    // with its default `metaSendsEscape: false` Alt-s and Alt-r arrive as the
    // letters `ó` and `ò` — never as a chord — which left stopping and
    // restarting a source reachable only through the palette.
    //
    // `X` and `R` rather than `S` and `R`: `S` already opens Storage, and the
    // sidebar and the log share one namespace on purpose (see `Focus::Selector`
    // below), so making `S` mean something else in the sidebar would cost every
    // palette row a focus-dependent chord to save one letter. Alt-s and Alt-r
    // keep working as the unlisted aliases §8.10 allows.
    if matches!(focus, Focus::Logs | Focus::Selector) {
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        match key.code {
            KeyCode::Char('X') => return Action::StopCapture,
            KeyCode::Char('R') => return Action::RestartCapture,
            KeyCode::Char('s') if alt => return Action::StopCapture,
            KeyCode::Char('r') if alt => return Action::RestartCapture,
            _ => {}
        }
    }
    if focus == Focus::Selector {
        // §8.10: the sidebar binds only what is its own — moving between
        // views — and every other base key means what it means in the log,
        // so a chord the palette shows works whichever pane has focus.
        match key.code {
            KeyCode::Down | KeyCode::Char('j') => return Action::SelectSidebar(1),
            KeyCode::Up | KeyCode::Char('k') => return Action::SelectSidebar(-1),
            KeyCode::Left | KeyCode::Right | KeyCode::Enter => return Action::None,
            _ => {}
        }
    }
    if focus == Focus::Details {
        return match key.code {
            KeyCode::Up => Action::DetailsCursor(-1),
            KeyCode::Down => Action::DetailsCursor(1),
            KeyCode::Enter => Action::DetailsPath(None),
            KeyCode::Right => Action::DetailsPath(Some(true)),
            KeyCode::Left => Action::DetailsPath(Some(false)),
            KeyCode::Tab => Action::CycleFocus,
            KeyCode::Char('d') => Action::ToggleDetails,
            KeyCode::Char('?') => Action::Open(crate::component::Open::Help),
            // The shell's dismissal rule makes `q` hide the pane, not quit;
            // the table says so too, so the palette never shows `q` for Quit
            // here (§8.10: the chord shown is the chord that works).
            KeyCode::Char('q') => Action::ToggleDetails,
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
        KeyCode::Char('o') => Action::RawContext {
            anchor: None,
            layer: None,
        },
        KeyCode::Char('b') => Action::ToggleBookmark,
        KeyCode::Char('B') => Action::Open(Open::Bookmarks),
        KeyCode::Char('v') => Action::Open(crate::component::Open::View),
        // `V` beside `v`: the view's summary beside the view's actions.
        KeyCode::Char('V') => Action::Open(crate::component::Open::ViewSummary),
        KeyCode::Char('?') => Action::Open(crate::component::Open::Help),
        KeyCode::Char('f') => Action::ToggleFollow,
        // `/` is the one key for filtering: the dialog opens on its Search tab
        // and Alt-A reaches Advanced inside it (§12.1). `p` is retired.
        KeyCode::Char('/') => Action::Open(crate::component::Open::Search),
        KeyCode::Char('e') => Action::Open(crate::component::Open::Enrichment),
        KeyCode::Char('m') => Action::Open(crate::component::Open::Grouping),
        KeyCode::Char('S') => Action::Open(crate::component::Open::Storage),
        KeyCode::Char(',') => Action::Open(crate::component::Open::Settings),
        KeyCode::Enter => Action::ToggleExpandedGroup,
        KeyCode::Char('A') => Action::Open(Open::Ask(crate::components::ask::AskOpen::Generic)),
        KeyCode::Char('I') => Action::Open(crate::component::Open::Investigation),
        KeyCode::Char('n') => Action::Open(crate::component::Open::Source),
        KeyCode::Char('r') => Action::Open(crate::component::Open::Recipes {
            mode: RecipeDialogMode::Browse,
        }),
        KeyCode::Char('t') => Action::Open(crate::component::Open::Time),
        // `{`/`}` are the free, idiomatic "previous/next section" pair: no
        // other binding in the log pane uses them, and they read as navigation
        // rather than as a mode.
        KeyCode::Char('}') => Action::JumpToGap(GapDirection::Forward),
        KeyCode::Char('{') => Action::JumpToGap(GapDirection::Backward),
        KeyCode::Char('i') => Action::Open(Open::Fields),
        // `z` is vim's fold prefix and is unbound here; the palette-only
        // `Fold repeated events` toggle keeps working unchanged.
        KeyCode::Char('z') => Action::Open(Open::Folding),
        KeyCode::Char('c') => Action::Open(crate::component::Open::ColorRules),
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
}

#[cfg(test)]
mod command_activity_tests {
    use super::*;

    #[test]
    fn command_activity_includes_commit_after_dialog_closes_but_not_review() {
        let (_, sources, views) = crate::fixture::FixtureProvider::demo();
        let mut app = App::new(sources, views, true);
        let (provider, _, _) = crate::fixture::FixtureProvider::demo();
        app.handle(
            Action::Open(Open::ExternalCommand {
                stage: None,
                insert_at: usize::MAX,
            }),
            &provider,
        );
        for state in [
            CommandEnrichmentRunState::Saving,
            CommandEnrichmentRunState::Preparing,
            CommandEnrichmentRunState::Running,
            CommandEnrichmentRunState::SavingResults,
        ] {
            app.layers.external_command.set_run_state(state);
            assert!(app.command_work_pending());
        }
        app.layers
            .external_command
            .set_run_state(CommandEnrichmentRunState::Ready);
        assert!(!app.command_work_pending());
        app.layers.external_command.note_pending_run(1, "view", 1);
        app.layers.external_command.forget();
        assert!(app.command_work_pending());
        app.layers.external_command.clear_pending_runs();
        assert!(!app.command_work_pending());
    }
}
