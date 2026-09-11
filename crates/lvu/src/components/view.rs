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
use crate::dialog_controls::DialogStyles;
use crate::text_edit::{EditCommand, EditPolicy, TextCursor, edit, reset_cursor_to_end};
use crate::ui::{
    FIELD_GUTTER, MessageState, clipped_width, dialog_frame_regions, help_rows, message_rows,
    packed_button_rows, place_input_cursor_at, render_action_row, render_help_text, render_message,
    render_scrollbar, render_segmented_control,
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
///
/// `Tabs` is the one header focus stop for the four mode segments (§8.6, §8.8):
/// Tab reaches the header once, not four times. The active mode itself lives
/// on `ViewDialog::mode`, never in this enum.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ViewDialogControl {
    Tabs,
    Input,
    Sources,
    Apply,
}

/// Everything View draws that can be clicked.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ViewHit {
    Tab(ViewDialogMode),
    Control(ViewDialogControl),
    Source(usize),
    Body,
}

/// Recorded by `render`, consumed by `hit()` (§5.1). Every rect here was
/// painted this frame. Tab segment rects come from the same
/// `render_segmented_control` call that painted the header, so rendering,
/// hit-testing, focus and scrolling share them.
#[derive(Clone, Debug, Default)]
struct ViewGeometry {
    body: Rect,
    sources: Vec<(Rect, usize)>,
    tabs: Vec<(Rect, ViewDialogMode)>,
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

