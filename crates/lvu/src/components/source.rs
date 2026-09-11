//! The Source layer (`docs/dialog-system.md` §12.2), converted to the component
//! contract as step 11 of `docs/component-model.md` §6.3.
//!
//! Three surfaces in one dialog — a manual file/command path, a bounded
//! discovery scan, and a Source 🧠 proposal reviewed before anything starts —
//! and four kinds of background work that §6.3 folds into one `SourceRequest`
//! enum over one `Outbox`. §8 leaves it open whether `lvu-app` merges its four
//! loops or keeps four drains of that one queue; it keeps four, so the queue
//! offers one typed drain per kind and the request loops are unchanged.
//!
//! The path-completion debounce travels with the requests it gates: a scan is
//! only handed over once `SOURCE_PATH_COMPLETION_DEBOUNCE` has passed, which is
//! what keeps automatic completion from asking on every keystroke.

use std::time::Instant;

use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    widgets::{Paragraph, Wrap},
};
use unicode_width::UnicodeWidthStr;

use crate::app::{
    DiscoveryItem, DiscoveryUiRequest, MAX_AI_PROMPT_BYTES, MAX_EDITOR_BYTES,
    PathCompletionRequest, SourceAiPreview, SourceAiRequest, SourceAiStage, SourceKind,
    SourceLaunchRequest,
};
use crate::command_palette::CommandId;
use crate::component::{
    CommandEntry, CommandSpec, Component, Ctx, Event, Outbox, Outcome, RenderCtx, Surface,
    is_typed_char,
};
use crate::dialog_controls::{ActionRow, DialogStyles, render_role_button, stable_action_rows};
use crate::dialog_layout::{DialogSpec, PresentationKind, plan_list, policy_size, resolve_dialog};
use crate::text_edit::{EditCommand, EditPolicy, TextCursor, edit};
use crate::theme::Theme;
use crate::ui::{
    FIELD_GUTTER, MessageState, render_form_field, render_help_text, render_message,
    render_radio_row, render_responsive_frame, render_scrollbar, render_segmented_control,
    truncated, wrap_sentence,
};

/// Typing pauses this long before a path scan is asked for. Moved with the
/// requests it gates.
const SOURCE_PATH_COMPLETION_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(35);

/// The queue depths the dialog refused at, unchanged.
const MAX_SOURCE_REQUESTS: usize = 8;
const MAX_DISCOVERY_REQUESTS: usize = 4;
const MAX_SOURCE_AI_REQUESTS: usize = 8;

/// One queue for four kinds of work; the cap only has to stop an unbounded
/// queue if `lvu-app` stops draining (AGENTS.md). Each kind's own refusal
/// threshold is the constant above it.
const SOURCE_OUTBOX_CAP: usize = 64;

/// §6.3: the four request types fold into one enum over one outbox. They are
/// still four independent jobs — a path scan is debounced, discovery is
/// cancellable, the agent runs a session — so `lvu-app` drains them by kind.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SourceRequest {
    Launch(SourceLaunchRequest),
    Discovery(DiscoveryUiRequest),
    PathCompletion(PathCompletionRequest),
    Ai(SourceAiRequest),
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
    Refresh,
}

impl SourceControl {
    pub(crate) fn visible(mode: SourceDialogMode, kind: SourceKind) -> &'static [Self] {
        match mode {
            SourceDialogMode::Manual if kind == SourceKind::File => &[
                Self::Input,
                Self::Manual,
                Self::Discovery,
                Self::Agent,
                Self::File,
                Self::Command,
            ],
            SourceDialogMode::Manual => &[
                Self::Input,
                Self::Manual,
                Self::Discovery,
                Self::Agent,
                Self::File,
                Self::Command,
            ],
            SourceDialogMode::Discovery => &[
                Self::Input,
                Self::Manual,
                Self::Discovery,
                Self::Agent,
                Self::Refresh,
            ],
            SourceDialogMode::Ai => &[Self::Input, Self::Manual, Self::Discovery, Self::Agent],
        }
    }
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

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PathCompletionState {
    pub generation: u64,
    pub scanning: bool,
    pub candidates: Vec<String>,
    pub selected: usize,
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
    pub preview_scroll_limit: usize,
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
            preview_scroll_limit: 0,
        }
    }
}

/// The dialog's own data, moved off `App` unchanged.
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

/// Everything Source draws that can be clicked (§5.1).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceHit {
    Control(SourceControl),
    PathCompletion(usize),
    Discovery(usize),
    /// The scrollable status or proposal pane.
    Scroll,
}

/// Recorded by `render`, consumed by `hit()`. Every rect here was painted this
/// frame.
#[derive(Clone, Debug, Default)]
struct SourceGeometry {
    controls: Vec<(Rect, SourceControl)>,
    path_completion_rows: Vec<(Rect, usize)>,
    discovery_rows: Vec<(Rect, usize)>,
    scroll: Option<Rect>,
}

/// Which of the dialog's three single-line fields a caret belongs to. They
/// shared one `CursorBank` identity with three field names; three cursors here
/// reproduce that exactly.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SourceField {
    Manual,
    Discovery,
    Ai,
}

#[derive(Debug)]
pub struct SourceDialog {
    /// Whether the layer is on the stack. The slot is permanent so a completion
    /// arriving after close still lands somewhere.
    open: bool,
    state: SourceDialogState,
    /// Tab moves the discovery status pane into the arrow keys' path; the
    /// renderer reads it too. It was `App::dialog_scroll_focused`, shared with
    /// the legacy dialogs; this is Source's own copy.
    scroll_focused: bool,
    cursors: [Option<TextCursor>; 3],
    next_path_completion_generation: u64,
    next_ai_generation: u64,
    /// When the debounced path scan may be handed to `lvu-app`.
    path_completion_ready_at: Option<Instant>,
    geometry: SourceGeometry,
    surface: Surface,
    pub outbox: Outbox<SourceRequest>,
}

impl Default for SourceDialog {
    fn default() -> Self {
        Self {
            open: false,
            state: SourceDialogState::default(),
            scroll_focused: false,
            cursors: [None; 3],
            next_path_completion_generation: 1,
            next_ai_generation: 1,
            path_completion_ready_at: None,
            geometry: SourceGeometry::default(),
            surface: Surface::default(),
            outbox: Outbox::new(SOURCE_OUTBOX_CAP),
        }
    }
}

impl SourceDialog {
    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn state(&self) -> &SourceDialogState {
        &self.state
    }

    /// Marks the layer open without a `Ctx`, so a palette test can ask for the
    /// entries it contributes while it is on the stack.
    pub fn open_for_test(&mut self) {
        self.open = true;
    }

    /// An empty workspace opens on Add source before any event has been
    /// handled, so the layer is on the stack from `App::new`.
    pub(crate) fn open_at_startup(&mut self) {
        self.open = true;
        self.seed_surface();
    }

    /// `Surface.text_focus` is documented as coming from the last render, and
    /// the shell reads it to decide whether `q` dismisses. A layer that opens
    /// straight onto a text field would take the first `q` as a dismissal
    /// before it has ever drawn, which `is_text_editing` never did, so opening
    /// seeds it. In the running app a frame always intervenes; `App::new` on an
    /// empty workspace is the case where one does not.
    fn seed_surface(&mut self) {
        self.surface = Surface {
            text_focus: self.text_focus(),
            ..Surface::default()
        };
    }

    /// Geometry recorded by the last `render`; `hit()` is how input reaches it.
    pub fn control_rects(&self) -> &[(Rect, SourceControl)] {
        &self.geometry.controls
    }

    pub fn path_completion_rects(&self) -> &[(Rect, usize)] {
        &self.geometry.path_completion_rows
    }

    pub fn discovery_rects(&self) -> &[(Rect, usize)] {
        &self.geometry.discovery_rows
    }

    /// The scrollable status or proposal pane, when one was drawn.
    pub fn scroll_rect(&self) -> Option<Rect> {
        self.geometry.scroll
    }

    // ---- the four drains `lvu-app` calls (§2.4, §8) --------------------

    pub fn take_launches(&mut self) -> Vec<SourceLaunchRequest> {
        self.outbox.take_where(launch)
    }

    pub fn take_discovery(&mut self) -> Vec<DiscoveryUiRequest> {
        self.outbox.take_where(discovery)
    }

    /// Withheld until the typing pause has elapsed, exactly as
    /// `App::take_path_completion_requests` withheld it.
    pub fn take_path_completions(&mut self) -> Vec<PathCompletionRequest> {
        if self
            .path_completion_ready_at
            .is_some_and(|ready_at| Instant::now() < ready_at)
        {
            return Vec::new();
        }
        self.path_completion_ready_at = None;
        self.outbox.take_where(path_completion)
    }

