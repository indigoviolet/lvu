//! Pure, bounded pixel-art presentation for the startup title and footer cue.
//! The host owns eligibility, input routing, and monotonic time injection.

use crate::theme::Theme;
use ratatui::{
    Frame,
    layout::{Alignment, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Clear, Paragraph},
};
use std::time::Duration;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Legacy settings/API compatibility. Startup visibility no longer expires.
pub const MAX_STARTUP_DURATION: Duration = Duration::from_millis(600);
pub const ANIMATION_TICK: Duration = Duration::from_millis(125);
pub const STARTUP_TITLE: &str = "LOVE YOU LOG TIME";
pub const FOOTER_MAX_WIDTH: u16 = 18;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DelightConfig {
    pub enabled: bool,
    pub reduced_motion: bool,
    pub ascii: bool,
    legacy_startup_duration: Duration,
}

impl DelightConfig {
    /// `startup_duration` is retained for settings compatibility but ignored by
    /// the explicit-Escape title screen.
    pub fn new(
        enabled: bool,
        reduced_motion: bool,
        ascii: bool,
        startup_duration: Duration,
    ) -> Self {
        Self {
            enabled,
            reduced_motion,
            ascii,
            legacy_startup_duration: startup_duration.min(MAX_STARTUP_DURATION),
        }
    }

    pub fn startup_duration(self) -> Duration {
        self.legacy_startup_duration
    }
}

