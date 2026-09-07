//! Identity colors on a terminal without truecolor.
//!
//! `value_color` drives both the color-by column and JSON key highlighting. Its
//! hue comes from a hash, so on a 256-color terminal the 24-bit sequence it used
//! to emit was approximated by the emulator: two values could collapse onto one
//! displayed color, and a color lvu had measured as readable could land
//! somewhere else entirely. These check that the downgraded palette keeps the
//! two properties that matter -- the same value gets the same color, and every
//! color clears the contrast floor against the background it is drawn on -- and
//! that what reaches the screen is genuinely a cube index.

use lvu::{
    App, Focus,
    fixture::FixtureProvider,
    theme::{ColorDepth, MIN_IDENTITY_CONTRAST, Theme, ThemeId, contrast, resolved_rgb},
    ui,
};
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer, style::Color};
use std::collections::HashSet;

/// Concrete-background themes. `Terminal` inherits an unknowable background, so
/// no contrast against it can be measured either way.
const CONCRETE: [ThemeId; 5] = [
    ThemeId::LoveDark,
    ThemeId::LoveLight,
    ThemeId::Dracula,
    ThemeId::Nord,
    ThemeId::GruvboxDark,
];

fn identities() -> Vec<String> {
    (0..256)
        .map(|index| format!("identity-{index}-東京-e\u{301}-{}", index * 7919))
        .collect()
}

fn render(theme: Theme) -> Buffer {
    let (provider, sources, views) = FixtureProvider::json_demo();
    let mut app = App::new(sources, views, true);
    app.focus = Focus::Logs;
    let backend = TestBackend::new(120, 25);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| ui::render_with_theme(frame, &mut app, &provider, theme, None))
        .unwrap();
    terminal.backend().buffer().clone()
}

#[test]
fn colorterm_selects_the_depth_and_only_a_positive_claim_means_truecolor() {
    assert_eq!(
        ColorDepth::from_colorterm(Some("truecolor")),
        ColorDepth::TrueColor
    );
    assert_eq!(
        ColorDepth::from_colorterm(Some("24bit")),
        ColorDepth::TrueColor
    );
    assert_eq!(
        ColorDepth::from_colorterm(Some(" truecolor ")),
        ColorDepth::TrueColor
    );
    // An unset or unrecognised variable degrades; it never guesses upward.
    for value in [None, Some(""), Some("8bit"), Some("256"), Some("yes")] {
        assert_eq!(
            ColorDepth::from_colorterm(value),
            ColorDepth::Indexed256,
            "{value:?} must not be read as a truecolor claim"
        );
    }
    assert_eq!(ColorDepth::default(), ColorDepth::TrueColor);
    // The palette constants stay 24-bit; only the resolved theme downgrades.
    assert_eq!(Theme::LOVE_DARK.depth, ColorDepth::TrueColor);
    assert_eq!(
        Theme::LOVE_DARK.with_depth(ColorDepth::Indexed256).depth,
        ColorDepth::Indexed256
    );
}

#[test]
fn indexed_identity_colors_are_cube_indexes_that_clear_the_contrast_floor() {
    for id in CONCRETE {
        let theme = id.theme().with_depth(ColorDepth::Indexed256);
        for identity in identities() {
            let color = theme.value_color(&identity);
            let Color::Indexed(index) = color else {
                panic!(
                    "{:?} identity {identity:?} emitted {color:?}, not a cube index",
                    id
                );
            };
            assert!(
                (16..232).contains(&index),
                "{:?} identity {identity:?} left the 6x6x6 cube at index {index}",
                id
            );
            // Stable across calls: the same value must not shift color.
            assert_eq!(color, theme.value_color(&identity));
            let ratio = contrast(color, theme.base_bg).expect("cube and theme both resolve");
            assert!(
                ratio >= MIN_IDENTITY_CONTRAST,
                "{:?} identity {identity:?} displayed at {ratio:.2}, below the floor",
                id
            );
        }
    }
}

#[test]
fn the_cube_keeps_identities_distinguishable_on_dark_and_light_backgrounds() {
    for id in CONCRETE {
        let theme = id.theme().with_depth(ColorDepth::Indexed256);
        let colors = identities()
            .iter()
            .map(|identity| theme.value_color(identity))
            .collect::<HashSet<_>>();
        // The cube has 216 entries and its readable subset is smaller still, so
        // 256 identities must collide somewhere. The failure to catch is a
        // collapse onto a handful, which is exactly what quantising a single
        // fixed-lightness ring produces -- it yielded 12 to 24 here before the
        // hash was allowed to pick a radius as well. Measured range is 33 to 65.
        assert!(
            colors.len() >= 30,
            "{:?} collapsed 256 identities onto {} colors",
            id,
            colors.len()
        );
    }
}

