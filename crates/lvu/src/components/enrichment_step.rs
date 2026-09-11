//! Layer two of the enrichment stack: one step's expression, the record it
//! reads and what it produces. Converted per `docs/component-model.md` §6.3
//! step 13.
//!
//! This is the model's only true child (§5.3). `Enrichment` opens it with
//! `OpenChild`, it draws over the list it came from, and both Save and Escape
//! drop the user back onto that list with the selection intact. Nothing about
//! that needed a behavioural change: `ui::render_enrichment_step` already drew
//! the parent scrimmed and inset itself to `parent.width - 4`, which is
//! exactly what the shell's stack loop and §10 ask for.
//!
//! It is also the last consumer of the shared editor completion, so
//! `App::editor_completion`, its generation counter, the three
//! `Action::*EditorCompletion` variants and `HitRegions::editor_completion_rows`
//! go with this step (W18, §6.5). The implementation is the same one
//! `components::editors` uses.
//!
//! The product invariant it carries: **an invalid step leaves the last applied
//! view usable.** The draft, the accepted chain and the error are separate
//! fields of `ViewState`; a refusal writes `enrichment.error` and touches
//! neither `enrichments` nor the rows on screen.

use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::Style,
    text::Line,
    widgets::{Paragraph, Widget},
};
use unicode_width::UnicodeWidthStr;

use crate::app::{
    EditorCompletionKind, EditorCompletionState, EnrichmentStageId,
    EnrichmentStepControl as Control, MAX_EDITOR_BYTES, QueryPurpose, Views,
    sample_editor_completion,
};
use crate::command_palette::CommandId;
use crate::component::{
    CommandEntry, CommandSpec, Component, Ctx, Event, Outcome, RenderCtx, Surface, ViewEvent,
    is_typed_char,
};
use crate::dialog_controls::{ButtonRole, DialogStyles, render_role_button, stable_action_rows};
use crate::dialog_layout::{DialogSpec, PresentationKind, is_compact, resolve_dialog};
use crate::text_edit::{EditCommand, EditPolicy, TextTarget, edit};
use crate::ui::{
    InputSurface, MessageState, draw_editor_completion_anchored, render_help_text, render_message,
    render_pane_heading, render_responsive_frame, render_scrollbar, truncated,
};

/// §8.1 caps the expression field at three wrapped rows; §5.2.1 reserves all
/// three so the dialog does not resize as the draft wraps.
const EXPRESSION_ROW_CAP: u16 = 3;

/// What the step editor is opened on: an existing stage or a new one, and the
/// expression to start from when the opener already knows it (§6.5, W23).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct StepOpen {
    pub editing: Option<EnrichmentStageId>,
    pub prefill: Option<String>,
}

/// Everything the step editor draws that can be clicked.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StepHit {
    Completion(usize),
    Control(Control),
    Body,
}

/// Recorded by `render`, consumed by `hit()` (§5.1). These were
/// `HitRegions::{enrichment_step_controls, editor_completion_rows}`.
#[derive(Clone, Debug, Default)]
struct StepGeometry {
    controls: Vec<(Rect, Control)>,
    completion: Vec<(Rect, usize)>,
}

#[derive(Debug)]
pub struct EnrichmentStepLayer {
    open: bool,
    /// The view whose chain this step belongs to. Held across a submission so
    /// a view switch underneath cannot redirect it (§7.3).
    view_id: String,
    control: Control,
    /// `None` while adding.
    editing: Option<EnrichmentStageId>,
    /// Which record the Input pane previews, as an index into the unfolded
    /// stream.
    sample: usize,
    /// The Accepted-output pane's scroll, and whether the completion cycle has
    /// handed the arrows to it. Both were `App::dialog_scroll*`, shell state
    /// several dialogs shared by accident of focus.
    scroll: usize,
    scroll_limit: usize,
    scroll_focused: bool,
    completion: Option<EditorCompletionState>,
    next_completion_generation: u64,
    geometry: StepGeometry,
    surface: Surface,
}

impl Default for EnrichmentStepLayer {
    fn default() -> Self {
        Self {
            open: false,
            view_id: String::new(),
            control: Control::Expression,
            editing: None,
            sample: 0,
            scroll: 0,
            scroll_limit: 0,
            scroll_focused: false,
            completion: None,
            next_completion_generation: 1,
            geometry: StepGeometry::default(),
            surface: Surface::default(),
        }
    }
}

impl EnrichmentStepLayer {
    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn scroll(&self) -> usize {
        self.scroll
    }

    /// A view that forked under the layer keeps it pointed at the fork.
    /// Returns whether it was open on the origin, because a step editor whose
    /// view forked has already submitted: it is finished, not carried.
    pub(crate) fn retarget_view(&mut self, origin: &str, candidate: &str) -> bool {
        if self.open && self.view_id == origin {
            self.view_id = candidate.to_owned();
            return true;
        }
        false
    }

    pub fn control_rects(&self) -> &[(Rect, Control)] {
        &self.geometry.controls
    }

    pub fn completion_rects(&self) -> &[(Rect, usize)] {
        &self.geometry.completion
    }

    pub fn completion(&self) -> Option<&EditorCompletionState> {
        self.completion.as_ref()
    }

    pub fn control(&self) -> Control {
        self.control
    }