    pub fn take_ai(&mut self) -> Vec<SourceAiRequest> {
        self.outbox.take_where(ai)
    }

    // ---- completions -----------------------------------------------------

    /// The generation `lvu-app` may still answer, or `None` when nothing is
    /// outstanding.
    pub fn active_path_completion_generation(&self) -> Option<u64> {
        (self.state.mode == SourceDialogMode::Manual
            && self.state.kind == SourceKind::File
            && self.state.path_completion.scanning)
            .then_some(self.state.path_completion.generation)
    }

    pub fn complete_path(
        &mut self,
        generation: u64,
        original_draft: &str,
        candidates: Vec<String>,
        error: Option<String>,
    ) -> bool {
        if !self.open
            || self.state.mode != SourceDialogMode::Manual
            || self.state.kind != SourceKind::File
            || self.state.draft != original_draft
            || self.state.path_completion.generation != generation
        {
            return false;
        }
        self.state.path_completion.scanning = false;
        self.state.path_completion.candidates = candidates;
        self.state.path_completion.selected = 0;
        self.state.error = error;
        true
    }

    pub fn complete_discovery(
        &mut self,
        generation: u64,
        items: Vec<DiscoveryItem>,
        status: String,
    ) -> bool {
        if !self.open || self.state.discovery.generation != generation {
            return false;
        }
        self.state.discovery.items = items;
        self.state.discovery.selected = 0;
        self.state.discovery.scanning = false;
        self.state.discovery.status = status;
        self.state.discovery.status_scroll = 0;
        self.state.error = None;
        true
    }

    pub fn update_ai_progress(
        &mut self,
        generation: u64,
        stage: SourceAiStage,
        progress: String,
        session_id: Option<String>,
    ) -> bool {
        if !self.open || self.state.ai.generation != generation {
            return false;
        }
        self.state.ai.stage = stage;
        self.state.ai.progress = progress;
        if session_id.is_some() {
            self.state.ai.session_id = session_id;
        }
        true
    }

    pub fn finish_ai(&mut self, generation: u64, result: Result<SourceAiPreview, String>) -> bool {
        if !self.open || self.state.ai.generation != generation {
            return false;
        }
        let ai = &mut self.state.ai;
        match result {
            Ok(preview) => {
                ai.preview = Some(preview);
                ai.preview_scroll = 0;
                ai.preview_scroll_limit = 0;
                ai.stage = SourceAiStage::Proposal;
                ai.progress = "Review only — explicit confirmation starts this source".into();
            }
            Err(error) => {
                ai.preview = None;
                ai.preview_scroll = 0;
                ai.preview_scroll_limit = 0;
                ai.stage = SourceAiStage::Error;
                ai.progress = error;
            }
        }
        true
    }

    /// An all-failed Apply keeps its review: the same generation stays on the
    /// Proposal stage with the failure summary as its progress, so freeing a
    /// slot and confirming retries these exact identities instead of
    /// proposing anew. The preview is deliberately retained, unlike an
    /// `Err` completion, which ends the review.
    pub fn retain_ai_batch(&mut self, generation: u64, summary: String) -> bool {
        if !self.open || self.state.ai.generation != generation {
            return false;
        }
        let ai = &mut self.state.ai;
        ai.stage = SourceAiStage::Proposal;
        ai.progress = summary;
        true
    }

    /// Whether the reviewed agent source that just started is the one this
    /// dialog is showing. The shell closes the layer and selects the view.
    pub fn ai_launch_matches(&self, generation: u64) -> bool {
        self.open && self.state.ai.generation == generation
    }

    /// Whether the launch that just started is the draft this dialog submitted.
    pub fn launch_matches(&self, request: &SourceLaunchRequest) -> bool {
        self.open && self.state.kind == request.kind && self.state.draft == request.text
    }

    /// Whether the discovered source that just started is the one this dialog
    /// selected.
    pub fn discovery_matches(&self, generation: u64) -> bool {
        self.open
            && self.state.mode == SourceDialogMode::Discovery
            && self.state.discovery.generation == generation
    }

    /// A launch the runtime refused. Reported in the dialog's own error row;
    /// `false` means there is no dialog showing that draft, and the shell
    /// reopens one on it — the only way a failure is not silently lost.
    pub fn fail_launch(&mut self, request: &SourceLaunchRequest, message: &str) -> bool {
        if !self.open {
            return false;
        }
        if self.launch_matches(request) {
            self.state.error = Some(message.to_owned());
        }
        true
    }

    /// Reopened on a refused launch: the draft that failed, with its reason.
    pub fn open_on_failure(&mut self, request: SourceLaunchRequest, message: String) {
        self.state = SourceDialogState {
            kind: request.kind,
            draft: request.text,
            error: Some(message),
            ..SourceDialogState::default()
        };
        self.cursors = [None; 3];
        self.scroll_focused = false;
        self.open = true;
    }

    pub fn fail_discovery(&mut self, generation: u64, message: String) {
        if self.discovery_matches(generation) {
            self.state.error = Some(message);
        }
    }

    /// Closed by the shell rather than by a `Dismiss` — a source started, or
    /// `lvu-app` opened the workspace on sources that already exist. This drops
    /// the dialog's state, as `source_dialog = None` did: reopening after a
    /// reviewed launch must not find the proposal it already accepted, or the
    /// next Enter would re-apply that generation instead of starting a new one.
    pub fn close(&mut self) {
        self.open = false;
        self.state = SourceDialogState::default();
        self.cursors = [None; 3];
        self.scroll_focused = false;
        self.path_completion_ready_at = None;
    }

    // ---- state -----------------------------------------------------------

    fn field(&self) -> Option<SourceField> {
        // `active_text_target` returned `None` outright while the scroll pane
        // held focus, which is what handed it the arrows and made `q` a
        // dismissal there.
        if self.scroll_focused || self.state.control != SourceControl::Input {
            return None;
        }
        Some(match self.state.mode {
            SourceDialogMode::Manual => SourceField::Manual,
            SourceDialogMode::Discovery => SourceField::Discovery,
            SourceDialogMode::Ai
                if matches!(
                    self.state.ai.stage,
                    SourceAiStage::Input | SourceAiStage::Error
                ) =>
            {
                SourceField::Ai
            }
            SourceDialogMode::Ai => return None,
        })
    }

    fn text_focus(&self) -> bool {
        self.field().is_some()
    }

    fn value_of(&self, field: SourceField) -> &str {
        match field {
            SourceField::Manual => &self.state.draft,
            SourceField::Discovery => &self.state.discovery.query,
            SourceField::Ai => &self.state.ai.instruction,
        }
    }

    /// The caret the focused field draws from, or the end of its value the
    /// first time it takes focus — what `CursorBank::get_or_end` did for it.
    fn active_cursor(&self) -> Option<usize> {
        let field = self.field()?;
        let value = self.value_of(field);
        Some(
            self.cursors[field as usize]
                .map_or_else(|| value.chars().count(), |cursor| cursor.char_index)
                .min(value.chars().count()),
        )
    }

    fn reset_cursor(&mut self, field: SourceField) {
        let end = self.value_of(field).chars().count();
        self.cursors[field as usize] = Some(TextCursor { char_index: end });
    }

    fn record(
        &mut self,
        geometry: SourceGeometry,
        caret: Option<(u16, u16)>,
        surface: Surface,
    ) -> Surface {
        self.geometry = geometry;
        self.surface = Surface { caret, ..surface };
        self.surface
    }
}

/// The four selectors `Outbox::take_where` uses: each recognises one kind and
/// hands the rest back, so four consumers share one queue (§8).
fn launch(request: SourceRequest) -> Result<SourceLaunchRequest, SourceRequest> {
    match request {
        SourceRequest::Launch(request) => Ok(request),
        other => Err(other),
    }
}

fn discovery(request: SourceRequest) -> Result<DiscoveryUiRequest, SourceRequest> {
    match request {
        SourceRequest::Discovery(request) => Ok(request),
        other => Err(other),
    }
}

fn path_completion(request: SourceRequest) -> Result<PathCompletionRequest, SourceRequest> {
    match request {
        SourceRequest::PathCompletion(request) => Ok(request),
        other => Err(other),
    }
}

fn ai(request: SourceRequest) -> Result<SourceAiRequest, SourceRequest> {
    match request {
        SourceRequest::Ai(request) => Ok(request),
        other => Err(other),
    }
}

fn contains(area: Rect, point: (u16, u16)) -> bool {
    point.0 >= area.x && point.0 < area.right() && point.1 >= area.y && point.1 < area.bottom()
}

pub(crate) fn clear_path_completion(dialog: &mut SourceDialogState) {
    dialog.path_completion.generation = 0;
    dialog.path_completion.scanning = false;
    dialog.path_completion.candidates.clear();
    dialog.path_completion.selected = 0;
}