#[test]
fn the_cube_color_still_reads_as_the_hue_the_hash_chose() {
    // The point of hashing to a hue is that unrelated values look unrelated and
    // related ones do not drift between runs. Quantising has to move the color,
    // but if it moved the *hue* far the downgraded palette would no longer
    // agree with the truecolor one about what a value looks like. Near-grey
    // colors are excluded: hue is not meaningful once chroma is that low.
    let mut worst: f64 = 0.0;
    let mut compared = 0;
    for id in CONCRETE {
        for identity in identities() {
            let wide = resolved_rgb(id.theme().value_color(&identity)).unwrap();
            let narrow = resolved_rgb(
                id.theme()
                    .with_depth(ColorDepth::Indexed256)
                    .value_color(&identity),
            )
            .unwrap();
            assert!(matches!(id.theme().value_color(&identity), Color::Rgb(..)));
            if chroma(wide) < 0.10 || chroma(narrow) < 0.10 {
                continue;
            }
            let shift = hue_distance(hue_of(wide), hue_of(narrow));
            assert!(
                shift <= 60.0,
                "{:?} {identity:?} moved {shift:.0} degrees, from {wide:?} to {narrow:?}",
                id
            );
            worst = worst.max(shift);
            compared += 1;
        }
    }
    assert!(
        compared > 800,
        "only {compared} identities were chromatic enough to check"
    );
    assert!(worst > 0.0, "quantising must actually move colors");
}

fn chroma((red, green, blue): (u8, u8, u8)) -> f64 {
    let channels = [f64::from(red), f64::from(green), f64::from(blue)];
    let max = channels.iter().cloned().fold(f64::MIN, f64::max);
    let min = channels.iter().cloned().fold(f64::MAX, f64::min);
    (max - min) / 255.0
}

fn hue_of((red, green, blue): (u8, u8, u8)) -> f64 {
    let (red, green, blue) = (
        f64::from(red) / 255.0,
        f64::from(green) / 255.0,
        f64::from(blue) / 255.0,
    );
    let max = red.max(green).max(blue);
    let min = red.min(green).min(blue);
    let delta = max - min;
    if delta == 0.0 {
        return 0.0;
    }
    let hue = if max == red {
        60.0 * (((green - blue) / delta) % 6.0)
    } else if max == green {
        60.0 * ((blue - red) / delta + 2.0)
    } else {
        60.0 * ((red - green) / delta + 4.0)
    };
    (hue + 360.0) % 360.0
}

fn hue_distance(left: f64, right: f64) -> f64 {
    let raw = (left - right).abs();
    raw.min(360.0 - raw)
}

#[test]
fn rendered_json_keys_carry_indexed_colors_without_truecolor_and_rgb_with_it() {
    let truecolor = render(Theme::LOVE_DARK);
    let indexed = render(Theme::LOVE_DARK.with_depth(ColorDepth::Indexed256));
    assert_eq!(
        truecolor.area, indexed.area,
        "the depth must not change what is laid out, only how it is colored"
    );

    let foregrounds = |buffer: &Buffer| {
        (0..buffer.area.height)
            .flat_map(|y| (0..buffer.area.width).map(move |x| (x, y)))
            .map(|position| buffer[position].fg)
            .collect::<Vec<_>>()
    };
    let wide = foregrounds(&truecolor);
    let narrow = foregrounds(&indexed);

    // Identity colors are the only role that downgrades, so a cube index on the
    // screen can only have come from one.
    let indexed_cells = narrow
        .iter()
        .filter(|color| matches!(color, Color::Indexed(index) if (16..232).contains(index)))
        .count();
    assert!(
        indexed_cells > 0,
        "no identity-colored cell survived to the screen"
    );
    assert_eq!(
        wide.iter()
            .filter(|color| matches!(color, Color::Indexed(index) if (16..232).contains(index)))
            .count(),
        0,
        "a truecolor render must not contain cube indexes"
    );

    // Every cell that changed changed from a 24-bit color to a cube index, and
    // nothing else about the frame moved.
    for (position, (wide, narrow)) in wide.iter().zip(&narrow).enumerate() {
        if wide == narrow {
            continue;
        }
        assert!(
            matches!(wide, Color::Rgb(..)) && matches!(narrow, Color::Indexed(_)),
            "cell {position} changed from {wide:?} to {narrow:?}, which is not a depth downgrade"
        );
        let ratio = contrast(*narrow, Theme::LOVE_DARK.base_bg).expect("both resolve");
        assert!(
            ratio >= MIN_IDENTITY_CONTRAST,
            "cell {position} was drawn at {ratio:.2}, below the floor"
        );
    }
}

#[test]
fn an_unknowable_background_still_downgrades_and_stays_stable() {
    // `Terminal` inherits the user's background, so no contrast can be measured
    // against it -- but the sequence still has to be one the terminal can show.
    let theme = Theme::TERMINAL.with_depth(ColorDepth::Indexed256);
    assert!(contrast(theme.value_color("a"), theme.base_bg).is_none());
    for identity in identities() {
        let color = theme.value_color(&identity);
        assert!(
            matches!(color, Color::Indexed(index) if (16..232).contains(&index)),
            "{identity:?} emitted {color:?}"
        );
        assert_eq!(color, theme.value_color(&identity));
    }
}

#[test]
#[ignore = "measurement, not an assertion"]
fn report_distinct_identity_colors() {
    for id in CONCRETE {
        for depth in [ColorDepth::TrueColor, ColorDepth::Indexed256] {
            let theme = id.theme().with_depth(depth);
            let colors = identities()
                .iter()
                .map(|identity| theme.value_color(identity))
                .collect::<HashSet<_>>();
            println!("{:?} {:?}: {} distinct of 256", id, depth, colors.len());
        }
    }
}
