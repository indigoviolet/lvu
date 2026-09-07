//! The three query editors — Search (`/`), Advanced filter (`p`) and multiline
//! Grouping (`m`) — converted to the component contract per
//! `docs/component-model.md` §6.3 step 7.
//!
//! One type, three permanent slots. They are the same dialog with a different
//! `QueryPurpose`: the same keymap, the same caret rules, the same submit path
//! and the same message row, differing only in title, help, placeholder and
//! what the body draws beside the field. Splitting them into three types would
//! have duplicated all of that to express three constants.
//!
//! This is the first conversion whose drafts are *view*-owned rather than
//! dialog-owned (§2.5). `ViewState.{search,advanced,grouping}` keeps the draft,
//! the last accepted value, the error and the pending fence; the caret lives in
//! `ctx.cursors` under the same `TextTarget` the shell used. Nothing here
//! caches a copy: the layer renders from `ctx.views.active()` every frame, so a
//! query completion landing in `ViewState` reaches the screen with no event.
//!
//! The two product invariants this layer carries (AGENTS.md):
//!
//! * **An invalid draft leaves the last applied view usable.** The draft and
//!   the accepted value are separate fields; a refusal writes `editor.error`
//!   and touches neither `applied` nor the rows on screen.
//! * **A canonical view forks instead of filtering in place.** `Views::enqueue`
//!   refuses with `SubmitRefused::DefinitionFixed` and the layer hands the work
//!   back with `Outcome::Defer`, which keeps it open — staging the fork is the
//!   shell's until forking converts (§2.3). Live debounced search never forks;
//!   it goes through the shell's `enqueue_live_query`, which is why the layer
//!   only *arms* the debounce and never fires it.

use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use ratatui::{Frame, layout::Rect, style::Modifier, widgets::Paragraph};

use crate::app::{
    Action, EditorCompletionKind, EditorCompletionState, EditorState, MAX_EDITOR_BYTES,
    QueryPurpose, SubmitRefused, Views, sample_editor_completion,
};
use crate::command_palette::CommandId;
use crate::component::{
    CommandEntry, CommandSpec, Component, Ctx, Event, Outcome, RenderCtx, Surface, ViewEvent,
};
use crate::dialog_controls::DialogStyles;
use crate::text_edit::{EditCommand, EditPolicy, TextTarget, edit};
use crate::ui::{
    InputSurface, MESSAGE_SENTENCE_COLUMN, MessageState, dialog_frame_regions,
    draw_editor_completion, help_rows, message_rows, packed_button_rows, place_input_cursor_at,
    render_action_row, render_help_text, render_message, render_placeholder, render_scrollbar,
    truncated, wrap_sentence,
};
use ratatui::widgets::Widget;

/// §12.3. The preview pane is fixed content, so it is measured, not guessed.
const GROUPING_PREVIEW: [&str; 2] = ["RuntimeException: boom", "  at worker.rs:42"];

/// Everything an editor draws that can be clicked.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EditorHit {
    /// A row of the completion popup.
    Completion(usize),
    /// The drawn `[ Apply ]` button, which only Grouping has.
    Apply,
    /// The overflowing-diagnostic pane.
    Diagnostics,
    Body,
}

/// Recorded by `render`, consumed by `hit()` and by the scroll logic (§5.1).
#[derive(Clone, Debug, Default)]
struct EditorGeometry {
    body: Rect,
    completion: Vec<(Rect, usize)>,
    actions: Vec<Rect>,
    diagnostics: Option<Rect>,
}

#[derive(Debug)]
pub struct EditorDialog {
    /// Which of the three this slot is. Set once, at construction.
    purpose: QueryPurpose,
    open: bool,
    /// Body scroll for the overflowing-diagnostic pane, and whether Tab has
    /// handed the arrow keys to it. Both were `App::dialog_scroll*`, shell
    /// state three dialogs shared by accident of focus.
    scroll: usize,
    scroll_limit: usize,
    scroll_focused: bool,
    /// The completion popup, which only Advanced offers (§12.2). Its state is
    /// fenced against the draft and caret it was built for, so accepting a
    /// stale entry is impossible.
    completion: Option<EditorCompletionState>,
    next_completion_generation: u64,
    geometry: EditorGeometry,
    surface: Surface,
}

