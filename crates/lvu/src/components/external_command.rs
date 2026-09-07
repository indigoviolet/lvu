//! Command enrichment — the External command dialog — converted per
//! `docs/component-model.md` §6.3 step 13.
//!
//! §6.3 and `dialog-system.md` §10 both called this a *child* of Enrichment.
//! It is not one, and making it one would have been a visible delta rather
//! than a conversion (§6.5): `ui::render_command_enrichment` drew no parent
//! behind it, and its Escape returned the user to the log, not to the step
//! list. So Enrichment reaches it with `Outcome::Replace` and it closes to the
//! base, which is exactly what `Focus::CommandEnrichment` → `Focus::Logs` did.
//!
//! Everything else is a move. The dialog state, the request outbox, the two
//! pending-generation maps and the four completion paths that fence against
//! them all come across intact, so `Saving results`, the rest of the run-state
//! vocabulary and the results pane (W11, `b373109`) are byte-identical.

use std::collections::{HashMap, HashSet, VecDeque};

use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    widgets::{Paragraph, Widget},
};
use unicode_width::UnicodeWidthStr;

use crate::app::{
    CommandEnrichmentControl as Control, CommandEnrichmentDialogState, CommandEnrichmentField,
    CommandEnrichmentField as Field, CommandEnrichmentRequest, CommandEnrichmentReview,
    CommandEnrichmentRunState as RunState, CommandEnrichmentStage, MAX_COMMAND_FIELD_BYTES,
    MAX_COMMAND_REQUESTS, Views, command_candidate, command_draft_field_mut, command_stage_draft,
};
use crate::command_palette::CommandId;
use crate::component::{
    CommandEntry, CommandSpec, Component, Ctx, Event, Outcome, RenderCtx, Surface,
};
use crate::dialog_controls::{ActionRow, DialogStyles};
use crate::text_edit::{EditCommand, EditPolicy, TextTarget, edit};
use crate::ui::{
    InputSurface, MessageState, dialog_frame_regions, help_rows, input_tail, message_rows,
    packed_button_rows, place_input_cursor_at, render_actions, render_help_text, render_message,
    render_scrollbar, truncated, wrap_sentence,
};

/// The selected field's value, for the caret clamp.
fn command_field(dialog: &CommandEnrichmentDialogState) -> &str {
    match dialog.selected_field {
        Field::Program => &dialog.program,
        Field::Arguments => &dialog.arguments,
        Field::Cwd => &dialog.cwd,
        Field::Environment => &dialog.environment,
    }
}

/// §12.6: one label column for the four fields.
const COMMAND_LABEL_WIDTH: u16 = 14;
/// The painted rect of a multi-line field grows with its lines, up to this cap.
const COMMAND_MULTILINE_ROWS: usize = 3;

/// Everything this dialog draws that can be clicked.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommandHit {
    Control(Control),
    /// The results-and-review pane.
    Notes,
    Body,
}

/// Recorded by `render`, consumed by `hit()` (§5.1). This was
/// `HitRegions::command_enrichment_controls` plus `dialog_scroll`.
#[derive(Clone, Debug, Default)]
struct CommandGeometry {
    controls: Vec<(Rect, Control)>,
    notes: Option<Rect>,
}

#[derive(Debug, Default)]
pub struct ExternalCommandDialog {
    state: Option<CommandEnrichmentDialogState>,
    /// Tab has handed the arrows to the results pane. This was
    /// `App::dialog_scroll_focused`.
    scroll_focused: bool,
    scroll: usize,
    scroll_limit: usize,
    /// §8: one bounded queue `lvu-app` drains. The fence is the generation the
    /// dialog carries; a reply for any other generation is dropped.
    requests: VecDeque<CommandEnrichmentRequest>,
    /// Requests `lvu-app` has taken but not answered, so the queue-full check
    /// counts work in flight rather than only work not yet handed over.
    pending_saves: HashMap<u64, (String, u64)>,
    pending_runs: HashMap<u64, (String, u64)>,
    next_generation: u64,
    geometry: CommandGeometry,
    surface: Surface,
}

impl ExternalCommandDialog {
    pub fn is_open(&self) -> bool {
        self.state.is_some()
    }

    pub fn state(&self) -> Option<&CommandEnrichmentDialogState> {
        self.state.as_ref()
    }

    #[cfg(test)]
    pub(crate) fn set_run_state(&mut self, run_state: RunState) {
        if let Some(dialog) = &mut self.state {
            dialog.run_state = run_state;
        }
    }

    #[cfg(test)]
    pub(crate) fn forget(&mut self) {
        self.state = None;
    }

    #[cfg(test)]
    pub(crate) fn note_pending_run(&mut self, generation: u64, view_id: &str, revision: u64) {
        self.pending_runs
            .insert(generation, (view_id.to_owned(), revision));
    }

    #[cfg(test)]
    pub(crate) fn clear_pending_runs(&mut self) {
        self.pending_runs.clear();
    }

    /// Palette availability is whether the dialog is open, and the catalog is
    /// built without a `Ctx`; this is how a test puts the layer in that state.
    pub fn open_for_test(&mut self) {
        self.state = Some(CommandEnrichmentDialogState {
            generation: 0,
            view_id: String::new(),
            base_definition_revision: 0,
            selected_field: Field::Program,
            selected_control: Control::Field,
            program: String::new(),
            arguments: String::new(),
            cwd: String::new(),
            environment: String::new(),
            accepted: None,
            error: None,
            run_state: RunState::Unrun,
            run_status: String::new(),
            review: None,
        });
    }

    pub fn control_rects(&self) -> &[(Rect, Control)] {
        &self.geometry.controls
    }