/// Discovery rows the query keeps, by index into `items`. Moved from `App`.
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

impl SourceDialog {
    // ---- moved from App's private source helpers -------------------------

    fn append_source(&mut self, text: &str) {
        let remaining = MAX_EDITOR_BYTES.saturating_sub(self.state.draft.len());
        let mut end = text.len().min(remaining);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        self.state.draft.push_str(&text[..end]);
        self.state.error = None;
        clear_path_completion(&mut self.state);
        self.reset_cursor(SourceField::Manual);
        if self.state.mode == SourceDialogMode::Manual && self.state.kind == SourceKind::File {
            self.schedule_path_completion();
        }
    }

    fn append_discovery_query(&mut self, text: &str) {
        let discovery = &mut self.state.discovery;
        let remaining = MAX_EDITOR_BYTES.saturating_sub(discovery.query.len());
        let mut end = text.len().min(remaining);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        discovery.query.push_str(&text[..end]);
        discovery.selected = 0;
        self.reset_cursor(SourceField::Discovery);
    }

    fn append_ai(&mut self, text: &str) {
        if !matches!(
            self.state.ai.stage,
            SourceAiStage::Input | SourceAiStage::Error
        ) {
            return;
        }
        let remaining = MAX_AI_PROMPT_BYTES.saturating_sub(self.state.ai.instruction.len());
        let mut end = text.len().min(remaining);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        self.state.ai.instruction.push_str(&text[..end]);
        self.state.ai.stage = SourceAiStage::Input;
        self.reset_cursor(SourceField::Ai);
    }

    /// Each kind refuses at its own depth, as it did when each had its own
    /// queue, so the count is per kind rather than of the shared outbox.
    fn discovery_queued(&self) -> usize {
        self.outbox
            .iter()
            .filter(|request| matches!(request, SourceRequest::Discovery(_)))
            .count()
    }

    fn launches_queued(&self) -> usize {
        self.outbox
            .iter()
            .filter(|request| matches!(request, SourceRequest::Launch(_)))
            .count()
    }

    fn ai_queued(&self) -> usize {
        self.outbox
            .iter()
            .filter(|request| matches!(request, SourceRequest::Ai(_)))
            .count()
    }

    fn drop_path_completions(&mut self) {
        let _ = self.take_pending_path_completions();
        self.path_completion_ready_at = None;
    }

    fn take_pending_path_completions(&mut self) -> Vec<PathCompletionRequest> {
        self.outbox.take_where(path_completion)
    }

    fn complete_path_request(&mut self) {
        let generation = self.next_path_completion_generation;
        self.next_path_completion_generation = self.next_path_completion_generation.wrapping_add(1);
        self.state.path_completion.generation = generation;
        self.state.path_completion.scanning = true;
        self.state.error = None;
        let draft = self.state.draft.clone();
        self.drop_path_completions();
        let _ = self
            .outbox
            .push(SourceRequest::PathCompletion(PathCompletionRequest {
                generation,
                draft,
            }));
        self.path_completion_ready_at = Some(Instant::now() + SOURCE_PATH_COMPLETION_DEBOUNCE);
    }

    fn schedule_path_completion(&mut self) {
        if self.state.mode != SourceDialogMode::Manual || self.state.kind != SourceKind::File {
            return;
        }
        if self.state.draft.is_empty() {
            clear_path_completion(&mut self.state);
            self.drop_path_completions();
            return;
        }
        self.state.path_completion.candidates.clear();
        self.state.path_completion.selected = 0;
        self.complete_path_request();
    }

    fn start_discovery_scan(&mut self) {
        let discovery = &mut self.state.discovery;
        let cancel =
            (discovery.generation > 0 && discovery.scanning).then_some(discovery.generation);
        discovery.generation = discovery.generation.saturating_add(1).max(1);
        discovery.scanning = true;
        discovery.items.clear();
        discovery.selected = 0;
        discovery.status = "scanning bounded local providers…".into();
        let generation = discovery.generation;
        if let Some(generation) = cancel {
            let _ = self
                .outbox
                .push(SourceRequest::Discovery(DiscoveryUiRequest::Cancel {
                    generation,
                }));
        }
        if self.discovery_queued() < MAX_DISCOVERY_REQUESTS {
            let _ = self
                .outbox
                .push(SourceRequest::Discovery(DiscoveryUiRequest::Scan {
                    generation,
                }));
        } else {
            self.state.discovery.scanning = false;
            self.state.discovery.status = "discovery request queue is full".into();
        }
    }

    fn move_discovery(&mut self, delta: i32) {
        let count = filtered_discovery_indices(&self.state.discovery).len();
        if count == 0 {
            self.state.discovery.selected = 0;
            return;
        }
        self.state.discovery.selected = self
            .state
            .discovery
            .selected
            .saturating_add_signed(delta as isize)
            .min(count - 1);
    }

    fn move_path_completion(&mut self, delta: i32) {
        if self.state.mode == SourceDialogMode::Discovery {
            self.move_discovery(delta);
            return;
        }
        if self.state.mode == SourceDialogMode::Ai && self.state.ai.stage == SourceAiStage::Proposal
        {
            // The visible window is only measured while rendering, so a key
            // pressed in the same frame the proposal arrived must not be
            // clamped against a stale zero limit. The renderer clamps and
            // writes back the settled offset.
            self.state.ai.preview_scroll = self
                .state
                .ai
                .preview_scroll
                .saturating_add_signed(delta as isize);
            return;
        }
        if self.state.mode == SourceDialogMode::Manual
            && self.state.kind == SourceKind::File
            && !self.state.path_completion.candidates.is_empty()
        {
            self.state.path_completion.selected = crate::app::move_index(
                self.state.path_completion.selected,
                self.state.path_completion.candidates.len(),
                delta,
            );
        }
    }

    fn scroll_discovery_status(&mut self, delta: i32) {
        self.state.discovery.status_scroll = self
            .state
            .discovery
            .status_scroll
            .saturating_add_signed(delta as isize)
            .min(self.state.discovery.status_scroll_limit);
    }

    fn submit_discovered_source(&mut self) {
        let indices = filtered_discovery_indices(&self.state.discovery);
        let Some(index) = indices.get(self.state.discovery.selected).copied() else {
            self.state.error = Some("no matching discovered source to start".into());
            return;
        };
        if self.discovery_queued() >= MAX_DISCOVERY_REQUESTS {
            self.state.error = Some("discovery action queue is full".into());
            return;
        }
        let generation = self.state.discovery.generation;
        let key = self.state.discovery.items[index].key.clone();
        let _ = self
            .outbox
            .push(SourceRequest::Discovery(DiscoveryUiRequest::Select {
                generation,
                key,
            }));
        self.state.error = Some("starting selected source…".into());
    }

    fn submit_source(&mut self) {
        let pending_directory = self.state.mode == SourceDialogMode::Manual
            && self.state.kind == SourceKind::File
            && self.state.draft.ends_with('/')
            && self.state.path_completion.candidates.is_empty();
        if pending_directory {
            if !self.state.path_completion.scanning {
                self.schedule_path_completion();
            }
            return;
        }
        let selected_path = (self.state.mode == SourceDialogMode::Manual
            && self.state.kind == SourceKind::File)
            .then(|| {
                self.state
                    .path_completion
                    .candidates
                    .get(self.state.path_completion.selected)
                    .cloned()
            })
            .flatten();
        if let Some(path) = selected_path {
            let is_directory = path.ends_with('/');
            self.state.draft = path;
            clear_path_completion(&mut self.state);
            self.reset_cursor(SourceField::Manual);
            if is_directory {
                self.schedule_path_completion();
                return;
            }
        }
        if self.state.draft.is_empty() {
            self.state.error = Some("enter a file path or shell command".into());
            return;
        }
        if self.launches_queued() >= MAX_SOURCE_REQUESTS {
            self.state.error = Some("source launch queue is full".into());
            return;
        }
        let kind = self.state.kind;
        let text = self.state.draft.clone();
        let _ = self
            .outbox
            .push(SourceRequest::Launch(SourceLaunchRequest { kind, text }));
        self.state.error = Some("starting source…".into());
    }

