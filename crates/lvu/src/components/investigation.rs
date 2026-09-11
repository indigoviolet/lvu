//! The Investigation 🧠 layer (`docs/dialog-system.md` §12.18), converted per
//! `docs/component-model.md` §6.3 step 12.
//!
//! Investigation is the second layer with a long-running remote stage and the
//! first whose stage is a *conversation*: a session outlives any single turn,
//! the transcript accumulates, and resuming a saved session picks the thread
//! back up. `Outbox<InvestigationRequest>` carries every turn out; `progress`,
//! `ready`, `push_event` and `append_output` bring the answers back, fenced by
//! the generation for the two that can arrive before a session exists and by
//! the session id for the two that cannot.
//!
//! The saved list is component state that survives a close: the slot is
//! permanent (§2.5), the scan that fills it is asynchronous, and `set_saved`
//! merges by durable identity so a session created while the scan ran cannot
//! be replaced by stale disk state.

use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    widgets::{Paragraph, Widget},
};
use std::collections::VecDeque;
use unicode_width::UnicodeWidthStr;

use crate::app::{
    InvestigationControl, InvestigationDialogState, InvestigationItem, InvestigationRequest,
    InvestigationStage, bounded_message, move_index, push_bounded_message,
};
use crate::command_palette::CommandId;
use crate::component::{Component, Ctx, Event, Outbox, Outcome, RenderCtx, Surface};
use crate::dialog_controls::{ActionRow, DialogStyles, render_role_button, stable_action_rows};
use crate::dialog_layout::{DialogSpec, PresentationKind, plan_list, policy_size, resolve_dialog};
use crate::text_edit::{EditCommand, EditPolicy, TextCursor, edit};
use crate::ui::{
    InputSurface, MessageState, render_help_text, render_message, render_responsive_frame,
    render_scrollbar, render_segmented_control, truncated, wrap_sentence,
};

/// The legacy `MAX_INVESTIGATION_REQUESTS`.
const MAX_INVESTIGATION_REQUESTS: usize = 4;
/// The legacy `MAX_SAVED_INVESTIGATIONS`.
const MAX_SAVED_INVESTIGATIONS: usize = 64;
/// The legacy `MAX_AI_PROMPT_BYTES`.
const MAX_QUESTION_BYTES: usize = 8 * 1024;
/// §12.18: the question is a multiline field, deep enough to see a follow-up
/// without scrolling it.
const QUESTION_ROWS: u16 = 3;
const LABEL_WIDTH: u16 = 11;

fn controls(dialog: &InvestigationDialogState) -> Vec<InvestigationControl> {
    use InvestigationControl as C;
    let mut controls = Vec::new();
    if !dialog.items.is_empty() {
        controls.extend([C::ModeNew, C::Saved]);
    }
    if matches!(
        dialog.stage,
        InvestigationStage::Input | InvestigationStage::Conversation | InvestigationStage::Error
    ) {
        controls.extend([C::Prompt, C::Submit]);
        if dialog.saved_mode && !dialog.items.is_empty() {
            controls.push(C::Open);
        }
        if dialog.investigation_id.is_some() || dialog.session_id.is_some() {
            controls.push(C::New);
        }
    }
    if dialog.review_scroll_limit > 0 {
        controls.push(C::More);
    }
    controls
}

/// Everything the Investigation layer draws that can be clicked.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InvestigationHit {
    Control(InvestigationControl),
    /// The transcript or saved-list viewport, for the wheel.
    Body,
}

/// Recorded by `render`, consumed by `hit()` (§5.1).
#[derive(Clone, Debug, Default)]
struct Geometry {
    controls: Vec<(Rect, InvestigationControl)>,
    body: Option<Rect>,
}

/// The multi-line Question field. Dialog-owned, so its caret lives here rather
/// than in `ctx.cursors` (§2.5).
#[derive(Clone, Debug, Default)]
struct QuestionField {
    value: String,
    cursor: TextCursor,
}

#[derive(Debug)]
pub struct InvestigationDialog {
    open: bool,
    state: Option<InvestigationDialogState>,
    question: QuestionField,
    /// Saved sessions, kept across opens: the slot is permanent and the scan
    /// that fills it runs whether or not the dialog is up.
    saved: Vec<InvestigationItem>,
    geometry: Geometry,
    surface: Surface,
    /// The agent defaults the shell holds; seeded by `App::configure_ai`.
    defaults: (String, String, String),
    pub outbox: Outbox<InvestigationRequest>,
}

impl Default for InvestigationDialog {
    fn default() -> Self {
        Self {
            open: false,
            state: None,
            question: QuestionField::default(),
            saved: Vec::new(),
            geometry: Geometry::default(),
            surface: Surface::default(),
            defaults: (
                "codex/gpt-5.6-sol".to_owned(),
                "full-access".to_owned(),
                "medium".to_owned(),
            ),
            outbox: Outbox::new(MAX_INVESTIGATION_REQUESTS),
        }
    }
}

fn contains(rect: Rect, point: (u16, u16)) -> bool {
    point.0 >= rect.x && point.0 < rect.right() && point.1 >= rect.y && point.1 < rect.bottom()
}

