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
//! Note editing is still one layer, not the `OpenChild` §6.3 anticipates: the
//! note form replaces the list today rather than stacking over it, so making
//! it a child would draw the list under a scrim — a presentation change, which
//! §7.13 keeps out of a conversion commit.

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
use crate::dialog_controls::DialogStyles;
use crate::provider::RowId;
use crate::text_edit::{EditCommand, EditPolicy, TextCursor, edit};
use crate::ui::{
    FIELD_GUTTER, MessageState, dialog_frame_regions, help_rows, message_rows, packed_button_rows,
    render_action_row, render_form_field, render_help_text, render_message, render_scrollbar,
    truncated,
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
            KeyCode::Char('e') if key.modifiers.contains(KeyModifiers::ALT) => self.edit_note(ctx),
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
                shortcut: self.open.then_some("Alt-E"),
                ..NOTE_COMMAND
            },
            unavailable_reason: (!self.open).then_some("open Bookmarks first"),
        }]
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
        use crate::dialog_layout::{DialogClass, DialogContent, content_width, pane};
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
        let width = content_width(area, DialogClass::M);

        // §12.10: editing a note is its own small dialog, named for the record it
        // belongs to, so the list behind it is not competing for attention.
        if let Some(editing) = dialog.editing.clone() {
            // The child is class S, so it measures at the S content width; using
            // the list's width would under-count the help and clip it.
            let width = content_width(area, DialogClass::S);
            let help = "Notes are capped at 1024 bytes and saved with the view.";
            let (state, sentence) = if dialog.status.is_empty() {
                (MessageState::Ready, String::new())
            } else {
                (MessageState::Applied, dialog.status.clone())
            };
            let action_labels = ["Save note"];
            let content = DialogContent {
                header: 0,
                body: 1,
                message: message_rows(&sentence, width).max(1),
                help: help_rows(help, width),
                actions: packed_button_rows(width, &action_labels),
            };
            let title = format!("Bookmarks › Note for #{}", editing.sequence);
            let regions =
                dialog_frame_regions(frame, area, DialogClass::S, &title, &content, theme);
            let mut surface = Surface {
                popup: regions.popup,
                interior: regions.interior,
                caret: None,
                scrollable: true,
                // The note form takes text; the list behind it does not (§1).
                text_focus: dialog.control == C::Input,
            };
            if regions.body.height > 0 {
                let (input, caret) = render_form_field(
                    frame,
                    Rect::new(regions.body.x, regions.body.y, regions.body.width, 1),
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
            render_message(frame, regions.message, state, &sentence, theme, ascii);
            render_help_text(frame, regions.help, help, theme);
            let controls = [C::Save];
            let focused = controls
                .iter()
                .position(|control| *control == dialog.control);
            for (index, rect) in
                render_action_row(frame, regions.actions, &action_labels, focused, &[], theme)
            {
                controls_hit.push((rect, controls[index]));
            }
            return self.record(rows_hit, controls_hit, surface);
        }

        let help = if bookmarks.is_empty() {
            "Press b on a record to bookmark it."
        } else {
            concat!(
                "Go to selects the record in its source's All events view, where it ",
                "is always present. Raw context shows its neighbours without leaving."
            )
        };
        let (state, sentence) = if dialog.status.contains("limit") {
            (MessageState::Error, dialog.status.clone())
        } else if dialog.status.is_empty() {
            (MessageState::Ready, String::new())
        } else {
            (MessageState::Applied, dialog.status.clone())
        };
        // §12.10 `[ Go to ]`, honest now that bookmarks are source-scoped: the
        // record is always present in its source's All events view.
        let mut actions: Vec<(&str, C)> = Vec::new();
        if !bookmarks.is_empty() {
            actions.extend([
                ("Go to", C::Goto),
                ("Edit note", C::Edit),
                ("Raw context", C::Context),
                ("Remove", C::Delete),
            ]);
        }
        let action_labels = actions.iter().map(|(label, _)| *label).collect::<Vec<_>>();

        // Each bookmark is two rows: the record, then its note.
        let row_pairs = bookmarks.len().clamp(1, 8);
        let content = DialogContent {
            header: 0,
            body: u16::try_from(row_pairs * 2 + 1).unwrap_or(u16::MAX),
            message: message_rows(&sentence, width).max(1),
            help: help_rows(help, width),
            actions: packed_button_rows(width, &action_labels),
        };
        let title = format!("Bookmarks · {view_name}");
        let regions = dialog_frame_regions(frame, area, DialogClass::M, &title, &content, theme);
        let surface = Surface {
            popup: regions.popup,
            interior: regions.interior,
            caret: None,
            scrollable: true,
            text_focus: false,
        };
        let inner = regions.body;
        if inner.width == 0 || inner.height == 0 {
            return self.record(rows_hit, controls_hit, surface);
        }
        let count = format!("{} of {BOOKMARK_CAPACITY}", bookmarks.len());
        let rects = pane(
            inner,
            u16::try_from(UnicodeWidthStr::width(count.as_str())).unwrap_or(0),
            bookmarks.len() * 2,
        );
        if rects.heading.height > 0 {
            frame.render_widget(
                Paragraph::new("Bookmarks").style(styles.label.add_modifier(Modifier::BOLD)),
                rects.heading,
            );
            if rects.count.width > 0 {
                frame.render_widget(Paragraph::new(count).style(styles.description), rects.count);
            }
        }
        let visible_pairs = usize::from(rects.viewport.height / 2).max(1);
        let first = dialog
            .selected
            .saturating_add(1)
            .saturating_sub(visible_pairs);
        if bookmarks.is_empty() {
            if rects.viewport.height > 0 {
                frame.render_widget(
                    Paragraph::new(truncated(
                        "No bookmarks in this view yet",
                        usize::from(rects.viewport.width),
                    ))
                    .style(styles.description),
                    Rect::new(rects.viewport.x, rects.viewport.y, rects.viewport.width, 1),
                );
            }
        } else {
            for (offset, (index, bookmark)) in bookmarks
                .iter()
                .enumerate()
                .skip(first)
                .take(visible_pairs)
                .enumerate()
            {
                let y = rects.viewport.y.saturating_add((offset * 2) as u16);
                if y >= rects.viewport.bottom() {
                    break;
                }
                let focused = index == dialog.selected;
                let row = Rect::new(rects.viewport.x, y, rects.viewport.width, 1);
                let record = provider.row_by_id(&dialog.view_id, &bookmark.id);
                let marker = if focused {
                    if ascii { "> " } else { "› " }
                } else {
                    "  "
                };
                let id = format!("{marker}#{}", bookmark.id.sequence);
                let id_width = (BOOKMARK_ID_WIDTH + 2).min(row.width);
                frame.render_widget(
                    Paragraph::new(truncated(&id, usize::from(id_width))).style(if focused {
                        styles.selection
                    } else {
                        styles.label
                    }),
                    Rect::new(row.x, y, id_width, 1),
                );
                let time_x = row.x.saturating_add(id_width).saturating_add(FIELD_GUTTER);
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
                if time_x < row.right() {
                    let time_width = BOOKMARK_TIME_WIDTH.min(row.right().saturating_sub(time_x));
                    frame.render_widget(
                        Paragraph::new(truncated(&time, usize::from(time_width)))
                            .style(styles.description),
                        Rect::new(time_x, y, time_width, 1),
                    );
                    let text_x = time_x
                        .saturating_add(time_width)
                        .saturating_add(FIELD_GUTTER);
                    if text_x < row.right() {
                        let text = record.as_ref().map_or_else(
                            || "record is no longer loaded".to_owned(),
                            |row| row.text.clone(),
                        );
                        frame.render_widget(
                            Paragraph::new(truncated(
                                &text,
                                usize::from(row.right().saturating_sub(text_x)),
                            ))
                            .style(styles.description),
                            Rect::new(text_x, y, row.right().saturating_sub(text_x), 1),
                        );
                    }
                }
                // §12.10: the note is the second line, muted only when absent —
                // "no note" is not information, the note itself is.
                let note_y = y.saturating_add(1);
                if note_y < rects.viewport.bottom() {
                    let note_x = row.x.saturating_add(id_width).saturating_add(FIELD_GUTTER);
                    let empty = bookmark.note.is_empty();
                    frame.render_widget(
                        Paragraph::new(truncated(
                            if empty { "no note" } else { &bookmark.note },
                            usize::from(row.right().saturating_sub(note_x)),
                        ))
                        .style(if empty {
                            styles.unavailable
                        } else {
                            styles.description
                        }),
                        Rect::new(note_x, note_y, row.right().saturating_sub(note_x), 1),
                    );
                }
                // The whole two-line block is the row's hitbox, so clicking a note
                // selects the bookmark it belongs to.
                rows_hit.push((
                    Rect::new(
                        row.x,
                        y,
                        row.width,
                        2.min(rects.viewport.bottom().saturating_sub(y)),
                    ),
                    index,
                ));
            }
        }
        if let Some(bar) = rects.scrollbar {
            render_scrollbar(
                frame,
                bar,
                first * 2,
                (bookmarks.len() * 2).saturating_sub(usize::from(rects.viewport.height)),
                theme,
                ascii,
            );
        }
        render_message(frame, regions.message, state, &sentence, theme, ascii);
        render_help_text(frame, regions.help, help, theme);
        let focused = actions
            .iter()
            .position(|(_, control)| *control == dialog.control);
        for (index, rect) in render_action_row(
            frame,
            regions.actions,
            &action_labels,
            focused,
            // Removing a bookmark is the destructive one.
            &[3],
            theme,
        ) {
            controls_hit.push((rect, actions[index].1));
        }
        self.record(rows_hit, controls_hit, surface)
    }
}