    pub fn sample(&self) -> usize {
        self.sample
    }

    fn record(&mut self, geometry: StepGeometry, surface: Surface) -> Surface {
        self.geometry = geometry;
        self.surface = surface;
        surface
    }

    pub fn open_for_test(&mut self) {
        self.open = true;
    }

    fn target(&self) -> TextTarget {
        TextTarget {
            identity: self.view_id.clone(),
            field: "enrichment",
        }
    }

    fn draft(&self, ctx: &Ctx<'_>) -> Option<String> {
        Some(
            ctx.views
                .editor(&self.view_id, QueryPurpose::Enrichment)?
                .draft
                .clone(),
        )
    }

    /// Whether the expression field, rather than a pane or a button, has the
    /// keys. This is what `App::active_text_target` answered from `focus` plus
    /// `EnrichmentStepDialog::control`.
    fn text_editing(&self) -> bool {
        self.control == Control::Expression && !self.scroll_focused
    }

    fn policy(&self) -> EditPolicy {
        EditPolicy {
            max_bytes: MAX_EDITOR_BYTES,
            multiline: true,
        }
    }

    fn text(&mut self, command: EditCommand<'_>, ctx: &mut Ctx<'_>) -> Outcome {
        if !self.text_editing() {
            return Outcome::Ignored;
        }
        let Some(mut value) = self.draft(ctx) else {
            return Outcome::Ignored;
        };
        let target = self.target();
        let mut cursor = ctx.cursors.get_or_end(target.clone(), &value);
        let outcome = edit(&mut value, &mut cursor, command, self.policy());
        ctx.cursors.store(target, cursor);
        if outcome.changed {
            if let Some(editor) = ctx
                .views
                .editor_mut(&self.view_id, QueryPurpose::Enrichment)
            {
                editor.draft = value;
            }
            ctx.views.touch(&self.view_id);
            // A changed draft invalidates the popup it was offered against.
            self.completion = None;
        }
        if outcome.changed || outcome.moved {
            Outcome::Consumed
        } else {
            Outcome::Ignored
        }
    }

    /// Whether this step's own save is still outstanding. A fixed view forks,
    /// so the query belongs to a candidate this layer cannot see; `fork_pending`
    /// is how the seam says so.
    fn save_outstanding(&self, ctx: &Ctx<'_>) -> bool {
        ctx.views
            .editor(&self.view_id, QueryPurpose::Enrichment)
            .is_some_and(|editor| editor.pending_generation.is_some() || editor.fork_pending)
    }

    /// Save. An empty or invalid expression is refused by the seam, which
    /// writes `enrichment.error` and keeps every accepted step — the layer
    /// stays open on the draft rather than losing it.
    fn submit(&mut self, ctx: &mut Ctx<'_>) -> Outcome {
        // One draft, one submission. Until the outcome is known — the child
        // closes on acceptance, or the error arrives — a second Enter on
        // `[ Save ]` must not send the same step again: on a fixed view that
        // appended it to the candidate's chain twice and the engine answered
        // `duplicate enrichment output field`.
        if self.save_outstanding(ctx) {
            return Outcome::Consumed;
        }
        ctx.views.touch(&self.view_id);
        // A full queue keeps the draft and says so; a fixed definition becomes
        // a derived view inside the seam, which keeps this layer open on the
        // view it created (§2.3).
        let _ = ctx
            .views
            .enqueue(&self.view_id, QueryPurpose::Enrichment, None);
        Outcome::Consumed
    }

    /// Remove the step being edited. Moved from
    /// `App::remove_edited_enrichment_step`: the layer leaves first, so the
    /// list is what the removal's outcome lands on, and the removal itself is
    /// the parent's (`Legacy`-free: it is one `Outcome::Close` plus the
    /// selection the parent reads back out of `ViewState`).
    fn remove(&mut self, ctx: &mut Ctx<'_>) -> Outcome {
        let Some(id) = self.editing.clone() else {
            return Outcome::Consumed;
        };
        let Some(index) = ctx
            .views
            .state(&self.view_id)
            .and_then(|state| state.enrichments.iter().position(|stage| stage.id == id))
        else {
            return Outcome::Consumed;
        };
        self.cancel(ctx);
        if let Some(state) = ctx.views.state_mut(&self.view_id) {
            state.enrichment_selected = index;
        }
        crate::components::enrichment::remove_selected_stage(ctx);
        Outcome::Close
    }

    /// Leaving without touching the accepted chain, and without discarding
    /// unfinished work. Moved verbatim from `App::cancel_enrichment_step`.
    fn cancel(&mut self, ctx: &mut Ctx<'_>) {
        let untouched = ctx.views.state(&self.view_id).is_some_and(|state| {
            self.editing.as_ref().is_some_and(|id| {
                state
                    .enrichments
                    .iter()
                    .any(|stage| &stage.id == id && stage.source == state.enrichment.draft)
            })
        });
        if untouched {
            ctx.cursors.reset(self.target(), "");
            if let Some(state) = ctx.views.state_mut(&self.view_id) {
                state.enrichment.draft.clear();
                state.enrichment_editing = None;
            }
        }
        self.completion = None;
        self.scroll = 0;
        self.scroll_focused = false;
        self.open = false;
    }

