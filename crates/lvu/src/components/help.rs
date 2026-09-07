//! The Help layer (`docs/dialog-system.md` §12.15), converted to the component
//! contract per `docs/component-model.md` §6.3 step 6.
//!
//! Help is the smallest conversion: it has no outbox, no draft, no child and
//! nothing to submit. All it owns is the scroll offset and the limit its own
//! layout computes, which is exactly the pair that used to live on `App` as
//! `help_scroll`/`help_scroll_limit` — geometry the render produced and the
//! keymap consumed, with `App` in between for no reason. `help_return_focus`
//! disappears into stack discipline (§1): `Close` restores whatever was under
//! the layer.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, MouseButton, MouseEventKind};
use ratatui::{
    Frame,
    layout::Rect,
    style::Modifier,
    text::{Line, Span},
    widgets::Paragraph,
};
use unicode_width::UnicodeWidthStr;

use crate::component::{Component, Ctx, Event, Outcome, RenderCtx, Surface};
use crate::dialog_controls::DialogStyles;
use crate::theme::Theme;
use crate::ui::{dialog_frame_regions, render_scrollbar, wrap_sentence};

/// §12.15: two columns once the *content* is this wide. Measured on the content,
/// not the body, so a 100-column terminal is not excluded by its own padding.
const HELP_TWO_COLUMN_WIDTH: u16 = 88;
/// The gutter between the two columns.
const HELP_COLUMN_GAP: u16 = 2;

/// Help draws one scrollable body and nothing else clickable; a press anywhere
/// inside the popup closes it, exactly as it did from the shell's mouse arm.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HelpHit {
    Body,
}

#[derive(Debug, Default)]
pub struct HelpDialog {
    /// Whether the layer is on the stack. The slot is permanent, as every
    /// component's is (§2.4).
    open: bool,
    scroll: usize,
    /// Recorded by the last `render`: the two-column layout decides how tall
    /// the content is, so the scroll bound is geometry, not state the keymap
    /// can know on its own (§5.1).
    scroll_limit: usize,
    body: Rect,
    surface: Surface,
}

impl HelpDialog {
    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn scroll(&self) -> usize {
        self.scroll
    }

    pub fn scroll_limit(&self) -> usize {
        self.scroll_limit
    }

    /// The bound is whatever the last `render` measured, so a terminal that got
    /// wider between frames pulls the offset back with it. `Action::ScrollHelp`
    /// also carried `i32::MIN`/`i32::MAX` jump-to-end encodings; no key or wheel
    /// ever produced one, so they are not moved across.
    fn scroll_by(&mut self, delta: i32) {
        self.scroll = self
            .scroll
            .saturating_add_signed(delta as isize)
            .min(self.scroll_limit);
    }

    fn key(&mut self, key: KeyEvent) -> Outcome {
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return Outcome::Ignored;
        }
        match key.code {
            // `?` is the toggle it always was; Escape and `q` reach the layer
            // as `Event::Dismiss` instead.
            KeyCode::Char('?') => {
                self.open = false;
                return Outcome::Close;
            }
            KeyCode::Up | KeyCode::Char('k') => self.scroll_by(-1),
            KeyCode::Down | KeyCode::Char('j') => self.scroll_by(1),
            _ => return Outcome::Ignored,
        }
        Outcome::Consumed
    }

    fn mouse(&mut self, kind: MouseEventKind) -> Outcome {
        match kind {
            MouseEventKind::ScrollUp => self.scroll_by(-1),
            MouseEventKind::ScrollDown => self.scroll_by(1),
            MouseEventKind::Down(MouseButton::Left) => {
                self.open = false;
                return Outcome::Close;
            }
            _ => return Outcome::Ignored,
        }
        Outcome::Consumed
    }
}

fn contains(area: Rect, point: (u16, u16)) -> bool {
    point.0 >= area.x && point.0 < area.right() && point.1 >= area.y && point.1 < area.bottom()
}

impl Component for HelpDialog {
    type Hit = HelpHit;
    type Open = ();

    fn open(&mut self, _params: (), _ctx: &mut Ctx<'_>) {
        self.open = true;
        self.scroll = 0;
    }