impl InvestigationDialog {
    pub fn configure(&mut self, provider: String, mode: String, thinking: String) {
        self.defaults = (provider, mode, thinking);
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Read-only view of the dialog, for tests. Nothing outside may mutate it.
    pub fn state(&self) -> Option<&InvestigationDialogState> {
        self.state.as_ref()
    }

    /// Whether a remote turn is in flight, for the activity indicator (§7.8).
    pub fn is_working(&self) -> bool {
        self.state.as_ref().is_some_and(|dialog| {
            matches!(
                dialog.stage,
                InvestigationStage::Snapshot
                    | InvestigationStage::StartingSession
                    | InvestigationStage::Resuming
                    | InvestigationStage::Sending
                    | InvestigationStage::Cancelling
            )
        })
    }

    /// The asynchronous disk scan's result. Merged by durable identity, because
    /// a session started while the scan ran must not be replaced by what the
    /// scan saw before it existed.
    pub fn set_saved(&mut self, items: Vec<InvestigationItem>) {
        for item in items {
            if !self.saved.iter().any(|existing| existing.id == item.id) {
                self.saved.push(item);
            }
        }
        self.saved.truncate(MAX_SAVED_INVESTIGATIONS);
        if let Some(dialog) = &mut self.state
            && matches!(dialog.stage, InvestigationStage::Input)
        {
            dialog.items = self.saved.clone();
            dialog.selected = dialog.selected.min(dialog.items.len().saturating_sub(1));
        }
    }

    /// A stage change from the agent runner, fenced by the generation the
    /// request carried.
    pub fn progress(
        &mut self,
        generation: u64,
        stage: InvestigationStage,
        progress: String,
        session_id: Option<String>,
        snapshot_dir: Option<String>,
        manifest_path: Option<String>,
    ) -> bool {
        let Some(dialog) = self
            .state
            .as_mut()
            .filter(|dialog| dialog.generation == generation)
        else {
            return false;
        };
        dialog.stage = stage;
        if matches!(
            stage,
            InvestigationStage::Conversation | InvestigationStage::Error
        ) {
            dialog.focus = InvestigationControl::Prompt;
        }
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

    /// The session exists and has been recorded on disk. Also the moment the
    /// saved list learns about it, newest first.
    pub fn ready(&mut self, generation: u64, item: InvestigationItem) -> bool {
        let Some(dialog) = self
            .state
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
            .saved
            .iter_mut()
            .find(|existing| existing.id == item.id)
        {
            *existing = item;
        } else {
            if self.saved.len() >= MAX_SAVED_INVESTIGATIONS {
                self.saved.pop();
            }
            self.saved.insert(0, item);
        }
        if let Some(dialog) = &mut self.state {
            dialog.items = self.saved.clone();
        }
        true
    }

    /// A turn ended, well or badly. Fenced by the session id rather than the
    /// generation: a resumed session's replies belong to the session, and the
    /// dialog may have been reopened around it.
    pub fn push_event(
        &mut self,
        session_id: &str,
        message: String,
        terminal: Result<(), String>,
    ) -> bool {
        let Some(dialog) = self
            .state
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
                dialog.focus = InvestigationControl::Prompt;
                dialog.progress = "turn complete; type a follow-up to continue".into();
            }
            Err(error) => {
                dialog.stage = InvestigationStage::Error;
                dialog.focus = InvestigationControl::Prompt;
                dialog.progress = error;
            }
        }
        true
    }

    /// Streamed output inside a turn.
    pub fn append_output(&mut self, session_id: &str, message: String) -> bool {
        let Some(dialog) = self
            .state
            .as_mut()
            .filter(|dialog| dialog.session_id.as_deref() == Some(session_id))
        else {
            return false;
        };
        push_bounded_message(&mut dialog.messages, bounded_message(message));
        true
    }

    fn editing(&self) -> bool {
        self.state.as_ref().is_some_and(|dialog| {
            dialog.focus == InvestigationControl::Prompt
                && matches!(
                    dialog.stage,
                    InvestigationStage::Input
                        | InvestigationStage::Conversation
                        | InvestigationStage::Error
                )
        })
    }

    fn edit_question(&mut self, command: EditCommand<'_>) -> Outcome {
        if !self.editing() {
            return Outcome::Ignored;
        }
        edit(
            &mut self.question.value,
            &mut self.question.cursor,
            command,
            EditPolicy {
                max_bytes: MAX_QUESTION_BYTES,
                multiline: true,
            },
        );
        if let Some(dialog) = &mut self.state {
            dialog.input = self.question.value.clone();
        }
        Outcome::Consumed
    }

    fn set_question(&mut self, value: String) {
        self.question = QuestionField {
            cursor: TextCursor {
                char_index: value.chars().count(),
            },
            value: value.clone(),
        };
        if let Some(dialog) = &mut self.state {
            dialog.input = value;
        }
    }

    fn move_control(&mut self, delta: i32) -> Outcome {
        if let Some(dialog) = &mut self.state {
            let available = controls(dialog);
            if !available.is_empty() {
                let current = available
                    .iter()
                    .position(|control| *control == dialog.focus)
                    .unwrap_or(0);
                dialog.focus = available[move_index(current, available.len(), delta)];
            }
        }
        Outcome::Consumed
    }

