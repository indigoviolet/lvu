//! The View actions layer (`docs/dialog-system.md` §12.8), converted to the
//! component contract per `docs/component-model.md` §6.3 step 8.
//!
//! View is the first layer that asks the shell to *change the set of views*
//! rather than to query one, so it is where the `ViewMutationRequest` outbox
//! and `ViewEvent::SourcesChanged` arrive. The shape is the one §2.4 and §4.2
//! describe: the component pushes a request and keeps its draft; `lvu-app`
//! drains the outbox, does the work, and answers either by broadcasting
//! `SourcesChanged` for the view — which closes this layer like any other
//! consumer of shared state — or by handing the refusal back through
//! `fail`, which is this component's `complete` (§2.4).
//!
//! The name field is dialog-owned, so per §2.5 it keeps its own `TextCursor`
//! and calls `text_edit::edit` directly instead of going through
//! `ctx.cursors`: the draft is regenerated from the view on every open and on
//! every mode switch, so there is nothing for the bank to remember between
//! them.

use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use ratatui::{Frame, layout::Rect, style::Modifier, text::Line, widgets::Paragraph};
use unicode_width::UnicodeWidthStr;

use crate::app::{ViewDialogMode, ViewMutationRequest, Views, move_control};
use crate::command_palette::CommandId;
use crate::component::{
    CommandEntry, CommandSpec, Component, Ctx, Event, Outbox, Outcome, RenderCtx, Surface,
    ViewEvent, is_typed_char,
};
use crate::dialog_controls::{ActionRow, DialogStyles, button_line};
use crate::text_edit::{EditCommand, EditPolicy, TextCursor, edit, reset_cursor_to_end};
use crate::ui::{
    FIELD_GUTTER, MessageState, clipped_width, dialog_frame_regions, help_rows, message_rows,
    packed_button_rows, place_input_cursor_at, render_actions, render_help_text, render_message,
    render_scrollbar,
};

/// The dialog refuses a submission when the queue is full and says so in its
/// own message row, so the cap is the bound that refusal is worded against.
const VIEW_OUTBOX_CAP: usize = 8;

/// §12.8: a view name is a label, not a document.
const VIEW_NAME_MAX_BYTES: usize = 128;

/// The membership list never offers more than this many sources at once.
const VIEW_MAX_SOURCES: usize = 32;

/// Which control inside the View dialog has focus. UI-only state, so it lives
/// with the component rather than on `App` (§3).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ViewDialogControl {
    Mode(ViewDialogMode),
    Input,
    Sources,
    Apply,
}

/// Everything View draws that can be clicked.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ViewHit {
    Control(ViewDialogControl),
    Source(usize),
    Body,
}

/// Recorded by `render`, consumed by `hit()` (§5.1). Every rect here was
/// painted this frame.
#[derive(Clone, Debug, Default)]
struct ViewGeometry {
    body: Rect,
    sources: Vec<(Rect, usize)>,
    controls: Vec<(Rect, ViewDialogControl)>,
}

#[derive(Debug)]
pub struct ViewDialog {
    /// Whether the layer is on the stack. The slot is permanent so a late
    /// refusal after close has somewhere to land.
    open: bool,
    source_ids: Vec<String>,
    selected_source: usize,
    mode: ViewDialogMode,
    control: ViewDialogControl,
    draft: String,
    /// Dialog-owned caret for the dialog-owned name draft (§2.5).
    cursor: TextCursor,
    error: Option<String>,
    geometry: ViewGeometry,
    surface: Surface,
    pub outbox: Outbox<ViewMutationRequest>,
}

impl Default for ViewDialog {
    fn default() -> Self {
        Self {
            open: false,
            source_ids: Vec::new(),
            selected_source: 0,
            mode: ViewDialogMode::Clone,
            control: ViewDialogControl::Input,
            draft: String::new(),
            cursor: TextCursor::default(),
            error: None,
            geometry: ViewGeometry::default(),
            surface: Surface::default(),
            outbox: Outbox::new(VIEW_OUTBOX_CAP),
        }
    }
}

