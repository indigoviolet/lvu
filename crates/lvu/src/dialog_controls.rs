//! Shared, bounded presentation primitives for dialog controls.

use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};
use unicode_width::UnicodeWidthStr;

use crate::theme::{Theme, ensure_contrast};

const TEXT_CONTRAST: f64 = 4.5;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DialogStyles {
    pub label: Style,
    pub description: Style,
    pub shortcut: Style,
    pub input: Style,
    pub selection: Style,
    pub applied: Style,
    pub pending: Style,
    pub error: Style,
    pub unavailable: Style,
}

impl DialogStyles {
    pub fn new(theme: Theme) -> Self {
        let readable = |color, background| ensure_contrast(color, background, TEXT_CONTRAST);
        Self {
            label: Style::default().fg(readable(theme.base_fg, theme.dialog_bg)),
            description: Style::default().fg(readable(theme.base_fg, theme.dialog_bg)),
            shortcut: Style::default()
                .fg(readable(theme.accent, theme.dialog_bg))
                .add_modifier(Modifier::BOLD),
            input: Style::default()
                .fg(readable(theme.input_fg, theme.input_bg))
                .bg(theme.input_bg),
            selection: Style::default()
                .fg(readable(theme.selection_fg, theme.selection_bg))
                .bg(theme.selection_bg),
            applied: Style::default().fg(readable(theme.severity.info, theme.dialog_bg)),
            pending: Style::default().fg(readable(theme.severity.warn, theme.dialog_bg)),
            error: Style::default().fg(readable(theme.severity.error, theme.dialog_bg)),
            unavailable: Style::default()
                .fg(readable(theme.base_fg, theme.dialog_bg))
                .add_modifier(Modifier::ITALIC),
        }
    }
}

/// §4.1 `gutter` between buttons in a row. One constant for measuring
/// (`ui::packed_button_rows`) and for placing (`button_layout`,
/// `ui::render_actions`): when the two disagreed, a row measured at one
/// gutter and drawn at another lost its last button at 80 columns.
pub const BUTTON_GUTTER: u16 = 2;

pub fn button_text(label: &str) -> String {
    format!("[ {label} ]")
}

pub fn button_width(label: &str) -> u16 {
    u16::try_from(UnicodeWidthStr::width(button_text(label).as_str())).unwrap_or(u16::MAX)
}

/// §8.9: what a button in an action row *is*. One role per button; the row
/// has at most one `Default`, and a destructive button is never the default.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ButtonRole {
    #[default]
    Normal,
    /// The dialog's default action: filled with `accent`, and what Enter
    /// executes from anywhere that does not consume Enter itself.
    Default,
    /// Styled `severity.error`; last in the row.
    Destructive,
}

/// §8.9 / §6.3: the one place a button's look is decided. Every action row
/// in the product goes through this, so a change to the treatment of the
/// default button is one edit.
///
/// * `Default`, unfocused: bold, `accent` fill, foreground pushed to read on it.
/// * Any role, focused: the selection colours, bold — the focus ring is the
///   same on every control, and the fill returns when focus leaves.
/// * `Normal`: `base_fg`. `Destructive`: `severity.error`.
pub fn role_style(theme: Theme, role: ButtonRole, focused: bool) -> Style {
    let styles = DialogStyles::new(theme);
    match (role, focused) {
        (_, true) => styles.selection.add_modifier(Modifier::BOLD),
        (ButtonRole::Default, false) => Style::default()
            .fg(default_fill_foreground(theme))
            .bg(theme.accent)
            .add_modifier(Modifier::BOLD),
        (ButtonRole::Destructive, false) => styles.error.bg(theme.dialog_bg),
        (ButtonRole::Normal, false) => styles.label.bg(theme.dialog_bg),
    }
}

/// Text that reads on an `accent` fill. The dialog surface colour is the
/// natural choice (dark text on a light fill and vice versa) and is pushed
/// until it clears 4.5:1; the terminal theme has no RGB surface, so it uses
/// the selection foreground, which is chosen to read on a coloured fill.
fn default_fill_foreground(theme: Theme) -> Color {
    match (theme.dialog_bg, theme.accent) {
        (Color::Rgb(..), Color::Rgb(..)) => {
            ensure_contrast(theme.dialog_bg, theme.accent, TEXT_CONTRAST)
        }
        _ => theme.selection_fg,
    }
}

