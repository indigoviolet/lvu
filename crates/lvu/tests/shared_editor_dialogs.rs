use lvu::{
    Action, App, Focus,
    app::{CommandEnrichmentControl, EnrichmentControl},
    dialog_controls::DialogStyles,
    fixture::FixtureProvider,
    theme::Theme,
    ui,
};
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer, layout::Position};

fn demo() -> (FixtureProvider, App) {
    let (provider, sources, views) = FixtureProvider::demo();
    (provider, App::new(sources, views, true))
}

fn screen(buffer: &Buffer) -> String {
    (0..buffer.area.height)
        .map(|y| {
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn render(provider: &FixtureProvider, app: &mut App, width: u16, height: u16) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| ui::render_with_theme(frame, app, provider, Theme::LOVE_DARK, None))
        .unwrap();
    terminal.backend().buffer().clone()
}

fn find(buffer: &Buffer, needle: &str) -> Position {
    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width {
            let suffix = (x..buffer.area.width)
                .map(|column| buffer[(column, y)].symbol())
                .collect::<String>();
            if suffix.starts_with(needle) {
                return Position::new(x, y);
            }
        }
    }
    panic!("missing {needle:?}\n{}", screen(buffer));
}

#[test]
fn enrichment_layers_use_shared_roles_and_one_bounded_geometry() {
    for theme in [Theme::LOVE_DARK, Theme::LOVE_LIGHT] {
        let (provider, mut app) = demo();
        app.handle(Action::OpenEnrichment, &provider);
        let styles = DialogStyles::new(theme);

        // Layer one shows steps and actions only: no expression input.
        let mut terminal = Terminal::new(TestBackend::new(52, 15)).unwrap();
        terminal
            .draw(|frame| ui::render_with_theme(frame, &mut app, &provider, theme, None))
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        let list = screen(&buffer);
        assert!(list.contains("Enrichment"), "{list}");
        assert!(list.contains("Steps"), "{list}");
        assert!(list.contains("[ Add ]"), "{list}");
        assert!(!list.contains("Expression"), "{list}");
        // One message row, one state vocabulary, no boxed status heading.
        assert!(list.contains("Ready"), "{list}");
        assert!(!list.contains("Status"), "{list}");
        for banned in ["Enter", "Tab", "Esc", "PgUp", "PgDn", "↑/↓ scroll"] {
            assert!(!list.contains(banned), "{banned} in {list}");
        }
        let applied = find(&buffer, "Ready");
        assert_eq!(buffer[applied].fg, styles.applied.fg.unwrap());
        assert_eq!(buffer[applied].bg, theme.dialog_bg);
        let modal = app.hit_regions.selection_modal.unwrap();
        assert!(!app.hit_regions.enrichment_controls.is_empty());
        for (rect, _) in &app.hit_regions.enrichment_controls {
            assert!(modal.contains(Position::new(rect.x, rect.y)));
            assert!(rect.right() <= modal.right());
            assert!(rect.bottom() <= modal.bottom());
        }

        // Layer two owns the editable expression and its visible cursor.
        app.handle(Action::AddEnrichment, &provider);
        assert_eq!(app.focus, Focus::EnrichmentStep);
        app.handle(Action::EditorPaste("界e\u{301}".into()), &provider);
        terminal
            .draw(|frame| ui::render_with_theme(frame, &mut app, &provider, theme, None))
            .unwrap();
        let cursor = terminal.backend().cursor_position();
        let buffer = terminal.backend().buffer().clone();
        let step = screen(&buffer);
        assert!(step.contains("Enrichment › New step"), "{step}");
        assert!(step.contains("Expression"), "{step}");
        assert!(step.contains("[ Save ]"), "{step}");
        // §7.5: Escape closes; there is no Cancel button anywhere.
        assert!(!step.contains("Cancel"), "{step}");
        assert!(step.contains("Input record"), "{step}");
        assert!(step.contains("Accepted output"), "{step}");
        for banned in ["Enter", "Tab", "Esc", "PgUp", "PgDn", "↑/↓ scroll"] {
            assert!(!step.contains(banned), "{banned} in {step}");
        }

        let status = find(&buffer, "Ready");
        assert_eq!(buffer[status].fg, styles.applied.fg.unwrap());
        assert_eq!(buffer[status].bg, theme.dialog_bg);
        let label = find(&buffer, "Input record");
        assert_eq!(buffer[label].fg, styles.label.fg.unwrap());
        assert_eq!(buffer[label].bg, theme.dialog_bg);
        let input = find(&buffer, "界");
        assert_eq!(buffer[input].fg, styles.input.fg.unwrap());
        assert_eq!(buffer[input].bg, theme.input_bg);
        assert_eq!(buffer[cursor].bg, theme.cursor);

        let modal = app.hit_regions.selection_modal.unwrap();
        assert!(modal.contains(cursor));
        assert!(app.hit_regions.enrichment_controls.is_empty());
        assert!(!app.hit_regions.enrichment_step_controls.is_empty());
        for (rect, _) in &app.hit_regions.enrichment_step_controls {
            assert!(modal.contains(Position::new(rect.x, rect.y)));
            assert!(rect.right() <= modal.right());
            assert!(rect.bottom() <= modal.bottom());
        }
    }
}

