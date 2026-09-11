//! The Bookmarks layer (`docs/dialog-system.md` §12.10), converted per
//! `docs/component-model.md` §6.3 step 5.
//!
//! The bookmark *store* is not the dialog's: bookmarks belong to the source
//! whose records they mark, they are persisted and restored, the log draws
//! markers from them and `b` toggles one from the base UI. That store moved
//! into `Views` alongside the view→source mapping needed to resolve it, and
//! the layer reaches it through `ctx.views`. What the component owns is which
//! bookmark is selected, which control has focus, the note draft and its
//! caret, and the geometry it draws.
//!
//! Note editing is the shared child presentation (§10): noncompact draws the
//! parent list behind the child with a second scrim pass, compact reuses the
//! parent frame with a breadcrumb title. Escape returns child→Bookmarks→base;
//! save semantics are unchanged. Hand-rolled layout below is presentation-only
//! folding, never query membership (AGENTS.md).

use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use ratatui::{Frame, layout::Rect, style::Modifier, widgets::Paragraph};
use unicode_width::UnicodeWidthStr;

use crate::app::{Action, Bookmark, BookmarkDialogControl, MAX_BOOKMARK_NOTE_BYTES, Views};
use crate::command_palette::CommandId;
use crate::component::{
    CommandEntry, CommandSpec, Component, Ctx, Event, Outcome, RenderCtx, Surface, is_typed_char,
};
use crate::dialog_controls::{ButtonRole, DialogStyles, stable_action_rows};
use crate::dialog_layout::{ContextFootprint, DialogSpec, PresentationKind, plan_list};
use crate::provider::RowId;
use crate::text_edit::{EditCommand, EditPolicy, TextCursor, edit};
use crate::ui::{
    FIELD_GUTTER, MessageState, help_rows, render_form_field, render_help_text, render_message,
    render_responsive_frame, render_scrollbar, truncated,
};

#[doc(hidden)]
/// §12.10 row columns: the record id, then its capture time, then the record
/// text. The note is the row's second line.
const BOOKMARK_ID_WIDTH: u16 = 5;
const BOOKMARK_TIME_WIDTH: u16 = 8;
const BOOKMARK_LABEL_WIDTH: u16 = 6;
/// The cap the store enforces, shown beside the count so reaching it is not a
/// surprise.
const BOOKMARK_CAPACITY: usize = 128;

/// Everything the Bookmarks layer draws that can be clicked.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BookmarksHit {
    Row(usize),
    Control(BookmarkDialogControl),
}

/// Recorded by `render`, consumed by `hit()` (§5.1).
#[derive(Clone, Debug, Default)]
struct BookmarksGeometry {
    rows: Vec<(Rect, usize)>,
    controls: Vec<(Rect, BookmarkDialogControl)>,
}

/// The dialog's own state. `view_id` is the view it was opened on, so a
/// bookmark's set is resolved against the same view for the dialog's lifetime.
#[derive(Clone, Debug)]
pub struct BookmarkState {
    pub view_id: String,
    pub selected: usize,
    /// The bookmark whose note is being edited, if any.
    pub editing: Option<RowId>,
    pub control: BookmarkDialogControl,
    pub draft: String,
    pub status: String,
}

impl Default for BookmarkState {
    fn default() -> Self {
        Self {
            view_id: String::new(),
            selected: 0,
            editing: None,
            control: BookmarkDialogControl::List,
            draft: String::new(),
            status: String::new(),
        }
    }
}

#[derive(Debug, Default)]
pub struct BookmarksDialog {
    open: bool,
    state: BookmarkState,
    /// The note draft is dialog-owned, so its caret is too (§2.2): it resets to
    /// the end whenever a different note is opened, exactly as a fresh
    /// `CursorBank` identity did.
    note_cursor: usize,
    geometry: BookmarksGeometry,
    surface: Surface,
}

fn contains(area: Rect, point: (u16, u16)) -> bool {
    point.0 >= area.x && point.0 < area.right() && point.1 >= area.y && point.1 < area.bottom()
}

/// §4.3: the palette entry Bookmarks owns.
const NOTE_COMMAND: CommandSpec = CommandSpec {
    id: CommandId::BookmarkNote,
    name: "Edit bookmark note",
    description: "Annotate the selected bookmark",
    category: "Views",
    aliases: &["annotation"],
    shortcut: None,
};

impl BookmarksDialog {
    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn state(&self) -> &BookmarkState {
        &self.state
    }

    /// Geometry recorded by the last `render`; `hit()` is how input reaches it.
    pub fn row_rects(&self) -> &[(Rect, usize)] {
        &self.geometry.rows
    }

    pub fn control_rects(&self) -> &[(Rect, BookmarkDialogControl)] {
        &self.geometry.controls
    }

    /// Marks the layer open without a `Ctx`, so a palette test can ask for the
    /// entries it contributes while it is on the stack.
    pub fn open_for_test(&mut self) {
        self.open = true;
    }

    fn note_caret(&self) -> Option<usize> {
        (self.state.editing.is_some() && self.state.control == BookmarkDialogControl::Input)
            .then(|| self.note_cursor.min(self.state.draft.chars().count()))
    }

