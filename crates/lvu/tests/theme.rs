use lvu::{
    Action, App, Focus,
    command_palette::{Palette, PaletteContext},
    delight::{ActivityState, DelightConfig, FooterDelight},
    fixture::FixtureProvider,
    theme::{MIN_IDENTITY_CONTRAST, Theme, ThemeId, stable_value_slot},
    ui,
};
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};
use std::time::Duration;

fn demo() -> (FixtureProvider, App) {
    let (provider, sources, views) = FixtureProvider::demo();
    (provider, App::new(sources, views, true))
}

fn render(provider: &FixtureProvider, app: &mut App, theme: Theme) -> Buffer {
    let backend = TestBackend::new(100, 25);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| ui::render_with_theme(frame, app, provider, theme, None))
        .unwrap();
    terminal.backend().buffer().clone()
}

fn find(buffer: &Buffer, needle: &str) -> (u16, u16) {
    for y in 0..buffer.area.height {
        let line = (0..buffer.area.width)
            .map(|x| buffer[(x, y)].symbol())
            .collect::<String>();
        if let Some(x) = line.find(needle) {
            return (x as u16, y);
        }
    }
    panic!("missing {needle:?}");
}

#[test]
fn builtin_ids_are_stable_and_terminal_preserves_default_background() {
    assert_eq!(ThemeId::Terminal.as_str(), "terminal");
    assert_eq!(ThemeId::LoveDark.as_str(), "love-dark");
    assert_eq!(ThemeId::LoveLight.as_str(), "love-light");
    assert_eq!(ThemeId::Dracula.as_str(), "dracula");
    assert_eq!(ThemeId::Nord.as_str(), "nord");
    assert_eq!(ThemeId::GruvboxDark.as_str(), "gruvbox-dark");
    assert_eq!(ThemeId::LoveDark.label(), "Love Dark");
    for id in ThemeId::ALL {
        assert_eq!(ThemeId::parse(id.as_str()), Some(id));
        assert_eq!(Theme::builtin(id).id, id);
    }
    assert_eq!(ThemeId::parse("Love Dark"), None);
    assert_eq!(Theme::TERMINAL.base_bg, ratatui::style::Color::Reset);
    assert_ne!(Theme::LOVE_DARK.base_bg, Theme::LOVE_LIGHT.base_bg);
    assert_ne!(Theme::LOVE_DARK.base_fg, Theme::LOVE_LIGHT.base_fg);
}

#[test]
fn standard_palettes_keep_canonical_anchor_colors() {
    use ratatui::style::Color::Rgb;
    assert_eq!(Theme::DRACULA.base_bg, Rgb(40, 42, 54));
    assert_eq!(Theme::DRACULA.base_fg, Rgb(248, 248, 242));
    assert_eq!(Theme::DRACULA.severity.error, Rgb(255, 85, 85));
    assert_eq!(Theme::NORD.base_bg, Rgb(46, 52, 64));
    assert_eq!(Theme::NORD.base_fg, Rgb(216, 222, 233));
    assert_eq!(Theme::NORD.accent, Rgb(136, 192, 208));
    assert_eq!(Theme::GRUVBOX_DARK.base_bg, Rgb(40, 40, 40));
    assert_eq!(Theme::GRUVBOX_DARK.base_fg, Rgb(235, 219, 178));
    assert_eq!(Theme::GRUVBOX_DARK.severity.warn, Rgb(250, 189, 47));
}

#[test]
fn dialog_and_editable_field_roles_are_distinct_and_readable() {
    for theme in [
        Theme::LOVE_DARK,
        Theme::LOVE_LIGHT,
        Theme::DRACULA,
        Theme::NORD,
        Theme::GRUVBOX_DARK,
    ] {
        assert_ne!(theme.dialog_bg, theme.base_bg, "{:?} dialog", theme.id);
        assert_ne!(theme.input_bg, theme.dialog_bg, "{:?} input", theme.id);
        assert!(
            contrast(theme.input_fg, theme.input_bg) >= 4.5,
            "{:?} input contrast was {}",
            theme.id,
            contrast(theme.input_fg, theme.input_bg)
        );
        assert_ne!(theme.focused_input_border, theme.border);
        assert!(contrast(theme.cursor, theme.input_bg) >= 3.0);
        assert!(rgb_distance(theme.dialog_bg, theme.base_bg) <= 40.0);
    }
    assert_eq!(Theme::TERMINAL.dialog_bg, ratatui::style::Color::Reset);
    assert_ne!(Theme::TERMINAL.input_bg, Theme::TERMINAL.dialog_bg);
    assert_ne!(Theme::TERMINAL.cursor, Theme::TERMINAL.input_bg);
}