    fn move_control(&mut self, delta: i32) -> Outcome {
        self.scroll_focused = false;
        let traversal = Control::traversal(self.editing.is_some());
        let index = traversal
            .iter()
            .position(|control| *control == self.control)
            .unwrap_or(0);
        self.control =
            traversal[(index as i32 + delta).rem_euclid(traversal.len() as i32) as usize];
        self.completion = None;
        Outcome::Consumed
    }

    fn focus_control(&mut self, control: Control) {
        self.control = control;
        self.scroll_focused = false;
        self.completion = None;
    }

    /// Enter. `Save` is the default (§8.9): it is what the field submits and
    /// what the two preview panes hand on, since a pane has no action of its
    /// own. Only the destructive `Remove` button presses itself.
    fn activate(&mut self, ctx: &mut Ctx<'_>) -> Outcome {
        match self.control {
            Control::Expression | Control::Save | Control::Input | Control::Output => {
                self.submit(ctx)
            }
            Control::Remove => self.remove(ctx),
        }
    }

    /// Stepping the previewed record. The total comes from the unfolded page,
    /// exactly as the render reads it.
    fn move_sample(&mut self, delta: i32, ctx: &mut Ctx<'_>) -> Outcome {
        let total = ctx
            .provider
            .page(
                &self.view_id,
                crate::provider::ViewportRequest { start: 0, len: 0 },
            )
            .total;
        if total > 0 {
            self.sample = (self.sample as i64 + delta as i64).clamp(0, total as i64 - 1) as usize;
            self.scroll = 0;
        }
        Outcome::Consumed
    }

    /// Tab: field completions, then sampled literals, then the output pane.
    /// The same three-way cycle `components::editors` runs for Advanced.
    fn toggle_completion(&mut self, ctx: &mut Ctx<'_>) -> Outcome {
        if self.scroll_focused {
            self.scroll_focused = false;
            self.completion = None;
            return Outcome::Consumed;
        }
        // Only the expression field has anything to complete; on a button or a
        // pane the key does nothing, which is what a `None` text target meant.
        if self.control != Control::Expression {
            return Outcome::Consumed;
        }
        let Some(draft) = self.draft(ctx) else {
            return Outcome::Consumed;
        };
        let target = self.target();
        let cursor = ctx.cursors.get_or_end(target.clone(), &draft).char_index;
        let current_kind = self
            .completion
            .as_ref()
            .filter(|state| {
                state.view_id == self.view_id
                    && state.purpose == QueryPurpose::Enrichment
                    && state.draft == draft
                    && state.target == target
                    && state.cursor == cursor
            })
            .map(|state| state.kind);
        if current_kind == Some(EditorCompletionKind::SampledValue) {
            self.completion = None;
            self.scroll_focused = true;
            return Outcome::Consumed;
        }
        let kind = current_kind.map_or(EditorCompletionKind::Field, |_| {
            EditorCompletionKind::SampledValue
        });
        let Some(state) = ctx.views.state(&self.view_id) else {
            return Outcome::Consumed;
        };
        let (items, status) = sample_editor_completion(
            ctx.provider,
            &self.view_id,
            state.top,
            state.viewport_height,
            kind,
        );
        let generation = self.next_completion_generation;
        self.next_completion_generation = generation.saturating_add(1);
        self.completion = Some(EditorCompletionState {
            generation,
            view_id: self.view_id.clone(),
            purpose: QueryPurpose::Enrichment,
            draft,
            target,
            cursor,
            kind,
            items,
            selected: 0,
            top: 0,
            status,
        });
        Outcome::Consumed
    }

    fn move_completion(&mut self, delta: i32) {
        if let Some(completion) = &mut self.completion
            && !completion.items.is_empty()
        {
            completion.selected = (completion.selected as i32 + delta)
                .rem_euclid(completion.items.len() as i32)
                as usize;
            // The popup shows eight rows, so the window follows the selection
            // from whichever edge it left.
            if completion.selected < completion.top {
                completion.top = completion.selected;
            } else if completion.selected >= completion.top + 8 {
                completion.top = completion.selected + 1 - 8;
            }
        }
    }

    /// Fenced against the exact draft and caret it was offered for.
    fn accept_completion(&mut self, ctx: &mut Ctx<'_>) -> Outcome {
        let Some(completion) = self.completion.take() else {
            return Outcome::Consumed;
        };
        let Some(draft) = self.draft(ctx) else {
            return Outcome::Consumed;
        };
        if self.view_id != completion.view_id || self.target() != completion.target {
            return Outcome::Consumed;
        }
        let cursor = ctx
            .cursors
            .get_or_end(completion.target.clone(), &draft)
            .char_index;
        if draft != completion.draft || cursor != completion.cursor {
            return Outcome::Consumed;
        }
        if let Some(item) = completion.items.get(completion.selected) {
            let insertion = item.insertion.clone();
            let _ = self.text(EditCommand::Insert(&insertion), ctx);
            if let Some(editor) = ctx
                .views
                .editor_mut(&self.view_id, QueryPurpose::Enrichment)
            {
                editor.error = None;
            }
        }
        Outcome::Consumed
    }