    fn handle(&mut self, event: Event<HelpHit>, _ctx: &mut Ctx<'_>) -> Outcome {
        match event {
            Event::Key(key) => self.key(key),
            Event::Mouse { kind, .. } => self.mouse(kind),
            Event::Dismiss => {
                self.open = false;
                Outcome::Close
            }
            Event::Command(_) | Event::Paste(_) | Event::View(_) | Event::Resize => {
                Outcome::Ignored
            }
        }
    }

    fn surface(&self) -> Surface {
        self.surface
    }

    fn hit(&self, point: (u16, u16)) -> Option<HelpHit> {
        contains(self.body, point).then_some(HelpHit::Body)
    }

    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &RenderCtx<'_>) -> Surface {
        use crate::dialog_layout::{DialogClass, DialogContent, content_width};
        let theme = ctx.theme;
        let ascii = ctx.ascii;
        let agent = if ascii { "Agent" } else { "🧠" };
        let sections = help_sections(agent);
        let width = content_width(area, DialogClass::L);
        // §12.15: nothing here is actionable — `?` and Escape close it — so there
        // is no action row and no message row to carry a state it does not have.
        let content = DialogContent {
            header: 0,
            body: u16::MAX,
            message: 0,
            help: 0,
            actions: 0,
        };
        let regions = dialog_frame_regions(frame, area, DialogClass::L, "Help", &content, theme);
        // Nothing here is a text field, so `q` dismisses the layer (§1); the whole
        // body scrolls, so the wheel is wanted anywhere over the popup.
        let surface = Surface {
            popup: regions.popup,
            interior: regions.interior,
            caret: None,
            scrollable: true,
            text_focus: false,
        };
        let body = regions.body;
        self.body = body;
        self.surface = surface;
        if body.width == 0 || body.height == 0 {
            self.scroll_limit = 0;
            return surface;
        }

        let two_columns = width >= HELP_TWO_COLUMN_WIDTH;
        // §9: the scrollbar lives in the last body column, so the text never runs
        // into the border.
        let text = Rect::new(body.x, body.y, body.width.saturating_sub(1), body.height);
        let columns: Vec<Rect> = if two_columns {
            let each = text.width.saturating_sub(HELP_COLUMN_GAP) / 2;
            vec![
                Rect::new(text.x, text.y, each, text.height),
                Rect::new(
                    text.x.saturating_add(each).saturating_add(HELP_COLUMN_GAP),
                    text.y,
                    text.width
                        .saturating_sub(each)
                        .saturating_sub(HELP_COLUMN_GAP),
                    text.height,
                ),
            ]
        } else {
            vec![text]
        };
        let split = if two_columns { 3 } else { sections.len() };
        let groups: Vec<&[HelpSection<'_>]> = if two_columns {
            vec![&sections[..split], &sections[split..]]
        } else {
            vec![&sections[..]]
        };

        let mut tallest = 0usize;
        let laid_out: Vec<Vec<Line<'static>>> = groups
            .iter()
            .zip(&columns)
            .map(|(group, column)| {
                let lines = help_lines(group, column.width, theme);
                tallest = tallest.max(lines.len());
                lines
            })
            .collect();
        self.scroll_limit = tallest.saturating_sub(usize::from(body.height));
        self.scroll = self.scroll.min(self.scroll_limit);
        let scroll = self.scroll;
        for (lines, column) in laid_out.into_iter().zip(&columns) {
            frame.render_widget(
                Paragraph::new(
                    lines
                        .into_iter()
                        .skip(scroll)
                        .take(usize::from(column.height))
                        .collect::<Vec<_>>(),
                ),
                *column,
            );
        }
        if self.scroll_limit > 0 {
            render_scrollbar(
                frame,
                Rect::new(body.right().saturating_sub(1), body.y, 1, body.height),
                scroll,
                self.scroll_limit,
                theme,
                ascii,
            );
        }
        surface
    }
}

struct HelpSection<'a> {
    title: &'a str,
    entries: Vec<(&'a str, String)>,
}