    fn focus_control(&mut self, control: InvestigationControl) {
        if let Some(dialog) = &mut self.state
            && controls(dialog).contains(&control)
        {
            dialog.focus = control;
        }
    }

    fn move_selection(&mut self, delta: i32) -> Outcome {
        if let Some(dialog) = &mut self.state
            && dialog.stage == InvestigationStage::Input
            && !dialog.items.is_empty()
        {
            dialog.selected = move_index(dialog.selected, dialog.items.len(), delta);
        }
        Outcome::Consumed
    }

    fn scroll(&mut self, delta: i32) -> Outcome {
        if let Some(dialog) = &mut self.state {
            dialog.review_scroll = (i32::from(dialog.review_scroll) + delta)
                .clamp(0, i32::from(dialog.review_scroll_limit))
                as u16;
        }
        Outcome::Consumed
    }

    /// A fresh snapshot on the same view: the conversation is dropped and the
    /// dialog returns to its Input stage without leaving the layer.
    fn start_over(&mut self) -> Outcome {
        if let Some(dialog) = &mut self.state {
            dialog.stage = InvestigationStage::Input;
            dialog.investigation_id = None;
            dialog.session_id = None;
            dialog.snapshot_dir = None;
            dialog.manifest_path = None;
            dialog.messages.clear();
            dialog.focus = InvestigationControl::Prompt;
            dialog.review_scroll = 0;
            dialog.review_scroll_limit = 0;
            dialog.progress = "enter a question for a new fixed snapshot".into();
        }
        self.set_question(String::new());
        Outcome::Consumed
    }

    fn activate(&mut self) -> Outcome {
        use InvestigationControl as C;
        match self.state.as_ref().map(|dialog| dialog.focus) {
            Some(C::Saved) => {
                if let Some(dialog) = &mut self.state {
                    dialog.saved_mode = true;
                }
                Outcome::Consumed
            }
            Some(C::ModeNew) => {
                if let Some(dialog) = &mut self.state {
                    dialog.saved_mode = false;
                }
                Outcome::Consumed
            }
            // Opening a saved investigation resumes it, which is what the
            // primary already does with an empty question.
            Some(C::Submit | C::Open) => self.submit(),
            Some(C::New) => self.start_over(),
            Some(C::Prompt | C::More) | None => Outcome::Consumed,
        }
    }

    fn submit(&mut self) -> Outcome {
        let pending = self.outbox.len();
        let Some(dialog) = &mut self.state else {
            return Outcome::Consumed;
        };
        if pending >= MAX_INVESTIGATION_REQUESTS {
            dialog.stage = InvestigationStage::Error;
            dialog.progress = "investigation request queue is full".into();
            return Outcome::Consumed;
        }
        let request = match dialog.stage {
            InvestigationStage::Input if dialog.input.trim().is_empty() => {
                let Some(item) = dialog.items.get(dialog.selected).cloned() else {
                    dialog.stage = InvestigationStage::Error;
                    dialog.progress = "enter a question to start an investigation".into();
                    return Outcome::Consumed;
                };
                dialog.stage = InvestigationStage::Resuming;
                dialog.progress = "resuming selected local agent session".into();
                // Back to the conversation: the saved list answers "which one",
                // and once that is answered the transcript is what the user
                // came for. Staying on the list left a resumed session's
                // replies with nowhere on screen to appear.
                dialog.saved_mode = false;
                InvestigationRequest::Resume {
                    generation: dialog.generation,
                    item,
                }
            }
            InvestigationStage::Input => {
                let question = std::mem::take(&mut dialog.input);
                dialog.stage = InvestigationStage::Snapshot;
                dialog.progress = "freezing applied view snapshot".into();
                push_bounded_message(&mut dialog.messages, format!("You: {question}"));
                let (provider, mode, thinking) = self.defaults.clone();
                let request = InvestigationRequest::Start {
                    generation: dialog.generation,
                    view_id: dialog.view_id.clone(),
                    definition_revision: dialog.definition_revision,
                    question,
                    provider,
                    mode,
                    thinking,
                };
                self.question = QuestionField::default();
                request
            }
            InvestigationStage::Conversation | InvestigationStage::Error => {
                let Some(session_id) = dialog.session_id.clone() else {
                    dialog.stage = InvestigationStage::Error;
                    dialog.progress = "session is unavailable; start or resume again".into();
                    return Outcome::Consumed;
                };
                if dialog.input.trim().is_empty() {
                    dialog.progress = "enter a follow-up question".into();
                    return Outcome::Consumed;
                }
                let prompt = std::mem::take(&mut dialog.input);
                push_bounded_message(&mut dialog.messages, format!("You: {prompt}"));
                dialog.stage = InvestigationStage::Sending;
                dialog.progress = "sending follow-up to local agent".into();
                let request = InvestigationRequest::Send {
                    generation: dialog.generation,
                    session_id,
                    prompt,
                };
                self.question = QuestionField::default();
                request
            }
            InvestigationStage::Snapshot
            | InvestigationStage::StartingSession
            | InvestigationStage::Resuming
            | InvestigationStage::Sending
            | InvestigationStage::Cancelling => return Outcome::Consumed,
        };
        let _ = self.outbox.push(request);
        Outcome::Consumed
    }

