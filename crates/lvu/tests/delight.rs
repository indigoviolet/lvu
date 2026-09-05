use lvu::{
    delight::{
        ANIMATION_TICK, ActivityState, DelightConfig, FOOTER_MAX_WIDTH, FooterDelight,
        InputDisposition, MAX_STARTUP_DURATION, Progress, STARTUP_TITLE, StartupDelight,
    },
    theme::Theme,
};
use ratatui::{
    Terminal,
    backend::TestBackend,
    buffer::Buffer,
    style::{Color, Style},
    text::Line,
    widgets::Paragraph,
};
use std::time::Duration;

fn render_startup(width: u16, height: u16, at: Duration, config: DelightConfig) -> Buffer {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| {
            StartupDelight::new().render_with_theme(
                frame,
                frame.area(),
                at,
                config,
                Theme::LOVE_DARK,
            )
        })
        .unwrap();
    terminal.backend().buffer().clone()
}

fn text(buffer: &Buffer) -> String {
    (0..buffer.area.height)
        .map(|y| {
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn footer(activity: ActivityState<'_>, at: Duration, config: DelightConfig) -> Buffer {
    let backend = TestBackend::new(30, 1);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| {
            frame.render_widget(
                Paragraph::new(Line::from("XXXXXXXXXXXXXXXXXXXXXXXXXXXXXX"))
                    .style(Style::default().fg(Color::Blue)),
                frame.area(),
            );
            FooterDelight::render_with_theme(
                frame,
                frame.area(),
                at,
                config,
                activity,
                Theme::LOVE_DARK,
            );
        })
        .unwrap();
    terminal.backend().buffer().clone()
}

#[test]
fn startup_is_infinite_until_escape_or_host_dismissal() {
    let config = DelightConfig::new(true, false, false, Duration::from_millis(1));
    assert_eq!(config.startup_duration(), Duration::from_millis(1));
    let mut startup = StartupDelight::new();
    for elapsed in [
        Duration::ZERO,
        MAX_STARTUP_DURATION,
        Duration::from_secs(86_400),
    ] {
        assert!(startup.is_visible(elapsed, config));
    }
    assert_eq!(startup.observe_input(), InputDisposition::KeepTitleModal);
    assert!(startup.is_visible(Duration::from_secs(2), config));
    assert_eq!(
        startup.observe_escape(),
        InputDisposition::DismissedAndConsumed
    );
    assert!(!startup.is_visible(Duration::ZERO, config));

    let mut bypassed = StartupDelight::new();
    bypassed.dismiss();
    assert!(!bypassed.is_visible(Duration::ZERO, config));
    assert!(config.enabled); // Footer eligibility remains independent.
}

#[test]
fn disabled_and_reduced_motion_are_static_and_bounded() {
    let disabled = DelightConfig::new(false, false, false, MAX_STARTUP_DURATION);
    assert!(!StartupDelight::new().is_visible(Duration::ZERO, disabled));
    assert_eq!(StartupDelight::redraw_interval(disabled), None);

    let reduced = DelightConfig::new(true, true, false, MAX_STARTUP_DURATION);
    assert_eq!(StartupDelight::redraw_interval(reduced), None);
    assert_eq!(
        render_startup(80, 24, Duration::ZERO, reduced),
        render_startup(80, 24, Duration::from_secs(90), reduced)
    );
}

#[test]
fn normal_title_is_exact_big_bold_and_has_shaded_highlighted_heart() {
    let buffer = render_startup(80, 24, Duration::ZERO, DelightConfig::default());
    let screen = text(&buffer);
    assert!(screen.contains(STARTUP_TITLE));
    assert!(screen.contains("ESC TO ENTER"));
    assert!(
        screen.matches('█').count() > 70,
        "bitmap title was not prominent"
    );
    assert!(screen.contains('▀'));
    let colors = buffer
        .content()
        .iter()
        .flat_map(|cell| [cell.fg, cell.bg])
        .collect::<Vec<_>>();
    assert!(
        colors.contains(&Color::Rgb(246, 24, 47)),
        "red heart body missing"
    );
    assert!(
        colors.contains(&Color::Rgb(140, 5, 22)),
        "heart shadow missing"
    );
    assert!(
        colors.contains(&Color::White),
        "reflective highlight missing"
    );
    assert!(
        colors.contains(&Color::Rgb(255, 213, 55)),
        "gold lettering missing"
    );
    let heart_row = screen.lines().position(|line| line.contains('▀')).unwrap();
    let title_row = screen.lines().position(|line| line.contains('█')).unwrap();
    assert!(heart_row < title_row, "heart must sit above lettering");
}

#[test]
fn large_title_scales_bitmap_without_clipping() {
    let buffer = render_startup(120, 35, ANIMATION_TICK, DelightConfig::default());
    let screen = text(&buffer);
    assert!(screen.contains(STARTUP_TITLE));
    assert!(screen.contains("ESC TO ENTER"));
    let occupied_rows = screen.lines().filter(|line| line.contains('█')).count();
    assert!(occupied_rows >= 10, "large bitmap did not scale vertically");
    assert!(
        screen
            .lines()
            .map(|line| line.chars().filter(|character| *character != ' ').count())
            .max()
            .unwrap()
            > 45
    );
    assert_eq!(buffer.area.width, 120);
    assert_eq!(buffer.area.height, 35);
}

#[test]
fn tiny_twenty_by_six_keeps_exact_title_and_escape_prompt() {
    let buffer = render_startup(20, 6, Duration::ZERO, DelightConfig::default());
    let screen = text(&buffer);
    assert!(screen.contains(STARTUP_TITLE));
    assert!(screen.contains("ESC TO ENTER"));
    assert!(screen.contains("◆♥◆"));
    assert_eq!(buffer.area.width, 20);
    assert_eq!(buffer.area.height, 6);
}

#[test]
fn ascii_mode_uses_pixel_symbols_without_unicode_heart_blocks() {
    let config = DelightConfig::new(true, false, true, MAX_STARTUP_DURATION);
    let screen = text(&render_startup(80, 24, Duration::ZERO, config));
    assert!(screen.contains(STARTUP_TITLE));
    assert!(screen.contains('@'));
    assert!(screen.contains('*'));
    assert!(!screen.contains('♥'));
    assert!(!screen.contains('▓'));
}

#[test]
fn startup_effect_has_deterministic_two_beat_frames() {
    let config = DelightConfig::default();
    let beat_one = render_startup(80, 24, Duration::ZERO, config);
    let rest = render_startup(80, 24, ANIMATION_TICK, config);
    let beat_two = render_startup(80, 24, ANIMATION_TICK * 2, config);
    assert_ne!(beat_one, rest);
    assert_eq!(beat_one, beat_two);
    assert_eq!(
        StartupDelight::redraw_interval(config),
        Some(ANIMATION_TICK)
    );
}

#[test]
fn footer_is_shaded_animated_and_never_touches_past_eighteen_columns() {
    let config = DelightConfig::default();
    let beat = footer(
        ActivityState::Active { label: "capturing" },
        Duration::ZERO,
        config,
    );
    let rest = footer(
        ActivityState::Active { label: "capturing" },
        ANIMATION_TICK,
        config,
    );
    assert_eq!(beat[(0, 0)].fg, Theme::LOVE_DARK.heart.primary);
    assert!(text(&beat).contains("♥ ─╱╲"));
    assert!(text(&rest).contains("♥ ───"));
    assert_eq!(
        beat[(6, 0)].symbol(),
        rest[(6, 0)].symbol(),
        "label must not jump"
    );
    assert_eq!(
        beat,
        footer(
            ActivityState::Active { label: "capturing" },
            ANIMATION_TICK * 2,
            config
        )
    );
    for tick in 3..8 {
        assert_eq!(
            rest,
            footer(
                ActivityState::Active { label: "capturing" },
                ANIMATION_TICK * tick,
                config
            )
        );
    }
    for x in FOOTER_MAX_WIDTH..30 {
        assert_eq!(beat[(x, 0)].symbol(), "X");
    }
}

#[test]
fn footer_idle_error_and_progress_labels_remain_truthful() {
    let config = DelightConfig::default();
    let idle = text(&footer(ActivityState::Idle, Duration::ZERO, config));
    assert!(idle.contains("idle"));
    assert!(!idle.contains('%'));
    let unknown = text(&footer(
        ActivityState::Pending {
            label: "scan",
            progress: Progress::Unknown,
        },
        Duration::ZERO,
        config,
    ));
    assert!(unknown.contains("pending"));
    assert!(!unknown.contains('%'));
    let measured = text(&footer(
        ActivityState::Pending {
            label: "scan",
            progress: Progress::Measured {
                completed: 3,
                total: 4,
            },
        },
        Duration::ZERO,
        config,
    ));
    assert!(measured.contains("3/4 75%"));
    let error = footer(
        ActivityState::Error { label: "disk" },
        Duration::ZERO,
        config,
    );
    assert!(text(&error).contains("error"));
    assert_eq!(error[(0, 0)].fg, Theme::LOVE_DARK.heart.error);
}

#[test]
fn visual_preview_artifact_when_requested() {
    let normal = text(&render_startup(
        80,
        24,
        Duration::ZERO,
        DelightConfig::default(),
    ));
    assert!(normal.contains(STARTUP_TITLE));
    if let Some(path) = std::env::var_os("LVU_DELIGHT_ARTIFACT") {
        let large = text(&render_startup(
            120,
            35,
            ANIMATION_TICK,
            DelightConfig::default(),
        ));
        let compact = text(&render_startup(
            20,
            6,
            Duration::ZERO,
            DelightConfig::default(),
        ));
        std::fs::write(
            path,
            format!("80x24\n{normal}\n\n120x35\n{large}\n\n20x6\n{compact}\n"),
        )
        .unwrap();
    }
}