impl Default for DelightConfig {
    fn default() -> Self {
        Self::new(true, false, false, MAX_STARTUP_DURATION)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InputDisposition {
    KeepTitleModal,
    DismissedAndConsumed,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct StartupDelight {
    dismissed: bool,
}

impl StartupDelight {
    pub fn new() -> Self {
        Self::default()
    }

    /// Generic input remains captured by the title and does not dismiss it.
    pub fn observe_input(&mut self) -> InputDisposition {
        InputDisposition::KeepTitleModal
    }

    /// Hosts call this only for Escape. Escape dismisses and is consumed.
    pub fn observe_escape(&mut self) -> InputDisposition {
        self.dismissed = true;
        InputDisposition::DismissedAndConsumed
    }

    /// Bypasses startup for ineligible launch modes without disabling footer delight.
    pub fn dismiss(&mut self) {
        self.dismissed = true;
    }

    pub fn is_visible(&self, _elapsed: Duration, config: DelightConfig) -> bool {
        config.enabled && !self.dismissed
    }

    pub fn redraw_interval(config: DelightConfig) -> Option<Duration> {
        (config.enabled && !config.reduced_motion).then_some(ANIMATION_TICK)
    }

    pub fn render(
        &self,
        frame: &mut Frame<'_>,
        area: Rect,
        elapsed: Duration,
        config: DelightConfig,
    ) {
        self.render_with_theme(frame, area, elapsed, config, Theme::TERMINAL);
    }

    pub fn render_with_theme(
        &self,
        frame: &mut Frame<'_>,
        area: Rect,
        elapsed: Duration,
        config: DelightConfig,
        theme: Theme,
    ) {
        if !self.is_visible(elapsed, config) || area.width == 0 || area.height == 0 {
            return;
        }
        frame.render_widget(Clear, area);
        frame.render_widget(
            Block::default().style(Style::default().fg(theme.base_fg).bg(theme.base_bg)),
            area,
        );
        let lines = title_screen(area, elapsed, config, theme);
        let height = lines.len().min(area.height as usize) as u16;
        let top = area.y + area.height.saturating_sub(height) / 2;
        frame.render_widget(
            Paragraph::new(lines).alignment(Alignment::Center),
            Rect::new(area.x, top, area.width, height),
        );
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActivityState<'a> {
    Idle,
    Active { label: &'a str },
    Pending { label: &'a str, progress: Progress },
    Error { label: &'a str },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Progress {
    Unknown,
    Measured { completed: u64, total: u64 },
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FooterDelight;

impl FooterDelight {
    pub fn render(
        frame: &mut Frame<'_>,
        area: Rect,
        elapsed: Duration,
        config: DelightConfig,
        activity: ActivityState<'_>,
    ) {
        Self::render_with_theme(frame, area, elapsed, config, activity, Theme::TERMINAL);
    }

    pub fn render_with_theme(
        frame: &mut Frame<'_>,
        area: Rect,
        elapsed: Duration,
        config: DelightConfig,
        activity: ActivityState<'_>,
        theme: Theme,
    ) {
        if !config.enabled || area.width == 0 || area.height == 0 {
            return;
        }
        let area = Rect::new(area.x, area.y, area.width.min(FOOTER_MAX_WIDTH), 1);
        let pulse = is_animated(activity, config) && pulse_phase(elapsed);
        let mut spans = footer_badge(config.ascii, pulse, activity, theme);
        let used = spans
            .iter()
            .map(|span| UnicodeWidthStr::width(span.content.as_ref()))
            .sum::<usize>();
        let label = truncate_width(
            &activity_label(activity),
            usize::from(area.width).saturating_sub(used + 1),
        );
        if !label.is_empty() {
            spans.push(Span::raw(" "));
            spans.push(Span::styled(label, Style::default().fg(theme.base_fg)));
        }
        frame.render_widget(
            Paragraph::new(Line::from(spans))
                .style(Style::default().fg(theme.base_fg).bg(theme.base_bg)),
            area,
        );
    }
}

fn title_screen(
    area: Rect,
    elapsed: Duration,
    config: DelightConfig,
    theme: Theme,
) -> Vec<Line<'static>> {
    if area.width < 60 || area.height < 22 {
        return compact_title(area, config, theme);
    }
    let large = area.width >= 108 && area.height >= 28;
    let pulse = !config.reduced_motion && pulse_phase(elapsed);
    let mut lines = pixel_heart(theme, config.ascii, pulse, large);
    lines.push(Line::from(""));
    lines.extend(bitmap_title(
        theme,
        if large { 2 } else { 1 },
        if large { 2 } else { 1 },
    ));
    lines.push(Line::from(""));
    lines.push(Line::styled(
        STARTUP_TITLE,
        Style::default()
            .fg(theme.heart.primary)
            .add_modifier(Modifier::BOLD),
    ));
    lines.push(Line::styled(
        "ESC TO ENTER",
        Style::default()
            .fg(if pulse { theme.accent } else { theme.muted })
            .add_modifier(Modifier::BOLD),
    ));
    lines
}

fn compact_title(area: Rect, config: DelightConfig, theme: Theme) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    if area.height >= 3 {
        lines.push(Line::styled(
            if config.ascii { "<##>" } else { "◆♥◆" },
            Style::default()
                .fg(theme.heart.primary)
                .add_modifier(Modifier::BOLD),
        ));
    }
    lines.push(Line::styled(
        truncate_width(STARTUP_TITLE, area.width as usize),
        Style::default()
            .fg(theme.heart.primary)
            .add_modifier(Modifier::BOLD),
    ));
    if area.height >= 2 {
        lines.push(Line::styled(
            truncate_width("ESC TO ENTER", area.width as usize),
            Style::default().fg(theme.muted),
        ));
    }
    lines
}

fn bitmap_title(_theme: Theme, scale_x: usize, scale_y: usize) -> Vec<Line<'static>> {
    const WORDS: [&str; 4] = ["LOVE", "YOU", "LOG", "TIME"];
    (0..5)
        .flat_map(|row| {
            let mut spans = Vec::new();
            for (word_index, word) in WORDS.iter().enumerate() {
                if word_index > 0 {
                    spans.push(Span::raw(" ".repeat(scale_x * 2)));
                }
                for (index, letter) in word.chars().enumerate() {
                    if index > 0 {
                        spans.push(Span::raw(" ".repeat(scale_x)));
                    }
                    for pixel in glyph(letter)[row].chars() {
                        let text = if pixel == '1' {
                            "█".repeat(scale_x)
                        } else {
                            " ".repeat(scale_x)
                        };
                        spans.push(Span::styled(
                            text,
                            Style::default()
                                .fg([
                                    Color::Rgb(255, 245, 168),
                                    Color::Rgb(255, 213, 55),
                                    Color::Rgb(255, 184, 0),
                                    Color::Rgb(244, 112, 14),
                                    Color::Rgb(176, 49, 8),
                                ][row])
                                .add_modifier(Modifier::BOLD),
                        ));
                    }
                }
            }
            std::iter::repeat_n(Line::from(spans), scale_y)
        })
        .collect()
}

fn glyph(letter: char) -> [&'static str; 5] {
    match letter {
        'L' => ["100", "100", "100", "100", "111"],
        'O' => ["111", "101", "101", "101", "111"],
        'V' => ["101", "101", "101", "101", "010"],
        'E' => ["111", "100", "110", "100", "111"],
        'Y' => ["101", "101", "010", "010", "010"],
        'U' => ["101", "101", "101", "101", "111"],
        'G' => ["111", "100", "101", "101", "111"],
        'T' => ["111", "010", "010", "010", "010"],
        'I' => ["111", "010", "010", "010", "111"],
        'M' => ["101", "111", "111", "101", "101"],
        _ => ["000"; 5],
    }
}

// Two vertical pixels per terminal cell keep the silhouette round rather than
// stretching it vertically. Geometry and highlights are fixed, bounded artwork.
fn pixel_heart(theme: Theme, ascii: bool, pulse: bool, large: bool) -> Vec<Line<'static>> {
    let width = if large { 52 } else { 40 };
    let height = if large { 28 } else { 24 };
    let pixel = |column: usize, row: usize| -> Option<Color> {
        let scale = if pulse { 1.0 } else { 0.95 };
        let x = (column as f64 + 0.5 - width as f64 / 2.0) / (width as f64 * 0.40 * scale);
        let y = (height as f64 * 0.52 - row as f64 - 0.5) / (height as f64 * 0.36 * scale);
        let q = x * x + y * y - 1.0;
        if q * q * q - x * x * y * y * y > 0.0 {
            return None;
        }
        let gloss_left = ((x + 0.55) / 0.24).powi(2) + ((y - 0.62 - x * 0.32) / 0.23).powi(2);
        let gloss_right = ((x - 0.48) / 0.16).powi(2) + ((y - 0.67) / 0.18).powi(2);
        Some(if gloss_left < 0.5 || gloss_right < 0.5 {
            Color::White
        } else if gloss_left < 1.5 || gloss_right < 1.6 {
            Color::Rgb(255, 167, 174)
        } else if y < -0.40 || x > 0.78 {
            Color::Rgb(140, 5, 22)
        } else if y < -0.14 || x > 0.61 {
            Color::Rgb(198, 8, 28)
        } else if x < -0.7 || y > 0.85 {
            Color::Rgb(255, 76, 89)
        } else {
            Color::Rgb(246, 24, 47)
        })
    };
    (0..height)
        .step_by(2)
        .map(|y| {
            Line::from(
                (0..width)
                    .map(|x| {
                        let top = pixel(x, y);
                        let bottom = pixel(x, y + 1);
                        if ascii {
                            let color = top.or(bottom).unwrap_or(theme.base_bg);
                            let symbol =
                                if top == Some(Color::White) || bottom == Some(Color::White) {
                                    "*"
                                } else if top.is_some() || bottom.is_some() {
                                    "@"
                                } else {
                                    " "
                                };
                            Span::styled(symbol, Style::default().fg(color))
                        } else {
                            match (top, bottom) {
                                (None, None) => Span::raw(" "),
                                (None, Some(color)) => {
                                    Span::styled("▄", Style::default().fg(color).bg(theme.base_bg))
                                }
                                (Some(color), bottom) => Span::styled(
                                    "▀",
                                    Style::default()
                                        .fg(color)
                                        .bg(bottom.unwrap_or(theme.base_bg)),
                                ),
                            }
                        }
                    })
                    .collect::<Vec<_>>(),
            )
        })
        .collect()
}

fn footer_badge(
    ascii: bool,
    pulse: bool,
    activity: ActivityState<'_>,
    theme: Theme,
) -> Vec<Span<'static>> {
    let error = matches!(activity, ActivityState::Error { .. });
    let primary = if error {
        theme.heart.error
    } else {
        theme.heart.primary
    };
    // Keep the animation inside one heart cell: adjoining line glyphs can look
    // like wedges in terminal fonts. A filled/outline double beat keeps labels fixed.
    let animated = matches!(
        activity,
        ActivityState::Active { .. } | ActivityState::Pending { .. }
    );
    let heart = if ascii {
        "<3"
    } else if pulse || !animated {
        "♥"
    } else {
        "♡"
    };
    vec![Span::styled(
        heart,
        Style::default().fg(primary).add_modifier(Modifier::BOLD),
    )]
}

fn is_animated(activity: ActivityState<'_>, config: DelightConfig) -> bool {
    !config.reduced_motion
        && matches!(
            activity,
            ActivityState::Active { .. } | ActivityState::Pending { .. }
        )
}

fn pulse_phase(elapsed: Duration) -> bool {
    matches!(
        (elapsed.as_millis() / ANIMATION_TICK.as_millis()) % 8,
        0 | 2
    )
}

fn activity_label(activity: ActivityState<'_>) -> String {
    match activity {
        ActivityState::Idle => "idle".to_owned(),
        ActivityState::Active { label } => bounded_label(label),
        ActivityState::Pending {
            label,
            progress: Progress::Unknown,
        } => format!("{} pending", bounded_label(label)),
        ActivityState::Pending {
            label,
            progress: Progress::Measured { completed, total },
        } if total > 0 => {
            let completed = completed.min(total);
            let percent = completed.saturating_mul(100) / total;
            format!("{} {completed}/{total} {percent}%", bounded_label(label))
        }
        ActivityState::Pending { label, .. } => format!("{} pending", bounded_label(label)),
        ActivityState::Error { label } => format!("error {}", bounded_label(label)),
    }
}

fn bounded_label(label: &str) -> String {
    truncate_width(label.trim(), 96)
}

fn truncate_width(value: &str, max_width: usize) -> String {
    if max_width == 0 {
        return String::new();
    }
    if UnicodeWidthStr::width(value) <= max_width {
        return value.to_owned();
    }
    let content_width = max_width.saturating_sub(1);
    let mut width = 0;
    let mut output = String::new();
    for character in value.chars() {
        let next = UnicodeWidthChar::width(character).unwrap_or(0);
        if width + next > content_width {
            break;
        }
        output.push(character);
        width += next;
    }
    output.push('…');
    output
}