    pub fn scroll(&self) -> usize {
        self.scroll
    }

    /// The results pane when it actually overflows. `None` means the layer
    /// claims no scroll affordance (§9).
    pub fn notes_rect(&self) -> Option<Rect> {
        self.geometry.notes
    }

    pub fn take_requests(&mut self) -> Vec<CommandEnrichmentRequest> {
        self.requests.drain(..).collect()
    }

    /// Whether a command operation is still settling, which the shell asks
    /// before it decides the workspace is idle.
    pub(crate) fn work_pending(&self) -> bool {
        !self.pending_saves.is_empty()
            || !self.pending_runs.is_empty()
            || self.state.as_ref().is_some_and(|dialog| {
                matches!(
                    dialog.run_state,
                    RunState::Saving
                        | RunState::Preparing
                        | RunState::Running
                        | RunState::SavingResults
                )
            })
    }

    /// A view that forked under the dialog keeps it pointed at the fork.
    pub(crate) fn retarget_view(&mut self, origin: &str, candidate: &str) {
        if let Some(dialog) = &mut self.state
            && dialog.view_id == origin
        {
            dialog.view_id = candidate.to_owned();
        }
    }

    fn request_count(&self) -> usize {
        let mut generations = self.pending_saves.keys().copied().collect::<HashSet<_>>();
        generations.extend(self.pending_runs.keys().copied());
        generations.extend(self.requests.iter().map(|request| match request {
            CommandEnrichmentRequest::Save { generation, .. }
            | CommandEnrichmentRequest::PrepareRun { generation, .. }
            | CommandEnrichmentRequest::Execute { generation, .. }
            | CommandEnrichmentRequest::Cancel { generation, .. } => *generation,
        }));
        generations.len()
    }

    /// The caret target, which is per view *and per open* — a reopened dialog
    /// is a fresh draft, so the generation is part of the identity.
    fn target(&self) -> Option<TextTarget> {
        let dialog = self.state.as_ref()?;
        if dialog.selected_control != Control::Field {
            return None;
        }
        if !matches!(
            dialog.run_state,
            RunState::Unrun | RunState::Error | RunState::Ready | RunState::Complete
        ) {
            return None;
        }
        Some(TextTarget {
            identity: format!("command:{}:{}", dialog.view_id, dialog.generation),
            field: match dialog.selected_field {
                Field::Program => "program",
                Field::Arguments => "arguments",
                Field::Cwd => "cwd",
                Field::Environment => "environment",
            },
        })
    }

    fn text_editing(&self) -> bool {
        !self.scroll_focused && self.target().is_some()
    }

    /// The busy guard every editing verb shares: `SavingResults` refuses in
    /// silence because the results are already committing, and the other
    /// in-flight states say what to wait for.
    fn editable(&mut self) -> bool {
        let Some(dialog) = &mut self.state else {
            return false;
        };
        if dialog.run_state == RunState::SavingResults {
            return false;
        }
        if matches!(
            dialog.run_state,
            RunState::Saving | RunState::Preparing | RunState::Running
        ) {
            dialog.error = Some("Wait for the current operation before editing".into());
            return false;
        }
        true
    }

    /// One editing verb against the selected field's draft and the bank-owned
    /// caret, in the shape `components::editors` uses.
    fn text(&mut self, command: EditCommand<'_>, ctx: &mut Ctx<'_>) -> Outcome {
        if self.scroll_focused {
            return Outcome::Ignored;
        }
        let Some(target) = self.target() else {
            return Outcome::Ignored;
        };
        if !self.editable() {
            return Outcome::Consumed;
        }
        let Some(dialog) = &mut self.state else {
            return Outcome::Ignored;
        };
        let multiline = matches!(dialog.selected_field, Field::Arguments | Field::Environment);
        let mut value = command_draft_field_mut(dialog).clone();
        let mut cursor = ctx.cursors.get_or_end(target.clone(), &value);
        let outcome = edit(
            &mut value,
            &mut cursor,
            command,
            EditPolicy {
                max_bytes: MAX_COMMAND_FIELD_BYTES,
                multiline,
            },
        );
        ctx.cursors.store(target, cursor);
        if outcome.changed {
            let view_id = dialog.view_id.clone();
            *command_draft_field_mut(dialog) = value;
            dialog.error = None;
            dialog.review = None;
            dialog.run_state = RunState::Unrun;
            dialog.run_status = "Draft changed · save before reviewing a run".into();
            if let Some(state) = ctx.views.state_mut(&view_id) {
                state.user_interaction_revision = state.user_interaction_revision.saturating_add(1);
            }
        }
        if outcome.changed || outcome.moved {
            Outcome::Consumed
        } else {
            Outcome::Ignored
        }
    }

    /// Tab: the four fields, then the four buttons, then round.
    fn next_field(&mut self) -> Outcome {
        if let Some(dialog) = &mut self.state {
            match dialog.selected_control {
                Control::Field => {
                    let index = Field::ALL
                        .iter()
                        .position(|field| *field == dialog.selected_field)
                        .unwrap_or(0);
                    if index + 1 < Field::ALL.len() {
                        dialog.selected_field = Field::ALL[index + 1];
                    } else {
                        dialog.selected_control = Control::NewLine;
                    }
                }
                Control::NewLine => dialog.selected_control = Control::Save,
                Control::Save => dialog.selected_control = Control::Review,
                Control::Review => dialog.selected_control = Control::Remove,
                Control::Remove => {
                    dialog.selected_control = Control::Field;
                    dialog.selected_field = Field::Program;
                }
            }
        }
        Outcome::Consumed
    }