impl ViewDialog {
    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn mode(&self) -> ViewDialogMode {
        self.mode
    }

    pub fn control(&self) -> ViewDialogControl {
        self.control
    }

    pub fn draft(&self) -> &str {
        &self.draft
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn selected_source(&self) -> usize {
        self.selected_source
    }

    pub fn source_ids(&self) -> &[String] {
        &self.source_ids
    }

    /// Geometry recorded by the last `render`; `hit()` is how input reaches it.
    pub fn source_rects(&self) -> &[(Rect, usize)] {
        &self.geometry.sources
    }

    pub fn control_rects(&self) -> &[(Rect, ViewDialogControl)] {
        &self.geometry.controls
    }

    /// A refusal from the worker. This is the component's `complete` (§2.4):
    /// the draft is kept so the user can correct it, exactly as the old
    /// `App::view_request_failed` kept `ViewDialogState`.
    pub fn fail(&mut self, message: String) {
        if self.open {
            self.error = Some(message);
        }
    }

    fn text_editing(&self) -> bool {
        self.mode != ViewDialogMode::Sources && self.control == ViewDialogControl::Input
    }

    /// Tab order: the four mode buttons, then the field or the list, then Apply.
    fn controls(&self) -> Vec<ViewDialogControl> {
        let mut controls = ViewDialogMode::ALL
            .iter()
            .copied()
            .map(ViewDialogControl::Mode)
            .collect::<Vec<_>>();
        controls.push(if self.mode == ViewDialogMode::Sources {
            ViewDialogControl::Sources
        } else {
            ViewDialogControl::Input
        });
        controls.push(ViewDialogControl::Apply);
        controls
    }

    fn seed(&mut self, mode: ViewDialogMode, ctx: &Ctx<'_>) {
        let Some(view) = ctx.views.active_item() else {
            return;
        };
        self.mode = mode;
        self.control = if mode == ViewDialogMode::Sources {
            ViewDialogControl::Sources
        } else {
            ViewDialogControl::Input
        };
        self.error = None;
        self.draft = match mode {
            ViewDialogMode::Blank => "New view".into(),
            ViewDialogMode::Clone => format!("Copy of {}", view.name),
            ViewDialogMode::Rename | ViewDialogMode::Sources => view.name.clone(),
        };
        reset_cursor_to_end(&self.draft, &mut self.cursor);
    }

    fn select_mode(&mut self, mode: ViewDialogMode, ctx: &Ctx<'_>) {
        self.seed(mode, ctx);
    }

    fn move_source(&mut self, delta: i32, ctx: &Ctx<'_>) {
        if self.mode == ViewDialogMode::Sources && !ctx.sources.is_empty() {
            self.selected_source = (self.selected_source as i32 + delta)
                .clamp(0, ctx.sources.len() as i32 - 1) as usize;
        }
    }

    fn toggle_source(&mut self, ctx: &Ctx<'_>) {
        if self.mode != ViewDialogMode::Sources {
            return;
        }
        let Some(source) = ctx.sources.get(self.selected_source) else {
            return;
        };
        if ctx
            .views
            .active_item()
            .is_some_and(|view| view.source_id == source.id)
        {
            self.error = Some("the owning source stays in this view".into());
        } else if let Some(index) = self.source_ids.iter().position(|id| id == &source.id) {
            self.source_ids.remove(index);
            self.error = None;
        } else if self.source_ids.len() < VIEW_MAX_SOURCES {
            self.source_ids.push(source.id.clone());
            self.error = None;
        }
    }

    fn reorder_source(&mut self, delta: i32, ctx: &Ctx<'_>) {
        if self.mode == ViewDialogMode::Sources
            && let Some(source) = ctx.sources.get(self.selected_source)
            && let Some(index) = self.source_ids.iter().position(|id| id == &source.id)
        {
            let target = (index as i32 + delta).clamp(0, self.source_ids.len() as i32 - 1) as usize;
            self.source_ids.swap(index, target);
        }
    }

    /// The one place a view mutation leaves this component. The draft survives
    /// a refusal, so an invalid name never destroys what the user typed.
    fn submit(&mut self, ctx: &Ctx<'_>) {
        let name = self.draft.trim();
        let Some(view) = ctx.views.active_item() else {
            return;
        };
        if name.is_empty() {
            self.error = Some("view name cannot be empty".into());
            return;
        }
        let request = ViewMutationRequest {
            source_ids: self.source_ids.clone(),
            mode: self.mode,
            source_id: view.source_id.clone(),
            view_id: view.id.clone(),
            name: name.to_owned(),
        };
        if self.outbox.push(request).is_err() {
            self.error = Some("view request queue is full".into());
        }
    }

    fn activate(&mut self, ctx: &Ctx<'_>) {
        match self.control {
            ViewDialogControl::Mode(mode) => self.select_mode(mode, ctx),
            ViewDialogControl::Apply | ViewDialogControl::Sources | ViewDialogControl::Input => {
                self.submit(ctx)
            }
        }
    }

    /// Editing verbs for the dialog-owned name field. `Insert` also carries a
    /// paste, which is why it takes a `&str` rather than a `char`.
    fn text(&mut self, command: EditCommand<'_>) -> Outcome {
        if !self.text_editing() {
            return Outcome::Ignored;
        }
        let policy = EditPolicy {
            max_bytes: VIEW_NAME_MAX_BYTES,
            multiline: false,
        };
        let outcome = edit(&mut self.draft, &mut self.cursor, command, policy);
        if outcome.changed {
            self.error = None;
        }
        if outcome.changed || outcome.moved {
            Outcome::Consumed
        } else {
            Outcome::Ignored
        }
    }

    fn key(&mut self, key: KeyEvent, ctx: &mut Ctx<'_>) -> Outcome {
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return Outcome::Ignored;
        }
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        // Line-editing chords reach the field before the dialog's own table,
        // and are inert while a button has focus — as they were when
        // `App::handle` dropped them for a focus that was not editing.
        if control {
            return match key.code {
                KeyCode::Char('a') => self.text(EditCommand::StartOfLine),
                KeyCode::Char('e') => self.text(EditCommand::EndOfLine),
                KeyCode::Char('k') => self.text(EditCommand::KillToEndOfLine),
                _ => Outcome::Ignored,
            };
        }
        match key.code {
            KeyCode::Up if alt => {
                self.reorder_source(-1, ctx);
                Outcome::Consumed
            }
            KeyCode::Down if alt => {
                self.reorder_source(1, ctx);
                Outcome::Consumed
            }
            // A focused name field owns the arrow keys; only a list or a
            // button lets them reach the membership selection.
            KeyCode::Left if self.text_editing() => self.text(EditCommand::MoveLeft),
            KeyCode::Right if self.text_editing() => self.text(EditCommand::MoveRight),
            KeyCode::Up if self.text_editing() => self.text(EditCommand::MoveUp),
            KeyCode::Down if self.text_editing() => self.text(EditCommand::MoveDown),
            KeyCode::Up => {
                self.move_source(-1, ctx);
                Outcome::Consumed
            }
            KeyCode::Down => {
                self.move_source(1, ctx);
                Outcome::Consumed
            }
            KeyCode::Char('m') | KeyCode::Char('s') if alt => {
                self.select_mode(ViewDialogMode::Sources, ctx);
                Outcome::Consumed
            }
            KeyCode::Char('b') if alt => {
                self.select_mode(ViewDialogMode::Blank, ctx);
                Outcome::Consumed
            }
            KeyCode::Char('d') | KeyCode::Char('c') if alt => {
                self.select_mode(ViewDialogMode::Clone, ctx);
                Outcome::Consumed
            }
            KeyCode::Char('r') if alt => {
                self.select_mode(ViewDialogMode::Rename, ctx);
                Outcome::Consumed
            }
            KeyCode::Tab if key.modifiers.contains(KeyModifiers::SHIFT) => {
                self.control = move_control(self.control, &self.controls(), -1);
                Outcome::Consumed
            }
            KeyCode::BackTab => {
                self.control = move_control(self.control, &self.controls(), -1);
                Outcome::Consumed
            }
            KeyCode::Tab => {
                self.control = move_control(self.control, &self.controls(), 1);
                Outcome::Consumed
            }
            KeyCode::Enter => {
                self.activate(ctx);
                Outcome::Consumed
            }
            KeyCode::Backspace => self.text(EditCommand::Backspace),
            // Space picks a source out of the list whichever control has
            // focus, because in Sources mode there is no field to type into.
            KeyCode::Char(' ') if self.mode == ViewDialogMode::Sources => {
                self.toggle_source(ctx);
                Outcome::Consumed
            }
            KeyCode::Char(character) if is_typed_char(&key) => {
                let mut buffer = [0u8; 4];
                self.text(EditCommand::Insert(character.encode_utf8(&mut buffer)))
            }
            _ => Outcome::Ignored,
        }
    }

