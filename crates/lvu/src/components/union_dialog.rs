//! The Union layer: pick input views for a timestamp-ordered union.
//!
//! Ownership: union-views worktree (new file). The draft/fence state machine
//! lives in `super::union` (std-only, unit-tested); this file is the thin
//! `Component` shell over it: list geometry, key/mouse map, action row and
//! outbox. Deliberately no text fields — the union is named by the shell
//! ("Union of A, B", renamable through the ordinary Rename flow) — so there
//! is no caret, no focus ring inside fields, and bare letters are mnemonics.
//!
//! Dialog contract (§3–§10, class M modal task): one default action (Create,
//! §8.9) that Enter runs; Space toggles the highlighted row (§8.2: Space
//! never presses a button); Escape closes; exactly one message row (§7.4)
//! naming the selection state or the last rejection; geometry recorded for
//! hit-testing, scrolling and selection from the same rects (AGENTS.md).

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, MouseButton, MouseEventKind};
use ratatui::{Frame, layout::Rect, widgets::Paragraph};

use crate::app::Views;
use crate::component::{CommandEntry, Component, Ctx, Event, Outcome, RenderCtx, Surface};
use crate::dialog_controls::{ActionRow, DialogStyles};
use crate::dialog_layout::{DialogClass, DialogContent, content_width};
use crate::ui::{
    MessageState, help_rows, message_rows, packed_button_rows, render_actions, render_help_text,
    render_message, truncated,
};

use super::union::{UnionDialog, UnionDialogRequest};

/// The accepted enriched cell whose native value constrains a new union.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SharedKeyUnionOrigin {
    pub origin_view_id: String,
    pub row_id: crate::provider::RowId,
    pub field: String,
}

/// How the one union chooser was opened.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UnionOpen {
    Plain,
    SharedKey(SharedKeyUnionOrigin),
}

/// What the layer draws that can be clicked.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UnionHit {
    Input(usize),
    Button(usize),
    Body,
}

/// Tab focus: the checklist, or one of the two buttons. Enter always runs
/// the default (Create) unless Cancel holds the focus ring (§8.9).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum UnionFocus {
    #[default]
    Inputs,
    Create,
    Cancel,
}

/// Recorded by `render`, consumed by `hit()`.
#[derive(Clone, Debug, Default)]
struct UnionGeometry {
    body: Rect,
    inputs: Vec<(Rect, usize)>,
    buttons: Vec<(Rect, usize)>,
}

fn contains(area: Rect, point: (u16, u16)) -> bool {
    point.0 >= area.x && point.0 < area.right() && point.1 >= area.y && point.1 < area.bottom()
}

/// Action row, in drawn order. Cancel carries no mnemonic: Escape closes the
/// layer, and `c` unambiguously creates.
const UNION_ACTIONS: [&str; 2] = ["&Create union", "Cancel"];

const UNION_HELP: &str = "Up/Down move · Space toggles a view · Enter creates · Esc closes. \
     Ties break by list order; equal times keep input order.";

#[derive(Debug, Default)]
pub struct UnionDialogComponent {
    draft: UnionDialog,
    highlighted: usize,
    scroll: usize,
    focus: UnionFocus,
    geometry: UnionGeometry,
    surface: Surface,
    outbox: Vec<UnionDialogRequest>,
    shared_key: Option<SharedKeyUnionOrigin>,
    /// A creation the shell now owns. Set on Create, cleared on open and on
    /// failure, so a second Enter cannot queue a duplicate candidate.
    submitted: bool,
}

impl UnionDialogComponent {
    /// Candidate input views in sidebar order: every open view is a legal
    /// union input, including raw and already-merged ones (cycles are fenced
    /// at submit, and a union over one source is a filter, not an error).
    fn candidates(views: &Views) -> Vec<(String, String)> {
        views
            .items()
            .iter()
            .map(|item| (item.id.clone(), item.name.clone()))
            .collect()
    }

    /// Requests the shell has not drained yet.
    pub fn take_requests(&mut self) -> Vec<UnionDialogRequest> {
        std::mem::take(&mut self.outbox)
    }

    /// A creation the shell refused: keep the draft, show why, and allow a
    /// corrected retry. Called while the layer is still open — the dialog
    /// only closes on success — mirroring the correlation accept-failed path.
    pub fn create_failed(&mut self, message: String) {
        self.submitted = false;
        self.draft.reject_candidate(message);
    }

    fn toggle_highlighted(&mut self, views: &Views) {
        let candidates = Self::candidates(views);
        let Some((id, _)) = candidates.get(self.highlighted) else {
            return;
        };
        if self.draft.inputs().contains(id) {
            self.draft.remove_input(id);
        } else if let Err(message) = self.draft.add_input(id.clone()) {
            self.draft.reject_candidate(message);
        }
    }

