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
    if area.width < 28 || area.height < 10 {
        return compact_title(area, config, theme);
    }
    let large = area.width >= 108 && area.height >= 28;
    let pulse = !config.reduced_motion && pulse_phase(elapsed);
    let mut lines = bitmap_title(theme, if large { 2 } else { 1 }, if large { 2 } else { 1 });
    lines.push(Line::from(""));
    lines.extend(pixel_heart(theme, config.ascii, pulse, large));
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

fn bitmap_title(theme: Theme, scale_x: usize, scale_y: usize) -> Vec<Line<'static>> {
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
                                .fg(theme.heart.primary)
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

fn pixel_heart(theme: Theme, ascii: bool, pulse: bool, large: bool) -> Vec<Line<'static>> {
    const HEART: [&str; 9] = [
        "  11211 11211  ",
        " 1233322333321 ",
        "123333333333321",
        "133344333333331",
        "133455433333331",
        " 1334433333331 ",
        "  13333333331  ",
        "    1333331    ",
        "      131      ",
    ];
    let unit = if large { 2 } else { 1 };
    HEART
        .into_iter()
        .map(|row| {
            Line::from(
                row.chars()
                    .map(|shade| {
                        let (symbol, color) = match shade {
                            '1' => (if ascii { "#" } else { "▓" }, theme.heart.deep),
                            '2' => (if ascii { "#" } else { "█" }, theme.heart.primary),
                            '3' => (
                                if ascii {
                                    "@"
                                } else if pulse {
                                    "█"
                                } else {
                                    "▓"
                                },
                                if pulse {
                                    theme.heart.primary
                                } else {
                                    theme.heart.soft
                                },
                            ),
                            '4' => (if ascii { "+" } else { "▒" }, theme.heart.soft),
                            '5' => (if ascii { "*" } else { "█" }, Color::White),
                            _ => (" ", theme.base_fg),
                        };
                        Span::styled(
                            symbol.repeat(unit),
                            Style::default().fg(color).add_modifier(Modifier::BOLD),
                        )
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
    let deep = if error {
        theme.heart.error
    } else {
        theme.heart.deep
    };
    if ascii {
        return vec![
            Span::styled("<", Style::default().fg(theme.heart.soft)),
            Span::styled(if pulse { "#" } else { "3" }, Style::default().fg(primary)),
            Span::styled(if pulse { "^" } else { ">" }, Style::default().fg(deep)),
        ];
    }
    vec![
        Span::styled("♥", Style::default().fg(primary)),
        Span::styled("▄", Style::default().fg(theme.heart.soft)),
        Span::styled(if pulse { "█" } else { "▓" }, Style::default().fg(deep)),
        Span::styled(
            if pulse { "⌁" } else { "·" },
            Style::default().fg(theme.accent),
        ),
    ]
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
