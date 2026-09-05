use lvu::delight::{
    ANIMATION_TICK, ActivityState, DelightConfig, FooterDelight, InputDisposition,
    MAX_STARTUP_DURATION, Progress, StartupDelight,
};
use ratatui::{Terminal, backend::TestBackend, layout::Rect};
use std::time::Duration;
use unicode_width::UnicodeWidthStr;

fn rendered(width: u16, height: u16, draw: impl FnOnce(&mut ratatui::Frame<'_>)) -> Vec<String> {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(draw).unwrap();
    let buffer = terminal.backend().buffer();
    (0..height)
        .map(|y| {
            (0..width)
                .map(|x| buffer[(x, y)].symbol().chars().next().unwrap_or(' '))
                .collect::<String>()
        })
        .collect()
}

fn footer(activity: ActivityState<'_>, elapsed: Duration, config: DelightConfig) -> String {
    rendered(48, 1, |frame| {
        FooterDelight::render(frame, frame.area(), elapsed, config, activity)
    })[0]
        .trim_end()
        .to_owned()
}

#[test]
fn startup_has_a_hard_duration_and_input_dismissal_passes_through() {
    let mut startup = StartupDelight::new();
    let config = DelightConfig::new(true, false, false, Duration::from_secs(20));
    assert_eq!(config.startup_duration(), MAX_STARTUP_DURATION);
    assert!(startup.is_visible(Duration::from_millis(599), config));
    assert!(!startup.is_visible(Duration::from_millis(600), config));

    assert_eq!(
        startup.observe_input(),
        InputDisposition::ContinueToApplication
    );
    assert!(!startup.is_visible(Duration::ZERO, config));

    let mut programmatic = StartupDelight::new();
    programmatic.dismiss();
    assert!(!programmatic.is_visible(Duration::ZERO, config));
}

#[test]
fn disabled_and_reduced_motion_require_no_animation_redraws() {
    let disabled = DelightConfig::new(false, false, false, Duration::from_millis(400));
    assert_eq!(StartupDelight::redraw_interval(disabled), None);
    assert!(!StartupDelight::new().is_visible(Duration::ZERO, disabled));

    let reduced = DelightConfig::new(true, true, false, Duration::from_millis(400));
    assert_eq!(StartupDelight::redraw_interval(reduced), None);
    let first = footer(
        ActivityState::Active { label: "capturing" },
        Duration::ZERO,
        reduced,
    );
    let later = footer(
        ActivityState::Active { label: "capturing" },
        Duration::from_secs(10),
        reduced,
    );
    assert_eq!(first, later);
}

#[test]
fn startup_renders_pixel_heart_brand_and_ascii_fallback() {
    let startup = StartupDelight::new();
    let unicode = rendered(40, 10, |frame| {
        startup.render(
            frame,
            frame.area(),
            Duration::ZERO,
            DelightConfig::default(),
        )
    })
    .join("\n");
    assert!(unicode.contains("████"));
    assert!(unicode.contains("lvu"));
    assert!(unicode.contains("love you"));

    let ascii = rendered(40, 10, |frame| {
        startup.render(
            frame,
            frame.area(),
            Duration::ZERO,
            DelightConfig::new(true, false, true, MAX_STARTUP_DURATION),
        )
    })
    .join("\n");
    assert!(ascii.contains("*********"));
    assert!(!ascii.contains('█'));
}

#[test]
fn tiny_rectangles_are_safe_and_never_exceed_unicode_cell_width() {
    let startup = StartupDelight::new();
    for (width, height) in [(0, 0), (1, 1), (2, 2), (7, 1), (15, 3)] {
        if width == 0 || height == 0 {
            continue;
        }
        let lines = rendered(width, height, |frame| {
            startup.render(
                frame,
                Rect::new(0, 0, width, height),
                Duration::ZERO,
                DelightConfig::default(),
            )
        });
        assert!(
            lines
                .iter()
                .all(|line| UnicodeWidthStr::width(line.as_str()) <= width as usize)
        );
    }
}

#[test]
fn active_footer_uses_predictable_two_beat_quantized_pulse() {
    let config = DelightConfig::default();
    let beat_one = footer(
        ActivityState::Active { label: "capturing" },
        Duration::ZERO,
        config,
    );
    let rest = footer(
        ActivityState::Active { label: "capturing" },
        ANIMATION_TICK,
        config,
    );
    let beat_two = footer(
        ActivityState::Active { label: "capturing" },
        ANIMATION_TICK * 2,
        config,
    );
    assert!(beat_one.starts_with('♥'));
    assert!(rest.starts_with('♡'));
    assert!(beat_two.starts_with('♥'));
    assert_eq!(
        StartupDelight::redraw_interval(config),
        Some(ANIMATION_TICK)
    );
}

#[test]
fn idle_error_unknown_and_measured_progress_are_truthful() {
    let config = DelightConfig::default();
    let idle = footer(ActivityState::Idle, Duration::ZERO, config);
    assert!(idle.contains("idle"));
    assert!(!idle.contains('%'));

    let pending = footer(
        ActivityState::Pending {
            label: "indexing",
            progress: Progress::Unknown,
        },
        Duration::ZERO,
        config,
    );
    assert!(pending.contains("indexing · pending"));
    assert!(!pending.contains('%'));

    let measured = footer(
        ActivityState::Pending {
            label: "indexing",
            progress: Progress::Measured {
                completed: 3,
                total: 12,
            },
        },
        Duration::ZERO,
        config,
    );
    assert!(measured.contains("3/12 (25%)"));

    let error = footer(
        ActivityState::Error { label: "disk full" },
        Duration::ZERO,
        config,
    );
    assert!(error.contains("error · disk full"));
    assert!(!error.contains('%'));
}

#[test]
fn zero_total_and_overshoot_do_not_claim_false_completion() {
    let config = DelightConfig::default();
    let unknown = footer(
        ActivityState::Pending {
            label: "loading",
            progress: Progress::Measured {
                completed: 4,
                total: 0,
            },
        },
        Duration::ZERO,
        config,
    );
    assert!(unknown.contains("pending"));
    assert!(!unknown.contains('%'));

    let bounded = footer(
        ActivityState::Pending {
            label: "loading",
            progress: Progress::Measured {
                completed: 20,
                total: 10,
            },
        },
        Duration::ZERO,
        config,
    );
    assert!(bounded.contains("10/10 (100%)"));
    assert!(!bounded.contains("20/10"));
}

#[test]
fn long_labels_are_bounded_before_terminal_clipping() {
    let label = "界".repeat(200);
    let line = footer(
        ActivityState::Active { label: &label },
        Duration::ZERO,
        DelightConfig::default(),
    );
    assert!(line.chars().count() <= 48);
    assert!(line.contains('…'));
}

#[test]
fn narrow_live_layout_keeps_the_original_status_width() {
    let (provider, sources, views) = lvu::fixture::FixtureProvider::demo();
    let mut app = lvu::App::new(sources, views, true);
    let plain = rendered(40, 10, |frame| lvu::ui::render(frame, &mut app, &provider));
    let decorated = rendered(40, 10, |frame| {
        lvu::ui::render_with_delight(
            frame,
            &mut app,
            &provider,
            Some((
                Duration::ZERO,
                DelightConfig::default(),
                ActivityState::Idle,
            )),
        )
    });
    assert_eq!(plain.last(), decorated.last());
    assert!(decorated.last().unwrap().contains("FOLLOW"));
}