/// One button with a §8.9 role, painted exactly over `rect`.
pub fn render_role_button(
    frame: &mut Frame<'_>,
    rect: Rect,
    label: &str,
    role: ButtonRole,
    focused: bool,
    theme: Theme,
) {
    if rect.is_empty() {
        return;
    }
    frame.render_widget(
        Paragraph::new(button_text(label)).style(role_style(theme, role, focused)),
        rect,
    );
}

/// An action row as a dialog declares it (§8.9): the labels in drawn order,
/// which one is the default (if any), which are destructive, and which one
/// holds the focus ring. `role(index)` is the only place the three are
/// reconciled, so a destructive button can never come out as the default.
#[derive(Clone, Copy, Debug, Default)]
pub struct ActionRow<'a> {
    pub labels: &'a [&'a str],
    pub default: Option<usize>,
    pub destructive: &'a [usize],
    pub focused: Option<usize>,
}

impl ActionRow<'_> {
    pub fn role(&self, index: usize) -> ButtonRole {
        if self.destructive.contains(&index) {
            ButtonRole::Destructive
        } else if self.default == Some(index) {
            ButtonRole::Default
        } else {
            ButtonRole::Normal
        }
    }
}

pub fn button_style(theme: Theme, focused: bool, selected: bool) -> Style {
    let styles = DialogStyles::new(theme);
    if focused {
        styles.selection.add_modifier(Modifier::BOLD)
    } else if selected {
        styles
            .applied
            .bg(theme.dialog_bg)
            .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
    } else {
        styles.label.bg(theme.dialog_bg)
    }
}

pub fn render_button(
    frame: &mut Frame<'_>,
    rect: Rect,
    label: &str,
    focused: bool,
    selected: bool,
    theme: Theme,
) {
    if rect.is_empty() {
        return;
    }
    frame.render_widget(
        Paragraph::new(button_text(label)).style(button_style(theme, focused, selected)),
        rect,
    );
}

pub fn button_layout(
    area: Rect,
    labels: &[&str],
    focused_index: Option<usize>,
) -> Vec<(usize, Rect)> {
    if area.is_empty() || labels.is_empty() {
        return Vec::new();
    }

    let place = |indices: &[usize], target: Rect| {
        let mut output = Vec::new();
        let mut x = target.x;
        let mut y = target.y;
        for &index in indices {
            let width = button_width(labels[index]).min(target.width);
            if x != target.x && x.saturating_add(width) > target.right() {
                x = target.x;
                y = y.saturating_add(1);
            }
            if y >= target.bottom() {
                break;
            }
            output.push((index, Rect::new(x, y, width, 1)));
            x = x.saturating_add(width).saturating_add(BUTTON_GUTTER);
        }
        output
    };

    let indices: Vec<_> = (0..labels.len()).collect();
    let normal = place(&indices, area);
    let Some(focused) = focused_index.filter(|index| *index < labels.len()) else {
        return normal;
    };
    if normal.iter().any(|(index, _)| *index == focused) {
        return normal;
    }

    // Keep the stable prefix in place and reserve only the final row for a
    // focused control that would otherwise be inaccessible.
    let mut visible = if area.height > 1 {
        place(
            &indices[..focused],
            Rect::new(area.x, area.y, area.width, area.height - 1),
        )
    } else {
        Vec::new()
    };
    visible.push((
        focused,
        Rect::new(
            area.x,
            area.bottom() - 1,
            button_width(labels[focused]).min(area.width),
            1,
        ),
    ));
    visible
}

pub fn action_line(actions: &[(&str, &str)], theme: Theme) -> Line<'static> {
    let styles = DialogStyles::new(theme);
    let mut spans = Vec::with_capacity(actions.len().saturating_mul(4));
    for (index, (shortcut, description)) in actions.iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled(" · ", styles.description));
        }
        spans.push(Span::styled((*shortcut).to_owned(), styles.shortcut));
        if !shortcut.is_empty() && !description.is_empty() {
            spans.push(Span::styled(" ", styles.description));
        }
        spans.push(Span::styled((*description).to_owned(), styles.description));
    }
    Line::from(spans)
}