fn contrast(foreground: ratatui::style::Color, background: ratatui::style::Color) -> f64 {
    let (lighter, darker) = {
        let foreground = luminance(foreground);
        let background = luminance(background);
        if foreground >= background {
            (foreground, background)
        } else {
            (background, foreground)
        }
    };
    (lighter + 0.05) / (darker + 0.05)
}

fn luminance(color: ratatui::style::Color) -> f64 {
    let ratatui::style::Color::Rgb(red, green, blue) = color else {
        panic!("contrast helper requires RGB")
    };
    [red, green, blue]
        .into_iter()
        .zip([0.2126, 0.7152, 0.0722])
        .map(|(channel, weight)| {
            let channel = f64::from(channel) / 255.0;
            let linear = if channel <= 0.04045 {
                channel / 12.92
            } else {
                ((channel + 0.055) / 1.055).powf(2.4)
            };
            linear * weight
        })
        .sum()
}

fn rgb_distance(left: ratatui::style::Color, right: ratatui::style::Color) -> f64 {
    let ratatui::style::Color::Rgb(lr, lg, lb) = left else {
        panic!("RGB")
    };
    let ratatui::style::Color::Rgb(rr, rg, rb) = right else {
        panic!("RGB")
    };
    let squared = [
        (f64::from(lr) - f64::from(rr)).powi(2),
        (f64::from(lg) - f64::from(rg)).powi(2),
        (f64::from(lb) - f64::from(rb)).powi(2),
    ];
    squared.into_iter().sum::<f64>().sqrt()
}

#[test]
fn stable_value_slot_is_identical_across_views_and_palettes() {
    let first = stable_value_slot("request-東京", 5);
    let second_view = stable_value_slot("request-東京", 5);
    assert_eq!(first, second_view);
    assert_eq!(
        Theme::LOVE_DARK.value_color("request-東京"),
        Theme::LOVE_DARK.value_color("request-東京")
    );
    assert_ne!(
        Theme::LOVE_DARK.value_color("request-東京"),
        Theme::LOVE_LIGHT.value_color("request-東京")
    );
    assert!(matches!(
        Theme::LOVE_DARK.value_color("request-東京"),
        ratatui::style::Color::Rgb(..)
    ));
    assert_eq!(stable_value_slot("anything", 0), 0);
    let colors = (0..24)
        .map(|index| Theme::LOVE_DARK.value_color(&format!("identity-{index}")))
        .collect::<std::collections::HashSet<_>>();
    assert!(
        colors.len() > 5,
        "identity colors must not repeat a finite five-color palette"
    );
    let identities = (0..256)
        .map(|index| format!("identity-{index}-東京-e\u{301}-{}", index * 7919))
        .collect::<Vec<_>>();
    for theme in [
        Theme::LOVE_DARK,
        Theme::LOVE_LIGHT,
        Theme::DRACULA,
        Theme::NORD,
        Theme::GRUVBOX_DARK,
    ] {
        for identity in &identities {
            let color = theme.value_color(identity);
            assert_eq!(color, theme.value_color(identity));
            assert!(
                contrast(color, theme.base_bg) >= MIN_IDENTITY_CONTRAST,
                "{:?} identity {identity:?} contrast was {}",
                theme.id,
                contrast(color, theme.base_bg)
            );
        }
        for (role, color) in [
            ("string", theme.json.string),
            ("number", theme.json.number),
            ("boolean", theme.json.boolean),
            ("null", theme.json.null),
            ("punctuation", theme.json.punctuation),
        ] {
            assert!(
                contrast(color, theme.base_bg) >= 3.0,
                "{:?} JSON {role} contrast was {}",
                theme.id,
                contrast(color, theme.base_bg)
            );
        }
        assert!(contrast(theme.selection_fg, theme.selection_bg) >= 3.0);
    }
}

