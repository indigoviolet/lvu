//! Shared, bounded presentation primitives for dialog controls.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
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

/// §8.10: a button label may mark one letter with `&` — `"&Add"`,
/// `"External &command…"` — meaning that letter presses the button
/// (`mnemonic_press`). The marker is never drawn; the letter is underlined
/// instead, the way a GUI shows a mnemonic. `&&` is a literal ampersand.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Mnemonic {
    /// The label with the marker removed: what is drawn and measured.
    pub text: String,
    /// The mnemonic letter, lower-cased, and its char index in `text`.
    pub key: Option<(char, usize)>,
}

pub fn mnemonic(label: &str) -> Mnemonic {
    let mut text = String::with_capacity(label.len());
    let mut key = None;
    let mut chars = label.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '&' {
            match chars.peek() {
                Some('&') => {
                    chars.next();
                    text.push('&');
                }
                Some(next) if key.is_none() => {
                    key = Some((next.to_ascii_lowercase(), text.chars().count()));
                }
                _ => {}
            }
            continue;
        }
        text.push(ch);
    }
    Mnemonic { text, key }
}

/// The accelerator letter a label declares, lower-cased.
pub fn mnemonic_key(label: &str) -> Option<char> {
    mnemonic(label).key.map(|(key, _)| key)
}

/// §8.10: the button of an action row that a key press activates, or `None`.
///
/// The **bare underlined letter** is the accelerator, and it is live whenever
/// no text field has focus — with no field to type into the letter is not
/// text, so it is a key. While a text field has focus the letter *is* text and
/// only Alt+letter presses the button. The match is case-insensitive: the
/// label's letter is compared lower-cased, so `x` and `X` both press `E&xclude`.
///
/// Alt is the fallback, not the mechanism, because a terminal need not deliver
/// it. Measured on xterm 400 under Xvfb (`docs/dialog-system.md` §8.10): with
/// its default `metaSendsEscape: false` Alt-f arrives as the 8-bit meta
/// character U+00E6 — the letter `æ` — and is not a chord at all; only with
/// `metaSendsEscape: true` does it arrive as `ESC f`, which is what crossterm
/// reports as `KeyModifiers::ALT`. The 8-bit form is deliberately *not*
/// translated back: U+00E6/U+00F8/U+00E4 are ordinary letters someone may need
/// to type, and the only place the translation would help is a focused text
/// field, which is exactly where those letters must stay text.
///
/// A chord that is neither of those is not a mnemonic (§8.10): Ctrl-C stays
/// Ctrl-C, which is why this reuses `is_typed_char`'s test for "no modifier
/// but Shift".
#[must_use]
pub fn mnemonic_press<S: AsRef<str>>(
    labels: &[S],
    key: &KeyEvent,
    text_focus: bool,
) -> Option<usize> {
    if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
        return None;
    }
    let KeyCode::Char(pressed) = key.code else {
        return None;
    };
    // Alt+letter presses the button from anywhere, including a text field;
    // the bare letter only where the letter cannot be text.
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    if !alt && (text_focus || !crate::component::is_typed_char(key)) {
        return None;
    }
    let pressed = pressed.to_ascii_lowercase();
    labels
        .iter()
        .position(|label| mnemonic_key(label.as_ref()) == Some(pressed))
}

pub fn button_text(label: &str) -> String {
    format!("[ {} ]", mnemonic(label).text)
}

pub fn button_width(label: &str) -> u16 {
    u16::try_from(UnicodeWidthStr::width(button_text(label).as_str())).unwrap_or(u16::MAX)
}

/// `[ Label ]` as spans, with the mnemonic letter underlined (§8.10). The
/// underline is a modifier on top of `style`, so every role and the focus
/// ring keep their colours.
pub fn button_line(label: &str, style: Style) -> Line<'static> {
    let Mnemonic { text, key } = mnemonic(label);
    let Some((_, index)) = key else {
        return Line::from(Span::styled(format!("[ {text} ]"), style));
    };
    let before: String = text.chars().take(index).collect();
    let letter: String = text.chars().skip(index).take(1).collect();
    let after: String = text.chars().skip(index + 1).collect();
    Line::from(vec![
        Span::styled(format!("[ {before}"), style),
        Span::styled(letter, style.add_modifier(Modifier::UNDERLINED)),
        Span::styled(format!("{after} ]"), style),
    ])
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
        // §8.9 at sixteen colours: the fill is one of the sixteen, so its text
        // is chosen against it rather than pushed toward it. Black on cyan
        // clears the floor by a wide margin on xterm's palette; the selection
        // foreground, which the RGB-less branch below reaches for, would be
        // white on cyan and would not.
        _ if theme.depth == crate::theme::ColorDepth::Ansi16 => Color::Black,
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
    let style = role_style(theme, role, focused);
    frame.render_widget(Paragraph::new(button_line(label, style)).style(style), rect);
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
    let style = button_style(theme, focused, selected);
    frame.render_widget(Paragraph::new(button_line(label, style)).style(style), rect);
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
