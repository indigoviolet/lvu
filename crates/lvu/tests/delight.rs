use lvu::{
    delight::{
        ANIMATION_TICK, ActivityState, DelightConfig, FOOTER_MAX_WIDTH, FooterDelight,
        InputDisposition, MAX_STARTUP_DURATION, Progress, STARTUP_ANIMATION_TICK, STARTUP_TITLE,
        StartupDelight,
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
            for cell in &mut frame.buffer_mut().content {
                cell.set_symbol("Z");
            }
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
fn startup_is_infinite_until_key_or_host_dismissal() {
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
    assert_eq!(
        startup.observe_input(),
        InputDisposition::DismissedAndConsumed
    );
    assert!(!startup.is_visible(Duration::from_secs(2), config));
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
fn converted_title_keeps_true_black_canvas_red_heart_and_gold_lettering() {
    for (width, height) in [(80, 24), (120, 40), (140, 48)] {
        let buffer = render_startup(width, height, Duration::ZERO, DelightConfig::default());
        let screen = text(&buffer);
        assert!(screen.contains(STARTUP_TITLE));
        assert!(screen.contains("PRESS ANY KEY"));
        let colors: Vec<_> = buffer
            .content()
            .iter()
            .flat_map(|cell| [cell.fg, cell.bg])
            .collect();
        assert!(
            colors.iter().any(
                |color| matches!(color, Color::Rgb(r, g, b) if *r > 160 && *g < 70 && *b < 70)
            )
        );
        assert!(
            colors.iter().any(
                |color| matches!(color, Color::Rgb(r, g, b) if *r > 220 && *g > 130 && *b < 110)
            )
        );
        assert!(
            colors.iter().any(
                |color| matches!(color, Color::Rgb(r, g, b) if *r > 220 && *g > 220 && *b > 220)
            )
        );
        for y in 0..height {
            for x in [0, width - 1] {
                assert_eq!(buffer[(x, y)].bg, Color::Rgb(0, 0, 0));
            }
        }
        assert!(!screen.contains('\x1b'));
        assert!(
            !screen.contains('Z'),
            "underlying screen leaked around centered art"
        );
    }
}

#[test]
fn artwork_uses_large_variant_only_when_it_fits_and_recovers_after_resize() {
    let normal = render_startup(80, 24, Duration::ZERO, DelightConfig::default());
    let large = render_startup(120, 40, Duration::ZERO, DelightConfig::default());
    let blocks = |buffer: &Buffer| {
        buffer
            .content()
            .iter()
            .filter(|cell| matches!(cell.symbol(), "▀" | "▄"))
            .count()
    };
    assert!(blocks(&large) > blocks(&normal) * 2);
    let backend = TestBackend::new(120, 40);
    let mut terminal = Terminal::new(backend).unwrap();
    for (width, height) in [(120, 40), (20, 6), (80, 24), (120, 40)] {
        terminal.backend_mut().resize(width, height);
        terminal
            .resize(ratatui::layout::Rect::new(0, 0, width, height))
            .unwrap();
        terminal
            .draw(|frame| {
                StartupDelight::new().render_with_theme(
                    frame,
                    frame.area(),
                    Duration::ZERO,
                    DelightConfig::default(),
                    Theme::LOVE_DARK,
                )
            })
            .unwrap();
        assert_eq!(
            terminal.backend().buffer(),
            &render_startup(width, height, Duration::ZERO, DelightConfig::default())
        );
    }
}

#[test]
fn tiny_twenty_by_six_keeps_exact_title_and_escape_prompt() {
    let buffer = render_startup(20, 6, Duration::ZERO, DelightConfig::default());
    let screen = text(&buffer);
    assert!(screen.contains(STARTUP_TITLE));
    assert!(screen.contains("PRESS ANY KEY"));
    assert!(screen.contains("◆♥◆"));
    assert_eq!(buffer.area.width, 20);
    assert_eq!(buffer.area.height, 6);
}

#[test]
fn ascii_mode_uses_pixel_symbols_without_unicode_heart_blocks() {
    let config = DelightConfig::new(true, false, true, MAX_STARTUP_DURATION);
    let screen = text(&render_startup(80, 24, Duration::ZERO, config));
    assert!(screen.contains(STARTUP_TITLE));
    assert!(screen.contains("<##>"));
    assert!(screen.is_ascii());
    assert!(!screen.contains('♥'));
    assert!(!screen.contains('▓'));
}

#[test]
fn startup_animation_preserves_gif_frame_duration_and_loop() {
    let config = DelightConfig::default();
    let first = render_startup(120, 40, Duration::ZERO, config);
    assert_eq!(
        first,
        render_startup(120, 40, Duration::from_millis(109), config)
    );
    assert_ne!(
        first,
        render_startup(120, 40, Duration::from_millis(440), config)
    );
    assert_eq!(
        first,
        render_startup(120, 40, Duration::from_millis(1100), config)
    );
    assert_eq!(
        StartupDelight::redraw_interval(config),
        Some(STARTUP_ANIMATION_TICK)
    );
    assert_eq!(STARTUP_ANIMATION_TICK, Duration::from_millis(110));
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
    assert!(text(&beat).contains("♥"));
    assert!(!text(&beat).contains("capturing"));
    assert!(text(&rest).contains("♡"));
    assert_eq!(
        beat[(2, 0)].symbol(),
        rest[(2, 0)].symbol(),
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
    assert!(!idle.contains("idle"));
    assert!(!idle.contains('%'));
    let unknown = text(&footer(
        ActivityState::Pending {
            label: "scan",
            progress: Progress::Unknown,
        },
        Duration::ZERO,
        config,
    ));
    assert!(!unknown.contains("pending"));
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

#[test]
fn corner_sprite_animates_only_active_work_and_has_no_routine_label() {
    let render = |at, activity, config, theme: Theme| {
        let mut terminal = Terminal::new(TestBackend::new(24, 10)).unwrap();
        terminal
            .draw(|frame| {
                frame.render_widget(
                    Paragraph::new("sentinel"),
                    ratatui::layout::Rect::new(18, 9, 6, 1),
                );
                FooterDelight::render_with_theme(
                    frame,
                    ratatui::layout::Rect::new(0, 2, 18, 8),
                    at,
                    config,
                    activity,
                    theme,
                );
            })
            .unwrap();
        terminal.backend().buffer().clone()
    };
    for theme in [Theme::LOVE_DARK, Theme::LOVE_LIGHT, Theme::TERMINAL] {
        let config = DelightConfig::default();
        let rest = render(Duration::ZERO, ActivityState::Idle, config, theme);
        let active = render(
            Duration::from_millis(750),
            ActivityState::Active {
                label: "agent working",
            },
            config,
            theme,
        );
        assert_ne!(rest, active);
        assert!(!text(&active).contains("agent working"));
        assert!(!text(&rest).contains("idle"));
        assert_eq!(rest[(5, 5)].bg, theme.base_bg);
        for buffer in [&rest, &active] {
            for y in 5..9 {
                for x in 5..12 {
                    let cell = &buffer[(x, y)];
                    if cell.symbol() != " " {
                        assert_ne!(
                            cell.fg,
                            Color::Reset,
                            "opaque half-block must never use default foreground as transparency"
                        );
                    }
                }
            }
        }
        assert!(
            (0..5).all(|x| rest[(x, 5)].symbol() == " "),
            "transparent side margin must not become a white stripe"
        );
        assert_eq!(active[(18, 9)].symbol(), "s");
        let reduced = DelightConfig::new(true, true, false, MAX_STARTUP_DURATION);
        assert_eq!(
            rest,
            render(
                Duration::from_millis(750),
                ActivityState::Active {
                    label: "agent working"
                },
                reduced,
                theme
            )
        );
        assert_eq!(
            rest,
            render(
                Duration::from_millis(750),
                ActivityState::Idle,
                config,
                theme
            )
        );
    }
}