    /// A reviewed run is waiting for its confirmation. While it is, the
    /// review is the frontmost surface and owns Enter (§8.9): confirming it is
    /// the default from every control, and Escape drops it.
    fn review_pending(&self) -> bool {
        self.state
            .as_ref()
            .is_some_and(|dialog| dialog.run_state == RunState::Ready && dialog.review.is_some())
    }

    /// Whether the focused field takes Enter as a newline (§8.1): only the
    /// two multi-line fields, and only while they are taking text.
    fn multiline_editing(&self) -> bool {
        self.text_editing()
            && self.state.as_ref().is_some_and(|dialog| {
                matches!(dialog.selected_field, Field::Arguments | Field::Environment)
            })
    }

    /// Enter on the focused control. The default is `Save` until a review is
    /// pending, when it is the run the review describes; a field hands Enter
    /// to whichever of those is current, the buttons press themselves.
    fn activate(&mut self, ctx: &mut Ctx<'_>) -> Outcome {
        if self.review_pending() {
            return self.confirm_run(ctx);
        }
        match self.state.as_ref().map(|dialog| dialog.selected_control) {
            Some(Control::Field) => self.save(Some(()), ctx),
            Some(Control::NewLine) => {
                // The button inserts into whichever field is selected, so it
                // borrows the field's focus for exactly one edit.
                if let Some(dialog) = &mut self.state {
                    dialog.selected_control = Control::Field;
                }
                let outcome = self.input('\n', ctx);
                if let Some(dialog) = &mut self.state {
                    dialog.selected_control = Control::NewLine;
                }
                outcome
            }
            Some(Control::Save) => self.save(Some(()), ctx),
            Some(Control::Review) => self.prepare_run(ctx),
            Some(Control::Remove) => self.save(None, ctx),
            None => Outcome::Consumed,
        }
    }

    /// A character, which only the multi-line fields accept a newline into.
    fn input(&mut self, ch: char, ctx: &mut Ctx<'_>) -> Outcome {
        if ch.is_control() && ch != '\n' {
            return Outcome::Consumed;
        }
        if ch == '\n'
            && !self.state.as_ref().is_some_and(|dialog| {
                matches!(dialog.selected_field, Field::Arguments | Field::Environment)
            })
        {
            return Outcome::Consumed;
        }
        let mut buffer = [0u8; 4];
        self.text(EditCommand::Insert(ch.encode_utf8(&mut buffer)), ctx)
    }

    /// Save the draft (`candidate = Some`) or remove the saved step
    /// (`candidate = None`). One request either way, because both are one
    /// definition write that `lvu-app` fences on the base revision.
    fn save(&mut self, candidate: Option<()>, ctx: &mut Ctx<'_>) -> Outcome {
        if !self.editable() {
            return Outcome::Consumed;
        }
        let queue_full = self.request_count() >= MAX_COMMAND_REQUESTS;
        let Some(dialog) = &mut self.state else {
            return Outcome::Consumed;
        };
        if queue_full {
            dialog.error =
                Some("Command request queue is full; wait for the current operation".into());
            return Outcome::Consumed;
        }
        let stage = match candidate {
            Some(()) => match command_candidate(dialog) {
                Ok(stage) => Some(stage),
                Err(error) => {
                    dialog.error = Some(error);
                    return Outcome::Consumed;
                }
            },
            None => None,
        };
        let generation = self.next_generation;
        self.next_generation = generation.saturating_add(1);
        let Some(dialog) = &mut self.state else {
            return Outcome::Consumed;
        };
        dialog.generation = generation;
        self.pending_saves.insert(
            generation,
            (dialog.view_id.clone(), dialog.base_definition_revision),
        );
        self.requests.push_back(CommandEnrichmentRequest::Save {
            generation,
            view_id: dialog.view_id.clone(),
            base_definition_revision: dialog.base_definition_revision,
            candidate: stage.clone(),
        });
        dialog.error = None;
        dialog.run_status = if stage.is_some() {
            "Saving definition…".into()
        } else {
            "Removing definition…".into()
        };
        dialog.run_state = RunState::Saving;
        dialog.review = None;
        let view_id = dialog.view_id.clone();
        if let Some(state) = ctx.views.state_mut(&view_id) {
            state.user_interaction_revision = state.user_interaction_revision.saturating_add(1);
        }
        Outcome::Consumed
    }

    /// Ask for the bounded review. Nothing runs until the review comes back
    /// and the user confirms it.
    fn prepare_run(&mut self, _ctx: &mut Ctx<'_>) -> Outcome {
        if !self.editable() {
            return Outcome::Consumed;
        }
        let queue_full = self.request_count() >= MAX_COMMAND_REQUESTS;
        let generation = self.next_generation;
        let Some(dialog) = &mut self.state else {
            return Outcome::Consumed;
        };
        if queue_full {
            dialog.error =
                Some("Command request queue is full; wait for the current operation".into());
            return Outcome::Consumed;
        }
        let candidate = match command_candidate(dialog) {
            Ok(candidate) => candidate,
            Err(error) => {
                dialog.error = Some(error);
                return Outcome::Consumed;
            }
        };
        // Reviewing an unsaved draft would review something the run would not
        // execute, so the draft has to be the saved definition first.
        if dialog.accepted.as_ref() != Some(&candidate) {
            dialog.review = None;
            dialog.run_state = RunState::Unrun;
            dialog.error =
                Some("Draft differs from the saved command; save it before reviewing a run".into());
            return Outcome::Consumed;
        }
        let Some(stage) = dialog.accepted.clone() else {
            dialog.error = Some("Save a valid command step before preparing a run".into());
            return Outcome::Consumed;
        };
        self.next_generation = generation.saturating_add(1);
        dialog.generation = generation;
        dialog.run_state = RunState::Preparing;
        dialog.run_status = "Preparing bounded review…".into();
        dialog.review = None;
        self.requests
            .push_back(CommandEnrichmentRequest::PrepareRun {
                generation,
                view_id: dialog.view_id.clone(),
                definition_revision: dialog.base_definition_revision,
                stage_id: stage.id.clone(),
            });
        Outcome::Consumed
    }

