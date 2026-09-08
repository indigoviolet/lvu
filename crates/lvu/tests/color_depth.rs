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

// ---------------------------------------------------------------------------
// Sixteen colors. `TERM=xterm` and friends have six usable hues and nothing
// else; emitting a cube index there is how two identities become one color.
// ---------------------------------------------------------------------------

/// What the sixteen ANSI colors conventionally display as, so a test can
/// measure what the theme claims to have measured. These are xterm's defaults,
/// the same assumption the theme makes.
fn ansi_rgb(color: Color, bold: bool) -> (u8, u8, u8) {
    let base = match color {
        Color::Red => [(205, 0, 0), (255, 0, 0)],
        Color::Green => [(0, 205, 0), (0, 255, 0)],
        Color::Yellow => [(205, 205, 0), (255, 255, 0)],
        Color::Blue => [(0, 0, 238), (92, 92, 255)],
        Color::Magenta => [(205, 0, 205), (255, 0, 255)],
        Color::Cyan => [(0, 205, 205), (0, 255, 255)],
        other => panic!("{other:?} is not one of the six usable hues"),
    };
    base[usize::from(bold)]
}

fn ratio_against(color: Color, bold: bool, background: Color) -> f64 {
    let (red, green, blue) = ansi_rgb(color, bold);
    contrast(Color::Rgb(red, green, blue), background).expect("the theme background resolves")
}

/// The depth the environment implies. `TERM` decides unless `COLORTERM` makes a
/// positive claim; `tput` is only asked when `TERM` says nothing.
#[test]
fn a_terminal_without_256color_is_detected_as_sixteen_colors() {
    let never = || panic!("tput must not be consulted when TERM answers");
    for term in ["xterm", "screen", "linux", "vt100", "rxvt"] {
        assert_eq!(
            ColorDepth::detect(None, Some(term), never),
            ColorDepth::Ansi16,
            "{term}"
        );
    }
    for term in ["xterm-256color", "screen-256color", "xterm-direct"] {
        assert_eq!(
            ColorDepth::detect(None, Some(term), never),
            ColorDepth::Indexed256,
            "{term}"
        );
    }
    // A positive truecolor claim outranks TERM, which is what it is for.
    assert_eq!(
        ColorDepth::detect(Some("truecolor"), Some("xterm"), never),
        ColorDepth::TrueColor
    );
    assert_eq!(
        ColorDepth::detect(Some("24bit"), Some("xterm"), never),
        ColorDepth::TrueColor
    );
    // Only with no TERM at all is the terminal asked directly.
    assert_eq!(
        ColorDepth::detect(None, None, || Some(16)),
        ColorDepth::Ansi16
    );
    assert_eq!(
        ColorDepth::detect(None, Some(""), || Some(16)),
        ColorDepth::Ansi16
    );
    assert_eq!(
        ColorDepth::detect(None, None, || Some(256)),
        ColorDepth::Indexed256
    );
    assert_eq!(
        ColorDepth::detect(None, None, || Some(16_777_216)),
        ColorDepth::TrueColor
    );
    // Nothing said anything: the cube is where this started, and stays.
    assert_eq!(
        ColorDepth::detect(None, None, || None),
        ColorDepth::Indexed256
    );
}

/// Every identity lands on one of the six usable hues, is stable, and clears
/// the contrast floor against the theme's own background.
#[test]
fn ansi_identity_colors_are_named_hues_that_clear_the_contrast_floor() {
    for id in CONCRETE {
        let theme = id.theme().with_depth(ColorDepth::Ansi16);
        for identity in identities() {
            let style = theme.value_style(&identity);
            let color = style.fg.expect("an identity always has a foreground");
            assert!(
                matches!(
                    color,
                    Color::Red
                        | Color::Green
                        | Color::Yellow
                        | Color::Blue
                        | Color::Magenta
                        | Color::Cyan
                ),
                "{id:?} identity {identity:?} emitted {color:?}, not a usable ANSI hue"
            );
            // Stable across calls, and `value_color` agrees with `value_style`.
            assert_eq!(style, theme.value_style(&identity));
            assert_eq!(theme.value_color(&identity), color);
            let bold = style.add_modifier.contains(ratatui::style::Modifier::BOLD);
            // At this depth the painted background is the terminal's own, so
            // the floor is measured against the background the theme was
            // designed for — the statement the user made by choosing it.
            let ratio = ratio_against(color, bold, theme.contrast_background());
            assert!(
                ratio >= MIN_IDENTITY_CONTRAST,
                "{id:?} identity {identity:?} displays at {ratio:.2}, below the floor"
            );
        }
    }
}

