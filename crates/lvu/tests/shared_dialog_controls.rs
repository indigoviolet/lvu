use lvu::{
    dialog_controls::{
        DialogStyles, button_layout, button_style, button_text, button_width, render_button,
    },
    theme::{Theme, ThemeId},
};
use ratatui::{Terminal, backend::TestBackend, layout::Rect, style::Color};

fn luminance(color: Color) -> Option<f64> {
    let Color::Rgb(red, green, blue) = color else {
        return None;
    };
    Some(
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
            .sum(),
    )
}

fn contrast(foreground: Color, background: Color) -> Option<f64> {
    let (left, right) = (luminance(foreground)?, luminance(background)?);
    let (lighter, darker) = if left > right {
        (left, right)
    } else {
        (right, left)
    };
    Some((lighter + 0.05) / (darker + 0.05))
}

#[test]
fn concrete_theme_roles_are_readable_on_their_actual_backgrounds() {
    for theme in ThemeId::ALL.map(Theme::builtin) {
        let styles = DialogStyles::new(theme);
        if theme.id == ThemeId::Terminal {
            continue;
        }
        for (name, style, background) in [
            ("label", styles.label, theme.dialog_bg),
            ("description", styles.description, theme.dialog_bg),
            ("shortcut", styles.shortcut, theme.dialog_bg),
            ("input", styles.input, theme.input_bg),
            ("selection", styles.selection, theme.selection_bg),
            ("applied", styles.applied, theme.dialog_bg),
            ("pending", styles.pending, theme.dialog_bg),
            ("error", styles.error, theme.dialog_bg),
            ("unavailable", styles.unavailable, theme.dialog_bg),
        ] {
            let ratio = contrast(style.fg.unwrap(), background).unwrap();
            assert!(
                ratio >= 4.5,
                "{name} in {:?} has contrast {ratio}",
                theme.id
            );
        }
    }
}

#[test]
fn button_geometry_wraps_whole_controls_and_reveals_hidden_focus() {
    assert_eq!(button_text("Run"), "[ Run ]");
    assert_eq!(button_width("東京"), 8);
    let labels = ["One", "Two", "Three", "東京東京東京"];
    let normal = button_layout(Rect::new(4, 7, 12, 2), &labels, None);
    assert_eq!(normal[0], (0, Rect::new(4, 7, 7, 1)));
    assert_eq!(normal[1], (1, Rect::new(4, 8, 7, 1)));
    assert!(!normal.iter().any(|(index, _)| *index == 3));

    let focused = button_layout(Rect::new(4, 7, 12, 2), &labels, Some(3));
    assert_eq!(focused[0], normal[0]);
    assert_eq!(focused.last(), Some(&(3, Rect::new(4, 8, 12, 1))));
}

#[test]
fn button_rendering_is_clipped_to_its_hitbox_and_states_differ() {
    let theme = Theme::LOVE_LIGHT;
    assert_ne!(
        button_style(theme, true, false),
        button_style(theme, false, true)
    );
    assert_ne!(
        button_style(theme, false, true),
        button_style(theme, false, false)
    );

    let backend = TestBackend::new(12, 2);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| render_button(frame, Rect::new(2, 0, 4, 1), "東京東京", true, false, theme))
        .unwrap();
    let buffer = terminal.backend().buffer();
    assert_eq!(buffer[(1, 0)].symbol(), " ");
    assert_eq!(buffer[(6, 0)].symbol(), " ");
    assert_eq!(buffer[(2, 0)].bg, theme.selection_bg);
}