    /// Run exactly the reviewed set. The review token is consumed, so a second
    /// confirmation cannot start a second run off one review.
    fn confirm_run(&mut self, _ctx: &mut Ctx<'_>) -> Outcome {
        let queue_full = self.request_count() >= MAX_COMMAND_REQUESTS;
        let Some(dialog) = &mut self.state else {
            return Outcome::Consumed;
        };
        if dialog.run_state != RunState::Ready {
            return Outcome::Consumed;
        }
        let Some(review) = dialog.review.take() else {
            return Outcome::Consumed;
        };
        if queue_full {
            dialog.review = Some(review);
            dialog.error =
                Some("Command request queue is full; reviewed run was not started".into());
            return Outcome::Consumed;
        }
        dialog.run_state = RunState::Running;
        dialog.run_status = "Running reviewed records…".into();
        self.pending_runs.insert(
            dialog.generation,
            (dialog.view_id.clone(), dialog.base_definition_revision),
        );
        self.requests.push_back(CommandEnrichmentRequest::Execute {
            generation: dialog.generation,
            view_id: dialog.view_id.clone(),
            definition_revision: dialog.base_definition_revision,
            review_token: review.review_token,
        });
        Outcome::Consumed
    }

    fn scroll_body(&mut self, delta: i32) {
        self.scroll = self
            .scroll
            .saturating_add_signed(delta as isize)
            .min(self.scroll_limit);
    }