#[test]
fn enrichment_buttons_share_stable_bounded_geometry_and_hitboxes() {
    let (provider, mut app) = demo();
    app.handle(Action::OpenEnrichment, &provider);
    for _ in 0..4 {
        app.handle(Action::MoveEnrichmentControl(1), &provider);
    }
    let buffer = render(&provider, &mut app, 38, 18);
    let output = screen(&buffer);
    assert!(output.contains("[ External c"), "{output}");
    assert!(
        app.hit_regions
            .enrichment_controls
            .iter()
            .any(|(_, control)| *control == EnrichmentControl::ExternalCommand)
    );
    let modal = app.hit_regions.selection_modal.unwrap();
    for (rect, _) in &app.hit_regions.enrichment_controls {
        assert!(modal.contains(Position::new(rect.x, rect.y)));
        assert!(rect.right() <= modal.right());
        assert!(rect.bottom() <= modal.bottom());
    }
}

#[test]
fn command_buttons_keep_stable_order_and_semantic_status_without_fake_scroll() {
    let (provider, mut app) = demo();
    app.handle(Action::OpenCommandEnrichment, &provider);
    app.handle(
        Action::FocusCommandEnrichmentControl(CommandEnrichmentControl::Review),
        &provider,
    );
    let buffer = render(&provider, &mut app, 100, 30);
    let output = screen(&buffer);
    let new_line = output.find("[ New line (Alt-N) ]").unwrap();
    let save = output.find("[ Save ]").unwrap();
    let review = output.find("[ Review ]").unwrap();
    let remove = output.find("[ Remove ]").unwrap();
    assert!(
        new_line < save && save < review && review < remove,
        "{output}"
    );
    assert!(output.contains("Applied command step:"), "{output}");
    assert!(
        !output.contains("Status and review · ↑/↓ scroll"),
        "{output}"
    );
    assert!(app.hit_regions.dialog_scroll.is_none());
}

#[test]
fn grouping_uses_input_only_background_and_explicit_applied_state() {
    let (provider, mut app) = demo();
    app.handle(Action::OpenGrouping, &provider);
    assert_eq!(app.focus, Focus::GroupingEditor);
    let buffer = render(&provider, &mut app, 80, 18);
    let output = screen(&buffer);
    // dialog-system.md §7.4 replaces the `Applied:` vocabulary with the shared
    // message row, and §3 gives the dialog the action row it never had.
    assert!(output.contains("Disabled"), "{output}");
    assert!(
        output.contains("an empty draft turns grouping off"),
        "{output}"
    );
    assert!(output.contains("[ Apply ]"), "{output}");
    assert!(!output.contains("Applied:"), "{output}");
    assert!(!output.contains("Enter Apply"), "{output}");
    assert!(!output.contains("↑/↓ Scroll status"), "{output}");
    assert!(app.hit_regions.dialog_scroll.is_none());

    let input_y = output
        .lines()
        .position(|line| line.contains(r"^(\s+|Caused by:)"))
        .unwrap() as u16;
    let input_x = output
        .lines()
        .nth(input_y as usize)
        .unwrap()
        .find('^')
        .unwrap() as u16;
    assert_eq!(buffer[(input_x, input_y)].bg, Theme::LOVE_DARK.input_bg);
    assert_eq!(
        buffer[(input_x, input_y + 1)].bg,
        Theme::LOVE_DARK.dialog_bg
    );
}

#[test]
fn enrichment_layers_use_the_class_l_rect_and_child_layering_rules() {
    // docs/dialog-system.md §5.3 width table for class L, and §10 layering.
    for (width, height, expected_width, max_height) in [
        (140u16, 40u16, 120u16, 38u16),
        (100, 30, 86, 28),
        (80, 24, 72, 22),
        (54, 16, 52, 16),
    ] {
        let (provider, mut app) = demo();
        app.handle(Action::OpenEnrichment, &provider);
        render(&provider, &mut app, width, height);
        let list = app.hit_regions.selection_modal.unwrap();
        let popup_width = list.width + 2;
        assert_eq!(
            popup_width, expected_width,
            "{width}x{height} class L width"
        );
        assert!(
            list.height + 2 <= max_height,
            "{width}x{height} exceeds the class maximum"
        );

        app.handle(Action::AddEnrichment, &provider);
        render(&provider, &mut app, width, height);
        let child = app.hit_regions.selection_modal.unwrap();
        assert!(
            child.height + 2 <= max_height,
            "{width}x{height} child height"
        );
        if width >= 64 && height >= 20 {
            assert!(
                child.width + 2 <= popup_width - 4,
                "{width}x{height} child must stay inside its parent"
            );
        } else {
            assert_eq!(
                child.width + 2,
                popup_width,
                "compact child replaces parent"
            );
        }
    }
}