    fn dismiss(&mut self) -> Outcome {
        self.open = false;
        // A turn in flight is cancelled on the way out; an idle or failed
        // dialog has nothing to cancel.
        let cancel = self
            .state
            .take()
            .filter(|dialog| {
                !matches!(
                    dialog.stage,
                    InvestigationStage::Input | InvestigationStage::Error
                )
            })
            .map(|dialog| dialog.generation);
        self.question = QuestionField::default();
        if let Some(generation) = cancel {
            let _ = self
                .outbox
                .push(InvestigationRequest::Cancel { generation });
        }
        Outcome::Close
    }

    fn key(&mut self, key: KeyEvent) -> Outcome {
        use InvestigationControl as C;
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return Outcome::Consumed;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            let command = match key.code {
                KeyCode::Char('a') => Some(EditCommand::StartOfLine),
                KeyCode::Char('e') => Some(EditCommand::EndOfLine),
                KeyCode::Char('k') => Some(EditCommand::KillToEndOfLine),
                _ => None,
            };
            if let Some(command) = command {
                return self.edit_question(command);
            }
            return Outcome::Ignored;
        }
        if key.modifiers.contains(KeyModifiers::ALT) {
            return match key.code {
                KeyCode::Char('n') => self.start_over(),
                _ => Outcome::Ignored,
            };
        }
        // The panes take the arrows while they hold focus, as `ScrollInvestigation`
        // and `MoveInvestigation` did.
        if let Some(focus) = self.state.as_ref().map(|dialog| dialog.focus) {
            match (focus, key.code) {
                (C::More, KeyCode::Up) => return self.scroll(-1),
                (C::More, KeyCode::Down) => return self.scroll(1),
                (C::Saved | C::Open, KeyCode::Up) => return self.move_selection(-1),
                (C::Saved | C::Open, KeyCode::Down) => return self.move_selection(1),
                _ => {}
            }
        }
        // While the Question field is editing, the caret owns Left/Right/Up/Down
        // and Enter inserts a newline.
        if self.editing() {
            match key.code {
                KeyCode::Left => return self.edit_question(EditCommand::MoveLeft),
                KeyCode::Right => return self.edit_question(EditCommand::MoveRight),
                KeyCode::Up => return self.edit_question(EditCommand::MoveUp),
                KeyCode::Down => return self.edit_question(EditCommand::MoveDown),
                KeyCode::Enter => return self.edit_question(EditCommand::Insert("\n")),
                KeyCode::Backspace => return self.edit_question(EditCommand::Backspace),
                KeyCode::Char(character) => {
                    let mut buffer = [0u8; 4];
                    return self
                        .edit_question(EditCommand::Insert(character.encode_utf8(&mut buffer)));
                }
                _ => {}
            }
        }
        match key.code {
            KeyCode::Tab | KeyCode::Down => self.move_control(1),
            KeyCode::BackTab | KeyCode::Up => self.move_control(-1),
            KeyCode::Enter | KeyCode::Char(' ') => self.activate(),
            KeyCode::Backspace => self.edit_question(EditCommand::Backspace),
            _ => Outcome::Ignored,
        }
    }

    fn mouse(&mut self, kind: MouseEventKind, hit: Option<InvestigationHit>) -> Outcome {
        use InvestigationControl as C;
        match (kind, hit) {
            (MouseEventKind::Down(MouseButton::Left), Some(InvestigationHit::Control(control))) => {
                self.focus_control(control);
                if matches!(control, C::Submit | C::New) {
                    return self.activate();
                }
                Outcome::Consumed
            }
            (MouseEventKind::ScrollUp, Some(InvestigationHit::Body)) => self.scroll(-1),
            (MouseEventKind::ScrollDown, Some(InvestigationHit::Body)) => self.scroll(1),
            _ => Outcome::Consumed,
        }
    }
}

impl Component for InvestigationDialog {
    type Hit = InvestigationHit;
    type Open = ();

    fn open(&mut self, _params: (), ctx: &mut Ctx<'_>) {
        let Some(view_id) = ctx.views.active_id().map(str::to_owned) else {
            return;
        };
        let generation = self.outbox.next_generation();
        let definition_revision = ctx.views.definition_revision(&view_id).unwrap_or_default();
        self.state = Some(InvestigationDialogState {
            generation,
            definition_revision,
            view_id,
            stage: InvestigationStage::Input,
            focus: InvestigationControl::Prompt,
            input: String::new(),
            progress: if self.saved.is_empty() {
                "enter a question for a new fixed snapshot".into()
            } else {
                "type a new question, or leave blank to resume the selected investigation".into()
            },
            selected: 0,
            items: self.saved.clone(),
            investigation_id: None,
            session_id: None,
            snapshot_dir: None,
            manifest_path: None,
            messages: VecDeque::new(),
            review_scroll: 0,
            review_scroll_limit: 0,
            // With saved investigations and nothing in flight, the saved list
            // is what there is to act on.
            saved_mode: !self.saved.is_empty(),
        });
        self.question = QuestionField::default();
        self.geometry = Geometry::default();
        self.open = true;
    }