/// Bold is the seventh axis: six hues alone would halve what a user can tell
/// apart, so the hash picks the weight as well as the hue.
#[test]
fn bold_widens_the_sixteen_colour_palette_beyond_its_six_hues() {
    use ratatui::style::Modifier;
    for id in CONCRETE {
        let theme = id.theme().with_depth(ColorDepth::Ansi16);
        let styles = identities()
            .iter()
            .map(|identity| theme.value_style(identity))
            .collect::<HashSet<_>>();
        let hues = identities()
            .iter()
            .map(|identity| theme.value_color(identity))
            .collect::<HashSet<_>>();
        assert!(
            styles.len() > hues.len(),
            "{id:?}: bold added nothing ({} styles for {} hues)",
            styles.len(),
            hues.len()
        );
        assert!(
            identities().iter().any(|identity| theme
                .value_style(identity)
                .add_modifier
                .contains(Modifier::BOLD)),
            "{id:?}: no identity is bold"
        );
        // Every hue the floor allows is in use, so the palette is not collapsing
        // onto one or two colours the way an emulator's approximation would.
        assert!(hues.len() >= 3, "{id:?}: only {} hues in use", hues.len());
    }
}

/// The unknowable terminal background: nothing can be measured, so the hash's
/// own choice stands and stays stable.
#[test]
fn the_terminal_theme_keeps_stable_ansi_identities_without_a_background() {
    let theme = Theme::TERMINAL.with_depth(ColorDepth::Ansi16);
    for identity in identities() {
        let style = theme.value_style(&identity);
        assert_eq!(style, theme.value_style(&identity));
        assert!(style.fg.is_some());
    }
}

/// Levels are named rather than approximated, and the JSON kinds keep their
/// distinctions instead of collapsing onto whatever the emulator picks.
#[test]
fn levels_and_json_kinds_are_named_colours_at_sixteen() {
    use lvu::theme::JsonScalar;
    let theme = Theme::LOVE_DARK.with_depth(ColorDepth::Ansi16);
    assert_eq!(theme.severity_color("ERROR"), Some(Color::Red));
    assert_eq!(theme.severity_color("FATAL"), Some(Color::Red));
    assert_eq!(theme.severity_color("WARN"), Some(Color::Yellow));
    assert_eq!(theme.severity_color("INFO"), Some(Color::Green));
    assert_eq!(theme.severity_color("DEBUG"), Some(Color::Cyan));
    assert_eq!(theme.severity_color("TRACE"), Some(Color::Blue));
    assert_eq!(theme.severity_color("nonsense"), None);

    let kinds = [
        JsonScalar::String,
        JsonScalar::Number,
        JsonScalar::Boolean,
        JsonScalar::Null,
        JsonScalar::Punctuation,
    ];
    let colours = kinds
        .iter()
        .map(|kind| theme.json_color(*kind))
        .collect::<HashSet<_>>();
    assert_eq!(colours.len(), kinds.len(), "two kinds share a colour");
    // At every other depth the theme's own RGB still applies.
    let wide = Theme::LOVE_DARK.with_depth(ColorDepth::TrueColor);
    assert_eq!(wide.json_color(JsonScalar::String), wide.json.string);
    assert_eq!(wide.severity_color("ERROR"), Some(wide.severity.error));
}

// ---------------------------------------------------------------------------
// Chrome at sixteen colours. The identity palette was the first pass; this is
// everything the user reads *around* the data — and the two that matter most
// are the ones lvu paints both halves of, because an approximated selection
// can land on the background it is meant to stand out from.
// ---------------------------------------------------------------------------

/// Every ANSI colour lvu may name, and the xterm RGB it displays as.
fn ansi_display(color: Color) -> Option<(u8, u8, u8)> {
    Some(match color {
        Color::Black => (0, 0, 0),
        Color::Red => (205, 0, 0),
        Color::Green => (0, 205, 0),
        Color::Yellow => (205, 205, 0),
        Color::Blue => (0, 0, 238),
        Color::Magenta => (205, 0, 205),
        Color::Cyan => (0, 205, 205),
        Color::Gray => (229, 229, 229),
        Color::DarkGray => (127, 127, 127),
        Color::LightRed => (255, 0, 0),
        Color::LightGreen => (0, 255, 0),
        Color::LightYellow => (255, 255, 0),
        Color::LightBlue => (92, 92, 255),
        Color::LightMagenta => (255, 0, 255),
        Color::LightCyan => (0, 255, 255),
        Color::White => (255, 255, 255),
        _ => return None,
    })
}

fn pair_contrast(foreground: Color, background: Color) -> f64 {
    let (fr, fg, fb) = ansi_display(foreground).expect("a named ANSI foreground");
    let (br, bg, bb) = ansi_display(background).expect("a named ANSI background");
    contrast(Color::Rgb(fr, fg, fb), Color::Rgb(br, bg, bb)).expect("both resolve")
}