    fn mouse(&mut self, kind: MouseEventKind, hit: Option<ViewHit>, ctx: &mut Ctx<'_>) -> Outcome {
        match kind {
            MouseEventKind::Down(MouseButton::Left) => match hit {
                Some(ViewHit::Control(control)) => {
                    if self.controls().contains(&control) {
                        self.control = control;
                    }
                    // A field or a list only takes focus; a button acts.
                    if !matches!(
                        control,
                        ViewDialogControl::Input | ViewDialogControl::Sources
                    ) {
                        self.activate(ctx);
                    }
                    Outcome::Consumed
                }
                Some(ViewHit::Source(index)) => {
                    self.selected_source = index;
                    Outcome::Consumed
                }
                _ => Outcome::Consumed,
            },
            MouseEventKind::ScrollUp => {
                self.move_source(-1, ctx);
                Outcome::Consumed
            }
            MouseEventKind::ScrollDown => {
                self.move_source(1, ctx);
                Outcome::Consumed
            }
            _ => Outcome::Ignored,
        }
    }
}

fn contains(area: Rect, point: (u16, u16)) -> bool {
    point.0 >= area.x && point.0 < area.right() && point.1 >= area.y && point.1 < area.bottom()
}

/// The action row, in the order it is drawn. The mode buttons double as the
/// mode indicator, so rendering and Tab order share this list.
pub(crate) fn view_dialog_button_controls(
    mode: ViewDialogMode,
) -> Vec<(ViewDialogControl, &'static str)> {
    let mut controls = vec![
        // §8.10 mnemonics. Alt-D (clone) and Alt-M (membership) predate the rule
        // that the letter is one of the label's; they keep working unlisted.
        (ViewDialogControl::Mode(ViewDialogMode::Blank), "New &blank"),
        (ViewDialogControl::Mode(ViewDialogMode::Clone), "&Clone"),
        (ViewDialogControl::Mode(ViewDialogMode::Rename), "&Rename"),
        (ViewDialogControl::Mode(ViewDialogMode::Sources), "&Sources"),
    ];
    controls.push((
        ViewDialogControl::Apply,
        if mode == ViewDialogMode::Sources {
            "Apply membership"
        } else {
            "Apply"
        },
    ));
    controls
}