    /// Arrow keys, which belong to the caret, the popup, the sample or the
    /// output pane depending on what has focus.
    fn vertical(&mut self, delta: i32, ctx: &mut Ctx<'_>) -> Outcome {
        if self.completion.is_some() {
            self.move_completion(delta);
            return Outcome::Consumed;
        }
        if self.scroll_focused {
            self.scroll = self
                .scroll
                .saturating_add_signed(delta as isize)
                .min(self.scroll_limit);
            return Outcome::Consumed;
        }
        match self.control {
            Control::Expression => self.text(
                if delta < 0 {
                    EditCommand::MoveUp
                } else {
                    EditCommand::MoveDown
                },
                ctx,
            ),
            Control::Input => self.move_sample(delta, ctx),
            // Every other control leaves the arrows to the overflowing
            // accepted-output pane, which is where `ScrollDialog` sent them.
            Control::Output | Control::Save | Control::Remove => {
                self.scroll = self
                    .scroll
                    .saturating_add_signed(delta as isize)
                    .min(self.scroll_limit);
                Outcome::Consumed
            }
        }
    }

    fn key(&mut self, key: KeyEvent, ctx: &mut Ctx<'_>) -> Outcome {
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return Outcome::Ignored;
        }
        // Ctrl-Space is the completion, Tab moves the focus ring: the two are
        // separate here because the step editor has five controls to walk.
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            return match key.code {
                KeyCode::Char(' ') => self.toggle_completion(ctx),
                KeyCode::Char('a') => self.text(EditCommand::StartOfLine, ctx),
                KeyCode::Char('e') => self.text(EditCommand::EndOfLine, ctx),
                KeyCode::Char('k') => self.text(EditCommand::KillToEndOfLine, ctx),
                _ => Outcome::Ignored,
            };
        }
        match key.code {
            KeyCode::Tab => self.move_control(if key.modifiers.contains(KeyModifiers::SHIFT) {
                -1
            } else {
                1
            }),
            KeyCode::BackTab => self.move_control(-1),
            KeyCode::Up => self.vertical(-1, ctx),
            KeyCode::Down => self.vertical(1, ctx),
            KeyCode::Left if self.text_editing() && self.completion.is_none() => {
                self.text(EditCommand::MoveLeft, ctx)
            }
            KeyCode::Right if self.text_editing() && self.completion.is_none() => {
                self.text(EditCommand::MoveRight, ctx)
            }
            KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::ALT) => {
                self.text(EditCommand::Insert("\n"), ctx)
            }
            KeyCode::Enter => {
                if self.completion.is_some() {
                    self.accept_completion(ctx)
                } else {
                    self.activate(ctx)
                }
            }
            KeyCode::Backspace => self.text(EditCommand::Backspace, ctx),
            KeyCode::Char(character) if is_typed_char(&key) => {
                let mut buffer = [0u8; 4];
                self.text(EditCommand::Insert(character.encode_utf8(&mut buffer)), ctx)
            }
            _ => Outcome::Ignored,
        }
    }

    fn mouse(&mut self, kind: MouseEventKind, hit: Option<StepHit>, ctx: &mut Ctx<'_>) -> Outcome {
        let pressed = matches!(kind, MouseEventKind::Down(MouseButton::Left));
        if self.completion.is_some() {
            if pressed
                && let Some(StepHit::Completion(index)) = hit
                && let Some(completion) = &mut self.completion
            {
                completion.selected = index;
            }
            match kind {
                MouseEventKind::ScrollUp => self.move_completion(-1),
                MouseEventKind::ScrollDown => self.move_completion(1),
                _ => {}
            }
            return Outcome::Consumed;
        }
        if pressed && let Some(StepHit::Control(control)) = hit {
            self.focus_control(control);
            if matches!(control, Control::Save | Control::Remove) {
                return self.activate(ctx);
            }
            return Outcome::Consumed;
        }
        // Scrolling over the Input pane steps the previewed record whether or
        // not it has the focus ring, which is what `handle_mouse` did.
        let over_input = matches!(hit, Some(StepHit::Control(Control::Input)));
        match kind {
            MouseEventKind::ScrollUp if over_input => self.move_sample(-1, ctx),
            MouseEventKind::ScrollDown if over_input => self.move_sample(1, ctx),
            _ => Outcome::Consumed,
        }
    }
}

fn contains(area: Rect, point: (u16, u16)) -> bool {
    point.0 >= area.x && point.0 < area.right() && point.1 >= area.y && point.1 < area.bottom()
}

impl Component for EnrichmentStepLayer {
    type Hit = StepHit;
    type Open = StepOpen;