    fn record(
        &mut self,
        rows: Vec<(Rect, usize)>,
        controls: Vec<(Rect, BookmarkDialogControl)>,
        surface: Surface,
    ) -> Surface {
        self.geometry = BookmarksGeometry { rows, controls };
        self.surface = surface;
        surface
    }

    fn bookmarks(&self, views: &Views) -> Vec<Bookmark> {
        views.bookmarks_for_view(&self.state.view_id)
    }

    /// The record the actions would act on, resolved before anything changes.
    fn selected_id(&self, views: &Views) -> Option<RowId> {
        self.bookmarks(views)
            .get(self.state.selected)
            .map(|bookmark| bookmark.id.clone())
    }

    fn controls(&self, views: &Views) -> Vec<BookmarkDialogControl> {
        crate::app::bookmark_controls(
            self.state.editing.is_some(),
            !self.bookmarks(views).is_empty(),
        )
    }

    /// The list is resolved before every action, so a bookmark removed
    /// elsewhere cannot leave the selection pointing past the end.
    fn clamp(&mut self, views: &Views) {
        let len = self.bookmarks(views).len();
        self.state.selected = self.state.selected.min(len.saturating_sub(1));
    }

    /// Writes the selection into the view (§7.3) so reopening lands on it.
    fn remember_selection(&mut self, ctx: &mut Ctx<'_>) {
        if let Some(state) = ctx.views.state_mut(&self.state.view_id) {
            state.bookmark_selected = self.state.selected;
        }
    }

    fn move_control(&mut self, delta: i32, ctx: &Ctx<'_>) {
        let controls = self.controls(ctx.views);
        self.state.control = crate::app::move_control(self.state.control, &controls, delta);
    }

    fn focus_control(&mut self, control: BookmarkDialogControl, ctx: &Ctx<'_>) {
        if self.controls(ctx.views).contains(&control) {
            self.state.control = control;
        }
    }

    fn move_selection(&mut self, delta: i32, ctx: &Ctx<'_>) {
        if self.state.editing.is_some() {
            return;
        }
        let len = self.bookmarks(ctx.views).len();
        self.state.selected = self
            .state
            .selected
            .saturating_add_signed(delta as isize)
            .min(len.saturating_sub(1));
    }

    fn select(&mut self, index: usize, ctx: &Ctx<'_>) {
        if self.state.editing.is_some() {
            return;
        }
        let len = self.bookmarks(ctx.views).len();
        self.state.selected = index.min(len.saturating_sub(1));
    }

    fn edit_note(&mut self, ctx: &mut Ctx<'_>) {
        if self.state.editing.is_some() {
            return;
        }
        let Some(bookmark) = self.bookmarks(ctx.views).get(self.state.selected).cloned() else {
            return;
        };
        self.state.editing = Some(bookmark.id.clone());
        self.state.control = BookmarkDialogControl::Input;
        self.state.draft = bookmark.note.clone();
        self.note_cursor = self.state.draft.chars().count();
        self.state.status.clear();
        ctx.views.note_bookmark_change(&bookmark.id.source_id);
    }

    fn editing_source(&self) -> String {
        self.state
            .editing
            .as_ref()
            .map(|id| id.source_id.clone())
            .unwrap_or_default()
    }

    fn typing(&self) -> bool {
        self.state.editing.is_some() && self.state.control == BookmarkDialogControl::Input
    }

    fn insert(&mut self, ch: char, ctx: &mut Ctx<'_>) {
        if !ch.is_control()
            && self.state.draft.len().saturating_add(ch.len_utf8()) <= MAX_BOOKMARK_NOTE_BYTES
        {
            self.state.draft.push(ch);
            self.note_cursor = self.state.draft.chars().count();
            let source = self.editing_source();
            ctx.views.note_bookmark_change(&source);
        } else {
            self.state.status = "note limit: 1024 bytes, single line".into();
        }
    }

    fn backspace(&mut self, ctx: &mut Ctx<'_>) {
        self.state.draft.pop();
        self.note_cursor = self.state.draft.chars().count();
        let source = self.editing_source();
        ctx.views.note_bookmark_change(&source);
    }

    fn paste(&mut self, text: &str, ctx: &mut Ctx<'_>) {
        if !self.typing() {
            return;
        }
        if self.state.draft.len().saturating_add(text.len()) <= MAX_BOOKMARK_NOTE_BYTES
            && !text.chars().any(char::is_control)
        {
            self.state.draft.push_str(text);
            self.note_cursor = self.state.draft.chars().count();
            let source = self.editing_source();
            ctx.views.note_bookmark_change(&source);
        } else {
            self.state.status = "note must be a single line, at most 1024 bytes".into();
        }
    }

    /// Line editing against the note, using the dialog's own caret.
    fn edit_command(&mut self, command: EditCommand<'_>, ctx: &mut Ctx<'_>) {
        if !self.typing() {
            return;
        }
        let mut value = self.state.draft.clone();
        let mut cursor = TextCursor {
            char_index: self.note_cursor.min(value.chars().count()),
        };
        let outcome = edit(
            &mut value,
            &mut cursor,
            command,
            EditPolicy {
                max_bytes: MAX_BOOKMARK_NOTE_BYTES,
                multiline: false,
            },
        );
        self.note_cursor = cursor.char_index;
        if outcome.changed {
            self.state.draft = value;
            let source = self.editing_source();
            ctx.views.note_bookmark_change(&source);
        }
    }