fn help_sections(agent: &str) -> Vec<HelpSection<'_>> {
    vec![
        HelpSection {
            title: "EVERYWHERE",
            entries: vec![
                ("Ctrl-P", "Open the command palette".into()),
                ("?", "Open or close this help".into()),
                ("Ctrl-L", "Redraw the terminal".into()),
                (",", "Open settings".into()),
                ("q / Ctrl-C", "Quit".into()),
            ],
        },
        HelpSection {
            title: "LOGS & VIEWS",
            entries: vec![
                ("g / G", "Jump to first / last record".into()),
                ("←/→ · 0", "Pan the selected event / reset pan".into()),
                ("[ / ]", "Previous or next view".into()),
                ("f", "Toggle follow / history".into()),
                ("d", "Toggle selected-record details".into()),
                ("o", "Open raw context".into()),
                ("b", "Toggle a bookmark".into()),
                ("B", "Open bookmarks and notes".into()),
                ("Alt-S", "Stop the selected source".into()),
                ("Alt-R", "Restart the selected source".into()),
            ],
        },
        HelpSection {
            title: "FILTER & SHAPE",
            entries: vec![
                ("/", "Literal or field-aware search".into()),
                ("p", "Open the advanced filter".into()),
                ("e", "Open the ordered enrichment steps".into()),
                (
                    "Alt-C in Enrichment",
                    "Add, edit, remove, or explicitly run the terminal command step".into(),
                ),
                ("m", "Open display-only grouping".into()),
                (
                    "i",
                    "Inspect fields; Space pins, c colors, r correlates open sources".into(),
                ),
                (
                    "Ctrl-P Fold",
                    "Collapse repeated events; Enter expands one run".into(),
                ),
                ("t", "Choose capture or event time window".into()),
                ("S", "Review derived storage usage".into()),
            ],
        },
        HelpSection {
            title: "SOURCES",
            entries: vec![
                ("n", "Add a source".into()),
                (
                    "Alt-F / Alt-C",
                    "Choose file / command in Add source".into(),
                ),
                (
                    "Ctrl-D",
                    "Discover sources; selection never auto-starts".into(),
                ),
                (
                    "Ctrl-A",
                    format!("Ask {agent} to draft a source for review"),
                ),
                ("v", "Open view actions".into()),
                ("Alt-M", "Edit source membership in View actions".into()),
            ],
        },
        HelpSection {
            title: "VIEWS & RECIPES",
            entries: vec![
                ("Alt-B", "Create a blank view".into()),
                ("Alt-D", "Clone the current view".into()),
                ("Alt-R", "Rename the current view".into()),
                ("r", "Browse named recipes".into()),
            ],
        },
        HelpSection {
            title: "ASSISTANCE",
            entries: vec![
                (
                    "A",
                    format!("Ask {agent} for a filter or enrichment proposal"),
                ),
                ("I", format!("Open a local {agent} investigation")),
                ("Alt-N", "Start a new investigation snapshot".into()),
            ],
        },
    ]
}

/// §4.4: an entry is a key column and a description column, and a description
/// too long for its column continues *under the description*, not back at the
/// left edge where it would read as another key.
fn help_lines(sections: &[HelpSection<'_>], width: u16, theme: Theme) -> Vec<Line<'static>> {
    let styles = DialogStyles::new(theme);
    let mut lines = Vec::new();
    let key_width = sections
        .iter()
        .flat_map(|section| section.entries.iter())
        .map(|(key, _)| UnicodeWidthStr::width(*key))
        .max()
        .unwrap_or(0);
    let indent = 2 + key_width + 2;
    let description_width = usize::from(width).saturating_sub(indent).max(1);
    for (section_index, section) in sections.iter().enumerate() {
        if section_index > 0 {
            lines.push(Line::default());
        }
        lines.push(Line::from(Span::styled(
            section.title.to_owned(),
            styles.label.add_modifier(Modifier::BOLD),
        )));
        for (key, description) in &section.entries {
            let padding = " ".repeat(key_width.saturating_sub(UnicodeWidthStr::width(*key)) + 2);
            let wrapped = wrap_sentence(description, description_width, usize::MAX);
            let mut wrapped = wrapped.into_iter();
            lines.push(Line::from(vec![
                Span::styled(format!("  {key}"), styles.shortcut),
                Span::styled(padding, styles.description),
                Span::styled(wrapped.next().unwrap_or_default(), styles.description),
            ]));
            for continuation in wrapped {
                lines.push(Line::from(vec![
                    Span::styled(" ".repeat(indent), styles.description),
                    Span::styled(continuation, styles.description),
                ]));
            }
        }
    }
    lines
}