    /// The header segment rects, in `ViewDialogMode::ALL` order, from the same
    /// `render_segmented_control` call that painted them.
    pub fn tab_rects(&self) -> &[(Rect, ViewDialogMode)] {
        &self.geometry.tabs
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

    /// Tab order (§8.8): the one header stop, then the mode-specific body
    /// (the name field or the membership list), then Apply.
    fn controls(&self) -> Vec<ViewDialogControl> {
        vec![
            ViewDialogControl::Tabs,
            if self.mode == ViewDialogMode::Sources {
                ViewDialogControl::Sources
            } else {
                ViewDialogControl::Input
            },
            ViewDialogControl::Apply,
        ]
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

    /// A mode switch from a click, a mnemonic, the palette or a command.
    /// The keys land in the arriving mode's body, as the Filter tabs do.
    fn select_mode(&mut self, mode: ViewDialogMode, ctx: &Ctx<'_>) {
        self.seed(mode, ctx);
    }

    /// Left/Right on the focused header: the switch is immediate and the
    /// header keeps the keys so the user can keep moving, as the Filter tabs
    /// do. `seed` puts focus in the body, so this restores the header stop.
    fn move_tab(&mut self, delta: i32, ctx: &Ctx<'_>) {
        let index = ViewDialogMode::ALL
            .iter()
            .position(|mode| *mode == self.mode)
            .unwrap_or(0);
        let next = ViewDialogMode::ALL
            [(index as i32 + delta).rem_euclid(ViewDialogMode::ALL.len() as i32) as usize];
        self.seed(next, ctx);
        self.control = ViewDialogControl::Tabs;
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
            // §8.9: a segmented control consumes Enter to select the focused
            // segment, which Left/Right already made the active one.
            ViewDialogControl::Tabs => {}
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
            // The header owns Left/Right while it has focus (§8.6); a focused
            // name field owns them otherwise. Only a list lets Up/Down reach
            // the membership selection.
            KeyCode::Left if self.control == ViewDialogControl::Tabs => {
                self.move_tab(-1, ctx);
                Outcome::Consumed
            }
            KeyCode::Right if self.control == ViewDialogControl::Tabs => {
                self.move_tab(1, ctx);
                Outcome::Consumed
            }
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
            // The segments' own letters are §8.10 mnemonics, resolved by the
            // shell before the key arrives here. Alt-M and Alt-D are the two
            // chords that are *not* letters of their labels (§8.10), so they
            // stay here as the unlisted aliases they have always been.
            KeyCode::Char('m') if alt => {
                self.select_mode(ViewDialogMode::Sources, ctx);
                Outcome::Consumed
            }
            KeyCode::Char('d') if alt => {
                self.select_mode(ViewDialogMode::Clone, ctx);
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
            // §8.9: Enter and Space on the header select the focused segment,
            // which Left/Right already made the active one.
            KeyCode::Enter if self.control == ViewDialogControl::Tabs => Outcome::Consumed,
            KeyCode::Char(' ') if self.control == ViewDialogControl::Tabs => Outcome::Consumed,
            KeyCode::Enter => {
                self.activate(ctx);
                Outcome::Consumed
            }
            KeyCode::Backspace => self.text(EditCommand::Backspace),
            // Space picks a source out of the list wherever the keys are
            // except on the header, because in Sources mode there is no field
            // to type into.
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
                // A segment click switches immediately and puts the keys in
                // the arriving mode's body, as the Filter tabs do.
                Some(ViewHit::Tab(mode)) => {
                    self.select_mode(mode, ctx);
                    Outcome::Consumed
                }
                Some(ViewHit::Control(control)) => {
                    if self.controls().contains(&control) {
                        self.control = control;
                    }
                    // The body only takes focus; Apply acts.
                    if control == ViewDialogControl::Apply {
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

/// The four header segments in `ViewDialogMode::ALL` order (§8.6). They render
/// through `render_segmented_control` and never in the action row (§2.5).
/// §8.10 mnemonics: `b`, `c`, `r`, `s`. Alt-D (clone) and Alt-M (membership)
/// predate the rule that the letter is one of the label's; they keep working
/// unlisted, and Alt-only, because a bare `d` or `m` is not a letter the
/// header underlines.
const VIEW_TAB_LABELS: [&str; 4] = ["New &blank", "&Clone", "&Rename", "&Sources"];

/// The one action-row verb (§8.9). It is the only button the dialog draws.
fn view_apply_label(mode: ViewDialogMode) -> &'static str {
    if mode == ViewDialogMode::Sources {
        "Apply membership"
    } else {
        "Apply"
    }
}

/// Everything the shell's §8.10 resolver may press, in `press_action` order:
/// the one action first, then the four header segments (as the Filter dialog
/// lists its buttons before its `Search │ Advanced` segments).
fn view_mnemonic_targets(mode: ViewDialogMode) -> Vec<&'static str> {
    let mut targets = vec![view_apply_label(mode)];
    targets.extend(VIEW_TAB_LABELS);
    targets
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

/// §8.10: the chord the palette prints. Unlike Fields or Storage, this dialog
/// opens with the caret in its Name field, where `b`/`c`/`r`/`s` are text; the
/// bare letters press the buttons once focus leaves the field, but the chord
/// that works from where the dialog opens is Alt, so that is what is listed.
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
    /// The mode to open in: `Clone` from `v`, `Sources` when the View summary
    /// hands over its Sources row.
    type Open = ViewDialogMode;

    fn open(&mut self, mode: ViewDialogMode, ctx: &mut Ctx<'_>) {
        let Some(view) = ctx.views.active_item() else {
            return;
        };
        let view_id = view.id.clone();
        self.open = true;
        self.selected_source = 0;
        self.source_ids = ctx.views.source_ids(&view_id);
        self.geometry = ViewGeometry::default();
        self.seed(mode, ctx);
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

    fn action_labels(&self, _ctx: &Ctx<'_>) -> Vec<&'static str> {
        view_mnemonic_targets(self.mode)
    }

    /// Press the action or segment at `index` of `action_labels`, as a click
    /// on it would; a tab switch puts the keys in the arriving tab's body.
    fn press_action(&mut self, index: usize, ctx: &mut Ctx<'_>) -> Outcome {
        match index {
            0 => {
                self.submit(ctx);
                Outcome::Consumed
            }
            _ => match ViewDialogMode::ALL.get(index.saturating_sub(1)) {
                Some(mode) => {
                    self.select_mode(*mode, ctx);
                    Outcome::Consumed
                }
                None => Outcome::Ignored,
            },
        }
    }

    fn surface(&self) -> Surface {
        self.surface
    }

    fn hit(&self, point: (u16, u16)) -> Option<ViewHit> {
        let g = &self.geometry;
        g.tabs
            .iter()
            .find_map(|(rect, mode)| contains(*rect, point).then_some(ViewHit::Tab(*mode)))
            .or_else(|| {
                g.controls.iter().find_map(|(rect, control)| {
                    contains(*rect, point).then_some(ViewHit::Control(*control))
                })
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
        let apply_label = view_apply_label(self.mode);
        let apply_labels = [apply_label];
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
            header: 1,
            body: body_rows,
            message: message_rows(&sentence, width),
            help: help_rows(help, width),
            actions: packed_button_rows(width, &apply_labels),
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

        // §8.6: the mode control is the sticky header. The active segment
        // carries the selection style; the focus ring follows it when Tab
        // reaches the header. The rects below are the same ones hit-testing
        // reads, so rendering, mouse, focus and scrolling share them.
        let active = ViewDialogMode::ALL
            .iter()
            .position(|mode| *mode == self.mode)
            .unwrap_or(0);
        let focused = (self.control == ViewDialogControl::Tabs).then_some(active);
        let tabs: Vec<(Rect, ViewDialogMode)> = render_segmented_control(
            frame,
            regions.header,
            &VIEW_TAB_LABELS,
            active,
            focused,
            theme,
        )
        .into_iter()
        .zip(ViewDialogMode::ALL)
        .collect();

        // Recorded before the early return so a dialog too narrow to draw a
        // body still hit-tests its header rather than last frame's body.
        self.geometry = ViewGeometry {
            body: regions.body,
            tabs: tabs.clone(),
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

        // §3: the action row holds exactly one verb (§8.9). It is the default
        // the name field and the membership list submit; the modes live in the
        // header above and never here.
        let focused = (self.control == ViewDialogControl::Apply).then_some(0);
        for (_, rect) in
            render_action_row(frame, regions.actions, &apply_labels, focused, &[], theme)
        {
            controls_hit.push((rect, ViewDialogControl::Apply));
        }

        surface.caret = caret;
        self.geometry = ViewGeometry {
            body: regions.body,
            sources,
            tabs,
            controls: controls_hit,
        };
        self.surface = surface;
        surface
    }
}