    /// Moved verbatim from `App::open_enrichment_step`. The parent decides
    /// which stage (or none) before opening the child, so the child never
    /// reads the parent back (§5.3).
    fn open(&mut self, params: StepOpen, ctx: &mut Ctx<'_>) {
        let StepOpen { editing, prefill } = params;
        let Some(view_id) = ctx.views.active_id().map(str::to_owned) else {
            return;
        };
        let selected = ctx
            .views
            .state(&view_id)
            .and_then(|state| state.selected.clone());
        let sample = selected
            .and_then(|id| ctx.provider.index_of_id(&view_id, &id))
            .unwrap_or(0);
        let stage = editing.as_ref().and_then(|id| {
            ctx.views.state(&view_id).and_then(|state| {
                state
                    .enrichments
                    .iter()
                    .find(|stage| &stage.id == id)
                    .cloned()
            })
        });
        let Some(state) = ctx.views.state_mut(&view_id) else {
            return;
        };
        // A draft is only resumed when it already belongs to the step being
        // opened, so a restored unfinished edit survives but never leaks into
        // another stage or into a brand new step.
        match &stage {
            Some(stage) => {
                if state.enrichment_editing.as_ref() != Some(&stage.id)
                    || state.enrichment.draft.trim().is_empty()
                {
                    state.enrichment.draft = stage.source.clone();
                }
                state.enrichment_editing = Some(stage.id.clone());
            }
            None => {
                if state.enrichment_editing.is_some() {
                    state.enrichment.draft.clear();
                }
                state.enrichment_editing = None;
            }
        }
        // A caller that already knows what the step should say replaces the
        // draft outright. Folding's `[ New column… ]` is the only one; it opens
        // the editor on a concatenation the user then reviews, so the draft it
        // supplies wins over whatever unfinished text the view was holding.
        if let Some(prefill) = prefill {
            state.enrichment.draft = prefill;
        }
        state.enrichment.error = None;
        let draft = state.enrichment.draft.clone();
        ctx.cursors.reset(
            TextTarget {
                identity: view_id.clone(),
                field: "enrichment",
            },
            &draft,
        );
        self.open = true;
        self.view_id = view_id;
        self.control = Control::Expression;
        self.editing = stage.map(|stage| stage.id);
        self.sample = sample;
        self.completion = None;
        self.scroll = 0;
        self.scroll_limit = 0;
        self.scroll_focused = false;
        self.geometry = StepGeometry::default();
        // The first key after `open` must already know whether `q` is a
        // character; geometry can wait for the first frame, focus cannot.
        self.surface = Surface {
            text_focus: true,
            ..Surface::default()
        };
    }

    fn handle(&mut self, event: Event<StepHit>, ctx: &mut Ctx<'_>) -> Outcome {
        match event {
            Event::Key(key) => self.key(key, ctx),
            Event::Paste(text) => self.text(EditCommand::Insert(&text), ctx),
            Event::Mouse { kind, hit, .. } => self.mouse(kind, hit, ctx),
            // §5.3: Escape closes the innermost thing first, so an open
            // completion popup absorbs it rather than the whole layer.
            Event::Dismiss => {
                if self.completion.take().is_some() {
                    return Outcome::Consumed;
                }
                self.cancel(ctx);
                Outcome::Close
            }
            // §4.2: what used to be `App`'s `close_enrichment_step` flag. A
            // saved step returns the user to the layer-one list, and the
            // layer decides that for itself from an event fenced on its view.
            Event::View(ViewEvent::QueryAccepted {
                view_id,
                purpose: QueryPurpose::Enrichment,
                ..
            }) if view_id == self.view_id => {
                self.completion = None;
                self.scroll = 0;
                self.scroll_focused = false;
                self.open = false;
                Outcome::Close
            }
            Event::Command(CommandId::EditorCompletion) => self.toggle_completion(ctx),
            Event::View(_) | Event::Command(_) | Event::Resize => Outcome::Ignored,
        }
    }

    /// §4.3: the completion is the shared catalog row `components::editors`
    /// also contributes; whichever of the two is open takes the row over.
    fn commands(&self, _views: &Views) -> Vec<CommandEntry> {
        vec![CommandEntry {
            spec: CommandSpec {
                id: CommandId::EditorCompletion,
                name: "Complete editor field or value",
                description: "Insert a sampled field expression or lexical string without applying",
                category: "Filter",
                aliases: &["autocomplete", "field picker", "sampled value"],
                shortcut: self.open.then_some("Ctrl-Space"),
            },
            unavailable_reason: (!self.open)
                .then_some("open Filter › Advanced or Enrichment first"),
        }]
    }

    fn surface(&self) -> Surface {
        Surface {
            text_focus: self.text_editing() && self.completion.is_none(),
            ..self.surface
        }
    }

    fn hit(&self, point: (u16, u16)) -> Option<StepHit> {
        // The popup is drawn last and over everything, so it takes the point
        // first — and while it is open nothing under it is clickable.
        if self.completion.is_some() {
            return self
                .geometry
                .completion
                .iter()
                .find_map(|(rect, index)| {
                    contains(*rect, point).then_some(StepHit::Completion(*index))
                })
                .or(Some(StepHit::Body));
        }
        self.geometry
            .controls
            .iter()
            .find_map(|(rect, control)| {
                contains(*rect, point).then_some(StepHit::Control(*control))
            })
            .or_else(|| contains(self.surface.popup, point).then_some(StepHit::Body))
    }

    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &RenderCtx<'_>) -> Surface {
        render_enrichment_step(self, frame, area, ctx)
    }
}

/// Moved verbatim from `ui::render_enrichment_step`. The parent pass and the
/// scrim it used to do itself are the shell's stack loop now (§5.3).
///
/// Stable LongContent budgets: outer size is policy-only, never the draft or
/// preview length, so every keystroke shares one `frame` and sticky tail
/// origins. `body_content_rows` sizes only the scroll extent. Hand-rolled row
/// assignment below is presentation-only folding, never query membership
/// (AGENTS.md).
fn step_spec(area: Rect) -> DialogSpec {
    let (policy_w, _) = crate::dialog_layout::policy_size(area, PresentationKind::LongContent);
    let estimate = policy_w.saturating_sub(4).max(1);
    let actions = stable_action_rows(estimate, &STEP_MAX_LABELS).clamp(1, 2);
    DialogSpec::new(PresentationKind::LongContent, 0, 3, 2, 1, actions)
}