/// §4.3: the four mode choices the palette contributes for this layer. Their
/// availability and their shortcut column come from the component, so the
/// palette no longer probes a focus-specific key table to find `Alt-B`.
const VIEW_COMMANDS: &[(CommandId, CommandSpec)] = &[
    (
        CommandId::ViewBlank,
        CommandSpec {
            id: CommandId::ViewBlank,
            name: "Create blank view",
            description: "Select a blank view in the view dialog",
            category: "Views",
            aliases: &["new empty"],
            shortcut: None,
        },
    ),
    (
        CommandId::ViewClone,
        CommandSpec {
            id: CommandId::ViewClone,
            name: "Clone view",
            description: "Select clone in the view dialog",
            category: "Views",
            aliases: &["duplicate", "copy"],
            shortcut: None,
        },
    ),
    (
        CommandId::ViewRename,
        CommandSpec {
            id: CommandId::ViewRename,
            name: "Rename view",
            description: "Select rename in the view dialog",
            category: "Views",
            aliases: &["name"],
            shortcut: None,
        },
    ),
    (
        CommandId::ViewSources,
        CommandSpec {
            id: CommandId::ViewSources,
            name: "Merge / edit view sources",
            description: "Select ordered open sources; share existing captures",
            category: "Views",
            aliases: &["merge", "sources", "membership"],
            shortcut: None,
        },
    ),
];