/// Nothing lvu paints at this depth is a 24-bit colour. That is the whole
/// point: a terminal with sixteen colours does not reject an RGB sequence, it
/// approximates it, and an approximated chrome role is one nobody measured.
#[test]
fn no_chrome_role_is_rgb_at_sixteen_colours() {
    for id in ThemeId::ALL {
        let theme = id.theme().with_depth(ColorDepth::Ansi16);
        let roles: [(&str, Color); 14] = [
            ("base_fg", theme.base_fg),
            ("base_bg", theme.base_bg),
            ("dialog_bg", theme.dialog_bg),
            ("input_bg", theme.input_bg),
            ("input_fg", theme.input_fg),
            ("muted", theme.muted),
            ("border", theme.border),
            ("active_border", theme.active_border),
            ("focused_input_border", theme.focused_input_border),
            ("accent", theme.accent),
            ("cursor", theme.cursor),
            ("selection_fg", theme.selection_fg),
            ("selection_bg", theme.selection_bg),
            ("severity.error", theme.severity.error),
        ];
        for (name, color) in roles {
            assert!(
                !matches!(color, Color::Rgb(..) | Color::Indexed(..)),
                "{id:?} {name} is {color:?}, which this terminal cannot show"
            );
        }
        // The surfaces lvu cannot know inherit rather than guess.
        assert_eq!(theme.base_bg, Color::Reset, "{id:?}");
        assert_eq!(theme.dialog_bg, Color::Reset, "{id:?}");
        assert_eq!(theme.input_bg, Color::Reset, "{id:?}");
    }
}

/// The two filled regions: lvu paints both halves, so the floor is real.
#[test]
fn the_selection_and_the_default_fill_clear_the_floor_at_sixteen_colours() {
    use lvu::dialog_controls::{ButtonRole, role_style};
    for id in ThemeId::ALL {
        let theme = id.theme().with_depth(ColorDepth::Ansi16);
        let ratio = pair_contrast(theme.selection_fg, theme.selection_bg);
        assert!(
            ratio >= 4.5,
            "{id:?}: selection reads at {ratio:.2}, below 4.5"
        );

        // §8.9's accent fill, through the one function that decides a button's
        // look, so this is what the screen gets rather than what the theme says.
        let fill = role_style(theme, ButtonRole::Default, false);
        let foreground = fill.fg.expect("the default button has a foreground");
        let background = fill.bg.expect("the default button is filled");
        assert_eq!(background, theme.accent, "{id:?}");
        let ratio = pair_contrast(foreground, background);
        assert!(
            ratio >= 4.5,
            "{id:?}: the default button reads at {ratio:.2}, below 4.5"
        );
        // And it is not the selection, or the fill would say "cursor here".
        assert_ne!(background, theme.selection_bg, "{id:?}");
    }
}

/// The message roles stay three different colours, because Applied, Pending and
/// Error are the three states a dialog reports and they are told apart by hue.
#[test]
fn the_message_roles_stay_distinct_at_every_depth() {
    for depth in [
        ColorDepth::TrueColor,
        ColorDepth::Indexed256,
        ColorDepth::Ansi16,
    ] {
        for id in ThemeId::ALL {
            let theme = id.theme().with_depth(depth);
            let roles = HashSet::from([
                theme.severity.info,
                theme.severity.warn,
                theme.severity.error,
            ]);
            assert_eq!(
                roles.len(),
                3,
                "{id:?}/{depth:?}: two message states share a colour"
            );
        }
    }
}

/// A border is structure and a focused border is not: the two must differ, or
/// the dialog that has the keys looks like the one that does not.
#[test]
fn borders_and_the_scrim_stay_told_apart_at_sixteen_colours() {
    for id in ThemeId::ALL {
        let theme = id.theme().with_depth(ColorDepth::Ansi16);
        assert_ne!(theme.border, theme.active_border, "{id:?}");
        assert_eq!(theme.focused_input_border, theme.active_border, "{id:?}");
        // §6.2: the scrim is what separates a dialog from the workspace when
        // the surfaces cannot. It is dim, and dim is bright black here.
        assert_eq!(theme.muted, Color::DarkGray, "{id:?}");
        assert_ne!(theme.muted, theme.base_fg, "{id:?}");
    }
}

/// The depth is what changes, not the palette definitions: asking for a wide
/// depth after a narrow one gives the original colours back.
#[test]
fn resolving_a_depth_does_not_consume_the_theme() {
    for id in ThemeId::ALL {
        let wide = id.theme().with_depth(ColorDepth::TrueColor);
        let narrow = id.theme().with_depth(ColorDepth::Ansi16);
        assert_eq!(wide.selection_bg, id.theme().selection_bg, "{id:?}");
        assert_ne!(narrow.selection_bg, wide.selection_bg, "{id:?}");
        // And the floor is still measured against the background the theme
        // declares, whatever depth it was resolved for.
        assert_eq!(narrow.contrast_background(), wide.base_bg, "{id:?}");
    }
}