    fn handle(&mut self, event: Event<InvestigationHit>, _ctx: &mut Ctx<'_>) -> Outcome {
        match event {
            Event::Key(key) => self.key(key),
            Event::Paste(text) => self.edit_question(EditCommand::Insert(&text)),
            Event::Mouse { kind, hit, .. } => self.mouse(kind, hit),
            Event::Dismiss => self.dismiss(),
            Event::Command(CommandId::NewInvestigation) => self.start_over(),
            Event::Command(CommandId::ResumeInvestigation | CommandId::InvestigationFollowup) => {
                self.submit()
            }
            Event::Command(_) | Event::View(_) | Event::Resize => Outcome::Ignored,
        }
    }

    /// §4.3: three catalog rows the layer takes over while it can serve them.
    /// Their availability used to be computed in `terminal.rs` from a peek at
    /// `App::investigation_dialog`; the layer answers for itself now, which is
    /// the point of the seam.
    fn commands(&self, _views: &crate::app::Views) -> Vec<crate::component::CommandEntry> {
        use crate::component::{CommandEntry, CommandSpec};
        let dialog = self.state.as_ref();
        let can_resume = dialog.is_some_and(|dialog| {
            dialog.stage == InvestigationStage::Input
                && dialog.input.trim().is_empty()
                && dialog.items.get(dialog.selected).is_some()
        });
        let can_follow_up = dialog.is_some_and(|dialog| {
            matches!(
                dialog.stage,
                InvestigationStage::Conversation | InvestigationStage::Error
            ) && dialog.session_id.is_some()
                && !dialog.input.trim().is_empty()
        });
        vec![
            CommandEntry {
                spec: CommandSpec {
                    id: CommandId::NewInvestigation,
                    name: "New investigation",
                    description: "Start a new investigation draft",
                    category: "Agent",
                    aliases: &["question", "conversation"],
                    shortcut: dialog.is_some().then_some("Alt-N"),
                },
                unavailable_reason: dialog.is_none().then_some("open Investigations first"),
            },
            CommandEntry {
                spec: CommandSpec {
                    id: CommandId::ResumeInvestigation,
                    name: "Resume selected investigation",
                    description: "Resume the selected saved session",
                    category: "Agent",
                    aliases: &["continue session", "history"],
                    shortcut: None,
                },
                unavailable_reason: (!can_resume)
                    .then_some("open Investigations and select a saved session first"),
            },
            CommandEntry {
                spec: CommandSpec {
                    id: CommandId::InvestigationFollowup,
                    name: "Send investigation follow-up",
                    description: "Send the current prompt to the active session",
                    category: "Agent",
                    aliases: &["reply", "continue conversation"],
                    shortcut: None,
                },
                unavailable_reason: (!can_follow_up)
                    .then_some("open an active investigation and enter a follow-up first"),
            },
        ]
    }

    /// Geometry is the last render's, but `text_focus` is derived from state:
    /// a burst that opens the layer and types into it reaches `handle` before
    /// any frame exists, and a `q` in the question must be a character
    /// (§ "`Surface::text_focus` is derived, not recorded").
    fn surface(&self) -> Surface {
        Surface {
            text_focus: self.editing(),
            ..self.surface
        }
    }

    fn hit(&self, point: (u16, u16)) -> Option<InvestigationHit> {
        self.geometry
            .controls
            .iter()
            .find_map(|(rect, control)| {
                contains(*rect, point).then_some(InvestigationHit::Control(*control))
            })
            .or_else(|| {
                self.geometry
                    .body
                    .filter(|rect| contains(*rect, point))
                    .map(|_| InvestigationHit::Body)
            })
    }

    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &RenderCtx<'_>) -> Surface {
        let mut hits = Vec::new();
        let mut body = None;
        let cursor = self.editing().then_some(self.question.cursor.char_index);
        let Some(state) = &mut self.state else {
            self.surface = Surface::default();
            return self.surface;
        };
        let surface = draw(state, frame, area, ctx, cursor, &mut hits, &mut body);
        self.geometry = Geometry {
            controls: hits,
            body,
        };
        self.surface = surface;
        surface
    }
}

/// The pane's heading, its count and its lines. Shared by sizing and drawing so
/// the rows the dialog asks for are the rows it then lays out.
fn pane_content(
    dialog: &InvestigationDialogState,
    saved_mode: bool,
    help: &str,
    ascii: bool,
) -> (&'static str, String, Vec<String>) {
    if saved_mode {
        return (
            "Saved investigations",
            format!("{} saved", dialog.items.len()),
            dialog
                .items
                .iter()
                .enumerate()
                .map(|(index, item)| {
                    let marker = if index == dialog.selected {
                        if ascii { "> " } else { "› " }
                    } else {
                        "  "
                    };
                    format!("{marker}{} — {}", item.session_id, item.question)
                })
                .collect(),
        );
    }
    (
        "Transcript",
        format!(
            "{} message{}",
            dialog.messages.len(),
            if dialog.messages.len() == 1 { "" } else { "s" }
        ),
        if dialog.messages.is_empty() {
            vec![help.to_owned()]
        } else {
            dialog.messages.iter().cloned().collect()
        },
    )
}