fn view_command_mode(id: CommandId) -> Option<ViewDialogMode> {
    match id {
        CommandId::ViewBlank => Some(ViewDialogMode::Blank),
        CommandId::ViewClone => Some(ViewDialogMode::Clone),
        CommandId::ViewRename => Some(ViewDialogMode::Rename),
        CommandId::ViewSources => Some(ViewDialogMode::Sources),
        _ => None,
    }
}

fn view_command_shortcut(id: CommandId) -> Option<&'static str> {
    match id {
        CommandId::ViewBlank => Some("Alt-B"),
        CommandId::ViewClone => Some("Alt-C"),
        CommandId::ViewRename => Some("Alt-R"),
        CommandId::ViewSources => Some("Alt-S"),
        _ => None,
    }
}

impl Component for ViewDialog {
    type Hit = ViewHit;
    type Open = ();

    fn open(&mut self, _params: (), ctx: &mut Ctx<'_>) {
        let Some(view) = ctx.views.active_item() else {
            return;
        };
        let view_id = view.id.clone();
        self.open = true;
        self.selected_source = 0;
        self.source_ids = ctx.views.source_ids(&view_id);
        self.geometry = ViewGeometry::default();
        self.seed(ViewDialogMode::Clone, ctx);
    }

    fn handle(&mut self, event: Event<ViewHit>, ctx: &mut Ctx<'_>) -> Outcome {
        match event {
            Event::Key(key) => self.key(key, ctx),
            Event::Paste(text) => self.text(EditCommand::Insert(&text)),
            Event::Mouse { kind, hit, .. } => self.mouse(kind, hit, ctx),
            Event::Dismiss => {
                self.open = false;
                Outcome::Close
            }
            // §4.2: the shell says what happened to the view, not what this
            // dialog should do about it. A view whose membership the store has
            // accepted is a view this dialog has nothing left to edit.
            Event::View(ViewEvent::SourcesChanged { .. }) => {
                self.open = false;
                Outcome::Close
            }
            Event::Command(id) => match view_command_mode(id) {
                Some(mode) => {
                    self.select_mode(mode, ctx);
                    Outcome::Consumed
                }
                None => Outcome::Ignored,
            },
            Event::View(_) | Event::Resize => Outcome::Ignored,
        }
    }

    fn commands(&self, _views: &Views) -> Vec<CommandEntry> {
        VIEW_COMMANDS
            .iter()
            .map(|(id, spec)| CommandEntry {
                spec: CommandSpec {
                    shortcut: self.open.then(|| view_command_shortcut(*id)).flatten(),
                    ..*spec
                },
                unavailable_reason: (!self.open).then_some("open Manage views first"),
            })
            .collect()
    }

    fn surface(&self) -> Surface {
        self.surface
    }