/// The stable maximum action set, so the budget never moves between adding
/// (Save alone) and editing (Save + Remove).
const STEP_MAX_LABELS: [&str; 2] = ["Save", "Remove"];
fn render_enrichment_step(
    this: &mut EnrichmentStepLayer,
    frame: &mut Frame<'_>,
    area: Rect,
    ctx: &RenderCtx<'_>,
) -> Surface {
    let theme = ctx.theme;
    let styles = DialogStyles::new(theme);
    let mut geometry = StepGeometry::default();
    let dialog = StepView {
        view_id: this.view_id.clone(),
        control: this.control,
        sample: this.sample,
    };
    let Some(state) = ctx.views.state(&dialog.view_id) else {
        return this.record(geometry, Surface::default());
    };
    let editor = state.enrichment.clone();
    // The caret is the bank's, clamped to the draft it points into — which is
    // what `App::active_text_cursor` returned, `None` included, while the
    // expression field did not have the keys.
    let cursor = this
        .text_editing()
        .then(|| ctx.cursors.peek(&this.target(), &editor.draft))
        .flatten();
    let editing_index = this
        .editing
        .as_ref()
        .and_then(|id| state.enrichments.iter().position(|stage| &stage.id == id));
    let accepted_source = editing_index
        .and_then(|index| state.enrichments.get(index))
        .map(|stage| stage.source.clone());
    let unsaved_draft = accepted_source.as_ref().map_or_else(
        || !editor.draft.trim().is_empty(),
        |source| *source != editor.draft,
    );

    // §10/§5.3: the shell drew the parent under one more scrim pass before it
    // reached this layer. In a compact terminal the child takes the parent's
    // frame (the shell skips the parent; the breadcrumb keeps the context);
    // otherwise it resolves against an inset viewport so the scrimmed parent
    // frame stays visible behind it. No parent geometry is computed here.
    let child_area = if is_compact(area) {
        area
    } else {
        Rect::new(
            area.x.saturating_add(3),
            area.y.saturating_add(1),
            area.width.saturating_sub(6),
            area.height.saturating_sub(2),
        )
    };
    let title = if editing_index.is_some() {
        "Enrichment › Edit step".to_owned()
    } else {
        "Enrichment › New step".to_owned()
    };
    let labels: Vec<&str> = if editing_index.is_some() {
        vec!["Save", "Remove"]
    } else {
        vec!["Save"]
    };
    let controls: Vec<Control> = if editing_index.is_some() {
        vec![Control::Save, Control::Remove]
    } else {
        vec![Control::Save]
    };

    let (message_state, sentence) = if let Some(error) = &editor.error {
        (
            MessageState::Error,
            format!("{error} · every accepted step is retained"),
        )
    } else if editor.pending_generation.is_some() || editor.fork_pending {
        (
            MessageState::Updating,
            "Evaluating this step · the accepted chain stays active".to_owned(),
        )
    } else if editing_index.is_some() {
        (
            MessageState::Ready,
            "saving replaces this accepted step".to_owned(),
        )
    } else {
        (
            MessageState::Ready,
            "saving appends this step after the accepted ones".to_owned(),
        )
    };

    // §5.2: measure the real content — the previewed record and its outputs —
    // before choosing a height, so the dialog never pads itself to a shape.
    // The step editor previews the record the expression reads, so it reads the
    // unfolded stream: a collapsed run is presentation, not an input row.
    let page = ctx.provider.unfolded_page(
        &dialog.view_id,
        crate::provider::ViewportRequest {
            start: dialog.sample,
            len: 1,
        },
    );
    let total = page.total;
    let sample = page.rows.into_iter().next();
    let mut input_lines: Vec<String> = Vec::new();
    let mut output_lines: Vec<String> = Vec::new();
    match &sample {
        Some(row) => {
            input_lines.push(row.text.clone());
            if !row.fields.is_empty() {
                input_lines.push(format!(
                    "Fields  {}",
                    row.fields
                        .iter()
                        .map(|field| field.0.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            for (name, value) in row
                .details
                .iter()
                .filter(|(name, _)| name.starts_with("derived."))
            {
                output_lines.push(format!("{}  {value}", name.trim_start_matches("derived.")));
            }
            if output_lines.is_empty() {
                output_lines.push("No accepted outputs yet".to_owned());
            }
            if unsaved_draft {
                output_lines.push("Save this step to evaluate the draft above".to_owned());
            }
        }
        None => {
            input_lines.push("No record available to preview".to_owned());
            output_lines.push("No output to show".to_owned());
        }
    }

    let (policy_w, _) =
        crate::dialog_layout::policy_size(child_area, PresentationKind::LongContent);
    let estimate = policy_w.saturating_sub(4).max(1);
    let side_by_side = estimate >= 72;
    // The field grows with the wrapped draft up to its cap (§8.1); measure it
    // exactly the way the input renders it.
    let expression_rows = crate::text_edit::wrapped_text(
        &editor.draft,
        usize::from(estimate.saturating_sub(12).max(1)),
    )
    .lines
    .len()
    .clamp(1, EXPRESSION_ROW_CAP as usize) as u16;
    let input_rows = 1 + input_lines.len().clamp(1, 4) as u16;
    // §5.2.1: the output pane gains its "save this step" line the moment the
    // draft diverges from the accepted expression, so the row is reserved
    // whether or not it is shown. Otherwise the first character typed into a
    // saved step moved the whole dialog by a row.
    let output_rows = 1 + output_lines
        .len()
        .saturating_add(usize::from(!unsaved_draft))
        .clamp(1, 4) as u16;
    let preview_rows = if side_by_side {
        input_rows.max(output_rows)
    } else {
        input_rows + output_rows
    };
    let help = "name = expression  or  /regex with (?P<name>…) groups/";
    // §5.2.1: the field reserves its cap rows so the preview below sits at a
    // reserved offset on every keystroke; a compact terminal still spends its
    // rows on the preview panes first through the shared body viewport.
    let field_rows: u16 = EXPRESSION_ROW_CAP + 1;
    let content_rows = usize::from(field_rows + 1 + preview_rows);
    let spec = step_spec(child_area);
    let Ok(resolved) = resolve_dialog(child_area, &spec, content_rows, &labels, Some(0), None)
    else {
        // Below the 20x6 floor the tiny fallback owns the frame; stay open
        // with nothing drawn, as the palette does.
        return this.record(geometry, Surface::default());
    };
    render_responsive_frame(frame, &resolved, &title, true, theme);
    let mut surface = Surface {
        popup: resolved.frame,
        interior: resolved.interior,
        scrollable: resolved.body.overflow() > 0,
        ..Surface::default()
    };

    let body = resolved.body.viewport;
    if body.height == 0 || body.width == 0 {
        return this.record(geometry, surface);
    }

    // §4.2 two-column form row for the expression.
    let label_w = UnicodeWidthStr::width("Expression") as u16;
    let stacked = body.width < label_w + 2 + 20;
    let (label_rect, field_rect) = if stacked {
        (
            Rect::new(body.x, body.y, body.width, 1),
            Rect::new(
                body.x,
                body.y.saturating_add(1),
                body.width,
                body.height
                    .saturating_sub(1)
                    .min(expression_rows.min(field_rows))
                    .max(1),
            ),
        )
    } else {
        (
            Rect::new(body.x, body.y, label_w, 1),
            Rect::new(
                body.x.saturating_add(label_w).saturating_add(2),
                body.y,
                body.width.saturating_sub(label_w + 2),
                body.height.min(expression_rows.min(field_rows)).max(1),
            ),
        )
    };
    let focused_expression = dialog.control == Control::Expression;
    frame.render_widget(
        Paragraph::new("Expression").style(if focused_expression {
            styles.shortcut
        } else {
            styles.label
        }),
        label_rect,
    );
    if field_rect.width > 0 && field_rect.height > 0 {
        InputSurface {
            style: styles.input,
        }
        .render(field_rect, frame.buffer_mut());
        geometry.controls.push((field_rect, Control::Expression));
        let wrapped = crate::text_edit::wrapped_text(&editor.draft, usize::from(field_rect.width));
        let (cursor_row, cursor_column) = cursor.map_or((0, 0), |cursor| {
            let mut logical = crate::text_edit::TextCursor { char_index: cursor };
            crate::text_edit::wrapped_cursor(
                &editor.draft,
                &mut logical,
                usize::from(field_rect.width),
            )
        });
        let top = cursor_row
            .saturating_add(1)
            .saturating_sub(usize::from(field_rect.height));
        let visible = wrapped.lines.get(top..).unwrap_or(&[]).join("\n");
        frame.render_widget(Paragraph::new(visible).style(styles.input), field_rect);
        if focused_expression && this.completion.is_none() {
            let x = field_rect.x + cursor_column.min(usize::from(field_rect.width - 1)) as u16;
            let y = field_rect.y + cursor_row.saturating_sub(top) as u16;
            frame.buffer_mut()[(x, y)]
                .set_style(Style::default().bg(theme.cursor).fg(theme.input_fg));
            frame.set_cursor_position((x, y));
            surface.caret = Some((x, y));
        }
    }

    // Input record and accepted output panes. §5.2.1: anchored to the field's
    // reserved rows, not to the rows the draft happens to occupy, so a draft
    // that wraps to a second line does not shift the preview under it.
    let preview_y = body
        .y
        .saturating_add(if stacked { 1 } else { 0 })
        .saturating_add(field_rows)
        .saturating_add(1)
        .max(field_rect.bottom().saturating_add(1))
        .min(body.bottom());
    if preview_y >= body.bottom() {
        render_enrichment_step_tail(
            frame,
            &mut geometry,
            ctx,
            &resolved,
            message_state,
            sentence,
            &labels,
            &controls,
            dialog.control,
            help,
        );
        return this.record(geometry, surface);
    }
    let preview = Rect::new(
        body.x,
        preview_y,
        body.width,
        body.bottom().saturating_sub(preview_y),
    );
    let (input_area, output_area) = if side_by_side && preview.width >= 72 {
        let split = Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
            .spacing(2)
            .split(preview);
        (split[0], split[1])
    } else {
        let top = input_rows
            .min(preview.height.saturating_sub(2))
            .max(preview.height.min(2));
        (
            Rect::new(preview.x, preview.y, preview.width, top),
            Rect::new(
                preview.x,
                preview.y.saturating_add(top),
                preview.width,
                preview.height.saturating_sub(top),
            ),
        )
    };

    let input_pane = render_pane_heading(
        frame,
        input_area,
        "Input record",
        (total > 0).then(|| format!("{} of {total}", dialog.sample.saturating_add(1).min(total))),
        input_lines.len(),
        theme,
    );
    if !input_pane.viewport.is_empty() {
        geometry.controls.push((input_area, Control::Input));
        let focused = dialog.control == Control::Input;
        frame.render_widget(
            Paragraph::new(
                input_lines
                    .iter()
                    .map(|line| {
                        Line::styled(
                            truncated(line, usize::from(input_pane.viewport.width)),
                            if focused {
                                styles.selection
                            } else {
                                styles.description
                            },
                        )
                    })
                    .collect::<Vec<_>>(),
            ),
            input_pane.viewport,
        );
        // The heading count is the list affordance; a scrollbar only earns its
        // column when it can show arrows and a thumb.
        if let Some(bar) = input_pane.scrollbar.filter(|bar| bar.height >= 3) {
            render_scrollbar(
                frame,
                bar,
                dialog.sample,
                total.saturating_sub(1),
                theme,
                ctx.ascii,
            );
        }
    }

    let output_pane = render_pane_heading(
        frame,
        output_area,
        "Accepted output",
        None,
        output_lines.len(),
        theme,
    );
    if !output_pane.viewport.is_empty() {
        geometry.controls.push((output_area, Control::Output));
        let visible = usize::from(output_pane.viewport.height);
        let limit = output_lines.len().saturating_sub(visible);
        this.scroll_limit = limit;
        this.scroll = this.scroll.min(limit);
        surface.scrollable = surface.scrollable || limit > 0;
        frame.render_widget(
            Paragraph::new(
                output_lines
                    .iter()
                    .skip(this.scroll)
                    .map(|line| {
                        Line::styled(
                            truncated(line, usize::from(output_pane.viewport.width)),
                            styles.description,
                        )
                    })
                    .collect::<Vec<_>>(),
            ),
            output_pane.viewport,
        );
        if let Some(bar) = output_pane.scrollbar {
            render_scrollbar(frame, bar, this.scroll, limit, theme, ctx.ascii);
        }
    }

    render_enrichment_step_tail(
        frame,
        &mut geometry,
        ctx,
        &resolved,
        message_state,
        sentence,
        &labels,
        &controls,
        dialog.control,
        help,
    );
    // Anchored class A: the completion popup is placed from the actual painted
    // field rect through shared `anchored_geometry` (below preferred with a
    // one-row gap, above when below cannot fit, edge-clamped, scrolling past
    // eight rows with a status footer), bounded by the full render area so it
    // may overhang its parent. One geometry drives its paint, row hitboxes
    // and scrollbar; no centered fallback and no manual popup rect. While a
    // popup is drawn it owns the text selection bound; when the field painted
    // nothing no popup is drawn rather than anchoring to a stale rect.
    if let Some(completion) = &this.completion
        && !field_rect.is_empty()
    {
        let (popup, rows, overflows) =
            draw_editor_completion_anchored(frame, area, field_rect, completion, theme, ctx.ascii);
        // A refused placement (empty popup: the field's bands hold no
        // gap-honoring popup) leaves frame, selection bound, caret and wheel
        // interest exactly as the body render left them.
        if !popup.is_empty() {
            surface.popup = surface.popup.union(popup);
            surface.interior = popup.inner(ratatui::layout::Margin::new(1, 1));
            surface.caret = None;
            surface.scrollable = surface.scrollable || overflows;
            geometry.completion = rows;
        }
    }
    this.record(geometry, surface)
}

/// The fields of the old `EnrichmentStepDialog` the render reads, snapshotted
/// so the borrow of `this` ends before `ctx` is read.
struct StepView {
    view_id: String,
    control: Control,
    sample: usize,
}

#[allow(clippy::too_many_arguments)]
fn render_enrichment_step_tail(
    frame: &mut Frame<'_>,
    geometry: &mut StepGeometry,
    ctx: &RenderCtx<'_>,
    resolved: &crate::dialog_layout::DialogGeometry,
    message_state: MessageState,
    sentence: String,
    labels: &[&str],
    controls: &[Control],
    focused: Control,
    help: &str,
) {
    let theme = ctx.theme;
    render_message(
        frame,
        resolved.message,
        message_state,
        &sentence,
        theme,
        ctx.ascii,
    );
    render_help_text(frame, resolved.help, help, theme);
    // One shared action geometry drives paint and hitboxes: the filled
    // default (§8.9, always Save here) plus the drawn rects the mouse handler
    // is given, so click and paint cannot disagree.
    let focused_index = controls.iter().position(|control| *control == focused);
    for (index, hit) in &resolved.actions.buttons {
        geometry.controls.push((*hit, controls[*index]));
        render_role_button(
            frame,
            *hit,
            labels[*index],
            if resolved.actions.default == Some(*index) {
                ButtonRole::Default
            } else {
                ButtonRole::Normal
            },
            focused_index == Some(*index),
            theme,
        );
    }
}