    fn create(&mut self) -> Outcome {
        if self.submitted {
            // The shell owns the submission now; another Enter must not queue
            // a second candidate behind it.
            return Outcome::Consumed;
        }
        if let Err(message) = self.draft.validate_for_create("") {
            // The union id is fresh at creation time, so self-reference
            // cannot trigger here; count is what fails, actionably.
            self.draft.reject_candidate(message);
            return Outcome::Consumed;
        }
        self.outbox.push(UnionDialogRequest::Create {
            inputs: self.draft.inputs().to_vec(),
            shared_key: self.shared_key.clone(),
        });
        self.submitted = true;
        Outcome::Consumed
    }

    fn move_highlight(&mut self, views: &Views, delta: isize) {
        let count = Self::candidates(views).len();
        if count == 0 {
            self.highlighted = 0;
            return;
        }
        let next = self.highlighted as isize + delta;
        self.highlighted = next.clamp(0, count as isize - 1) as usize;
    }

    fn cycle_focus(&mut self, delta: isize) {
        const ORDER: [UnionFocus; 3] = [UnionFocus::Inputs, UnionFocus::Create, UnionFocus::Cancel];
        let position = ORDER
            .iter()
            .position(|focus| *focus == self.focus)
            .unwrap_or(0);
        let next = (position as isize + delta).rem_euclid(ORDER.len() as isize) as usize;
        self.focus = ORDER[next];
    }

    fn key(&mut self, key: KeyEvent, ctx: &mut Ctx<'_>) -> Outcome {
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return Outcome::Ignored;
        }
        match key.code {
            KeyCode::Up => {
                self.move_highlight(ctx.views, -1);
                Outcome::Consumed
            }
            KeyCode::Down => {
                self.move_highlight(ctx.views, 1);
                Outcome::Consumed
            }
            KeyCode::Char(' ') => {
                self.toggle_highlighted(ctx.views);
                Outcome::Consumed
            }
            KeyCode::Tab => {
                self.cycle_focus(1);
                Outcome::Consumed
            }
            KeyCode::BackTab => {
                self.cycle_focus(-1);
                Outcome::Consumed
            }
            KeyCode::Enter if self.focus == UnionFocus::Cancel => Outcome::Close,
            KeyCode::Enter => self.create(),
            _ => Outcome::Ignored,
        }
    }

    fn mouse(&mut self, kind: MouseEventKind, hit: Option<UnionHit>, ctx: &mut Ctx<'_>) -> Outcome {
        match kind {
            MouseEventKind::Down(MouseButton::Left) => match hit {
                Some(UnionHit::Input(index)) => {
                    self.highlighted = index;
                    self.focus = UnionFocus::Inputs;
                    self.toggle_highlighted(ctx.views);
                    Outcome::Consumed
                }
                Some(UnionHit::Button(0)) => self.create(),
                Some(UnionHit::Button(_)) => Outcome::Close,
                _ => Outcome::Consumed,
            },
            _ => Outcome::Ignored,
        }
    }
}

impl Component for UnionDialogComponent {
    type Hit = UnionHit;
    type Open = UnionOpen;

    fn open(&mut self, params: UnionOpen, ctx: &mut Ctx<'_>) {
        self.draft = UnionDialog::new();
        // The active view arrives pre-selected so the common two-view union
        // is one toggle plus Create; anything unopenable is simply absent.
        self.shared_key = match params {
            UnionOpen::Plain => None,
            UnionOpen::SharedKey(origin) => Some(origin),
        };
        let origin = self
            .shared_key
            .as_ref()
            .map(|origin| origin.origin_view_id.clone())
            .or_else(|| ctx.views.active_id().map(str::to_owned));
        if let Some(origin) = origin.as_deref() {
            let _ = self.draft.add_input(origin.to_owned());
        }
        let candidates = Self::candidates(ctx.views);
        self.highlighted = origin
            .as_deref()
            .and_then(|origin| candidates.iter().position(|(id, _)| id == origin))
            .unwrap_or(0);
        self.scroll = 0;
        self.focus = UnionFocus::Inputs;
        self.geometry = UnionGeometry::default();
        self.surface = Surface::default();
        self.outbox.clear();
        self.submitted = false;
    }

    fn handle(&mut self, event: Event<Self::Hit>, ctx: &mut Ctx<'_>) -> Outcome {
        match event {
            Event::Key(key) => self.key(key, ctx),
            Event::Mouse { kind, hit, .. } => self.mouse(kind, hit, ctx),
            Event::Dismiss => Outcome::Close,
            Event::Paste(_) | Event::Command(_) | Event::View(_) | Event::Resize => {
                Outcome::Ignored
            }
        }
    }

    fn surface(&self) -> Surface {
        self.surface
    }

    fn hit(&self, point: (u16, u16)) -> Option<UnionHit> {
        let geometry = &self.geometry;
        geometry
            .inputs
            .iter()
            .find_map(|(rect, index)| contains(*rect, point).then_some(UnionHit::Input(*index)))
            .or_else(|| {
                geometry.buttons.iter().find_map(|(rect, index)| {
                    contains(*rect, point).then_some(UnionHit::Button(*index))
                })
            })
            .or_else(|| contains(geometry.body, point).then_some(UnionHit::Body))
    }