/// Stable LongContent budgets for Investigation (see `draw`). The header
/// band is always reserved so the arrival of saved sessions never moves the
/// frame; the message keeps its two-row maximum and the actions keep the
/// stable width budget across Start/Resume/Send/Open/New-snapshot states.
fn investigation_spec_for(area: Rect) -> DialogSpec {
    // Stable maximum action labels across stages so the band never moves as
    // the primary relabels or Open/New-snapshot appear. Display width via
    // button_width, capped at two rows.
    let max_labels = ["New snapshot", "Resume", "Open"];
    let (policy_w, _) = policy_size(area, PresentationKind::LongContent);
    let estimate = policy_w.saturating_sub(4).max(1);
    let action_rows = stable_action_rows(estimate, &max_labels).clamp(1, 2);
    // The help budget covers the longer of the two help sentences so a
    // session binding never moves the tail. The body minimum holds the
    // compacted question, provenance and a two-row transcript viewport, so
    // height pressure sheds the help band (first in the degradation order)
    // before it can starve the pane.
    let help_rows = [
        "Follow-ups reuse the fixed snapshot this session started from.",
        "Start an investigation on a fixed snapshot of this view; follow-ups reuse it.",
    ]
    .iter()
    .map(|sentence| {
        wrap_sentence(sentence, usize::from(estimate), 2)
            .len()
            .min(2) as u16
    })
    .max()
    .unwrap_or(0);
    DialogSpec::new(
        PresentationKind::LongContent,
        1,
        5,
        2,
        help_rows,
        action_rows,
    )
}

fn title_for(ascii: bool) -> String {
    if ascii {
        "Investigation Agent".to_owned()
    } else {
        "Investigation 🧠".to_owned()
    }
}