    fn submit_source_ai(&mut self, ctx: &mut Ctx<'_>) {
        if self.state.ai.stage == SourceAiStage::Proposal {
            if self.ai_queued() >= MAX_SOURCE_AI_REQUESTS {
                self.state.ai.progress = "source agent request queue is full".into();
            } else {
                let generation = self.state.ai.generation;
                let count = self
                    .state
                    .ai
                    .preview
                    .as_ref()
                    .map_or(0, |preview| preview.sources.len());
                // The review leaves the actionable state before the Apply is
                // enqueued, so a repeated confirmation while starts settle
                // cannot queue a second Apply for this generation.
                self.state.ai.stage = SourceAiStage::Applying;
                self.state.ai.progress = if count > 1 {
                    format!("starting {count} reviewed sources…")
                } else {
                    "starting reviewed source…".into()
                };
                let _ = self
                    .outbox
                    .push(SourceRequest::Ai(SourceAiRequest::Apply { generation }));
            }
            return;
        }
        if !matches!(
            self.state.ai.stage,
            SourceAiStage::Input | SourceAiStage::Error
        ) {
            return;
        }
        if self.state.ai.instruction.trim().is_empty() {
            self.state.ai.stage = SourceAiStage::Error;
            self.state.ai.progress = "describe the source to follow".into();
            return;
        }
        if self.ai_queued() >= MAX_SOURCE_AI_REQUESTS {
            self.state.ai.stage = SourceAiStage::Error;
            self.state.ai.progress = "source agent request queue is full".into();
            return;
        }
        self.next_ai_generation = self.next_ai_generation.saturating_add(1);
        self.state.ai.generation = self.next_ai_generation;
        self.state.ai.stage = SourceAiStage::Preparing;
        self.state.ai.progress = "collecting bounded read-only discovery context".into();
        self.state.ai.preview = None;
        self.state.ai.preview_scroll = 0;
        self.state.ai.preview_scroll_limit = 0;
        let agent = ctx.agent.clone();
        let _ = self.outbox.push(SourceRequest::Ai(SourceAiRequest::Start {
            generation: self.state.ai.generation,
            instruction: self.state.ai.instruction.clone(),
            provider: agent.provider,
            mode: agent.mode,
            thinking: agent.thinking,
        }));
    }

    fn submit(&mut self, ctx: &mut Ctx<'_>) {
        match self.state.mode {
            SourceDialogMode::Discovery => self.submit_discovered_source(),
            SourceDialogMode::Ai => self.submit_source_ai(ctx),
            SourceDialogMode::Manual => self.submit_source(),
        }
    }

    // ---- moved from the `Action::*Source*` arms ---------------------------

    fn set_mode(&mut self, mode: SourceDialogMode) {
        self.state.mode = mode;
        self.state.control = SourceControl::Input;
        self.state.controls_focused = false;
        clear_path_completion(&mut self.state);
        self.drop_path_completions();
        if mode == SourceDialogMode::Discovery && self.state.discovery.generation == 0 {
            self.start_discovery_scan();
        }
    }

    fn toggle_discovery(&mut self) {
        let mode = match self.state.mode {
            SourceDialogMode::Manual => SourceDialogMode::Discovery,
            SourceDialogMode::Discovery | SourceDialogMode::Ai => SourceDialogMode::Manual,
        };
        self.set_mode(mode);
    }

    fn toggle_ai(&mut self) {
        let mode = if self.state.mode == SourceDialogMode::Ai {
            SourceDialogMode::Manual
        } else {
            SourceDialogMode::Ai
        };
        self.set_mode(mode);
    }

    fn toggle_control_focus(&mut self) {
        let controls = SourceControl::visible(self.state.mode, self.state.kind);
        let index = controls
            .iter()
            .position(|control| *control == self.state.control)
            .unwrap_or(0);
        self.state.control = controls[(index + 1) % controls.len()];
        self.state.controls_focused = self.state.control != SourceControl::Input;
    }

    fn move_source_mode(&mut self, delta: i32) {
        const MODES: [SourceDialogMode; 3] = [
            SourceDialogMode::Manual,
            SourceDialogMode::Discovery,
            SourceDialogMode::Ai,
        ];
        if !self.state.controls_focused {
            return;
        }
        let index = MODES
            .iter()
            .position(|mode| *mode == self.state.mode)
            .unwrap_or(0);
        self.state.mode = MODES[(index as i32 + delta).rem_euclid(MODES.len() as i32) as usize];
        self.state.control = match self.state.mode {
            SourceDialogMode::Manual => SourceControl::Manual,
            SourceDialogMode::Discovery => SourceControl::Discovery,
            SourceDialogMode::Ai => SourceControl::Agent,
        };
        clear_path_completion(&mut self.state);
        if self.state.mode == SourceDialogMode::Discovery && self.state.discovery.generation == 0 {
            self.start_discovery_scan();
        }
    }

    fn select_kind(&mut self, kind: SourceKind) {
        if self.state.mode == SourceDialogMode::Manual {
            self.state.kind = kind;
            if !SourceControl::visible(self.state.mode, kind).contains(&self.state.control) {
                self.state.control = SourceControl::Input;
                self.state.controls_focused = false;
            }
            self.state.error = None;
            clear_path_completion(&mut self.state);
        }
        if kind == SourceKind::Command {
            self.drop_path_completions();
        }
    }

    fn activate(&mut self, ctx: &mut Ctx<'_>) {
        match self.state.control {
            SourceControl::Input => self.submit(ctx),
            SourceControl::Manual => self.set_mode(SourceDialogMode::Manual),
            SourceControl::Discovery => self.set_mode(SourceDialogMode::Discovery),
            SourceControl::Agent => self.set_mode(SourceDialogMode::Ai),
            SourceControl::File => self.select_kind(SourceKind::File),
            SourceControl::Command => self.select_kind(SourceKind::Command),
            SourceControl::Refresh => self.start_discovery_scan(),
        }
    }

    fn focus_control(&mut self, control: SourceControl, ctx: &mut Ctx<'_>) {
        self.state.control = control;
        self.state.controls_focused = control != SourceControl::Input;
        self.activate(ctx);
    }

    /// `Action::ModalVertical` for this focus, unchanged.
    fn modal_vertical(&mut self, delta: i32) {
        match self.state.mode {
            SourceDialogMode::Discovery if self.scroll_focused => {
                self.scroll_discovery_status(delta)
            }
            SourceDialogMode::Discovery => self.move_discovery(delta),
            SourceDialogMode::Manual | SourceDialogMode::Ai => self.move_path_completion(delta),
        }
    }

    fn edit_field(&mut self, command: EditCommand<'_>) {
        let Some(field) = self.field() else { return };
        let policy = EditPolicy {
            max_bytes: match field {
                SourceField::Ai => MAX_AI_PROMPT_BYTES,
                _ => MAX_EDITOR_BYTES,
            },
            multiline: false,
        };
        let value = self.value_of(field).to_owned();
        let mut cursor = TextCursor {
            char_index: self.cursors[field as usize]
                .map_or_else(|| value.chars().count(), |cursor| cursor.char_index)
                .min(value.chars().count()),
        };
        let mut edited = value.clone();
        let outcome = edit(&mut edited, &mut cursor, command, policy);
        self.cursors[field as usize] = Some(cursor);
        if !outcome.changed {
            return;
        }
        match field {
            SourceField::Manual => {
                self.state.draft = edited;
                self.state.error = None;
                clear_path_completion(&mut self.state);
                if self.state.kind == SourceKind::File {
                    self.schedule_path_completion();
                }
            }
            SourceField::Discovery => {
                self.state.discovery.query = edited;
                self.state.discovery.selected = 0;
                self.state.error = None;
            }
            SourceField::Ai => {
                self.state.ai.instruction = edited;
                self.state.ai.stage = SourceAiStage::Input;
                self.state.error = None;
            }
        }
    }
}

/// §4.3: the palette entries Source owns. They reached inside the dialog
/// through `Action`s the palette gated on `Focus::SourceDialog`.
const SOURCE_COMMANDS: [(CommandId, CommandSpec); 4] = [
    (
        CommandId::DiscoverSources,
        CommandSpec {
            id: CommandId::DiscoverSources,
            name: "Discover recent sources",
            description: "Choose a remembered or discovered source",
            category: "Sources",
            aliases: &["recent", "docker", "journal"],
            shortcut: None,
        },
    ),
    (
        CommandId::AskAiSource,
        CommandSpec {
            id: CommandId::AskAiSource,
            name: "Describe source with agent",
            description: "Draft a source definition for review",
            category: "Sources",
            aliases: &["source ai", "generate source"],
            shortcut: None,
        },
    ),
    (
        CommandId::SourceFileMode,
        CommandSpec {
            id: CommandId::SourceFileMode,
            name: "Use file source",
            description: "Select file input in the source dialog",
            category: "Sources",
            aliases: &["path", "tail file"],
            shortcut: None,
        },
    ),
    (
        CommandId::SourceCommandMode,
        CommandSpec {
            id: CommandId::SourceCommandMode,
            name: "Use command source",
            description: "Select command input in the source dialog",
            category: "Sources",
            aliases: &["process", "argv", "shell"],
            shortcut: None,
        },
    ),
];