    /// The footer's accelerators, moved from `key_to_action`'s
    /// `Focus::CommandEnrichment` table unchanged: Ctrl-S and Ctrl-Enter save,
    /// Ctrl-R and Alt-Enter ask for the review, Alt-N and Shift-Enter insert a
    /// newline into a multi-line field, Alt-Delete removes the saved step.
    fn key(&mut self, key: KeyEvent, ctx: &mut Ctx<'_>) -> Outcome {
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return Outcome::Ignored;
        }
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        match key.code {
            KeyCode::Tab => self.next_field(),
            KeyCode::Backspace => self.text(EditCommand::Backspace, ctx),
            KeyCode::Char('s') if control || alt => self.save(Some(()), ctx),
            KeyCode::Char('r') if control || alt => self.prepare_run(ctx),
            KeyCode::Char('m') if alt => self.save(None, ctx),
            KeyCode::Char('n') if alt => self.input('\n', ctx),
            KeyCode::Char('a') if control => self.text(EditCommand::StartOfLine, ctx),
            KeyCode::Char('e') if control => self.text(EditCommand::EndOfLine, ctx),
            KeyCode::Char('k') if control => self.text(EditCommand::KillToEndOfLine, ctx),
            KeyCode::Enter if control => self.save(Some(()), ctx),
            KeyCode::Enter if alt => self.prepare_run(ctx),
            KeyCode::Enter if shift => self.input('\n', ctx),
            // §8.9: a pending review owns Enter; otherwise a multi-line field
            // takes it as a newline and everything else runs the default.
            KeyCode::Enter if !self.review_pending() && self.multiline_editing() => {
                self.input('\n', ctx)
            }
            KeyCode::Enter => self.activate(ctx),
            KeyCode::Delete if alt => self.save(None, ctx),
            KeyCode::Up => self.vertical(-1, ctx),
            KeyCode::Down => self.vertical(1, ctx),
            KeyCode::Left if self.text_editing() => self.text(EditCommand::MoveLeft, ctx),
            KeyCode::Right if self.text_editing() => self.text(EditCommand::MoveRight, ctx),
            KeyCode::Char(character) if !control => self.input(character, ctx),
            _ => Outcome::Ignored,
        }
    }

    /// Arrows belong to the caret while a field is taking text, and to the
    /// results pane otherwise — except on an editable draft with a button
    /// focused, where they did nothing and still do.
    fn vertical(&mut self, delta: i32, ctx: &mut Ctx<'_>) -> Outcome {
        if self.text_editing() {
            return self.text(
                if delta < 0 {
                    EditCommand::MoveUp
                } else {
                    EditCommand::MoveDown
                },
                ctx,
            );
        }
        let editable = self
            .state
            .as_ref()
            .is_some_and(|dialog| matches!(dialog.run_state, RunState::Unrun | RunState::Error));
        if !editable || self.scroll_focused {
            self.scroll_body(delta);
        }
        Outcome::Consumed
    }

    fn mouse(
        &mut self,
        kind: MouseEventKind,
        hit: Option<CommandHit>,
        ctx: &mut Ctx<'_>,
    ) -> Outcome {
        if matches!(kind, MouseEventKind::Down(MouseButton::Left))
            && let Some(CommandHit::Control(control)) = hit
        {
            if let Some(dialog) = &mut self.state {
                dialog.selected_control = control;
            }
            return self.activate(ctx);
        }
        let over_notes = hit == Some(CommandHit::Notes);
        match kind {
            MouseEventKind::ScrollUp if over_notes => {
                self.scroll_body(-1);
                Outcome::Consumed
            }
            MouseEventKind::ScrollDown if over_notes => {
                self.scroll_body(1);
                Outcome::Consumed
            }
            _ => Outcome::Consumed,
        }
    }

    // ---- completion paths (§8) ------------------------------------------
    //
    // Each is fenced against the generation and the view the request was made
    // for; anything else is a reply to work this dialog no longer owns and is
    // dropped rather than applied.

    pub(crate) fn finish_save(
        &mut self,
        generation: u64,
        view_id: &str,
        definition_revision: u64,
        result: Result<Option<CommandEnrichmentStage>, String>,
        views: &mut Views,
        notice: &mut Option<String>,
    ) -> bool {
        let Some((pending_view, base_revision)) = self.pending_saves.get(&generation).cloned()
        else {
            return false;
        };
        if pending_view != view_id {
            return false;
        }
        self.pending_saves.remove(&generation);
        if views
            .state(view_id)
            .is_none_or(|state| state.command_enrichment_revision != base_revision)
            || (result.is_ok() && base_revision >= definition_revision)
        {
            return false;
        }
        match result {
            Ok(stage) => {
                if let Some(state) = views.state_mut(view_id) {
                    state.command_enrichment = stage.clone();
                    state.command_enrichment_revision = definition_revision;
                }
                if let Some(dialog) = self.state.as_mut()
                    && dialog.generation == generation
                    && dialog.view_id == view_id
                {
                    dialog.accepted = stage;
                    dialog.base_definition_revision = definition_revision;
                    dialog.error = None;
                    dialog.run_state = RunState::Unrun;
                    dialog.run_status =
                        "Saved · Unrun; new records wait for an explicit run".into();
                    dialog.review = None;
                } else {
                    *notice = Some("command enrichment definition saved; it was not run".into());
                }
            }
            Err(error) => {
                if let Some(dialog) = self.state.as_mut()
                    && dialog.generation == generation
                    && dialog.view_id == view_id
                {
                    dialog.error = Some(error);
                    dialog.run_state = RunState::Error;
                } else {
                    *notice = Some(format!("command enrichment unchanged: {error}"));
                }
            }
        }
        true
    }

    pub(crate) fn finish_review(
        &mut self,
        generation: u64,
        view_id: &str,
        definition_revision: u64,
        result: Result<CommandEnrichmentReview, String>,
    ) -> bool {
        let Some(dialog) = self.state.as_mut() else {
            return false;
        };
        if dialog.generation != generation
            || dialog.view_id != view_id
            || dialog.base_definition_revision != definition_revision
            || dialog.run_state != RunState::Preparing
        {
            return false;
        }
        match result {
            Ok(review) => {
                dialog.review = Some(review);
                dialog.run_state = RunState::Ready;
                dialog.run_status =
                    "Ready for review · confirmation runs exactly this bounded set".into();
            }
            Err(error) => {
                dialog.run_state = RunState::Error;
                dialog.run_status = error;
            }
        }
        true
    }

    pub(crate) fn finish_run(
        &mut self,
        generation: u64,
        view_id: &str,
        definition_revision: u64,
        result: Result<String, String>,
        views: &Views,
        notice: &mut Option<String>,
    ) -> bool {
        let Some((pending_view, pending_revision)) = self.pending_runs.get(&generation).cloned()
        else {
            return false;
        };
        if pending_view != view_id || pending_revision != definition_revision {
            return false;
        }
        self.pending_runs.remove(&generation);
        if views
            .state(view_id)
            .is_none_or(|state| state.command_enrichment_revision != definition_revision)
        {
            return false;
        }
        let matching_dialog = self.state.as_mut().filter(|dialog| {
            dialog.generation == generation
                && dialog.view_id == view_id
                && dialog.base_definition_revision == definition_revision
                && matches!(
                    dialog.run_state,
                    RunState::Running | RunState::SavingResults
                )
        });
        if let Some(dialog) = matching_dialog {
            dialog.review = None;
            match result {
                Ok(status) => {
                    dialog.run_state = RunState::Complete;
                    dialog.run_status = status;
                }
                Err(error) => {
                    dialog.run_state = RunState::Error;
                    dialog.run_status = error;
                }
            }
        } else {
            *notice = Some(match result {
                Ok(status) => format!("command enrichment results saved: {status}"),
                Err(error) => format!("command enrichment results unchanged: {error}"),
            });
        }
        true
    }

    pub(crate) fn begin_result_save(
        &mut self,
        generation: u64,
        view_id: &str,
        definition_revision: u64,
    ) -> bool {
        if self.pending_runs.get(&generation) != Some(&(view_id.to_owned(), definition_revision)) {
            return false;
        }
        let Some(dialog) = self.state.as_mut() else {
            return false;
        };
        if dialog.generation != generation
            || dialog.view_id != view_id
            || dialog.base_definition_revision != definition_revision
            || dialog.run_state != RunState::Running
        {
            return false;
        }
        dialog.run_state = RunState::SavingResults;
        dialog.run_status = "Saving results…".into();
        dialog.error = None;
        true
    }
}

fn contains(area: Rect, point: (u16, u16)) -> bool {
    point.0 >= area.x && point.0 < area.right() && point.1 >= area.y && point.1 < area.bottom()
}

impl Component for ExternalCommandDialog {
    type Hit = CommandHit;
    type Open = ();