    /// Enter: save the note when one is open, otherwise act on the selection.
    fn submit(&mut self, ctx: &mut Ctx<'_>) -> Outcome {
        if let Some(id) = self.state.editing.take() {
            let note = std::mem::take(&mut self.state.draft);
            self.note_cursor = 0;
            if ctx.views.set_bookmark_note(&id, note) {
                self.state.status = "note updated; workspace autosave pending".into();
                self.state.control = BookmarkDialogControl::List;
            }
            return Outcome::Consumed;
        }
        let Some(anchor) = self.selected_id(ctx.views) else {
            return Outcome::Consumed;
        };
        if self.state.control == BookmarkDialogControl::Context {
            // Raw context is a jump to All events (raw-context-as-jump.md):
            // this dialog closes and is re-pushed on return from the
            // selection the view remembers.
            self.remember_selection(ctx);
            self.open = false;
            return Outcome::Legacy(Action::RawContext {
                anchor: Some(anchor),
                layer: Some(crate::component::Open::Bookmarks),
            });
        }
        // Selecting the record in its canonical view is the shell's: it
        // switches view, moves the selection and chases the row (§8).
        self.open = false;
        Outcome::Legacy(Action::JumpToRecord {
            row: anchor,
            fallback_view: self.state.view_id.clone(),
        })
    }

    fn delete(&mut self, ctx: &mut Ctx<'_>) {
        if self.state.editing.is_some() {
            return;
        }
        let Some(id) = self.selected_id(ctx.views) else {
            return;
        };
        ctx.views.remove_bookmark(&id);
        self.clamp(ctx.views);
        self.state.status = "bookmark removed".into();
    }

    /// Press a named button, as a click or a §8.10 mnemonic does, without
    /// moving the focus ring.
    fn run(&mut self, control: BookmarkDialogControl, ctx: &mut Ctx<'_>) -> Outcome {
        match control {
            BookmarkDialogControl::Edit => {
                self.edit_note(ctx);
                Outcome::Consumed
            }
            BookmarkDialogControl::Delete => {
                self.delete(ctx);
                Outcome::Consumed
            }
            BookmarkDialogControl::Context => {
                // Raw context is the jump to All events; `submit` routes it by
                // the focused control, so press it the way a click would.
                self.state.control = BookmarkDialogControl::Context;
                self.submit(ctx)
            }
            _ => self.submit(ctx),
        }
    }

    fn activate(&mut self, ctx: &mut Ctx<'_>) -> Outcome {
        match self.state.control {
            BookmarkDialogControl::Edit => {
                self.edit_note(ctx);
                Outcome::Consumed
            }
            BookmarkDialogControl::Delete => {
                self.delete(ctx);
                Outcome::Consumed
            }
            BookmarkDialogControl::Goto
            | BookmarkDialogControl::Context
            | BookmarkDialogControl::Save
            | BookmarkDialogControl::List
            | BookmarkDialogControl::Input => self.submit(ctx),
        }
    }

