//! Small, bounded presentation accents for startup and background activity.
//!
//! The host supplies monotonic elapsed time. Nothing here sleeps, reads the
//! terminal or environment, schedules redraws, or consumes application input.

use ratatui::{
    Frame,
    layout::{Alignment, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Clear, Paragraph},
};
use std::time::Duration;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::theme::Theme;

pub const MAX_STARTUP_DURATION: Duration = Duration::from_millis(600);
pub const ANIMATION_TICK: Duration = Duration::from_millis(125);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DelightConfig {
    pub enabled: bool,
    pub reduced_motion: bool,
    pub ascii: bool,
    startup_duration: Duration,
}

impl DelightConfig {
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
            startup_duration: startup_duration.min(MAX_STARTUP_DURATION),
        }
    }

    pub fn startup_duration(self) -> Duration {
        self.startup_duration
    }
}

impl Default for DelightConfig {
    fn default() -> Self {
        Self::new(true, false, false, MAX_STARTUP_DURATION)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InputDisposition {
    ContinueToApplication,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct StartupDelight {
    dismissed: bool,
}

impl StartupDelight {
    pub fn new() -> Self {
        Self::default()
    }

    /// Dismisses the overlay but explicitly tells the host to continue handling
    /// this same input through its normal key path.
    pub fn observe_input(&mut self) -> InputDisposition {
        self.dismissed = true;
        InputDisposition::ContinueToApplication
    }

    pub fn dismiss(&mut self) {
        self.dismissed = true;
    }

    pub fn is_visible(&self, elapsed: Duration, config: DelightConfig) -> bool {
        config.enabled && !self.dismissed && elapsed < config.startup_duration()
    }

    /// The earliest useful redraw interval. Hosts may redraw less often and
    /// should never redraw faster merely for this animation.
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
            ratatui::widgets::Block::default()
                .style(Style::default().fg(theme.base_fg).bg(theme.base_bg)),
            area,
        );
        let lines = startup_lines(area, elapsed, config, theme);
        let height = lines.len().min(area.height as usize) as u16;
        let top = area.y + area.height.saturating_sub(height) / 2;
        frame.render_widget(
            Paragraph::new(lines)
                .alignment(Alignment::Center)
                .style(Style::default().fg(theme.heart.primary)),
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
        let (heart, heart_style) = heart_frame(elapsed, config, activity, theme);
        let status = activity_label(activity);
        let available = area.width as usize;
        let heart_width = UnicodeWidthStr::width(heart);
        let status_width = available.saturating_sub(heart_width + 1);
        let status = truncate_width(&status, status_width);
        let mut spans = vec![Span::styled(heart, heart_style)];
        if !status.is_empty() {
            spans.push(Span::raw(" "));
            spans.push(Span::raw(status));
        }
        frame.render_widget(
            Paragraph::new(Line::from(spans))
                .style(Style::default().fg(theme.base_fg).bg(theme.base_bg)),
            area,
        );
    }
}

fn startup_lines(
    area: Rect,
    elapsed: Duration,
    config: DelightConfig,
    theme: Theme,
) -> Vec<Line<'static>> {
    let compact = if config.ascii {
        "<3 lvu · love you"
    } else {
        "♥ lvu · love you"
    };
    if area.width < 24 || area.height < 7 {
        return vec![Line::styled(
            truncate_width(compact, area.width as usize),
            Style::default()
                .fg(theme.heart.primary)
                .add_modifier(Modifier::BOLD),
        )];
    }

    let pulse = pulse_phase(elapsed, config.reduced_motion);
    let heart = if config.ascii {
        [
            "  **   **  ",
            " ********* ",
            "  *******  ",
            "   *****   ",
            "    ***    ",
        ]
    } else if pulse {
        [
            "  ▄██▄ ▄██▄  ",
            " ███████████ ",
            "  █████████  ",
            "   ▀█████▀   ",
            "     ▀█▀     ",
        ]
    } else {
        [
            "   ▄█▄ ▄█▄   ",
            "  █████████  ",
            "   ███████   ",
            "    ▀███▀    ",
            "      ▀      ",
        ]
    };
    let heart_style = Style::default()
        .fg(if pulse {
            theme.heart.primary
        } else {
            theme.heart.soft
        })
        .add_modifier(Modifier::BOLD);
    let mut lines: Vec<Line<'static>> = heart
        .into_iter()
        .map(|line| Line::styled(line.to_owned(), heart_style))
        .collect();
    lines.push(Line::from(vec![
        Span::styled(
            "lvu",
            Style::default()
                .fg(theme.heart.primary)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("  ·  love you", Style::default().fg(theme.heart.soft)),
    ]));
    lines
}

fn heart_frame(
    elapsed: Duration,
    config: DelightConfig,
    activity: ActivityState<'_>,
    theme: Theme,
) -> (&'static str, Style) {
    let animated = matches!(
        activity,
        ActivityState::Active { .. } | ActivityState::Pending { .. }
    ) && !config.reduced_motion;
    let pulse = animated && pulse_phase(elapsed, false);
    let heart = if config.ascii {
        if pulse { "<3!" } else { "<3" }
    } else if pulse {
        "♥"
    } else {
        "♡"
    };
    let color = match activity {
        ActivityState::Error { .. } => theme.heart.error,
        ActivityState::Idle => theme.heart.soft,
        ActivityState::Active { .. } | ActivityState::Pending { .. } => {
            if pulse {
                theme.heart.primary
            } else {
                theme.heart.deep
            }
        }
    };
    (
        heart,
        Style::default().fg(color).add_modifier(Modifier::BOLD),
    )
}

fn pulse_phase(elapsed: Duration, reduced_motion: bool) -> bool {
    if reduced_motion {
        return true;
    }
    // Quantized to eight frames/second. Two short beats occur per one-second cycle.
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
        } => {
            format!("{} · pending", bounded_label(label))
        }
        ActivityState::Pending {
            label,
            progress: Progress::Measured { completed, total },
        } if total > 0 => {
            let completed = completed.min(total);
            let percent = completed.saturating_mul(100) / total;
            format!(
                "{} · {completed}/{total} ({percent}%)",
                bounded_label(label)
            )
        }
        ActivityState::Pending { label, .. } => {
            format!("{} · pending", bounded_label(label))
        }
        ActivityState::Error { label } => format!("error · {}", bounded_label(label)),
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
    let ellipsis = "…";
    let content_width = max_width.saturating_sub(UnicodeWidthStr::width(ellipsis));
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
    if max_width >= UnicodeWidthStr::width(ellipsis) {
        output.push_str(ellipsis);
    }
    output
}