/// §12.18 Investigation 🧠 on the shared anatomy: title, the `New │ Saved`
/// segmented header, the Question field with provenance and the transcript
/// pane, one message row, help, and the actions last.
///
/// New/Saved, provenance, transcript and saved list share one responsive
/// geometry that never resizes as content grows: the frame is policy-only
/// and the transcript/saved list scroll inside their pane. Geometry the
/// dialog used to publish into the global `HitRegions` is handed back to the
/// component instead (§5.1).
fn draw(
    state: &mut InvestigationDialogState,
    frame: &mut Frame<'_>,
    area: Rect,
    ctx: &RenderCtx<'_>,
    cursor: Option<usize>,
    controls_hit: &mut Vec<(Rect, InvestigationControl)>,
    body_hit: &mut Option<Rect>,
) -> Surface {
    use crate::app::InvestigationControl as C;
    use crate::app::InvestigationStage as Stage;
    let theme = ctx.theme;
    let styles = DialogStyles::new(theme);
    let ascii = ctx.ascii;
    let mut caret = None;
    let dialog = state.clone();

    let editable = matches!(
        dialog.stage,
        Stage::Input | Stage::Conversation | Stage::Error
    );
    let saved_mode = dialog.saved_mode && !dialog.items.is_empty();

    // §7.4: the state word, and the progress line the agent is producing.
    let (message_state, sentence) = match dialog.stage {
        Stage::Input | Stage::Conversation => (MessageState::Ready, dialog.progress.clone()),
        Stage::Error => (MessageState::Error, dialog.progress.clone()),
        _ => (MessageState::Pending, dialog.progress.clone()),
    };
    let help = if dialog.session_id.is_some() {
        "Follow-ups reuse the fixed snapshot this session started from."
    } else {
        "Start an investigation on a fixed snapshot of this view; follow-ups reuse it."
    };
    // An empty transcript already shows this sentence as its own empty state;
    // §11 does not want it twice.
    let help_row = if dialog.messages.is_empty() && !dialog.saved_mode {
        ""
    } else {
        help
    };

    // Provenance: which session is answering, and the snapshot it is bound to.
    // One line, because it identifies the conversation rather than joining it.
    let provenance = match (&dialog.session_id, &dialog.snapshot_dir) {
        (Some(session), Some(snapshot)) => {
            Some(format!("Session: {session} · Snapshot: {snapshot}"))
        }
        (Some(session), None) => Some(format!("Session: {session}")),
        (None, Some(snapshot)) => Some(format!("Snapshot: {snapshot}")),
        (None, None) => None,
    };

    let primary = if dialog.stage == Stage::Conversation {
        "Send"
    } else if dialog.input.trim().is_empty() && !dialog.items.is_empty() {
        "Resume"
    } else {
        "Start"
    };
    let mut actions: Vec<(&str, C)> = Vec::new();
    if editable {
        actions.push((primary, C::Submit));
        if saved_mode {
            actions.push(("Open", C::Open));
        }
        if dialog.investigation_id.is_some() || dialog.session_id.is_some() {
            actions.push(("New snapshot", C::New));
        }
    }
    let action_labels = actions.iter().map(|(label, _)| *label).collect::<Vec<_>>();

    // Stable LongContent budgets: outer size is policy-only, never transcript
    // length or saved-list size. The header band is always reserved so
    // New/Saved arrival never moves the frame; the message keeps its two-row
    // maximum and the actions keep the stable width budget. Hand-rolled row
    // assignment below is presentation-only folding, never query membership
    // (AGENTS.md).
    let spec = investigation_spec_for(area);
    let Ok(resolved) = resolve_dialog(area, &spec, 1, &action_labels, Some(0), None) else {
        // Below the floor the tiny fallback owns the frame; stay open with
        // nothing drawn, as the palette does. The transcript is not
        // focusable with no viewport to scroll.
        state.review_scroll_limit = 0;
        if state.focus == C::More {
            state.focus = if editable { C::Submit } else { C::Prompt };
        }
        return Surface {
            popup: Rect::default(),
            interior: Rect::default(),
            caret: None,
            scrollable: false,
            text_focus: false,
        };
    };
    // Shared frame so geometry and paint share one definition; compactness
    // comes from the geometry, never recomputed from the frame.
    render_responsive_frame(frame, &resolved, &title_for(ascii), ctx.active, theme);
    let mut surface = Surface {
        popup: resolved.frame,
        interior: resolved.interior,
        caret: None,
        // Derived below from real pane overflow, never blanket true.
        scrollable: false,
        text_focus: editable && dialog.focus == C::Prompt,
    };
    let body = resolved.body.viewport;
    if body.width == 0 || body.height == 0 {
        return surface;
    }
    let message_rect = resolved.message;
    let help_rect = resolved.help;
    let action_geom = resolved.actions.clone();
    let roomy = !resolved.compact;

    let segmented = !dialog.items.is_empty();
    if segmented && resolved.header.height > 0 {
        let labels = ["New", "Saved"];
        let active = usize::from(saved_mode);
        let focused = match dialog.focus {
            C::ModeNew => Some(0),
            C::Saved => Some(1),
            _ => None,
        };
        for (index, rect) in
            render_segmented_control(frame, resolved.header, &labels, active, focused, theme)
                .into_iter()
                .enumerate()
        {
            controls_hit.push((rect, if index == 0 { C::ModeNew } else { C::Saved }));
        }
    }

    // The Question field and provenance stay fixed at the top of the body
    // while the transcript/saved pane scrolls beneath them, so the input and
    // every action stay reachable however long the conversation grows. The
    // pane keeps at least its heading and two viewport rows while there is
    // a body to split — a one-row scrollbar never shows its `▼` — and the
    // question field compacts first (its caret line stays visible through
    // its own scroll), so review content stays reachable at compact sizes.
    const PANE_MIN: u16 = 3;
    let provenance_rows = u16::from(provenance.is_some()).min(body.height.saturating_sub(1));
    let question_rows = QUESTION_ROWS.min(body.height).min(
        body.height
            .saturating_sub(provenance_rows)
            .saturating_sub(PANE_MIN)
            .max(1),
    );
    let gap = u16::from(roomy).min(
        body.height
            .saturating_sub(question_rows)
            .saturating_sub(provenance_rows)
            .saturating_sub(PANE_MIN),
    );
    let fixed_end = question_rows
        .saturating_add(provenance_rows)
        .saturating_add(gap);
    let pane_area = Rect::new(
        body.x,
        body.y.saturating_add(fixed_end),
        body.width,
        body.height.saturating_sub(fixed_end),
    );

    let question = Rect::new(
        body.x.saturating_add(LABEL_WIDTH),
        body.y,
        body.width.saturating_sub(LABEL_WIDTH),
        question_rows,
    );
    frame.render_widget(
        Paragraph::new("Question").style(if dialog.focus == C::Prompt {
            styles.shortcut
        } else {
            styles.label
        }),
        Rect::new(body.x, body.y, LABEL_WIDTH, 1),
    );
    if editable && question.width > 0 {
        InputSurface {
            style: styles.input,
        }
        .render(question, frame.buffer_mut());
        controls_hit.push((question, C::Prompt));
    }
    // §8.1: a multiline field shows the line the caret is on, and the caret
    // sits where the next character will land.
    let wrapped = crate::text_edit::wrapped_text(&dialog.input, usize::from(question.width.max(1)));
    let (cursor_row, cursor_col) = cursor.map_or((0, 0), |cursor| {
        let mut value = crate::text_edit::TextCursor { char_index: cursor };
        crate::text_edit::wrapped_cursor(
            &dialog.input,
            &mut value,
            usize::from(question.width.max(1)),
        )
    });
    let top = cursor_row
        .saturating_add(1)
        .saturating_sub(usize::from(question.height.max(1)));
    frame.render_widget(
        Paragraph::new(wrapped.lines.get(top..).unwrap_or(&[]).join("\n")).style(if editable {
            styles.input
        } else {
            styles.description
        }),
        question,
    );
    if editable && dialog.focus == C::Prompt && question.width > 0 && question.height > 0 {
        let x = question.x + cursor_col.min(usize::from(question.width - 1)) as u16;
        let y = question.y
            + cursor_row
                .saturating_sub(top)
                .min(usize::from(question.height - 1)) as u16;
        frame.buffer_mut()[(x, y)].set_style(Style::default().bg(theme.cursor).fg(theme.input_fg));
        frame.set_cursor_position((x, y));
        caret = Some((x, y));
    }

    if let Some(provenance) = &provenance {
        let y = body.y.saturating_add(question_rows);
        if provenance_rows > 0 {
            frame.render_widget(
                Paragraph::new(truncated(provenance, usize::from(body.width)))
                    .style(styles.description),
                Rect::new(body.x, y, body.width, 1),
            );
        }
    }

    let (heading, count, lines) = pane_content(&dialog, saved_mode, help, ascii);
    // One authoritative list plan over the pane rows: the same rects drive
    // paint, selection, scrollbar and mouse. A transcript line is prose and
    // wraps; a saved row is a row and does not. The wrap accounts for the
    // scrollbar column the plan takes on overflow, measured again once it is
    // known, so paint never truncates a line the measure claimed fits.
    let viewport_h = pane_area
        .height
        .saturating_sub(u16::from(pane_area.height > 1));
    let wrap_pane = |text_width: usize| -> Vec<(String, bool)> {
        lines
            .iter()
            .enumerate()
            .flat_map(|(index, line)| {
                let selected = saved_mode && index == dialog.selected;
                if saved_mode {
                    vec![(line.clone(), selected)]
                } else {
                    wrap_sentence(line, text_width.max(1), usize::MAX)
                        .into_iter()
                        .map(|part| (part, false))
                        .collect()
                }
            })
            .collect()
    };
    let pane_probe_width = usize::from(
        pane_area
            .width
            .saturating_sub(crate::dialog_layout::PANE_INDENT),
    )
    .max(1);
    let mut wrapped_lines = wrap_pane(pane_probe_width);
    if wrapped_lines.len() > usize::from(viewport_h) && pane_probe_width > 1 {
        wrapped_lines = wrap_pane(pane_probe_width.saturating_sub(1).max(1));
    }
    let total = wrapped_lines.len().max(1);
    let rects = plan_list(
        pane_area,
        u16::try_from(UnicodeWidthStr::width(count.as_str())).unwrap_or(0),
        total,
        None,
        usize::from(dialog.review_scroll),
    );
    if rects.heading.height > 0 {
        frame.render_widget(
            Paragraph::new(heading).style(if dialog.focus == C::More {
                styles.shortcut.add_modifier(Modifier::BOLD)
            } else {
                styles.label.add_modifier(Modifier::BOLD)
            }),
            rects.heading,
        );
        if rects.count.width > 0 {
            frame.render_widget(Paragraph::new(count).style(styles.description), rects.count);
        }
    }
    let limit = total.saturating_sub(rects.row_rects.len());
    let scroll = rects.first_row.min(limit);
    for (offset, (line, selected)) in wrapped_lines
        .iter()
        .skip(scroll)
        .take(rects.row_rects.len().max(1))
        .enumerate()
    {
        let Some(row) = rects.row_rects.get(offset).copied() else {
            continue;
        };
        frame.render_widget(
            Paragraph::new(truncated(line, usize::from(row.width))).style(if *selected {
                styles.selection
            } else {
                styles.description
            }),
            row,
        );
    }
    if let Some(bar) = rects.scrollbar {
        render_scrollbar(frame, bar, scroll, limit, theme, ascii);
        surface.scrollable = true;
    }
    if pane_area.height > 0 {
        *body_hit = Some(rects.viewport);
    }

    // Geometry settled during the draw is state: the scroll limit, and the
    // focus the render normalised.
    state.review_scroll_limit = u16::try_from(limit).unwrap_or(u16::MAX);
    state.review_scroll = u16::try_from(scroll).unwrap_or(0);
    if state.focus == C::More && limit == 0 {
        state.focus = if editable { C::Submit } else { C::Prompt };
    }

    render_message(frame, message_rect, message_state, &sentence, theme, ascii);
    render_help_text(frame, help_rect, help_row, theme);
    // §3: the actions come last, after every field they act on. Painted from
    // the shared band plan so paint and mouse share rects; one filled
    // default, never a destructive one.
    let focused = actions
        .iter()
        .position(|(_, control)| *control == dialog.focus);
    let action_row = ActionRow {
        labels: &action_labels,
        default: Some(0),
        destructive: &[],
        focused,
    };
    for (index, rect) in action_geom.buttons.iter().copied() {
        let role = action_row.role(index);
        render_role_button(
            frame,
            rect,
            action_labels[index],
            role,
            focused == Some(index),
            theme,
        );
        if let Some((_, control)) = actions.get(index) {
            controls_hit.push((rect, *control));
        }
    }
    Surface {
        popup: surface.popup,
        interior: surface.interior,
        caret,
        scrollable: surface.scrollable,
        text_focus: editable && dialog.focus == C::Prompt,
    }
}