    fn key(&mut self, key: KeyEvent, ctx: &mut Ctx<'_>) -> Outcome {
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return Outcome::Ignored;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            let command = match key.code {
                KeyCode::Char('a') => Some(EditCommand::StartOfLine),
                KeyCode::Char('e') => Some(EditCommand::EndOfLine),
                KeyCode::Char('k') => Some(EditCommand::KillToEndOfLine),
                _ => None,
            };
            if let Some(command) = command {
                self.edit_command(command, ctx);
                return Outcome::Consumed;
            }
        }
        match key.code {
            KeyCode::Tab if key.modifiers.contains(KeyModifiers::SHIFT) => {
                self.move_control(-1, ctx)
            }
            KeyCode::BackTab => self.move_control(-1, ctx),
            KeyCode::Tab => self.move_control(1, ctx),
            KeyCode::Up => self.move_selection(-1, ctx),
            KeyCode::Down => self.move_selection(1, ctx),
            // `e` and `r` are the §8.10 mnemonics of the two buttons that carry
            // one, resolved by the shell. Alt-D is not a letter of `Remove`, so
            // it stays here as the unlisted alias it has always been.
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::ALT) => self.delete(ctx),
            KeyCode::Enter => return self.activate(ctx),
            KeyCode::Backspace if self.typing() => self.backspace(ctx),
            KeyCode::Char(ch) if self.typing() && is_typed_char(&key) => self.insert(ch, ctx),
            _ => return Outcome::Ignored,
        }
        Outcome::Consumed
    }

    fn mouse(
        &mut self,
        kind: MouseEventKind,
        hit: Option<BookmarksHit>,
        ctx: &mut Ctx<'_>,
    ) -> Outcome {
        match kind {
            MouseEventKind::Down(MouseButton::Left) => match hit {
                Some(BookmarksHit::Control(control)) => {
                    self.focus_control(control, ctx);
                    if matches!(
                        control,
                        BookmarkDialogControl::List | BookmarkDialogControl::Input
                    ) {
                        Outcome::Consumed
                    } else {
                        self.activate(ctx)
                    }
                }
                Some(BookmarksHit::Row(index)) => {
                    self.select(index, ctx);
                    Outcome::Consumed
                }
                None => Outcome::Consumed,
            },
            MouseEventKind::ScrollUp => {
                self.move_selection(-1, ctx);
                Outcome::Consumed
            }
            MouseEventKind::ScrollDown => {
                self.move_selection(1, ctx);
                Outcome::Consumed
            }
            _ => Outcome::Consumed,
        }
    }

    /// Parent list behind the Note child (roomy viewports only): same rows as
    /// the foreground list, painted inactive with no hitboxes recorded while
    /// the child is modal. Hand-rolled painting here is presentation-only
    /// folding, never query membership (AGENTS.md).
    fn paint_parent_list(
        frame: &mut ratatui::Frame<'_>,
        parent: &crate::dialog_layout::DialogGeometry,
        bookmarks: &[Bookmark],
        dialog: &BookmarkState,
        provider: &dyn crate::provider::RowProvider,
        theme: crate::theme::Theme,
        ascii: bool,
    ) {
        use crate::dialog_layout::plan_list;
        let styles = DialogStyles::new(theme);
        let inner = parent.body.viewport;
        if inner.width == 0 || inner.height == 0 {
            return;
        }
        // Parent message/help/actions are still painted by the caller? No:
        // the background shows the list only; the child's second scrim dims
        // the whole parent frame including its sticky tail, so paint the list
        // pane plus the parent message/help/actions bands as dimmed context?
        // To keep the background legible but clearly inactive, paint only the
        // list pane here; the frame title/border already marks the parent.
        // Message/help/actions bands are left to the scrimmed parent frame
        // background (blank dimmed rows), which is what a stacked child shows:
        // the parent's chrome stays visible as a frame, not as live controls.
        let count = format!("{} of {BOOKMARK_CAPACITY}", bookmarks.len());
        let count_w = u16::try_from(UnicodeWidthStr::width(count.as_str())).unwrap_or(0);
        let total_rows = bookmarks.len().saturating_mul(2);
        let selected_logical = (!bookmarks.is_empty()).then(|| {
            dialog
                .selected
                .min(bookmarks.len().saturating_sub(1))
                .saturating_mul(2)
                .saturating_add(1)
        });
        let list = plan_list(inner, count_w, total_rows, selected_logical, 0);
        if list.heading.height > 0 {
            frame.render_widget(
                Paragraph::new("Bookmarks").style(styles.label.add_modifier(Modifier::BOLD)),
                list.heading,
            );
            if list.count.width > 0 {
                frame.render_widget(Paragraph::new(count).style(styles.description), list.count);
            }
        }
        if bookmarks.is_empty() {
            if list.viewport.height > 0 {
                frame.render_widget(
                    Paragraph::new(truncated(
                        "No bookmarks in this view yet",
                        usize::from(list.viewport.width),
                    ))
                    .style(styles.description),
                    Rect::new(
                        list.viewport.x,
                        list.viewport.y,
                        list.viewport.width,
                        1.min(list.viewport.height),
                    ),
                );
            }
            return;
        }
        for (offset, row_rect) in list.row_rects.iter().enumerate() {
            let logical = list.first_row.saturating_add(offset);
            let index = logical / 2;
            let is_first = logical % 2 == 0;
            let Some(bookmark) = bookmarks.get(index) else {
                continue;
            };
            let focused = index == dialog.selected;
            if is_first {
                let record = provider.row_by_id(&dialog.view_id, &bookmark.id);
                let marker = if focused {
                    if ascii { "> " } else { "› " }
                } else {
                    "  "
                };
                let id = format!("{marker}#{}", bookmark.id.sequence);
                let id_width = (BOOKMARK_ID_WIDTH + 2).min(row_rect.width);
                frame.render_widget(
                    Paragraph::new(truncated(&id, usize::from(id_width))).style(if focused {
                        styles.selection
                    } else {
                        styles.label
                    }),
                    Rect::new(row_rect.x, row_rect.y, id_width, 1),
                );
                let time_x = row_rect
                    .x
                    .saturating_add(id_width)
                    .saturating_add(FIELD_GUTTER);
                let time = record.as_ref().map_or_else(String::new, |row| {
                    row.timestamp
                        .split('.')
                        .next()
                        .unwrap_or(&row.timestamp)
                        .rsplit('T')
                        .next()
                        .unwrap_or_default()
                        .to_owned()
                });
                if time_x < row_rect.right() {
                    let time_width =
                        BOOKMARK_TIME_WIDTH.min(row_rect.right().saturating_sub(time_x));
                    frame.render_widget(
                        Paragraph::new(truncated(&time, usize::from(time_width)))
                            .style(styles.description),
                        Rect::new(time_x, row_rect.y, time_width, 1),
                    );
                    let text_x = time_x
                        .saturating_add(time_width)
                        .saturating_add(FIELD_GUTTER);
                    if text_x < row_rect.right() {
                        let text = record.as_ref().map_or_else(
                            || "record is no longer loaded".to_owned(),
                            |row| row.text.clone(),
                        );
                        frame.render_widget(
                            Paragraph::new(truncated(
                                &text,
                                usize::from(row_rect.right().saturating_sub(text_x)),
                            ))
                            .style(styles.description),
                            Rect::new(
                                text_x,
                                row_rect.y,
                                row_rect.right().saturating_sub(text_x),
                                1,
                            ),
                        );
                    }
                }
            } else {
                let note_x = row_rect
                    .x
                    .saturating_add(BOOKMARK_ID_WIDTH + 2)
                    .saturating_add(FIELD_GUTTER);
                let empty = bookmark.note.is_empty();
                if note_x < row_rect.right() {
                    frame.render_widget(
                        Paragraph::new(truncated(
                            if empty { "no note" } else { &bookmark.note },
                            usize::from(row_rect.right().saturating_sub(note_x)),
                        ))
                        .style(if empty {
                            styles.unavailable
                        } else {
                            styles.description
                        }),
                        Rect::new(
                            note_x,
                            row_rect.y,
                            row_rect.right().saturating_sub(note_x),
                            1,
                        ),
                    );
                }
            }
        }
        if let Some(bar) = list.scrollbar {
            render_scrollbar(
                frame,
                bar,
                list.first_row,
                total_rows.saturating_sub(list.row_rects.len()),
                theme,
                ascii,
            );
        }
    }
}