fn source_command_shortcut(id: CommandId) -> Option<&'static str> {
    Some(match id {
        CommandId::DiscoverSources => "Ctrl-D",
        CommandId::SourceFileMode => "Alt-F",
        CommandId::SourceCommandMode => "Alt-C",
        _ => return None,
    })
}

impl SourceDialog {
    /// The component's keymap, in the order `key_to_action` had it.
    fn key(&mut self, key: KeyEvent, ctx: &mut Ctx<'_>) -> Outcome {
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return Outcome::Ignored;
        }
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        if control {
            let command = match key.code {
                KeyCode::Char('a') => Some(EditCommand::StartOfLine),
                KeyCode::Char('e') => Some(EditCommand::EndOfLine),
                KeyCode::Char('k') => Some(EditCommand::KillToEndOfLine),
                _ => None,
            };
            if let Some(command) = command {
                self.edit_field(command);
                return Outcome::Consumed;
            }
        }
        // The input field owns Up/Down while it is taking text and something is
        // there to move through; `App::key_to_action` checked exactly this
        // before the base table saw the key.
        if key.modifiers.is_empty()
            && self.state.control == SourceControl::Input
            && !self.scroll_focused
            && matches!(key.code, KeyCode::Up | KeyCode::Down)
        {
            let delta = if key.code == KeyCode::Up { -1 } else { 1 };
            if self.state.mode == SourceDialogMode::Discovery {
                self.move_discovery(delta);
                return Outcome::Consumed;
            }
            if self.state.mode == SourceDialogMode::Manual
                && self.state.kind == SourceKind::File
                && !self.state.path_completion.candidates.is_empty()
            {
                self.move_path_completion(delta);
                return Outcome::Consumed;
            }
        }
        // Then the shared caret keys: `App::key_to_action` mapped the arrows to
        // `Text*` whenever a field was editing, which is why `Left`/`Right`
        // move the caret here and the mode selector only once a control has
        // focus.
        if key.modifiers.is_empty() && self.field().is_some() {
            let command = match key.code {
                KeyCode::Left => Some(EditCommand::MoveLeft),
                KeyCode::Right => Some(EditCommand::MoveRight),
                KeyCode::Up => Some(EditCommand::MoveUp),
                KeyCode::Down => Some(EditCommand::MoveDown),
                _ => None,
            };
            if let Some(command) = command {
                self.edit_field(command);
                return Outcome::Consumed;
            }
        }
        match key.code {
            KeyCode::Char('d') if control => self.toggle_discovery(),
            KeyCode::Char('r') if control => self.start_discovery_scan(),
            KeyCode::Char('f') if alt => self.select_kind(SourceKind::File),
            KeyCode::Char('c') if alt => self.select_kind(SourceKind::Command),
            KeyCode::Down => self.modal_vertical(1),
            KeyCode::Up => self.modal_vertical(-1),
            KeyCode::Left => self.move_source_mode(-1),
            KeyCode::Right => self.move_source_mode(1),
            KeyCode::Tab | KeyCode::BackTab => self.toggle_control_focus(),
            KeyCode::Enter => self.activate(ctx),
            KeyCode::Backspace => self.edit_field(EditCommand::Backspace),
            KeyCode::Char(character) if is_typed_char(&key) => {
                let mut buffer = [0u8; 4];
                self.edit_field(EditCommand::Insert(character.encode_utf8(&mut buffer)));
            }
            _ => return Outcome::Ignored,
        }
        Outcome::Consumed
    }

    /// `Action::ScrollHoveredDialog` for this focus.
    fn scroll_hovered(&mut self, delta: i32) {
        if self.state.mode == SourceDialogMode::Ai {
            self.move_path_completion(delta);
        } else {
            self.scroll_discovery_status(delta);
        }
    }

    /// The shell's mouse order for `Focus::SourceDialog`, unchanged: the
    /// scrollable pane took the point before any dialog branch, any other left
    /// press cleared its focus, then the dialog's own rows and controls.
    fn mouse(
        &mut self,
        kind: MouseEventKind,
        hit: Option<SourceHit>,
        ctx: &mut Ctx<'_>,
    ) -> Outcome {
        let pressed = matches!(kind, MouseEventKind::Down(MouseButton::Left));
        if hit == Some(SourceHit::Scroll) {
            match kind {
                MouseEventKind::Down(MouseButton::Left) => self.scroll_focused = true,
                MouseEventKind::ScrollUp => self.scroll_hovered(-1),
                MouseEventKind::ScrollDown => self.scroll_hovered(1),
                _ => {}
            }
            return Outcome::Consumed;
        }
        if pressed {
            self.scroll_focused = false;
            match hit {
                Some(SourceHit::PathCompletion(index)) => {
                    self.state.path_completion.selected = index;
                    self.state.control = SourceControl::Input;
                    self.state.controls_focused = false;
                    return Outcome::Consumed;
                }
                Some(SourceHit::Control(control)) => {
                    self.focus_control(control, ctx);
                    return Outcome::Consumed;
                }
                _ => {}
            }
        }
        if self.state.mode != SourceDialogMode::Discovery {
            return Outcome::Consumed;
        }
        if pressed && let Some(SourceHit::Discovery(index)) = hit {
            self.state.discovery.selected = index;
            self.state.control = SourceControl::Input;
            self.state.controls_focused = false;
        }
        match kind {
            MouseEventKind::ScrollUp => self.move_discovery(-1),
            MouseEventKind::ScrollDown => self.move_discovery(1),
            _ => {}
        }
        Outcome::Consumed
    }

    fn command(&mut self, id: CommandId) -> Outcome {
        match id {
            CommandId::DiscoverSources => self.toggle_discovery(),
            CommandId::AskAiSource => self.toggle_ai(),
            CommandId::SourceFileMode => self.select_kind(SourceKind::File),
            CommandId::SourceCommandMode => self.select_kind(SourceKind::Command),
            _ => return Outcome::Ignored,
        }
        Outcome::Consumed
    }
}

impl Component for SourceDialog {
    type Hit = SourceHit;
    type Open = ();

    fn open(&mut self, _params: (), _ctx: &mut Ctx<'_>) {
        // `Action::OpenSource` used `get_or_insert_with`: reopening kept the
        // draft, the discovered list and any proposal under review.
        self.open = true;
        self.scroll_focused = false;
        self.geometry = SourceGeometry::default();
        self.seed_surface();
    }

    fn handle(&mut self, event: Event<SourceHit>, ctx: &mut Ctx<'_>) -> Outcome {
        match event {
            Event::Key(key) => self.key(key, ctx),
            Event::Paste(text) => {
                match self.state.mode {
                    SourceDialogMode::Discovery => self.append_discovery_query(&text),
                    SourceDialogMode::Ai => self.append_ai(&text),
                    SourceDialogMode::Manual => self.append_source(&text),
                }
                Outcome::Consumed
            }
            Event::Mouse { kind, hit, .. } => self.mouse(kind, hit, ctx),
            // §5.3: the innermost thing closes first. An open completion list
            // absorbs the dismissal; then the layer closes, cancelling the scan
            // and any agent session it started.
            Event::Dismiss => {
                if self.state.path_completion.scanning
                    || !self.state.path_completion.candidates.is_empty()
                {
                    clear_path_completion(&mut self.state);
                    self.state.control = SourceControl::Input;
                    return Outcome::Consumed;
                }
                if self.state.discovery.scanning && self.discovery_queued() < MAX_DISCOVERY_REQUESTS
                {
                    let generation = self.state.discovery.generation;
                    let _ =
                        self.outbox
                            .push(SourceRequest::Discovery(DiscoveryUiRequest::Cancel {
                                generation,
                            }));
                }
                if !matches!(
                    self.state.ai.stage,
                    SourceAiStage::Input | SourceAiStage::Error
                ) && self.ai_queued() < MAX_SOURCE_AI_REQUESTS
                {
                    let generation = self.state.ai.generation;
                    let _ = self
                        .outbox
                        .push(SourceRequest::Ai(SourceAiRequest::Cancel { generation }));
                }
                self.state = SourceDialogState::default();
                self.cursors = [None; 3];
                self.open = false;
                Outcome::Close
            }
            Event::Command(id) => self.command(id),
            Event::View(_) | Event::Resize => Outcome::Ignored,
        }
    }

    fn commands(&self, _views: &crate::app::Views) -> Vec<CommandEntry> {
        SOURCE_COMMANDS
            .iter()
            .map(|(id, spec)| CommandEntry {
                spec: CommandSpec {
                    shortcut: self.open.then(|| source_command_shortcut(*id)).flatten(),
                    ..*spec
                },
                unavailable_reason: (!self.open).then_some("open Add source first"),
            })
            .collect()
    }

    fn surface(&self) -> Surface {
        self.surface
    }