    fn action_labels(&self, _ctx: &Ctx<'_>) -> Vec<&'static str> {
        UNION_ACTIONS.to_vec()
    }

    fn press_action(&mut self, index: usize, _ctx: &mut Ctx<'_>) -> Outcome {
        match index {
            0 => self.create(),
            1 => Outcome::Close,
            _ => Outcome::Ignored,
        }
    }

    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &RenderCtx<'_>) -> Surface {
        let theme = ctx.theme;
        let ascii = ctx.ascii;
        let styles = DialogStyles::new(theme);
        let candidates = Self::candidates(ctx.views);
        self.highlighted = self.highlighted.min(candidates.len().saturating_sub(1));
        let selected = self.draft.inputs();
        let width = content_width(area, DialogClass::M);
        let (message_state, sentence) = match self.draft.error() {
            Some(error) => (MessageState::Error, error.to_owned()),
            None => {
                let count = candidates
                    .iter()
                    .filter(|(id, _)| selected.contains(id))
                    .count();
                if count >= 2 {
                    let suffix = if self.shared_key.is_some() {
                        " · filtered by the selected enriched key"
                    } else {
                        ""
                    };
                    (
                        MessageState::Ready,
                        format!(
                            "{count} of {} views selected · ties break by list order{suffix}",
                            candidates.len(),
                        ),
                    )
                } else {
                    (
                        MessageState::Disabled,
                        format!(
                            "select at least two views ({} of {} views selected)",
                            count,
                            candidates.len(),
                        ),
                    )
                }
            }
        };
        let content = DialogContent {
            header: 0,
            body: u16::try_from(candidates.len().max(1)).unwrap_or(4),
            message: message_rows(&sentence, width),
            help: help_rows(UNION_HELP, width),
            actions: packed_button_rows(width, &UNION_ACTIONS),
        };
        let regions = crate::ui::dialog_frame_regions(
            frame,
            area,
            DialogClass::M,
            "Union views",
            &content,
            theme,
        );
        let surface = Surface {
            popup: regions.popup,
            interior: regions.interior,
            caret: None,
            scrollable: candidates.len() > usize::from(regions.body.height),
            text_focus: false,
        };
        self.geometry = UnionGeometry {
            body: regions.body,
            ..UnionGeometry::default()
        };
        self.surface = surface;
        if regions.content.width == 0 {
            return surface;
        }
        let mut inputs_hit: Vec<(Rect, usize)> = Vec::new();
        if candidates.is_empty() {
            frame.render_widget(
                Paragraph::new("no views are open").style(styles.label),
                regions.body,
            );
        }
        let visible = usize::from(regions.body.height).max(1);
        if self.highlighted < self.scroll {
            self.scroll = self.highlighted;
        } else if self.highlighted >= self.scroll.saturating_add(visible) {
            self.scroll = self.highlighted.saturating_add(1).saturating_sub(visible);
        }
        self.scroll = self.scroll.min(candidates.len().saturating_sub(visible));
        for (visible_offset, (offset, (id, name))) in candidates
            .iter()
            .enumerate()
            .skip(self.scroll)
            .take(visible)
            .enumerate()
        {
            let y = regions
                .body
                .y
                .saturating_add(u16::try_from(visible_offset).unwrap_or(0));
            if y >= regions.body.bottom() {
                break;
            }
            let row = Rect::new(regions.content.x, y, regions.content.width, 1);
            let checked = selected.contains(id);
            let mark = if checked { "[x]" } else { "[ ]" };
            let line = format!("{mark} {name}");
            let style = if offset == self.highlighted {
                styles.selection
            } else {
                styles.label
            };
            frame.render_widget(
                Paragraph::new(truncated(&line, usize::from(row.width))).style(style),
                row,
            );
            inputs_hit.push((row, offset));
        }
        render_message(
            frame,
            regions.message,
            message_state,
            &sentence,
            theme,
            ascii,
        );
        render_help_text(frame, regions.help, UNION_HELP, theme);
        let focused_button = match self.focus {
            UnionFocus::Inputs => None,
            UnionFocus::Create => Some(0),
            UnionFocus::Cancel => Some(1),
        };
        let mut buttons_hit: Vec<(Rect, usize)> = Vec::new();
        for (index, rect) in render_actions(
            frame,
            regions.actions,
            ActionRow {
                labels: &UNION_ACTIONS,
                default: Some(0),
                destructive: &[],
                focused: focused_button,
            },
            theme,
        ) {
            buttons_hit.push((rect, index));
        }
        self.geometry.inputs = inputs_hit;
        self.geometry.buttons = buttons_hit;
        surface
    }

    fn commands(&self, views: &Views) -> Vec<CommandEntry> {
        // No palette verbs: two buttons driven by mnemonic, Enter and mouse
        // need none. The static `Union views` catalog entry opens the layer.
        let _ = views;
        Vec::new()
    }
}