    fn hit(&self, point: (u16, u16)) -> Option<ViewHit> {
        let g = &self.geometry;
        g.controls
            .iter()
            .find_map(|(rect, control)| {
                contains(*rect, point).then_some(ViewHit::Control(*control))
            })
            .or_else(|| {
                g.sources.iter().find_map(|(rect, index)| {
                    contains(*rect, point).then_some(ViewHit::Source(*index))
                })
            })
            .or_else(|| contains(g.body, point).then_some(ViewHit::Body))
    }

    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &RenderCtx<'_>) -> Surface {
        use crate::dialog_layout::{DialogClass, DialogContent, content_width, pane};

        let theme = ctx.theme;
        let ascii = ctx.ascii;
        let styles = DialogStyles::new(theme);
        let mut sources: Vec<(Rect, usize)> = Vec::new();
        let mut controls_hit: Vec<(Rect, ViewDialogControl)> = Vec::new();
        let mut caret: Option<(u16, u16)> = None;

        let sources_mode = self.mode == ViewDialogMode::Sources;
        let controls = view_dialog_button_controls(self.mode);
        let labels: Vec<&str> = controls.iter().map(|(_, label)| *label).collect();
        let width = content_width(area, DialogClass::M);

        let (state, sentence) = match self.error.as_deref() {
            Some(error) => (MessageState::Error, error.to_owned()),
            None if sources_mode => (
                MessageState::Ready,
                "changing membership keeps every capture".to_owned(),
            ),
            None => (
                MessageState::Ready,
                "creating, cloning and renaming keep the capture".to_owned(),
            ),
        };
        let help = if sources_mode && ctx.sources.len() > 1 {
            "Sources are ordered by position, then by record sequence, not by clock time."
        } else {
            ""
        };

        // §5.2: the body asks for exactly the rows its content needs. The Sources
        // list is a pane (heading + one row per source) capped at 12.
        let body_rows = if sources_mode {
            1 + u16::try_from(ctx.sources.len().clamp(1, 12)).unwrap_or(1)
        } else {
            1
        };
        let content = DialogContent {
            header: 0,
            body: body_rows,
            message: message_rows(&sentence, width),
            help: help_rows(help, width),
            actions: packed_button_rows(width, &labels),
        };

        let title = match ctx.views.active_item() {
            Some(view) => format!("View · {}", view.name),
            None => "View".to_owned(),
        };
        let regions = dialog_frame_regions(frame, area, DialogClass::M, &title, &content, theme);
        let mut surface = Surface {
            popup: regions.popup,
            interior: regions.interior,
            caret: None,
            scrollable: true,
            text_focus: self.text_editing(),
        };
        // Recorded before the early return so a dialog too narrow to draw a
        // body still hit-tests against nothing rather than against last frame.
        self.geometry = ViewGeometry {
            body: regions.body,
            ..ViewGeometry::default()
        };
        self.surface = surface;
        if regions.content.width == 0 {
            return surface;
        }