    fn hit(&self, point: (u16, u16)) -> Option<SourceHit> {
        let geometry = &self.geometry;
        // The scrollable pane is tested first, as `handle_mouse`'s
        // `hit_regions.dialog_scroll` branch was: it ran before every
        // dialog-specific branch and returned.
        geometry
            .scroll
            .filter(|rect| contains(*rect, point))
            .map(|_| SourceHit::Scroll)
            .or_else(|| {
                geometry
                    .path_completion_rows
                    .iter()
                    .find_map(|(rect, index)| {
                        contains(*rect, point).then_some(SourceHit::PathCompletion(*index))
                    })
            })
            .or_else(|| {
                geometry.controls.iter().find_map(|(rect, control)| {
                    contains(*rect, point).then_some(SourceHit::Control(*control))
                })
            })
            .or_else(|| {
                geometry.discovery_rows.iter().find_map(|(rect, index)| {
                    contains(*rect, point).then_some(SourceHit::Discovery(*index))
                })
            })
    }

    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &RenderCtx<'_>) -> Surface {
        render_source(self, frame, area, ctx)
    }
}
/// Moved verbatim from `ui::render_source_dialog`: the hit regions it wrote
/// into `App` are the component's geometry now, and the three `App` reads it
/// made (`active_text_cursor`, `appearance.ascii`, `dialog_scroll_focused`) come
/// from the component and `RenderCtx`. A free function taking `&mut SourceDialog`
/// rather than a method, so the move stays line-for-line comparable.
fn render_source(
    this: &mut SourceDialog,
    frame: &mut Frame<'_>,
    area: Rect,
    ctx: &RenderCtx<'_>,
) -> Surface {
    use crate::app::SourceKind;
    use SourceControl as Control;
    use SourceDialogMode as Mode;

    let theme = ctx.theme;
    let ascii = ctx.ascii;
    let cursor = this.active_cursor();
    let mut geometry = SourceGeometry::default();
    let mut caret_cell: Option<(u16, u16)> = None;
    let dialog = this.state.clone();
    let styles = DialogStyles::new(theme);
    let agent_label = if ascii { "Agent" } else { "🧠 Agent" };

    // §8.6: the three modes are a segmented control in the header, not buttons
    // in the action row, so the primary action never shifts them sideways.
    let mode_controls = [Control::Manual, Control::Discovery, Control::Agent];
    let mode_labels = ["Manual", "Discover", agent_label];
    let active_mode = match dialog.mode {
        Mode::Manual => 0,
        Mode::Discovery => 1,
        Mode::Ai => 2,
    };

    let discovery_indices = filtered_discovery_indices(&dialog.discovery);
    let completions = &dialog.path_completion;
    let suggestion_count = if dialog.kind == SourceKind::File {
        completions.candidates.len()
    } else {
        0
    };

    // §12.7 review lines. Built once so the body can be measured before the
    // popup exists and rendered from the same list afterwards. A batch lists
    // every proposed source under a numbered heading with one shared Why, so
    // a one-source review reads exactly as it did before batches existed.
    let mut review = Vec::new();
    if let Some(preview) = &dialog.ai.preview {
        let numbered = preview.sources.len() > 1;
        for (index, source) in preview.sources.iter().enumerate() {
            if numbered {
                if index > 0 {
                    review.push(String::new());
                }
                review.push(format!(
                    "Source {} of {}: {}",
                    index + 1,
                    preview.sources.len(),
                    source.name
                ));
            } else {
                review.push(format!("Name: {}", source.name));
            }
            review.push(format!("Kind: {}", source.kind));
            review.push(format!("Launch: {}", source.launch));
            review.push(format!(
                "Effective path/cwd: {}",
                source.effective_path_or_cwd
            ));
            review.push(format!("Restart: {}", source.restart));
            if source.environment.is_empty() {
                review.push("Env: (none)".into());
            } else {
                review.extend(
                    source
                        .environment
                        .iter()
                        .map(|value| format!("Env: {value}")),
                );
            }
        }
        review.push(format!("Why: {}", preview.explanation));
    }
    if let Some(session) = &dialog.ai.session_id {
        review.push(format!("Local session: {session}"));
    }

    // §7.4 message row, one per dialog, replacing the boxed one-line State pane.
    let (state, sentence) = match dialog.mode {
        _ if dialog.error.is_some() => (
            MessageState::Error,
            dialog.error.clone().unwrap_or_default(),
        ),
        Mode::Ai => {
            let (label, _) = source_ai_status(dialog.ai.stage, theme);
            let state = match label {
                "Error" => MessageState::Error,
                "Updating" => MessageState::Updating,
                _ => MessageState::Ready,
            };
            (state, dialog.ai.progress.clone())
        }
        // The promise that selection never starts capture has to survive the
        // scanning state too: that is exactly when a candidate first appears.
        Mode::Discovery if dialog.discovery.scanning => (
            MessageState::Updating,
            // The promise leads so it survives the wrap at narrow widths.
            format!(
                "selecting a candidate never starts capture · scanning, {} so far",
                dialog.discovery.items.len()
            ),
        ),
        Mode::Discovery => (
            MessageState::Ready,
            "selecting a candidate never starts capture".to_owned(),
        ),
        Mode::Manual => (
            MessageState::Ready,
            "capture starts only when you open the source".to_owned(),
        ),
    };
    // The promise that review never executes belongs in the sticky message row,
    // not in help: help is the first region §5.4 drops under height pressure,
    // and this dialog is under pressure exactly when the proposal is long.
    let sentence = match dialog.mode {
        Mode::Ai if !sentence.contains("never executes") => {
            format!("{sentence} · the preview never executes")
        }
        _ => sentence,
    };
    let help = "";

    // A batch applies every listed source at once, so its action says how
    // many. A single reviewed source keeps the established label.
    let multi_start = match dialog.mode {
        Mode::Ai if dialog.ai.stage == SourceAiStage::Proposal => {
            let count = dialog
                .ai
                .preview
                .as_ref()
                .map_or(0, |preview| preview.sources.len());
            (count > 1).then(|| format!("Start {count} reviewed sources"))
        }
        _ => None,
    };
    let primary = match dialog.mode {
        Mode::Ai => match dialog.ai.stage {
            SourceAiStage::Input | SourceAiStage::Error => "Request proposal",
            SourceAiStage::Proposal => multi_start.as_deref().unwrap_or("Start reviewed source"),
            _ => "Working…",
        },
        _ => "Open",
    };
    let mut action_controls = vec![(Control::Input, primary)];
    if dialog.mode == Mode::Discovery {
        action_controls.push((Control::Refresh, "Rescan"));
    }
    let action_labels: Vec<&str> = action_controls.iter().map(|(_, label)| *label).collect();

    // Stable LongContent budgets: outer size is policy-only, never async
    // counts. Header 1 (the Manual/Discover/Agent segmented control, always
    // present), body minimum 3 useful rows, message 2 and no help row, actions
    // from the stable width budget so the frame and sticky tail origins are
    // identical across manual/discovery/loading/proposal/error states.
    // Hand-rolled row assignment below is presentation-only folding, never
    // query membership (AGENTS.md).
    let spec = source_spec_for(area);
    let Ok(resolved) = resolve_dialog(area, &spec, 1, &action_labels, Some(0), None) else {
        // Below the floor the tiny fallback owns the frame; stay open with
        // nothing drawn, as the palette does.
        return this.record(
            geometry,
            caret_cell,
            Surface {
                popup: Rect::default(),
                interior: Rect::default(),
                caret: None,
                scrollable: false,
                text_focus: this.text_focus(),
            },
        );
    };
    // Shared frame so geometry and paint share one definition; compactness
    // comes from the geometry, never recomputed from the frame.
    render_responsive_frame(frame, &resolved, "Add source", ctx.active, theme);
    let mut surface = Surface {
        popup: resolved.frame,
        interior: resolved.interior,
        caret: None,
        // Derived below from real pane overflow, never blanket true: wheel
        // and hitboxes must match viewports that actually scroll.
        scrollable: false,
        text_focus: this.text_focus(),
    };
    let message_rect = resolved.message;
    let help_rect = resolved.help;
    let action_geom = resolved.actions.clone();
    let roomy = !resolved.compact;

    let focused_mode = mode_controls
        .iter()
        .position(|control| *control == dialog.control);
    for (index, rect) in render_segmented_control(
        frame,
        resolved.header,
        &mode_labels,
        active_mode,
        focused_mode,
        theme,
    )
    .into_iter()
    .enumerate()
    {
        geometry.controls.push((rect, mode_controls[index]));
    }

    let body = resolved.body.viewport;
    if body.width == 0 || body.height == 0 {
        return this.record(geometry, caret_cell, surface);
    }
    // One-row section gaps while roomy, zero when tight (§4.1/§5.4); every
    // field and pane stays reachable either way.
    let gap = u16::from(roomy);
    match dialog.mode {
        Mode::Manual => {
            let label_width = u16::try_from(UnicodeWidthStr::width("Command")).unwrap_or(7);
            // §8.4: File/Command is a choice between two kinds, not two actions.
            let kind_controls = [Control::File, Control::Command];
            let focused_kind = kind_controls
                .iter()
                .position(|control| *control == dialog.control);
            frame.render_widget(
                Paragraph::new("Kind").style(styles.label),
                Rect::new(body.x, body.y, label_width.min(body.width), 1),
            );
            let radio_x = body
                .x
                .saturating_add(label_width)
                .saturating_add(FIELD_GUTTER);
            for (index, rect) in render_radio_row(
                frame,
                Rect::new(radio_x, body.y, body.right().saturating_sub(radio_x), 1),
                &["File", "Command"],
                usize::from(dialog.kind == SourceKind::Command),
                focused_kind,
                ascii,
                theme,
            )
            .into_iter()
            .enumerate()
            {
                geometry.controls.push((rect, kind_controls[index]));
            }

            let (label, placeholder) = if dialog.kind == SourceKind::File {
                ("Path", "path to a log file")
            } else {
                ("Command", "program and arguments, e.g. journalctl -f")
            };
            if body.height > 1 {
                caret_cell = render_form_field(
                    frame,
                    Rect::new(body.x, body.y.saturating_add(1), body.width, 1),
                    label_width,
                    label,
                    &dialog.draft,
                    placeholder,
                    !dialog.controls_focused,
                    cursor,
                    theme,
                )
                .1;
            }

            let rest = Rect::new(
                body.x,
                body.y.saturating_add(2).saturating_add(gap),
                body.width,
                body.height.saturating_sub(2).saturating_sub(gap),
            );
            if rest.height > 0 && completions.scanning {
                frame.render_widget(
                    Paragraph::new("Completing path…").style(styles.pending),
                    rest,
                );
            } else if rest.height > 0 && suggestion_count == 0 && dialog.kind == SourceKind::File {
                // The list rows are there whether or not there is a list to
                // put in them, so say what they are for rather than leaving
                // the dialog with a block of dead space.
                let rects = plan_list(rest, 0, 0, None, 0);
                frame.render_widget(
                    Paragraph::new("Suggestions").style(styles.label.add_modifier(Modifier::BOLD)),
                    rects.heading,
                );
                frame.render_widget(
                    Paragraph::new(if dialog.draft.is_empty() {
                        "type a path to see matching files"
                    } else {
                        "no matching paths"
                    })
                    .style(styles.unavailable),
                    rects.viewport,
                );
            } else if rest.height > 0 && suggestion_count > 0 {
                let count = format!(
                    "{suggestion_count} match{}",
                    if suggestion_count == 1 { "" } else { "es" }
                );
                // One authoritative list plan: heading/count/viewport/
                // scrollbar plus the selected window and painted row rects.
                // The same rects drive paint, selection, scrollbar and mouse.
                let selected = completions.selected.min(suggestion_count.saturating_sub(1));
                let rects = plan_list(
                    rest,
                    u16::try_from(UnicodeWidthStr::width(count.as_str())).unwrap_or(0),
                    suggestion_count,
                    Some(selected),
                    0,
                );
                frame.render_widget(
                    Paragraph::new("Suggestions").style(styles.label.add_modifier(Modifier::BOLD)),
                    rects.heading,
                );
                if rects.count.width > 0 {
                    frame.render_widget(
                        Paragraph::new(count)
                            .style(styles.description)
                            .right_aligned(),
                        rects.count,
                    );
                }
                let top = rects.first_row;
                for (offset, row) in rects.row_rects.iter().copied().enumerate() {
                    let index = top.saturating_add(offset);
                    let Some(candidate) = completions.candidates.get(index) else {
                        continue;
                    };
                    let chosen = index == selected;
                    let marker = if chosen {
                        if ascii { "> " } else { "› " }
                    } else {
                        "  "
                    };
                    frame.render_widget(
                        Paragraph::new(truncated(
                            &format!("{marker}{candidate}"),
                            usize::from(row.width),
                        ))
                        .style(if chosen {
                            styles.selection
                        } else {
                            styles.description
                        }),
                        row,
                    );
                    geometry.path_completion_rows.push((row, index));
                }
                if let Some(bar) = rects.scrollbar {
                    render_scrollbar(
                        frame,
                        bar,
                        top,
                        suggestion_count.saturating_sub(rects.row_rects.len().max(1)),
                        theme,
                        ascii,
                    );
                    surface.scrollable = true;
                }
            } else if rest.height > 0 && dialog.kind == SourceKind::Command {
                frame.render_widget(
                    Paragraph::new("Command completion is disabled; the command runs in the workspace directory.")
                        .wrap(Wrap { trim: true })
                        .style(styles.description),
                    rest,
                );
            }
        }
        Mode::Discovery => {
            let label_width = u16::try_from(UnicodeWidthStr::width("Filter")).unwrap_or(6);
            caret_cell = render_form_field(
                frame,
                Rect::new(body.x, body.y, body.width, 1),
                label_width,
                "Filter",
                &dialog.discovery.query,
                "narrow the candidates",
                !dialog.controls_focused,
                cursor,
                theme,
            )
            .1;

            let details_rows = 3u16.min(body.height.saturating_sub(1).saturating_sub(gap));
            let list_y = body.y.saturating_add(1).saturating_add(gap);
            let list_area = Rect::new(
                body.x,
                list_y,
                body.width,
                body.height
                    .saturating_sub(1)
                    .saturating_sub(gap)
                    .saturating_sub(details_rows),
            );
            if list_area.height > 0 {
                let total = discovery_indices.len();
                let selected = dialog
                    .discovery
                    .selected
                    .min(discovery_indices.len().saturating_sub(1));
                // One authoritative list plan: heading/count/viewport/
                // scrollbar plus the selected window and painted row rects.
                // The same rects drive paint, selection, scrollbar and mouse.
                // The heading counts what is shown against the total (§8.7).
                let plan_probe = plan_list(
                    list_area,
                    0,
                    total.max(1),
                    (!discovery_indices.is_empty()).then_some(selected),
                    0,
                );
                let shown = plan_probe.row_rects.len().min(total);
                let count = if total == 0 {
                    "none".to_owned()
                } else {
                    format!("{shown} of {total}")
                };
                let rects = plan_list(
                    list_area,
                    u16::try_from(UnicodeWidthStr::width(count.as_str())).unwrap_or(0),
                    total.max(1),
                    (!discovery_indices.is_empty()).then_some(selected),
                    0,
                );
                frame.render_widget(
                    Paragraph::new("Candidates").style(styles.label.add_modifier(Modifier::BOLD)),
                    rects.heading,
                );
                if rects.count.width > 0 {
                    frame.render_widget(
                        Paragraph::new(count)
                            .style(styles.description)
                            .right_aligned(),
                        rects.count,
                    );
                }
                let top = rects.first_row;
                if discovery_indices.is_empty() {
                    frame.render_widget(
                        Paragraph::new("No matching candidates").style(styles.unavailable),
                        rects.viewport,
                    );
                }
                for (offset, row) in rects.row_rects.iter().copied().enumerate() {
                    let position = top.saturating_add(offset);
                    let Some(index) = discovery_indices.get(position).copied() else {
                        continue;
                    };
                    let item = &dialog.discovery.items[index];
                    let chosen = position == selected;
                    let marker = if chosen {
                        if ascii { "> " } else { "› " }
                    } else {
                        "  "
                    };
                    frame.render_widget(
                        Paragraph::new(truncated(
                            &format!("{marker}{}  {}", item.label, item.status),
                            usize::from(row.width),
                        ))
                        .style(if chosen {
                            styles.selection
                        } else {
                            styles.description
                        }),
                        row,
                    );
                    geometry.discovery_rows.push((row, position));
                }
                if let Some(bar) = rects.scrollbar {
                    render_scrollbar(
                        frame,
                        bar,
                        top,
                        total.saturating_sub(rects.row_rects.len().max(1)),
                        theme,
                        ascii,
                    );
                    surface.scrollable = true;
                }
            }

            let details_area = Rect::new(
                body.x,
                list_area.bottom(),
                body.width,
                body.bottom().saturating_sub(list_area.bottom()),
            );
            if details_area.height > 0 {
                let detail = discovery_indices
                    .get(
                        dialog
                            .discovery
                            .selected
                            .min(discovery_indices.len().saturating_sub(1)),
                    )
                    .and_then(|index| dialog.discovery.items.get(*index))
                    .map_or_else(
                        || "No candidate selected".to_owned(),
                        |item| format!("{} · {}", item.label, item.detail),
                    );
                // Long scan summaries must stay readable, so the pane wraps
                // and scrolls rather than truncating (§9). One authoritative
                // list plan over the wrapped lines: the same rects drive
                // paint, scrollbar and mouse.
                let wrapped = wrap_pane_lines(
                    details_area,
                    &format!("{detail}\n{}", dialog.discovery.status)
                        .lines()
                        .map(str::to_owned)
                        .collect::<Vec<_>>(),
                );
                let total = wrapped.len().max(1);
                let rects = plan_list(details_area, 0, total, None, dialog.discovery.status_scroll);
                // §8.7 panes have no border, so focus is signalled on the
                // heading. The body text keeps its readable role either way.
                frame.render_widget(
                    Paragraph::new("Details").style(if this.scroll_focused {
                        styles.shortcut.add_modifier(Modifier::BOLD)
                    } else {
                        styles.label.add_modifier(Modifier::BOLD)
                    }),
                    rects.heading,
                );
                let scroll_limit = total.saturating_sub(rects.row_rects.len());
                let scroll = rects.first_row.min(scroll_limit);
                for (offset, row) in rects.row_rects.iter().copied().enumerate() {
                    let Some(line) = wrapped.get(scroll.saturating_add(offset)) else {
                        continue;
                    };
                    frame.render_widget(
                        Paragraph::new(truncated(line, usize::from(row.width)))
                            .style(styles.description),
                        row,
                    );
                }
                if let Some(bar) = rects.scrollbar {
                    render_scrollbar(frame, bar, scroll, scroll_limit, theme, ascii);
                    surface.scrollable = true;
                }
                geometry.scroll = Some(details_area);
                {
                    let state = &mut this.state;
                    state.discovery.status_scroll_limit = scroll_limit;
                    state.discovery.status_scroll = scroll;
                }
            }
        }
        Mode::Ai => {
            let label_width = u16::try_from(UnicodeWidthStr::width("Describe")).unwrap_or(8);
            let editable = matches!(dialog.ai.stage, SourceAiStage::Input | SourceAiStage::Error);
            caret_cell = render_form_field(
                frame,
                Rect::new(body.x, body.y, body.width, 1),
                label_width,
                "Describe",
                &dialog.ai.instruction,
                "the source to follow, e.g. tail the nginx access log",
                editable && !dialog.controls_focused,
                cursor,
                theme,
            )
            .1;

            // The proposal review is a shared pane — heading, count,
            // indented viewport, scrollbar — like every other pane, so the
            // frame never resizes as the proposal arrives. Its bounded
            // scroll is what makes every field reachable at 54x16 before an
            // irreversible launch; the preview never executes.
            let preview_area = Rect::new(
                body.x,
                body.y.saturating_add(1).saturating_add(gap),
                body.width,
                body.height.saturating_sub(1).saturating_sub(gap),
            );
            if preview_area.height > 0 {
                let wrapped: Vec<String> = if review.is_empty() {
                    wrap_pane_lines(
                        preview_area,
                        &["A reviewed source definition appears here; nothing runs until you start it."
                            .to_owned()],
                    )
                } else {
                    wrap_pane_lines(preview_area, &review)
                };
                let total = wrapped.len().max(1);
                // One authoritative list plan over the wrapped lines: the
                // same rects drive paint, the counter, scrollbar and mouse.
                // Count first without the counter width so the visible count
                // cannot shift the viewport it counts.
                let probe = plan_list(preview_area, 0, total, None, dialog.ai.preview_scroll);
                let visible = probe.row_rects.len().max(1);
                let limit = total.saturating_sub(visible);
                let scroll = probe.first_row.min(limit);
                let counter = (limit > 0).then(|| {
                    format!(
                        "lines {}–{} of {}",
                        scroll.saturating_add(1),
                        scroll.saturating_add(visible).min(total),
                        total,
                    )
                });
                let rects = plan_list(
                    preview_area,
                    counter.as_deref().map_or(0, |text| {
                        u16::try_from(UnicodeWidthStr::width(text)).unwrap_or(0)
                    }),
                    total,
                    None,
                    scroll,
                );
                frame.render_widget(
                    Paragraph::new("Preview").style(if this.scroll_focused {
                        styles.shortcut.add_modifier(Modifier::BOLD)
                    } else {
                        styles.label.add_modifier(Modifier::BOLD)
                    }),
                    rects.heading,
                );
                if let Some(counter) = &counter
                    && rects.count.width > 0
                {
                    frame.render_widget(
                        Paragraph::new(counter.clone())
                            .style(styles.description)
                            .right_aligned(),
                        rects.count,
                    );
                }
                for (offset, row) in rects.row_rects.iter().copied().enumerate() {
                    let Some(line) = wrapped.get(scroll.saturating_add(offset)) else {
                        continue;
                    };
                    frame.render_widget(
                        Paragraph::new(truncated(line, usize::from(row.width)))
                            .style(styles.description),
                        row,
                    );
                }
                if let Some(bar) = rects.scrollbar {
                    render_scrollbar(frame, bar, scroll, limit, theme, ascii);
                    surface.scrollable = true;
                }
                geometry.scroll = (limit > 0).then_some(preview_area);
                {
                    let state = &mut this.state;
                    state.ai.preview_scroll_limit = limit;
                    state.ai.preview_scroll = scroll;
                }
            }
        }
    }

    render_message(frame, message_rect, state, &sentence, theme, ascii);
    render_help_text(frame, help_rect, help, theme);

    // §8.9/§8.10: one filled default (the primary action), painted from the
    // shared band plan so paint and mouse share rects. The focus ring stays
    // on the focused control; hidden actions keep original indices.
    let focused_action = action_controls
        .iter()
        .position(|(control, _)| *control == dialog.control);
    let action_row = ActionRow {
        labels: &action_labels,
        default: Some(0),
        destructive: &[],
        focused: focused_action,
    };
    for (index, rect) in action_geom.buttons.iter().copied() {
        let role = action_row.role(index);
        render_role_button(
            frame,
            rect,
            action_labels[index],
            role,
            focused_action == Some(index),
            theme,
        );
        if let Some((control, _)) = action_controls.get(index) {
            geometry.controls.push((rect, *control));
        }
    }
    this.record(geometry, caret_cell, surface)
}