/// §8.9/§8.10: the row of bookmark actions, in drawn order. `Edit note` and
/// `Remove` are the two that carry a mnemonic — they are the two that had an
/// Alt chord and no underline to show it, which §8.10 calls the button gaining
/// the mnemonic its label affords. `Open in All events` is the default and
/// Enter runs it.
fn bookmark_actions() -> [(&'static str, BookmarkDialogControl); 4] {
    [
        ("Open in All events", BookmarkDialogControl::Goto),
        ("&Edit note", BookmarkDialogControl::Edit),
        ("Inspect c&ontext", BookmarkDialogControl::Context),
        ("&Remove", BookmarkDialogControl::Delete),
    ]
}

/// Stable LongContent budgets: outer size is policy-only, never bookmark
/// counts or note length. Header 0, body minimum 3, message 1 stable, help 2
/// stable maxima, actions from the stable 4-verb row so empty/populated share
/// one frame and sticky tail.
fn bookmarks_spec_for(area: Rect) -> DialogSpec {
    let labels = [
        "Open in All events",
        "&Edit note",
        "Inspect c&ontext",
        "&Remove",
    ];
    let (policy_w, _) = crate::dialog_layout::policy_size(area, PresentationKind::LongContent);
    let estimate = policy_w.saturating_sub(4).max(1);
    let action_rows = stable_action_rows(estimate, &labels).clamp(1, 2);
    DialogSpec::new(PresentationKind::LongContent, 0, 3, 1, 2, action_rows)
}

/// Stable child budgets for the Note editor (class S, one field): outer size
/// is policy-only, never draft length. Header 0, body minimum 1, message 1,
/// help from the stable note sentence, actions 1 (Save).
fn note_spec_for(area: Rect) -> DialogSpec {
    const NOTE_HELP: &str = "Notes are capped at 1024 bytes and saved with the view.";
    let (policy_w, _) = crate::dialog_layout::policy_size(
        area,
        PresentationKind::Contextual(ContextFootprint::Prompt),
    );
    let estimate = policy_w.saturating_sub(4).max(1);
    let help = help_rows(NOTE_HELP, estimate).clamp(1, 2);
    DialogSpec::new(
        PresentationKind::Contextual(ContextFootprint::Prompt),
        0,
        1,
        1,
        help,
        1,
    )
}

impl Component for BookmarksDialog {
    type Hit = BookmarksHit;
    type Open = ();

    fn open(&mut self, _params: (), ctx: &mut Ctx<'_>) {
        let Some(view_id) = ctx.views.active_id().map(str::to_owned) else {
            return;
        };
        self.open = true;
        // §7.3: the selected bookmark is view-owned, so the list reopens on
        // it — after a Raw context jump and return in particular.
        let selected = ctx
            .views
            .state(&view_id)
            .map_or(0, |state| state.bookmark_selected);
        self.state = BookmarkState {
            view_id,
            selected,
            ..BookmarkState::default()
        };
        self.clamp(ctx.views);
        self.note_cursor = 0;
        self.geometry = BookmarksGeometry::default();
    }

    fn handle(&mut self, event: Event<BookmarksHit>, ctx: &mut Ctx<'_>) -> Outcome {
        // A bookmark removed while the dialog is open must not leave the
        // selection past the end of the list.
        self.clamp(ctx.views);
        match event {
            Event::Key(key) => self.key(key, ctx),
            Event::Mouse { kind, hit, .. } => self.mouse(kind, hit, ctx),
            Event::Paste(text) => {
                self.paste(&text, ctx);
                Outcome::Consumed
            }
            // §5.3: Escape abandons an open note first, then the dialog.
            Event::Dismiss => {
                if self.state.editing.take().is_some() {
                    self.state.draft.clear();
                    self.note_cursor = 0;
                    self.state.control = BookmarkDialogControl::List;
                    return Outcome::Consumed;
                }
                self.remember_selection(ctx);
                self.open = false;
                Outcome::Close
            }
            Event::Command(CommandId::BookmarkNote) => {
                self.edit_note(ctx);
                Outcome::Consumed
            }
            Event::Command(_) | Event::View(_) | Event::Resize => Outcome::Ignored,
        }
    }

    fn commands(&self, _views: &Views) -> Vec<CommandEntry> {
        vec![CommandEntry {
            spec: CommandSpec {
                shortcut: self.open.then_some("e"),
                ..NOTE_COMMAND
            },
            unavailable_reason: (!self.open).then_some("open Bookmarks first"),
        }]
    }

    fn action_labels(&self, ctx: &Ctx<'_>) -> Vec<&'static str> {
        // The Note child's own row is `[ Save note ]`, which marks no letter,
        // so while it is up this layer offers no mnemonic (§10).
        if self.state.editing.is_some()
            || ctx.views.bookmarks_for_view(&self.state.view_id).is_empty()
        {
            return Vec::new();
        }
        bookmark_actions().iter().map(|(label, _)| *label).collect()
    }

    fn press_action(&mut self, index: usize, ctx: &mut Ctx<'_>) -> Outcome {
        match bookmark_actions().get(index).map(|(_, control)| *control) {
            Some(control) => self.run(control, ctx),
            None => Outcome::Ignored,
        }
    }

    fn surface(&self) -> Surface {
        self.surface
    }

    /// Clicking `[ Edit note ]` opens the input without a redraw in between,
    /// so the live state decides whether `q` types or dismisses.
    fn text_focus(&self) -> bool {
        self.typing()
    }

    fn hit(&self, point: (u16, u16)) -> Option<BookmarksHit> {
        self.geometry
            .controls
            .iter()
            .find_map(|(rect, control)| {
                contains(*rect, point).then_some(BookmarksHit::Control(*control))
            })
            .or_else(|| {
                self.geometry.rows.iter().find_map(|(rect, index)| {
                    contains(*rect, point).then_some(BookmarksHit::Row(*index))
                })
            })
    }

    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &RenderCtx<'_>) -> Surface {
        use BookmarkDialogControl as C;
        let theme = ctx.theme;
        let provider = ctx.provider;
        let styles = DialogStyles::new(theme);
        let ascii = ctx.ascii;
        let cursor = self.note_caret();
        let mut rows_hit: Vec<(Rect, usize)> = Vec::new();
        let mut controls_hit: Vec<(Rect, BookmarkDialogControl)> = Vec::new();
        // Snapshotted for the frame: the layout borrows the draft while the
        // geometry it produces is written back.
        let dialog = self.state.clone();
        let bookmarks = ctx.views.bookmarks_for_view(&dialog.view_id);
        let view_name = ctx
            .views
            .items()
            .iter()
            .find(|view| view.id == dialog.view_id)
            .map_or_else(|| "this view".to_owned(), |view| view.name.clone());

        // §12.10 child: the Note editor is a shared child presentation. In a
        // roomy viewport the parent list stays visible behind the child with a
        // second scrim pass; in compact the child reuses the parent frame with
        // a breadcrumb title. Escape returns child→Bookmarks→base; save
        // semantics are unchanged. Go to/Raw context behavior stays as-is.
        if let Some(editing) = dialog.editing.clone() {
            let parent_spec = bookmarks_spec_for(area);
            let parent_labels = [
                "Open in All events",
                "&Edit note",
                "Inspect c&ontext",
                "&Remove",
            ];
            let Ok(parent_geometry) = crate::dialog_layout::resolve_dialog(
                area,
                &parent_spec,
                1,
                &parent_labels,
                Some(0),
                None,
            ) else {
                return self.record(
                    rows_hit,
                    controls_hit,
                    Surface {
                        popup: Rect::default(),
                        interior: Rect::default(),
                        caret: None,
                        scrollable: false,
                        text_focus: dialog.control == C::Input,
                    },
                );
            };
            let compact = crate::dialog_layout::is_compact(area);
            // Parent background in roomy viewports only: the frame stays
            // visible behind the child with an inactive border, exactly as a
            // stacked child dims its parent once more than the base (§10).
            if !compact {
                let parent_title = format!("Bookmarks · {view_name}");
                render_responsive_frame(frame, &parent_geometry, &parent_title, false, theme);
                // Paint the parent list behind the child for context; no
                // hitboxes are recorded for it while the child is modal.
                Self::paint_parent_list(
                    frame,
                    &parent_geometry,
                    &bookmarks,
                    &dialog,
                    provider,
                    theme,
                    ascii,
                );
                // Second scrim pass over the parent before the child, as §10.
                crate::dialog_layout::scrim(frame.buffer_mut(), area, theme);
            }
            let child_spec = note_spec_for(area);
            let child_labels = ["Save note"];
            let Ok(mut child_geometry) = crate::dialog_layout::resolve_dialog(
                area,
                &child_spec,
                1,
                &child_labels,
                Some(0),
                None,
            ) else {
                return self.record(
                    rows_hit,
                    controls_hit,
                    Surface {
                        popup: Rect::default(),
                        interior: Rect::default(),
                        caret: None,
                        scrollable: false,
                        text_focus: dialog.control == C::Input,
                    },
                );
            };
            // Child has one verb; overflow cannot happen at covered sizes.
            // Compact child uses the parent frame; the breadcrumb keeps context.
            if compact {
                child_geometry.frame = parent_geometry.frame;
                child_geometry.interior = parent_geometry.interior;
                child_geometry.content = parent_geometry.content;
            }
            let title = format!("Bookmarks › Note for #{}", editing.sequence);
            // When compact reuses the parent frame, paint the child frame over
            // it with the active border; otherwise paint the resolved child
            // frame centred over the dimmed parent.
            render_responsive_frame(frame, &child_geometry, &title, ctx.active, theme);
            let help = "Notes are capped at 1024 bytes and saved with the view.";
            let (state, sentence) = if dialog.status.is_empty() {
                (MessageState::Ready, String::new())
            } else {
                (MessageState::Applied, dialog.status.clone())
            };
            let mut surface = Surface {
                popup: child_geometry.frontmost,
                interior: child_geometry.interior,
                caret: None,
                scrollable: false,
                // The note form takes text; the list behind it does not (§1).
                text_focus: dialog.control == C::Input,
            };
            // Child body is one reserved field row from the shared geometry.
            let body = child_geometry.body.viewport;
            if body.width > 0 && body.height > 0 {
                let (input, caret) = render_form_field(
                    frame,
                    Rect::new(body.x, body.y, body.width, 1.min(body.height)),
                    BOOKMARK_LABEL_WIDTH,
                    "Note",
                    &dialog.draft,
                    "what this record shows",
                    dialog.control == C::Input,
                    cursor,
                    theme,
                );
                surface.caret = caret;
                controls_hit.push((input, C::Input));
            }
            render_message(
                frame,
                child_geometry.message,
                state,
                &sentence,
                theme,
                ascii,
            );
            render_help_text(frame, child_geometry.help, help, theme);
            for (index, rect) in child_geometry.actions.buttons.iter() {
                let control = [C::Save][*index];
                crate::dialog_controls::render_role_button(
                    frame,
                    *rect,
                    child_labels[*index],
                    if child_geometry.actions.default == Some(*index) {
                        ButtonRole::Default
                    } else {
                        ButtonRole::Normal
                    },
                    dialog.control == control,
                    theme,
                );
                controls_hit.push((*rect, control));
            }
            // Surface popup stays the child frontmost for modal containment.
            surface.popup = child_geometry.frontmost;
            if compact {
                // Compact reuses the parent frame: keep the union for
                // containment so the breadcrumb frame is the modal bound.
                surface.popup = parent_geometry.frame.union(child_geometry.frontmost);
                surface.interior = child_geometry.interior;
            }
            return self.record(rows_hit, controls_hit, surface);
        }

        let help = if bookmarks.is_empty() {
            "Press b on a record to bookmark it."
        } else {
            concat!(
                "Open in All events stays on the record in its source's complete view. ",
                "Inspect context shows its neighbours temporarily; o returns."
            )
        };
        let (state, sentence) = if dialog.status.contains("limit") {
            (MessageState::Error, dialog.status.clone())
        } else if dialog.status.is_empty() {
            (MessageState::Ready, String::new())
        } else {
            (MessageState::Applied, dialog.status.clone())
        };
        // §12.10 `[ Open in All events ]`, honest now that bookmarks are
        // source-scoped: the record is always present in that complete view.
        let mut actions: Vec<(&'static str, C)> = Vec::new();
        if !bookmarks.is_empty() {
            actions.extend(bookmark_actions());
        }
        let action_labels = actions.iter().map(|(label, _)| *label).collect::<Vec<_>>();

        // Responsive frame: LongContent policy plus stable budgets only. Each
        // bookmark is two logical rows (record, note); the body owns surplus
        // via the shared list pane. Short/long bookmark counts share one frame
        // and sticky tail.
        let spec = bookmarks_spec_for(area);
        let Ok(geometry) =
            crate::dialog_layout::resolve_dialog(area, &spec, 1, &action_labels, Some(0), None)
        else {
            // Below the 20x6 floor the existing tiny fallback owns the frame;
            // stay open with nothing drawn, as the palette does.
            return self.record(
                rows_hit,
                controls_hit,
                Surface {
                    popup: Rect::default(),
                    interior: Rect::default(),
                    caret: None,
                    scrollable: false,
                    text_focus: false,
                },
            );
        };
        // One geometry authority for paint/mouse: visible buttons come from the
        // shared plan. Overflow (only the floor) stays keyboard-reachable via
        // mnemonics; a More menu is follow-up work.
        let title = format!("Bookmarks · {view_name}");
        render_responsive_frame(frame, &geometry, &title, ctx.active, theme);
        let surface = Surface {
            popup: geometry.frontmost,
            interior: geometry.interior,
            caret: None,
            scrollable: true,
            text_focus: false,
        };
        let inner = geometry.body.viewport;
        if inner.width == 0 || inner.height == 0 {
            return self.record(rows_hit, controls_hit, surface);
        }
        let count = format!("{} of {BOOKMARK_CAPACITY}", bookmarks.len());
        let count_w = u16::try_from(UnicodeWidthStr::width(count.as_str())).unwrap_or(0);
        // One authoritative list plan over 2*len logical rows (record, note).
        // The same rects drive paint, selection, scrollbar and mouse.
        let total_rows = bookmarks.len().saturating_mul(2);
        let selected_logical = (!bookmarks.is_empty()).then(|| {
            dialog
                .selected
                .min(bookmarks.len().saturating_sub(1))
                .saturating_mul(2)
                .saturating_add(1)
        });
        let list = plan_list(inner, count_w, total_rows, selected_logical, 0);
        if list.heading.height > 0 {
            frame.render_widget(
                Paragraph::new("Bookmarks").style(styles.label.add_modifier(Modifier::BOLD)),
                list.heading,
            );
            if list.count.width > 0 {
                frame.render_widget(Paragraph::new(count).style(styles.description), list.count);
            }
        }
        if bookmarks.is_empty() {
            if list.viewport.height > 0 {
                frame.render_widget(
                    Paragraph::new(truncated(
                        "No bookmarks in this view yet",
                        usize::from(list.viewport.width),
                    ))
                    .style(styles.description),
                    Rect::new(
                        list.viewport.x,
                        list.viewport.y,
                        list.viewport.width,
                        1.min(list.viewport.height),
                    ),
                );
            }
        } else {
            for (offset, row_rect) in list.row_rects.iter().enumerate() {
                let logical = list.first_row.saturating_add(offset);
                let index = logical / 2;
                let is_first = logical % 2 == 0;
                let Some(bookmark) = bookmarks.get(index) else {
                    continue;
                };
                let focused = index == dialog.selected;
                if is_first {
                    let record = provider.row_by_id(&dialog.view_id, &bookmark.id);
                    let marker = if focused {
                        if ascii { "> " } else { "› " }
                    } else {
                        "  "
                    };
                    let id = format!("{marker}#{}", bookmark.id.sequence);
                    let id_width = (BOOKMARK_ID_WIDTH + 2).min(row_rect.width);
                    frame.render_widget(
                        Paragraph::new(truncated(&id, usize::from(id_width))).style(if focused {
                            styles.selection
                        } else {
                            styles.label
                        }),
                        Rect::new(row_rect.x, row_rect.y, id_width, 1),
                    );
                    let time_x = row_rect
                        .x
                        .saturating_add(id_width)
                        .saturating_add(FIELD_GUTTER);
                    let time = record.as_ref().map_or_else(String::new, |row| {
                        // The seconds-resolution clock is what the log column shows.
                        row.timestamp
                            .split('.')
                            .next()
                            .unwrap_or(&row.timestamp)
                            .rsplit('T')
                            .next()
                            .unwrap_or_default()
                            .to_owned()
                    });
                    if time_x < row_rect.right() {
                        let time_width =
                            BOOKMARK_TIME_WIDTH.min(row_rect.right().saturating_sub(time_x));
                        frame.render_widget(
                            Paragraph::new(truncated(&time, usize::from(time_width)))
                                .style(styles.description),
                            Rect::new(time_x, row_rect.y, time_width, 1),
                        );
                        let text_x = time_x
                            .saturating_add(time_width)
                            .saturating_add(FIELD_GUTTER);
                        if text_x < row_rect.right() {
                            let text = record.as_ref().map_or_else(
                                || "record is no longer loaded".to_owned(),
                                |row| row.text.clone(),
                            );
                            frame.render_widget(
                                Paragraph::new(truncated(
                                    &text,
                                    usize::from(row_rect.right().saturating_sub(text_x)),
                                ))
                                .style(styles.description),
                                Rect::new(
                                    text_x,
                                    row_rect.y,
                                    row_rect.right().saturating_sub(text_x),
                                    1,
                                ),
                            );
                        }
                    }
                } else {
                    // §12.10: the note is the second line, muted only when absent —
                    // "no note" is not information, the note itself is.
                    let note_x = row_rect
                        .x
                        .saturating_add(BOOKMARK_ID_WIDTH + 2)
                        .saturating_add(FIELD_GUTTER);
                    let empty = bookmark.note.is_empty();
                    if note_x < row_rect.right() {
                        frame.render_widget(
                            Paragraph::new(truncated(
                                if empty { "no note" } else { &bookmark.note },
                                usize::from(row_rect.right().saturating_sub(note_x)),
                            ))
                            .style(if empty {
                                styles.unavailable
                            } else {
                                styles.description
                            }),
                            Rect::new(
                                note_x,
                                row_rect.y,
                                row_rect.right().saturating_sub(note_x),
                                1,
                            ),
                        );
                    } else {
                        frame.render_widget(
                            Paragraph::new(truncated(
                                if empty { "no note" } else { &bookmark.note },
                                usize::from(row_rect.width),
                            ))
                            .style(if empty {
                                styles.unavailable
                            } else {
                                styles.description
                            }),
                            *row_rect,
                        );
                    }
                }
                // Each logical line selects the bookmark it belongs to, so
                // clicking a note selects its record, as the 2-line block did.
                rows_hit.push((*row_rect, index));
            }
        }
        if let Some(bar) = list.scrollbar {
            render_scrollbar(
                frame,
                bar,
                list.first_row,
                total_rows.saturating_sub(list.row_rects.len()),
                theme,
                ascii,
            );
        }
        render_message(frame, geometry.message, state, &sentence, theme, ascii);
        render_help_text(frame, geometry.help, help, theme);
        let focused = actions
            .iter()
            .position(|(_, control)| *control == dialog.control);
        for (index, rect) in geometry.actions.buttons.iter() {
            let control = actions[*index].1;
            crate::dialog_controls::render_role_button(
                frame,
                *rect,
                actions[*index].0,
                if geometry.actions.default == Some(*index) {
                    ButtonRole::Default
                } else if *index == 3 {
                    ButtonRole::Destructive
                } else {
                    ButtonRole::Normal
                },
                focused == Some(*index),
                theme,
            );
            controls_hit.push((*rect, control));
        }
        self.record(rows_hit, controls_hit, surface)
    }
}