        if sources_mode {
            // §8.5/§8.7: a list is a pane — heading with a count, indented rows,
            // and a scrollbar only when the rows do not fit.
            let total = ctx.sources.len();
            let rects = pane(regions.body, 12, total);
            frame.render_widget(
                Paragraph::new("Sources").style(styles.label.add_modifier(Modifier::BOLD)),
                rects.heading,
            );
            if rects.count.width > 0 {
                frame.render_widget(
                    Paragraph::new(Line::from(format!(
                        "{} of {total}",
                        self.selected_source.saturating_add(1).min(total.max(1))
                    )))
                    .style(styles.description)
                    .right_aligned(),
                    rects.count,
                );
            }
            let visible = usize::from(rects.viewport.height);
            // §9: the viewport windows on the selection so the cursor is always
            // drawn, and the same window feeds the row hitboxes below.
            let first = self
                .selected_source
                .saturating_sub(visible.saturating_sub(1))
                .min(total.saturating_sub(visible.min(total)));
            for (offset, (index, source)) in ctx
                .sources
                .iter()
                .enumerate()
                .skip(first)
                .take(visible)
                .enumerate()
            {
                let order = self.source_ids.iter().position(|id| id == &source.id);
                let selected = index == self.selected_source;
                let checkbox = if order.is_some() { "[x]" } else { "[ ]" };
                let marker = if selected {
                    if ascii { "> " } else { "› " }
                } else {
                    "  "
                };
                let position = order
                    .map(|value| format!("{:>2} ", value + 1))
                    .unwrap_or_else(|| "   ".to_owned());
                let text = format!("{marker}{checkbox} {position}{}", source.name);
                let row = Rect::new(
                    rects.viewport.x,
                    rects.viewport.y.saturating_add(offset as u16),
                    rects.viewport.width,
                    1,
                );
                frame.render_widget(
                    Paragraph::new(clipped_width(&text, usize::from(row.width))).style(
                        if selected {
                            styles.selection
                        } else {
                            styles.description
                        },
                    ),
                    row,
                );
                sources.push((row, index));
            }
            if let Some(bar) = rects.scrollbar {
                render_scrollbar(
                    frame,
                    bar,
                    first,
                    total.saturating_sub(visible),
                    theme,
                    ascii,
                );
            }
        } else {
            // §4.2: one labelled row. The field rect is exactly what gets painted,
            // and the caret is placed inside it.
            let label_width = u16::try_from(UnicodeWidthStr::width("Name")).unwrap_or(4);
            let field_x = regions
                .content
                .x
                .saturating_add(label_width)
                .saturating_add(FIELD_GUTTER);
            let row = Rect::new(regions.content.x, regions.body.y, regions.content.width, 1);
            frame.render_widget(Paragraph::new("Name").style(styles.label), row);
            let field = Rect::new(
                field_x.min(regions.content.right()),
                row.y,
                regions.content.right().saturating_sub(field_x),
                1,
            );
            if field.width > 0 {
                // The field paints its caret whether or not it has focus, and
                // a button-focused dialog showed it at the end of the name:
                // `App::active_text_cursor` returned `None` for any control but
                // `Input`, and the old call fell back to the draft's length.
                let at = if self.text_editing() {
                    self.cursor.char_index
                } else {
                    self.draft.chars().count()
                };
                caret = place_input_cursor_at(frame, field, 0, 0, &self.draft, at, theme);
            }
        }

        render_message(frame, regions.message, state, &sentence, theme, ascii);
        render_help_text(frame, regions.help, help, theme);

        // §3: actions live in their own rect, so they can no longer be drawn into
        // the help text the way the old fixed-offset button block was.
        let focused = controls
            .iter()
            .position(|(control, _)| *control == self.control);
        // §8.9: the default is `Apply`, which is what the name field and the
        // membership list submit; the mode buttons before it are the mode
        // indicator (§12.8 moves them into a header segment).
        let default = controls
            .iter()
            .position(|(control, _)| *control == ViewDialogControl::Apply);
        for (index, rect) in render_actions(
            frame,
            regions.actions,
            ActionRow {
                labels: &labels,
                default,
                destructive: &[],
                focused,
            },
            theme,
        ) {
            let (control, label) = controls[index];
            let selected = matches!(control, ViewDialogControl::Mode(value) if value == self.mode);
            if selected && focused != Some(index) {
                // The active mode reads as applied and bold; the underline is
                // the mnemonic's (§8.10), so it is not borrowed for this.
                let style = styles.applied.add_modifier(Modifier::BOLD);
                frame.render_widget(Paragraph::new(button_line(label, style)).style(style), rect);
            }
            controls_hit.push((rect, control));
        }

        surface.caret = caret;
        self.geometry = ViewGeometry {
            body: regions.body,
            sources,
            controls: controls_hit,
        };
        self.surface = surface;
        surface
    }
}