    /// Moved verbatim from `Action::OpenCommandEnrichment`. The draft opens on
    /// the saved definition, so reopening never silently proposes something
    /// different from what is applied.
    fn open(&mut self, _params: (), ctx: &mut Ctx<'_>) {
        let Some(view_id) = ctx.views.active_id().map(str::to_owned) else {
            return;
        };
        let (accepted, revision) = ctx.views.state(&view_id).map_or((None, 0), |state| {
            (
                state.command_enrichment.clone(),
                state.command_enrichment_revision,
            )
        });
        let (program, arguments, cwd, environment) = accepted.as_ref().map_or_else(
            || (String::new(), String::new(), String::new(), String::new()),
            command_stage_draft,
        );
        let generation = self.next_generation;
        self.next_generation = generation.saturating_add(1);
        self.state = Some(CommandEnrichmentDialogState {
            generation,
            view_id,
            base_definition_revision: revision,
            selected_field: CommandEnrichmentField::Program,
            selected_control: Control::Field,
            program,
            arguments,
            cwd,
            environment,
            accepted,
            error: None,
            run_state: RunState::Unrun,
            run_status: "Unrun · new records wait for an explicit run".into(),
            review: None,
        });
        self.scroll = 0;
        self.scroll_focused = false;
        self.geometry = CommandGeometry::default();
        self.surface = Surface {
            text_focus: true,
            ..Surface::default()
        };
    }

    fn handle(&mut self, event: Event<CommandHit>, ctx: &mut Ctx<'_>) -> Outcome {
        match event {
            Event::Key(key) => self.key(key, ctx),
            Event::Paste(text) => self.text(EditCommand::Insert(&text), ctx),
            Event::Mouse { kind, hit, .. } => self.mouse(kind, hit, ctx),
            // Closing mid-flight cancels the work rather than leaving it to
            // land on a dialog that is gone; the caret bank is pruned because
            // the identity carries the generation and will never recur.
            Event::Dismiss => {
                if let Some(target) = self.target() {
                    ctx.cursors.prune_identity(&target.identity);
                }
                if let Some(dialog) = self.state.take()
                    && matches!(dialog.run_state, RunState::Preparing | RunState::Running)
                {
                    self.requests.push_back(CommandEnrichmentRequest::Cancel {
                        generation: dialog.generation,
                        view_id: dialog.view_id,
                    });
                }
                Outcome::Close
            }
            Event::Command(CommandId::CommandEnrichmentSave) => self.save(Some(()), ctx),
            Event::Command(CommandId::CommandEnrichmentRemove) => self.save(None, ctx),
            Event::Command(CommandId::CommandEnrichmentRun) => self.prepare_run(ctx),
            // §4.2: the dialog draws its own run state, and nothing about a
            // view event changes what a saved command definition is.
            Event::View(_) | Event::Command(_) | Event::Resize => Outcome::Ignored,
        }
    }

    /// §4.3: the three verbs are the palette's, contributed by the layer that
    /// owns them.
    fn commands(&self, _views: &Views) -> Vec<CommandEntry> {
        let open = self.is_open();
        vec![
            CommandEntry {
                spec: CommandSpec {
                    id: CommandId::CommandEnrichmentSave,
                    name: "Save external command",
                    description: "Store the command definition without running it",
                    category: "Enrichment",
                    aliases: &["save command"],
                    shortcut: open.then_some("Alt-S"),
                },
                unavailable_reason: (!open).then_some("open External command first"),
            },
            CommandEntry {
                spec: CommandSpec {
                    id: CommandId::CommandEnrichmentRun,
                    name: "Review and run external command",
                    description: "Prepare a bounded review; nothing runs until you confirm it",
                    category: "Enrichment",
                    aliases: &["run command"],
                    shortcut: open.then_some("Alt-R"),
                },
                unavailable_reason: (!open).then_some("open External command first"),
            },
            CommandEntry {
                spec: CommandSpec {
                    id: CommandId::CommandEnrichmentRemove,
                    name: "Remove external command",
                    description: "Drop the saved command step; enrichment steps still apply",
                    category: "Enrichment",
                    aliases: &["delete command"],
                    shortcut: open.then_some("Alt-M"),
                },
                unavailable_reason: (!open).then_some("open External command first"),
            },
        ]
    }

    fn surface(&self) -> Surface {
        Surface {
            text_focus: self.text_editing(),
            ..self.surface
        }
    }

