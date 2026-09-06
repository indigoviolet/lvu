//! Pure, bounded pixel-art presentation for the startup title and footer cue.
//! The host owns eligibility, input routing, and monotonic time injection.

mod art;

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
pub const STARTUP_ANIMATION_TICK: Duration = Duration::from_millis(art::FRAME_MILLIS as u64);
pub const INDICATOR_ANIMATION_TICK: Duration = Duration::from_millis(50);
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
    /// the explicit-input title screen.
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

    /// A key dismisses the title and is consumed by the host.
    pub fn observe_input(&mut self) -> InputDisposition {
        self.dismissed = true;
        InputDisposition::DismissedAndConsumed
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
        (config.enabled && !config.reduced_motion && !config.ascii)
            .then_some(STARTUP_ANIMATION_TICK)
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
        if !config.ascii && area.width >= 80 && area.height >= 22 {
            render_ansi_title(frame, area, elapsed, config);
            return;
        }
        frame.render_widget(Clear, area);
        frame.render_widget(
            Block::default().style(Style::default().fg(theme.base_fg).bg(theme.base_bg)),
            area,
        );
        let lines = compact_title(area, config, theme);
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
        if !config.ascii && area.width >= 14 && area.height >= 8 {
            render_corner_heart(frame, area, elapsed, config, activity, theme);
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

fn render_corner_heart(
    frame: &mut Frame<'_>,
    area: Rect,
    elapsed: Duration,
    config: DelightConfig,
    activity: ActivityState<'_>,
    theme: Theme,
) {
    frame.render_widget(
        Block::default().style(Style::default().bg(theme.base_bg)),
        area,
    );
    let phase = if !is_animated(activity, config) {
        0
    } else {
        match elapsed.as_millis() % 1000 {
            0..650 => 0,
            650..750 => 1,
            750..850 => 2,
            _ => 3,
        }
    };
    let art = art::indicator(phase);
    let left = area.x + (area.width - art.area.width) / 2;
    let top = area.bottom() - 8;
    let transparent = |color| matches!(color, Color::Rgb(r, g, b) if r.max(g).max(b) < 48);
    for y in 0..art.area.height {
        for x in 0..art.area.width {
            let mut cell = art[(x, y)].clone();
            let (upper, lower) = match cell.symbol() {
                "▀" => (cell.fg, cell.bg),
                "▄" => (cell.bg, cell.fg),
                "█" => (cell.fg, cell.fg),
                _ => (cell.bg, cell.bg),
            };
            // Default foreground and default background are different colors.
            // Put transparent pixels in the background channel, never fg Reset.
            match (transparent(upper), transparent(lower)) {
                (true, true) => {
                    cell.set_symbol(" ");
                    cell.fg = theme.base_fg;
                    cell.bg = theme.base_bg;
                }
                (true, false) => {
                    cell.set_symbol("▄");
                    cell.fg = lower;
                    cell.bg = theme.base_bg;
                }
                (false, true) => {
                    cell.set_symbol("▀");
                    cell.fg = upper;
                    cell.bg = theme.base_bg;
                }
                (false, false) => {
                    cell.set_symbol("▀");
                    cell.fg = upper;
                    cell.bg = lower;
                }
            }
            frame.buffer_mut()[(left + x, top + y)] = cell;
        }
    }
    let label = activity_label(activity);
    if !label.is_empty() {
        frame.render_widget(
            Paragraph::new(truncate_width(&label, usize::from(area.width)))
                .style(Style::default().fg(theme.heart.error).bg(theme.base_bg)),
            Rect::new(area.x, area.bottom() - 1, area.width, 1),
        );
    }
}

fn render_ansi_title(frame: &mut Frame<'_>, area: Rect, elapsed: Duration, config: DelightConfig) {
    let black = Color::Rgb(0, 0, 0);
    frame.render_widget(Clear, area);
    frame.render_widget(Block::default().style(Style::default().bg(black)), area);
    let index = if config.reduced_motion {
        0
    } else {
        ((elapsed.as_millis() / art::FRAME_MILLIS) % 10) as usize
    };
    let art = art::frame(area.width >= 120 && area.height >= 40, index);
    let left = area.x + (area.width - art.area.width) / 2;
    let top = area.y + (area.height - art.area.height) / 2;
    for y in 0..art.area.height {
        for x in 0..art.area.width {
            frame.buffer_mut()[(left + x, top + y)] = art[(x, y)].clone();
        }
    }
    // The artwork's last row is blank. Keep a readable/accessibility label and
    // Escape hint even when the artwork occupies the entire available height.
    frame.render_widget(
        Paragraph::new("LOVE YOU LOG TIME · PRESS ANY KEY")
            .alignment(Alignment::Center)
            .style(Style::default().fg(Color::Rgb(220, 185, 115)).bg(black)),
        Rect::new(area.x, area.bottom() - 1, area.width, 1),
    );
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
            truncate_width("PRESS ANY KEY", area.width as usize),
            Style::default().fg(theme.muted),
        ));
    }
    lines
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
        ActivityState::Idle | ActivityState::Active { .. } => String::new(),
        ActivityState::Pending {
            progress: Progress::Unknown,
            ..
        } => String::new(),
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