/// What the screen actually gets, at each depth: the selected row is painted
/// with a pair that clears the floor, so the cursor is visible whatever the
/// terminal can show.
///
/// This is the failure the sixteen-colour pass exists to prevent. A selection
/// emitted as `48;2;…` on a sixteen-colour terminal is approximated onto the
/// nearest of sixteen, which on a dark theme is the background it was supposed
/// to stand out from — the cursor disappears and nothing in lvu knows.
#[test]
fn the_selected_row_is_visible_at_every_depth() {
    for depth in [
        ColorDepth::TrueColor,
        ColorDepth::Indexed256,
        ColorDepth::Ansi16,
    ] {
        for id in CONCRETE {
            let theme = id.theme().with_depth(depth);
            let buffer = render(theme);
            let painted = (0..buffer.area.height)
                .flat_map(|y| (0..buffer.area.width).map(move |x| (x, y)))
                .map(|point| &buffer[point])
                .find(|cell| cell.bg == theme.selection_bg && !cell.symbol().trim().is_empty())
                .unwrap_or_else(|| panic!("{id:?}/{depth:?}: nothing carries the selection"));
            assert_eq!(painted.fg, theme.selection_fg, "{id:?}/{depth:?}");
            let ratio = match depth {
                ColorDepth::Ansi16 => pair_contrast(painted.fg, painted.bg),
                _ => contrast(painted.fg, painted.bg).expect("both resolve"),
            };
            assert!(
                ratio >= 4.5,
                "{id:?}/{depth:?}: the selection reads at {ratio:.2}"
            );
        }
    }
}

/// §8.10's mnemonic is an attribute, not a colour, so it survives a palette
/// that has no colours to spare — and it has to, because it is the only thing
/// marking which letter reaches a control.
#[test]
fn the_mnemonic_underline_survives_every_depth() {
    use ratatui::style::Modifier;
    for depth in [
        ColorDepth::TrueColor,
        ColorDepth::Indexed256,
        ColorDepth::Ansi16,
    ] {
        let theme = Theme::LOVE_DARK.with_depth(depth);
        // The mnemonic lives on a dialog's action row, so open one.
        let (provider, sources, views) = FixtureProvider::json_demo();
        let mut app = App::new(sources, views, true);
        app.handle(lvu::Action::Open(lvu::component::Open::Time), &provider);
        let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
        terminal
            .draw(|frame| ui::render_with_theme(frame, &mut app, &provider, theme, None))
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        let underlined = (0..buffer.area.height)
            .flat_map(|y| (0..buffer.area.width).map(move |x| (x, y)))
            .filter(|point| buffer[*point].modifier.contains(Modifier::UNDERLINED))
            .map(|point| buffer[point].symbol().to_owned())
            .collect::<Vec<_>>();
        assert!(
            !underlined.is_empty(),
            "{depth:?}: no mnemonic is underlined"
        );
    }
}

/// A hue drawn without a hash — a predicate colour rule is the case that is
/// coming — gets all three depths from one function, so nothing has to grow its
/// own `match` on the depth and forget the third arm.
#[test]
fn a_fixed_hue_resolves_at_every_depth_and_clears_the_floor() {
    for hue in [0.0, 45.0, 120.0, 200.0, 280.0, 340.0] {
        for id in CONCRETE {
            let wide = id.theme().with_depth(ColorDepth::TrueColor).hue_color(hue);
            assert!(matches!(wide, Color::Rgb(..)), "{id:?} {hue}: {wide:?}");
            assert!(
                contrast(wide, id.theme().base_bg).expect("both resolve") >= MIN_IDENTITY_CONTRAST
            );

            let cube = id.theme().with_depth(ColorDepth::Indexed256).hue_color(hue);
            assert!(matches!(cube, Color::Indexed(..)), "{id:?} {hue}: {cube:?}");
            assert!(
                contrast(cube, id.theme().base_bg).expect("both resolve") >= MIN_IDENTITY_CONTRAST
            );

            let narrow = id.theme().with_depth(ColorDepth::Ansi16);
            let ansi = narrow.hue_color(hue);
            assert!(
                ansi_display(ansi).is_some(),
                "{id:?} {hue}: {ansi:?} is not a named ANSI colour"
            );
            let (red, green, blue) = ansi_display(ansi).unwrap();
            let ratio = contrast(Color::Rgb(red, green, blue), narrow.contrast_background())
                .expect("both resolve");
            assert!(
                ratio >= MIN_IDENTITY_CONTRAST,
                "{id:?} {hue}: displays at {ratio:.2}"
            );
        }
    }
}