    fn hit(&self, point: (u16, u16)) -> Option<CommandHit> {
        self.geometry
            .controls
            .iter()
            .find_map(|(rect, control)| {
                contains(*rect, point).then_some(CommandHit::Control(*control))
            })
            .or_else(|| {
                self.geometry
                    .notes
                    .filter(|rect| contains(*rect, point))
                    .map(|_| CommandHit::Notes)
            })
            .or_else(|| contains(self.surface.popup, point).then_some(CommandHit::Body))
    }

    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &RenderCtx<'_>) -> Surface {
        render_command_enrichment(self, frame, area, ctx)
    }
}
/// Moved verbatim from `ui::render_command_enrichment`. The hit regions it
/// wrote into `App` are the component's geometry now; the caret, the theme and
/// the ASCII flag come from `ctx`.
fn render_command_enrichment(
    this: &mut ExternalCommandDialog,
    frame: &mut Frame<'_>,
    area: Rect,
    ctx: &RenderCtx<'_>,
) -> Surface {
    use crate::dialog_layout::{DialogClass, DialogContent, content_width, pane};
    let theme = ctx.theme;
    let mut geometry = CommandGeometry::default();
    let styles = DialogStyles::new(theme);
    let ascii = ctx.ascii;
    let Some(dialog) = this.state.clone() else {
        this.geometry = geometry;
        this.surface = Surface::default();
        return this.surface;
    };
    let cursor = this
        .target()
        .filter(|_| !this.scroll_focused)
        .and_then(|target| ctx.cursors.peek(&target, command_field(&dialog)));
    let width = content_width(area, DialogClass::L);
    let busy = matches!(
        dialog.run_state,
        RunState::Saving | RunState::Preparing | RunState::Running | RunState::SavingResults
    );

    // §7.4: the state word, and one sentence about what will and will not run.
    let (state, mut sentence) = if dialog.error.is_some() || dialog.run_state == RunState::Error {
        (
            MessageState::Error,
            dialog
                .error
                .clone()
                .unwrap_or_else(|| dialog.run_status.clone()),
        )
    } else {
        let state = match dialog.run_state {
            RunState::Unrun => MessageState::Unrun,
            RunState::Ready => MessageState::Ready,
            RunState::Complete => MessageState::Applied,
            _ => MessageState::Pending,
        };
        // §7.4 retires the `Unrun: Unrun · …` stutter: the row already draws
        // the state word, so the sentence must not repeat it.
        let word = match dialog.run_state {
            RunState::Unrun => "Unrun",
            RunState::Saving => "Saving",
            RunState::Preparing => "Preparing",
            RunState::Ready => "Ready",
            RunState::Running => "Running",
            RunState::SavingResults => "Saving results",
            RunState::Complete => "Complete",
            RunState::Error => "Error",
        };
        // Strip the leading word only when the row already draws that same
        // word. `Saving results` is not in §7.4's vocabulary, so the row says
        // `Pending` and the phase has to survive in the sentence.
        let duplicated = matches!(
            dialog.run_state,
            RunState::Unrun | RunState::Ready | RunState::Error
        );
        let detail = dialog.run_status.clone();
        let detail = if duplicated {
            detail.strip_prefix(word).map_or(detail.clone(), |tail| {
                tail.trim_start_matches([' ', '·', '…', ':']).to_owned()
            })
        } else {
            detail
        };
        (state, detail)
    };
    if matches!(dialog.run_state, RunState::Unrun) && dialog.error.is_none() {
        if sentence.is_empty() {
            sentence = "saved definition".to_owned();
        }
        sentence.push_str(" · runs only when you confirm");
    }
    let help = "Program is an executable path; no shell parsing. One argument per line.";

    // The pane carries everything that is longer than a sentence: what a save
    // does and does not start, what a run would read, and where results land.
    let mut notes: Vec<String> = Vec::new();
    if let Some(stage) = &dialog.accepted {
        let crate::app::CommandEnrichmentStage { definition, .. } = stage;
        notes.push(match &definition.program {
            lvu_core::CommandProgram::Exec { executable, args } => format!(
                "Applied command step: {} ({} arguments) after {} enrichment step(s)",
                executable.display(),
                args.len(),
                ctx.views
                    .active()
                    .map_or(0, |state| state.enrichments.len())
            ),
            lvu_core::CommandProgram::Shell { .. } => "Invalid saved command form".to_owned(),
        });
    } else {
        notes.push("Applied command step: none · enrichment steps still apply".to_owned());
    }
    if ctx
        .views
        .active()
        .is_some_and(|state| state.command_publication.is_some())
        && dialog.run_state != RunState::Complete
    {
        notes.push(
            "Previous published results retained; changed and new records remain pending."
                .to_owned(),
        );
    }
    if let Some(review) = &dialog.review {
        notes.push(format!(
            "Run review · fixed snapshot: {} records from {} sources",
            review.record_count, review.source_count
        ));
        notes.push("Limit: 1,024 records / 4 MiB input; no sampling".to_owned());
        notes.push(format!("Executable: {}", review.executable));
        notes.push(format!("Arguments: {}", review.arguments.join(" | ")));
        notes.push(format!(
            "Working directory: {}",
            review.cwd.as_deref().unwrap_or("current")
        ));
        notes.push(format!(
            "Environment keys: {}",
            if review.environment_keys.is_empty() {
                "none".to_owned()
            } else {
                review.environment_keys.join(", ")
            }
        ));
    } else if dialog.run_state == RunState::Unrun {
        notes.push("Saving or restoring never starts this command.".to_owned());
        notes.push("New records stay pending until you run it again.".to_owned());
    }
    notes.push(
        "Results appear in Details as command.<field>; command.status shows Ready or Pending."
            .to_owned(),
    );
    let note_width = width
        .saturating_sub(crate::dialog_layout::PANE_INDENT)
        .max(1);
    let note_lines: Vec<String> = notes
        .iter()
        .flat_map(|note| wrap_sentence(note, usize::from(note_width), usize::MAX))
        .collect();

    let specs: [(Field, &str, &String, &str); 4] = [
        // §8.1 placeholders say what an empty field means, not what a
        // particular command would put there.
        (Field::Program, "Program", &dialog.program, "(required)"),
        (Field::Arguments, "Arguments", &dialog.arguments, "(none)"),
        (
            Field::Cwd,
            "Directory",
            &dialog.cwd,
            "(workspace directory)",
        ),
        (
            Field::Environment,
            "Environment",
            &dialog.environment,
            "(inherited)",
        ),
    ];
    let field_rows = |field: Field, value: &str| -> usize {
        if matches!(field, Field::Arguments | Field::Environment) {
            value.split('\n').count().clamp(1, COMMAND_MULTILINE_ROWS)
        } else {
            1
        }
    };
    let form_rows: usize = specs
        .iter()
        .map(|(field, _, value, _)| field_rows(*field, value))
        .sum();

    // §8.10 mnemonics: Alt-S, Alt-R, Alt-M, Alt-N press these; the Ctrl chords
    // and Alt-Delete stay as unlisted aliases.
    let action_labels = ["&Save", "&Review and run", "Re&move", "&New line"];
    let content = DialogContent {
        header: 0,
        // The form, a blank row, the pane heading, its lines.
        body: u16::try_from(form_rows + 2 + note_lines.len()).unwrap_or(u16::MAX),
        message: message_rows(&sentence, width).max(1),
        help: help_rows(help, width),
        actions: packed_button_rows(width, &action_labels),
    };
    let regions = dialog_frame_regions(
        frame,
        area,
        DialogClass::L,
        "Enrichment › External command",
        &content,
        theme,
    );
    let mut surface = Surface {
        popup: regions.popup,
        interior: regions.interior,
        scrollable: true,
        ..Surface::default()
    };
    let body = regions.body;
    if body.width == 0 || body.height == 0 {
        this.geometry = geometry;
        this.surface = surface;
        return surface;
    }

    let mut y = body.y;
    for (field, label, value, placeholder) in &specs {
        let rows = field_rows(*field, value);
        if y >= body.bottom() {
            break;
        }
        let focused = dialog.selected_field == *field && dialog.selected_control == Control::Field;
        frame.render_widget(
            Paragraph::new(*label).style(if focused {
                styles.shortcut
            } else {
                styles.label
            }),
            Rect::new(body.x, y, COMMAND_LABEL_WIDTH.min(body.width), 1),
        );
        let input = Rect::new(
            body.x.saturating_add(COMMAND_LABEL_WIDTH),
            y,
            body.width.saturating_sub(COMMAND_LABEL_WIDTH),
            u16::try_from(rows)
                .unwrap_or(1)
                .min(body.bottom().saturating_sub(y)),
        );
        InputSurface {
            style: styles.input,
        }
        .render(input, frame.buffer_mut());
        if value.is_empty() {
            frame.render_widget(
                Paragraph::new(truncated(placeholder, usize::from(input.width)))
                    .style(styles.input.patch(styles.unavailable.bg(theme.input_bg))),
                input,
            );
        } else {
            let shown = if rows == 1 {
                // A single-line field is usually a path: its tail is the part
                // that identifies it, so that is the end kept in view.
                input_tail(value, usize::from(input.width.saturating_sub(1)))
            } else {
                value.split('\n').take(rows).collect::<Vec<_>>().join("\n")
            };
            frame.render_widget(Paragraph::new(shown).style(styles.input), input);
        }
        geometry.controls.push((input, Control::Field));
        if focused && !busy && value.is_empty() {
            // `place_input_cursor_at` repaints the field's visible window, which
            // would wipe the placeholder. An empty focused field needs only the
            // caret, so set it directly and leave the hint in place.
            let caret = Rect::new(input.x, input.y, 1.min(input.width), 1);
            if caret.width > 0 {
                frame.buffer_mut()[(caret.x, caret.y)]
                    .set_style(Style::default().bg(theme.cursor).fg(theme.input_fg));
                frame.set_cursor_position((caret.x, caret.y));
                surface.caret = Some((caret.x, caret.y));
            }
        } else if focused && !busy {
            let last = value.rsplit('\n').next().unwrap_or("");
            surface.caret = place_input_cursor_at(
                frame,
                Rect::new(
                    input.x,
                    input.y.saturating_add(input.height.saturating_sub(1)),
                    input.width,
                    1,
                ),
                0,
                0,
                last,
                cursor
                    .unwrap_or_else(|| value.chars().count())
                    .min(last.chars().count()),
                theme,
            );
        }
        y = y.saturating_add(u16::try_from(rows).unwrap_or(1));
    }

    if y.saturating_add(2) < body.bottom() {
        y = y.saturating_add(1);
    }
    let pane_area = Rect::new(body.x, y, body.width, body.bottom().saturating_sub(y));
    let visible = usize::from(pane_area.height.saturating_sub(1));
    let count = format!("{} of {}", visible.min(note_lines.len()), note_lines.len());
    let rects = pane(
        pane_area,
        u16::try_from(UnicodeWidthStr::width(count.as_str())).unwrap_or(0),
        note_lines.len(),
    );
    if rects.heading.height > 0 {
        frame.render_widget(
            Paragraph::new("Results and review").style(if this.scroll_focused {
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
    let limit = note_lines
        .len()
        .saturating_sub(usize::from(rects.viewport.height));
    this.scroll_limit = limit;
    this.scroll = this.scroll.min(limit);
    let scroll = this.scroll;
    for (offset, line) in note_lines
        .iter()
        .skip(scroll)
        .take(usize::from(rects.viewport.height))
        .enumerate()
    {
        frame.render_widget(
            Paragraph::new(line.clone()).style(styles.description),
            Rect::new(
                rects.viewport.x,
                rects.viewport.y.saturating_add(offset as u16),
                rects.viewport.width,
                1,
            ),
        );
    }
    if let Some(bar) = rects.scrollbar {
        render_scrollbar(frame, bar, scroll, limit, theme, ascii);
    }
    geometry.notes = (limit > 0).then_some(rects.viewport);

    render_message(frame, regions.message, state, &sentence, theme, ascii);
    render_help_text(frame, regions.help, help, theme);
    let controls = [
        Control::Save,
        Control::Review,
        Control::Remove,
        Control::NewLine,
    ];
    let focused = controls
        .iter()
        .position(|control| *control == dialog.selected_control);
    // §8.9: `Save` is the default until a review is waiting, when the run it
    // describes is. `Remove` is the destructive one and never the default.
    let default = if this.review_pending() { 1 } else { 0 };
    for (index, rect) in render_actions(
        frame,
        regions.actions,
        ActionRow {
            labels: &action_labels,
            default: Some(default),
            destructive: &[2],
            focused,
        },
        theme,
    ) {
        geometry.controls.push((rect, controls[index]));
    }
    this.geometry = geometry;
    this.surface = surface;
    surface
}