/// Wrap logical lines for a list pane in `area`, accounting for the
/// scrollbar column the shared plan takes when the content overflows, so the
/// measure and the paint agree and no wrapped line is truncated on arrival.
/// Blank separator lines are preserved as blank rows. Hand-rolled here is
/// presentation-only folding, never query membership (AGENTS.md).
fn wrap_pane_lines(area: Rect, logical: &[String]) -> Vec<String> {
    use crate::dialog_layout::PANE_INDENT;
    let indent = PANE_INDENT.min(area.width);
    let probe_width = area.width.saturating_sub(indent).max(1);
    let wrap_at = |width: u16| -> Vec<String> {
        logical
            .iter()
            .flat_map(|line| {
                if line.trim().is_empty() {
                    vec![String::new()]
                } else {
                    wrap_sentence(line, usize::from(width.max(1)), usize::MAX)
                }
            })
            .collect()
    };
    let viewport_h = area.height.saturating_sub(u16::from(area.height > 1));
    let wrapped = wrap_at(probe_width);
    if wrapped.len() > usize::from(viewport_h) && probe_width > 1 {
        wrap_at(probe_width.saturating_sub(1))
    } else {
        wrapped
    }
}

/// Stable LongContent budgets for Add source (see `render_source`).
fn source_spec_for(area: Rect) -> DialogSpec {
    // Stable maximum first-row labels (longest primary plus Rescan) so
    // Manual/Discovery/Agent share one budget across Open/Request/Start
    // relabellings and no async arrival moves the tail. Display width via
    // button_width, capped at two rows.
    let max_labels = ["Start reviewed source", "Rescan"];
    let (policy_w, _) = policy_size(area, PresentationKind::LongContent);
    let estimate = policy_w.saturating_sub(4).max(1);
    let action_rows = stable_action_rows(estimate, &max_labels).clamp(1, 2);
    DialogSpec::new(PresentationKind::LongContent, 1, 3, 2, 0, action_rows)
}

fn source_ai_status(stage: SourceAiStage, theme: Theme) -> (&'static str, Style) {
    let styles = DialogStyles::new(theme);
    match stage {
        SourceAiStage::Input => ("Ready", styles.applied),
        SourceAiStage::Error => ("Error", styles.error),
        SourceAiStage::Proposal => ("Proposal", styles.applied),
        SourceAiStage::Preparing
        | SourceAiStage::Starting
        | SourceAiStage::Proposing
        | SourceAiStage::Applying => ("Updating", styles.pending),
    }
}