#[test]
fn testbackend_preserves_selected_then_color_by_then_severity_precedence() {
    for theme in ThemeId::ALL.map(Theme::builtin) {
        let (provider, mut app) = demo();
        let severity = render(&provider, &mut app, theme);
        let selected = find(&severity, "fixture request 16 completed");
        assert_eq!(severity[selected].fg, theme.selection_fg);
        assert_eq!(severity[selected].bg, theme.selection_bg);
        let warning = find(&severity, "fixture request 15 completed");
        assert_eq!(severity[warning].fg, theme.severity.warn);

        app.handle(Action::OpenFieldPicker, &provider);
        app.handle(Action::ToggleColorField, &provider);
        app.handle(Action::CancelEditor, &provider);
        let colored = render(&provider, &mut app, theme);
        let selected = find(&colored, "fixture request 16 completed");
        assert_eq!(colored[selected].fg, theme.selection_fg);
        assert_eq!(colored[selected].bg, theme.selection_bg);
        let warning = find(&colored, "fixture request 15 completed");
        assert!(warning.1 > 0);
        assert_eq!(colored[warning].fg, theme.value_color("api"));
    }
}

#[test]
fn dark_and_light_fill_background_without_changing_geometry_or_hitboxes() {
    let (provider, mut terminal_app) = demo();
    let terminal = render(&provider, &mut terminal_app, Theme::TERMINAL);
    let terminal_log = terminal_app.hit_regions.log;
    let terminal_rows = terminal_app.hit_regions.log_row_indices.clone();
    let terminal_sidebar = terminal_app.hit_regions.sidebar_views.clone();

    let (_, mut dark_app) = demo();
    let dark = render(&provider, &mut dark_app, Theme::LOVE_DARK);
    assert_eq!(dark[(50, 10)].bg, Theme::LOVE_DARK.base_bg);
    assert_eq!(dark_app.hit_regions.log, terminal_log);
    assert_eq!(dark_app.hit_regions.log_row_indices, terminal_rows);
    assert_eq!(dark_app.hit_regions.sidebar_views, terminal_sidebar);

    let (_, mut light_app) = demo();
    let light = render(&provider, &mut light_app, Theme::LOVE_LIGHT);
    assert_eq!(light[(50, 10)].bg, Theme::LOVE_LIGHT.base_bg);
    assert_eq!(light_app.hit_regions.log, terminal_log);
    assert_eq!(light_app.hit_regions.log_row_indices, terminal_rows);
    assert_eq!(light_app.hit_regions.sidebar_views, terminal_sidebar);
    assert_eq!(terminal.area, dark.area);
    assert_eq!(dark.area, light.area);
}

#[test]
fn command_palette_uses_theme_selection_and_background() {
    let mut palette = Palette::new();
    palette.open(PaletteContext::new(Focus::Logs, true));
    let backend = TestBackend::new(80, 20);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| palette.render_with_theme(frame, frame.area(), Theme::LOVE_LIGHT))
        .unwrap();
    let buffer = terminal.backend().buffer();
    let selected = find(buffer, "Add source");
    assert_eq!(buffer[selected].fg, Theme::LOVE_LIGHT.selection_fg);
    assert_eq!(buffer[selected].bg, Theme::LOVE_LIGHT.selection_bg);
    assert_eq!(buffer[(1, 1)].bg, Theme::LOVE_LIGHT.base_bg);
}

#[test]
fn footer_heart_uses_each_theme_without_affecting_width() {
    for theme in ThemeId::ALL.map(Theme::builtin) {
        let backend = TestBackend::new(24, 1);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                FooterDelight::render_with_theme(
                    frame,
                    frame.area(),
                    Duration::ZERO,
                    DelightConfig::default(),
                    ActivityState::Active { label: "capturing" },
                    theme,
                )
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(0, 0)].symbol(), "♥");
        assert_eq!(buffer[(0, 0)].fg, theme.heart.primary);
        assert_eq!(buffer.area.width, 24);
    }
}