impl EditorDialog {
    pub fn new(purpose: QueryPurpose) -> Self {
        Self {
            purpose,
            open: false,
            scroll: 0,
            scroll_limit: 0,
            scroll_focused: false,
            completion: None,
            next_completion_generation: 1,
            geometry: EditorGeometry::default(),
            surface: Surface::default(),
        }
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn purpose(&self) -> QueryPurpose {
        self.purpose
    }

    pub fn scroll(&self) -> usize {
        self.scroll
    }

    pub fn scroll_limit(&self) -> usize {
        self.scroll_limit
    }

    pub fn scroll_focused(&self) -> bool {
        self.scroll_focused
    }

    pub fn completion(&self) -> Option<&EditorCompletionState> {
        self.completion.as_ref()
    }

    pub fn completion_rects(&self) -> &[(Rect, usize)] {
        &self.geometry.completion
    }

    pub fn action_rects(&self) -> &[Rect] {
        &self.geometry.actions
    }

    /// The overflowing-diagnostic pane, when the message did not fit. `None`
    /// means the layer claims no scroll affordance (§9).
    pub fn diagnostics_rect(&self) -> Option<Rect> {
        self.geometry.diagnostics
    }

    /// The caret target for this view's draft. View-owned, so it is the bank's
    /// (§2.2) and survives closing and reopening the dialog.
    fn target(&self, view_id: &str) -> TextTarget {
        TextTarget {
            identity: view_id.to_owned(),
            field: match self.purpose {
                QueryPurpose::Search => "search",
                QueryPurpose::Grouping => "grouping",
                _ => "advanced",
            },
        }
    }

    /// Whether the field, rather than the diagnostics pane, has the keys. This
    /// is what `App::active_text_target` returned `None` for when
    /// `dialog_scroll_focused` was set.
    fn text_editing(&self) -> bool {
        !self.scroll_focused
    }

    fn draft(&self, ctx: &Ctx<'_>) -> Option<(String, String)> {
        let view_id = ctx.views.active_id()?.to_owned();
        let draft = ctx.views.editor(&view_id, self.purpose)?.draft.clone();
        Some((view_id, draft))
    }

    fn policy(&self) -> EditPolicy {
        EditPolicy {
            max_bytes: MAX_EDITOR_BYTES,
            multiline: self.purpose != QueryPurpose::Search,
        }
    }

    /// One editing verb against the view-owned draft and the bank-owned caret.
    /// The two are read and written in sequence, never held at once (§2.5).
    fn text(&mut self, command: EditCommand<'_>, ctx: &mut Ctx<'_>) -> Outcome {
        if !self.text_editing() {
            return Outcome::Ignored;
        }
        let Some((view_id, mut value)) = self.draft(ctx) else {
            return Outcome::Ignored;
        };
        let target = self.target(&view_id);
        let mut cursor = ctx.cursors.get_or_end(target.clone(), &value);
        let outcome = edit(&mut value, &mut cursor, command, self.policy());
        ctx.cursors.store(target, cursor);
        if outcome.changed {
            if let Some(editor) = ctx.views.editor_mut(&view_id, self.purpose) {
                editor.draft = value;
            }
            ctx.views.touch(&view_id);
            // A changed draft invalidates the popup it was offered against.
            self.completion = None;
            if self.purpose == QueryPurpose::Search {
                ctx.views.schedule_search(&view_id);
            }
        }
        if outcome.changed || outcome.moved {
            Outcome::Consumed
        } else {
            Outcome::Ignored
        }
    }

    /// Apply the draft. The refusal paths are the invariant: a full queue keeps
    /// the draft and says so, and a fixed definition becomes a derived view
    /// rather than a filter applied to All events.
    fn submit(&mut self, ctx: &mut Ctx<'_>) -> Outcome {
        let Some(view_id) = ctx.views.active_id().map(str::to_owned) else {
            return Outcome::Consumed;
        };
        if self.purpose == QueryPurpose::Search {
            // An explicit apply supersedes the armed debounce; leaving it would
            // fire a second, identical query a moment later.
            ctx.views.cancel_scheduled_search(&view_id);
        }
        ctx.views.touch(&view_id);
        match ctx.views.enqueue(&view_id, self.purpose, None) {
            // A full queue is worded in the editor's own message row by the
            // seam, which kept the draft; nothing else to do but redraw.
            Ok(_) | Err(SubmitRefused::QueueFull) => Outcome::Consumed,
            // Staging the derived view is the shell's (§2.3). `Defer` runs it
            // without popping this layer, because applying a filter on All
            // events leaves the user in the editor, on the view it created.
            Err(SubmitRefused::DefinitionFixed) => {
                Outcome::Defer(Action::StageEditorFork(self.purpose))
            }
        }
    }

    /// Tab. On Search and Grouping there is nothing to complete, so it hands
    /// the arrows to the diagnostics pane instead; on Advanced it cycles
    /// field completions, sampled literals, and then the pane.
    fn toggle_completion(&mut self, ctx: &mut Ctx<'_>) -> Outcome {
        if self.scroll_focused {
            self.scroll_focused = false;
            self.completion = None;
            return Outcome::Consumed;
        }
        if self.purpose != QueryPurpose::Advanced {
            self.scroll_focused = true;
            return Outcome::Consumed;
        }
        let Some((view_id, draft)) = self.draft(ctx) else {
            return Outcome::Consumed;
        };
        let target = self.target(&view_id);
        let cursor = ctx.cursors.get_or_end(target.clone(), &draft).char_index;
        let current_kind = self
            .completion
            .as_ref()
            .filter(|state| {
                state.view_id == view_id
                    && state.purpose == self.purpose
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
        let Some(state) = ctx.views.active() else {
            return Outcome::Consumed;
        };
        let (items, status) = sample_editor_completion(
            ctx.provider,
            &view_id,
            state.top,
            state.viewport_height,
            kind,
        );
        let generation = self.next_completion_generation;
        self.next_completion_generation = generation.saturating_add(1);
        self.completion = Some(EditorCompletionState {
            generation,
            view_id,
            purpose: self.purpose,
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
            completion.top = completion.top.min(completion.selected);
        }
    }

    /// Inserting a completion is fenced against the exact draft and caret it
    /// was offered for; anything else typed since makes it stale and it is
    /// dropped rather than inserted somewhere it no longer fits.
    fn accept_completion(&mut self, ctx: &mut Ctx<'_>) -> Outcome {
        let Some(completion) = self.completion.take() else {
            return Outcome::Consumed;
        };
        let Some((view_id, draft)) = self.draft(ctx) else {
            return Outcome::Consumed;
        };
        if view_id != completion.view_id || self.target(&view_id) != completion.target {
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
            if let Some(editor) = ctx.views.editor_mut(&view_id, self.purpose) {
                editor.error = None;
            }
        }
        Outcome::Consumed
    }

    fn scroll_body(&mut self, delta: i32) {
        self.scroll = self
            .scroll
            .saturating_add_signed(delta as isize)
            .min(self.scroll_limit);
    }

    /// Arrow keys, which belong to the caret, the popup or the pane depending
    /// on what has focus. `App::key_to_action` decided this from
    /// `is_text_editing`/`editor_completion`; the component has both to hand.
    fn vertical(&mut self, delta: i32, ctx: &mut Ctx<'_>) -> Outcome {
        if self.text_editing() && self.completion.is_none() {
            return self.text(
                if delta < 0 {
                    EditCommand::MoveUp
                } else {
                    EditCommand::MoveDown
                },
                ctx,
            );
        }
        if self.scroll_focused {
            self.scroll_body(delta);
            return Outcome::Consumed;
        }
        if self.completion.is_some() {
            self.move_completion(delta);
            return Outcome::Consumed;
        }
        Outcome::Consumed
    }

    fn key(&mut self, key: KeyEvent, ctx: &mut Ctx<'_>) -> Outcome {
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return Outcome::Ignored;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            return match key.code {
                KeyCode::Char('a') => self.text(EditCommand::StartOfLine, ctx),
                KeyCode::Char('e') => self.text(EditCommand::EndOfLine, ctx),
                KeyCode::Char('k') => self.text(EditCommand::KillToEndOfLine, ctx),
                _ => Outcome::Ignored,
            };
        }
        match key.code {
            KeyCode::Tab => self.toggle_completion(ctx),
            KeyCode::Up => self.vertical(-1, ctx),
            KeyCode::Down => self.vertical(1, ctx),
            KeyCode::Left if self.text_editing() && self.completion.is_none() => {
                self.text(EditCommand::MoveLeft, ctx)
            }
            KeyCode::Right if self.text_editing() && self.completion.is_none() => {
                self.text(EditCommand::MoveRight, ctx)
            }
            KeyCode::Enter if key.modifiers.contains(KeyModifiers::ALT) => {
                self.text(EditCommand::Insert("\n"), ctx)
            }
            KeyCode::Enter => {
                if self.completion.is_some() {
                    self.accept_completion(ctx)
                } else {
                    self.submit(ctx)
                }
            }
            KeyCode::Backspace => self.text(EditCommand::Backspace, ctx),
            KeyCode::Char(character) => {
                let mut buffer = [0u8; 4];
                self.text(EditCommand::Insert(character.encode_utf8(&mut buffer)), ctx)
            }
            _ => Outcome::Ignored,
        }
    }

    fn mouse(
        &mut self,
        kind: MouseEventKind,
        hit: Option<EditorHit>,
        ctx: &mut Ctx<'_>,
    ) -> Outcome {
        let pressed = matches!(kind, MouseEventKind::Down(MouseButton::Left));
        if self.completion.is_some() {
            if pressed
                && let Some(EditorHit::Completion(index)) = hit
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
        // The drawn `[ Apply ]` button submits the draft exactly as Enter does;
        // the rect comes from the same layout that painted it.
        if pressed && hit == Some(EditorHit::Apply) {
            return self.submit(ctx);
        }
        Outcome::Consumed
    }
}

impl Default for EditorDialog {
    fn default() -> Self {
        Self::new(QueryPurpose::Search)
    }
}

fn contains(area: Rect, point: (u16, u16)) -> bool {
    point.0 >= area.x && point.0 < area.right() && point.1 >= area.y && point.1 < area.bottom()
}

impl Component for EditorDialog {
    type Hit = EditorHit;
    type Open = ();

    fn open(&mut self, _params: (), ctx: &mut Ctx<'_>) {
        self.open = true;
        self.scroll = 0;
        self.scroll_focused = false;
        self.completion = None;
        self.geometry = EditorGeometry::default();
        // §12.3: an unset grouping rule opens on the one that matches the
        // common indented-continuation shape rather than on an empty field.
        if self.purpose == QueryPurpose::Grouping
            && let Some(view_id) = ctx.views.active_id().map(str::to_owned)
            && let Some(editor) = ctx.views.editor_mut(&view_id, QueryPurpose::Grouping)
            && editor.draft.is_empty()
            && editor.applied.is_empty()
        {
            editor.draft = r"^(\s+|Caused by:)".into();
        }
    }

    fn handle(&mut self, event: Event<EditorHit>, ctx: &mut Ctx<'_>) -> Outcome {
        match event {
            Event::Key(key) => self.key(key, ctx),
            Event::Paste(text) => self.text(EditCommand::Insert(&text), ctx),
            Event::Mouse { kind, hit, .. } => self.mouse(kind, hit, ctx),
            // §5.3: Escape closes the innermost thing first, so an open
            // completion popup absorbs it rather than the whole dialog.
            Event::Dismiss => {
                if self.completion.take().is_some() {
                    return Outcome::Consumed;
                }
                self.open = false;
                Outcome::Close
            }
            // §4.2: the layer renders its accepted value straight out of
            // `ViewState`, so a completion needs no reaction beyond a redraw.
            // A view whose sources changed underneath it invalidates a popup
            // sampled from the rows that view no longer has.
            Event::View(ViewEvent::SourcesChanged { .. }) => {
                self.completion = None;
                Outcome::Consumed
            }
            Event::Command(CommandId::EditorCompletion) => self.toggle_completion(ctx),
            Event::Command(_) | Event::View(_) | Event::Resize => Outcome::Ignored,
        }
    }

    /// §4.3: Advanced offers completion, so the shared catalog row routes to
    /// this layer while it is open. Search and Grouping have nothing to
    /// complete and contribute nothing.
    fn commands(&self, _views: &Views) -> Vec<CommandEntry> {
        if self.purpose != QueryPurpose::Advanced {
            return Vec::new();
        }
        vec![CommandEntry {
            spec: CommandSpec {
                id: CommandId::EditorCompletion,
                name: "Complete editor field or value",
                description: "Insert a sampled field expression or lexical string without applying",
                category: "Filter",
                aliases: &["autocomplete", "field picker", "sampled value"],
                shortcut: self.open.then_some("Tab"),
            },
            unavailable_reason: (!self.open).then_some("open Advanced filter or Enrichment first"),
        }]
    }

    /// Geometry is the last render's, but `text_focus` is not geometry: it is
    /// what decides whether `q` is a character, and a layer that has not been
    /// painted yet still knows the answer. Deriving it here keeps the first
    /// keystroke after `open` from being read as a dismissal.
    fn surface(&self) -> Surface {
        Surface {
            text_focus: self.text_editing() && self.completion.is_none(),
            ..self.surface
        }
    }

    fn hit(&self, point: (u16, u16)) -> Option<EditorHit> {
        let g = &self.geometry;
        // The popup is drawn last and over everything, so it takes the point
        // first — and while it is open nothing under it is clickable.
        if self.completion.is_some() {
            return g
                .completion
                .iter()
                .find_map(|(rect, index)| {
                    contains(*rect, point).then_some(EditorHit::Completion(*index))
                })
                .or(Some(EditorHit::Body));
        }
        g.actions
            .iter()
            .find_map(|rect| contains(*rect, point).then_some(EditorHit::Apply))
            .or_else(|| {
                g.diagnostics
                    .filter(|rect| contains(*rect, point))
                    .map(|_| EditorHit::Diagnostics)
            })
            .or_else(|| contains(g.body, point).then_some(EditorHit::Body))
    }

    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &RenderCtx<'_>) -> Surface {
        let theme = ctx.theme;
        let Some(view_id) = ctx.views.active_id().map(str::to_owned) else {
            self.surface = Surface::default();
            return self.surface;
        };
        let Some(editor) = ctx.views.editor(&view_id, self.purpose).cloned() else {
            self.surface = Surface::default();
            return self.surface;
        };
        // The caret is the bank's, and it is clamped to the draft it points
        // into, so a draft replaced by a completion can never leave it past
        // the end.
        let cursor = ctx
            .cursors
            .peek(&self.target(&view_id), &editor.draft)
            .unwrap_or_else(|| editor.draft.chars().count());
        let (mut surface, actions, diagnostics) = if self.purpose == QueryPurpose::Grouping {
            self.render_grouping(frame, area, &editor, cursor, ctx)
        } else {
            self.render_simple(frame, area, &editor, cursor, ctx)
        };
        // §5.2: the popup is part of what this layer drew, so containment is
        // against the union; and while it is open it owns the text selection
        // bound, exactly as it owned `selection_modal` before.
        let mut completion_rows = Vec::new();
        if let Some(completion) = &self.completion {
            let (popup, rows) = draw_editor_completion(frame, area, completion, theme);
            surface.popup = surface.popup.union(popup);
            surface.interior = popup.inner(ratatui::layout::Margin::new(1, 1));
            surface.caret = None;
            completion_rows = rows;
        }
        surface.scrollable = true;
        // `q` is a character only while the field itself has the keys (§1).
        surface.text_focus = self.text_editing() && self.completion.is_none();
        self.geometry.actions = actions;
        self.geometry.diagnostics = diagnostics;
        self.geometry.completion = completion_rows;
        self.surface = surface;
        surface
    }
}

impl EditorDialog {
    /// §12.3 multiline grouping: one field, a fixed worked preview, and the
    /// `[ Apply ]` button the dialog anatomy requires.
    fn render_grouping(
        &mut self,
        frame: &mut Frame<'_>,
        area: Rect,
        editor: &EditorState,
        cursor: usize,
        ctx: &RenderCtx<'_>,
    ) -> (Surface, Vec<Rect>, Option<Rect>) {
        let theme = ctx.theme;
        let mut surface = Surface::default();
        let mut actions: Vec<Rect> = Vec::new();
        let mut diagnostics: Option<Rect> = None;
        let editor = editor.clone();
        use crate::dialog_layout::{DialogClass, DialogContent, content_width, pane};

        let styles = DialogStyles::new(theme);
        let ascii = ctx.ascii;
        let width = content_width(area, DialogClass::S);

        let (state, sentence) = if let Some(error) = editor.error.as_deref() {
            (MessageState::Error, error.to_owned())
        } else if editor.pending_generation.is_some() {
            (
                MessageState::Updating,
                "checking this draft · the last applied grouping stays active".to_owned(),
            )
        } else if editor.applied.is_empty() {
            (
                MessageState::Disabled,
                "an empty draft turns grouping off".to_owned(),
            )
        } else {
            (MessageState::Applied, editor.applied.clone())
        };
        let help = "Continuation lines match this regex over raw bytes; grouping is display only.";
        // §3: the action row is part of the anatomy, not an afterthought. Without
        // it this dialog rendered no way to apply at all and relied on the user
        // knowing that Enter works.
        let labels = ["Apply"];

        let preview_rows = u16::try_from(GROUPING_PREVIEW.len()).unwrap_or(2);
        let content = DialogContent {
            header: 0,
            // input row, gap, preview pane heading, preview rows
            body: 3u16.saturating_add(preview_rows),
            message: message_rows(&sentence, width),
            help: help_rows(help, width),
            actions: packed_button_rows(width, &labels),
        };
        let regions = dialog_frame_regions(
            frame,
            area,
            DialogClass::S,
            "Multiline grouping",
            &content,
            theme,
        );
        surface.popup = regions.popup;
        surface.interior = regions.interior;
        self.geometry.body = regions.body;
        if regions.body.width == 0 || regions.body.height == 0 {
            return (surface, actions, diagnostics);
        }

        let field = Rect::new(regions.body.x, regions.body.y, regions.body.width, 1);
        if !self.scroll_focused {
            surface.caret = place_input_cursor_at(frame, field, 0, 0, &editor.draft, cursor, theme);
        } else {
            InputSurface {
                style: styles.input,
            }
            .render(field, frame.buffer_mut());
            frame.render_widget(
                Paragraph::new(truncated(&editor.draft, usize::from(field.width)))
                    .style(styles.input),
                field,
            );
        }

        // §8.7: the preview is a pane, not three loose rows under a colon label.
        let preview_area = Rect::new(
            regions.body.x,
            regions.body.y.saturating_add(2),
            regions.body.width,
            regions.body.height.saturating_sub(2),
        );
        if preview_area.height > 0 {
            let rects = pane(preview_area, 0, GROUPING_PREVIEW.len());
            if rects.heading.height > 0 {
                frame.render_widget(
                    Paragraph::new("Preview").style(styles.label.add_modifier(Modifier::BOLD)),
                    rects.heading,
                );
            }
            for (index, line) in GROUPING_PREVIEW.iter().enumerate() {
                let Some(y) = u16::try_from(index)
                    .ok()
                    .map(|offset| rects.viewport.y.saturating_add(offset))
                    .filter(|y| *y < rects.viewport.bottom())
                else {
                    continue;
                };
                frame.render_widget(
                    Paragraph::new(truncated(line, usize::from(rects.viewport.width)))
                        .style(styles.description),
                    Rect::new(rects.viewport.x, y, rects.viewport.width, 1),
                );
            }
        }

        // The status is one line now, so nothing overflows and no scroll
        // affordance is claimed (§9).
        self.scroll_limit = 0;
        diagnostics = None;

        render_message(frame, regions.message, state, &sentence, theme, ascii);
        render_help_text(frame, regions.help, help, theme);
        // The hitbox is the rect the button was drawn into, so click and paint can
        // never disagree.
        actions = render_action_row(frame, regions.actions, &labels, None, &[], theme)
            .into_iter()
            .map(|(_, rect)| rect)
            .collect();
        (surface, actions, diagnostics)
    }

    /// §12.1/§12.2: Search and Advanced share one field, one message row and
    /// one overflowing-diagnostic pane; only their words differ.
    fn render_simple(
        &mut self,
        frame: &mut Frame<'_>,
        area: Rect,
        editor: &EditorState,
        cursor: usize,
        ctx: &RenderCtx<'_>,
    ) -> (Surface, Vec<Rect>, Option<Rect>) {
        let theme = ctx.theme;
        let mut surface = Surface::default();
        let actions: Vec<Rect> = Vec::new();
        let mut diagnostics: Option<Rect> = None;
        let editor = editor.clone();
        use crate::dialog_layout::{DialogClass, DialogContent, content_width};

        let styles = DialogStyles::new(theme);
        let ascii = ctx.ascii;
        let search = self.purpose == QueryPurpose::Search;
        let title = if search { "Search" } else { "Advanced filter" };
        let help = if search {
            r#"Examples: text · "field name": text · /regex/ims · \/literal"#
        } else {
            "Use a Polars expression. Fields and sampled literals complete with Tab."
        };
        let width = content_width(area, DialogClass::S);

        // §7.4: one message row. The last accepted value stays in the sentence, so
        // a failing draft never hides the filter that is actually applied.
        let (state, mut sentence) = if let Some(error) = editor.error.as_deref() {
            (MessageState::Error, error.to_owned())
        } else if editor.pending_generation.is_some() {
            (
                MessageState::Updating,
                "checking this draft · the last applied view stays visible".to_owned(),
            )
        } else if editor.applied.is_empty() {
            (MessageState::NoFilter, "every record is shown".to_owned())
        } else {
            (MessageState::Applied, editor.applied.clone())
        };
        if !editor.applied.is_empty() && editor.draft != editor.applied {
            sentence.push_str(" · last accepted ");
            sentence.push_str(&editor.applied);
        }

        // §7.4 caps the message row at two rows, but a rejected expression can carry
        // a long diagnostic and AGENTS.md requires it to stay reachable. When the
        // sentence does not fit, the state stays in the message row and the full
        // text moves into a scrollable pane (§9) instead of being clipped away.
        let message_width = usize::from(width.saturating_sub(MESSAGE_SENTENCE_COLUMN)).max(1);
        let wrapped = wrap_sentence(&sentence, message_width, usize::MAX);
        let overflows = wrapped.len() > usize::from(message_rows(&sentence, width));
        let diagnostic_rows = if overflows {
            u16::try_from(wrapped.len()).unwrap_or(u16::MAX).min(8)
        } else {
            0
        };

        let content = DialogContent {
            header: 0,
            body: if overflows {
                2u16.saturating_add(diagnostic_rows)
            } else {
                1
            },
            message: message_rows(&sentence, width),
            help: help_rows(help, width),
            actions: 0,
        };
        let regions = dialog_frame_regions(frame, area, DialogClass::S, title, &content, theme);
        surface.popup = regions.popup;
        surface.interior = regions.interior;
        self.geometry.body = regions.body;
        if regions.body.width == 0 || regions.body.height == 0 {
            return (surface, actions, diagnostics);
        }

        let field = Rect::new(regions.body.x, regions.body.y, regions.body.width, 1);
        if self.completion.is_none() && !self.scroll_focused {
            surface.caret = place_input_cursor_at(frame, field, 0, 0, &editor.draft, cursor, theme);
            if editor.draft.is_empty() {
                let placeholder = if search {
                    "Type to filter…"
                } else {
                    r#"Polars expression, e.g. col("level") == "ERROR""#
                };
                render_placeholder(
                    frame,
                    Rect::new(
                        field.x.saturating_add(1),
                        field.y,
                        field.width.saturating_sub(1),
                        1,
                    ),
                    placeholder,
                    theme,
                );
            }
        } else {
            InputSurface {
                style: styles.input,
            }
            .render(field, frame.buffer_mut());
            frame.render_widget(
                Paragraph::new(truncated(&editor.draft, usize::from(field.width)))
                    .style(styles.input),
                field,
            );
        }

        if overflows && regions.body.height > 1 {
            let pane_area = Rect::new(
                regions.body.x,
                regions.body.y.saturating_add(1),
                regions.body.width,
                regions.body.height.saturating_sub(1),
            );
            let rects = crate::dialog_layout::pane(pane_area, 0, wrapped.len());
            if rects.heading.height > 0 {
                frame.render_widget(
                    Paragraph::new("Diagnostics").style(styles.label.add_modifier(Modifier::BOLD)),
                    rects.heading,
                );
            }
            let visible = usize::from(rects.viewport.height);
            let limit = wrapped.len().saturating_sub(visible);
            self.scroll_limit = limit;
            self.scroll = self.scroll.min(limit);
            diagnostics = (limit > 0).then_some(rects.viewport);
            for (offset, line) in wrapped.iter().skip(self.scroll).take(visible).enumerate() {
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
                render_scrollbar(frame, bar, self.scroll, limit, theme, ascii);
            }
        } else {
            // A status that fits claims no affordance (§9).
            self.scroll_limit = 0;
            diagnostics = None;
        }

        render_message(frame, regions.message, state, &sentence, theme, ascii);
        render_help_text(frame, regions.help, help, theme);
        (surface, actions, diagnostics)
    }
}
