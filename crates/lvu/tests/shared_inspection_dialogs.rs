use lvu::{
    Action, App, RowProvider, StorageCategory, StorageEntry, StorageSnapshot,
    dialog_controls::DialogStyles, fixture::FixtureProvider, theme::Theme, ui,
};
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer, style::Style};
use unicode_width::UnicodeWidthStr;

fn draw<P: RowProvider>(
    provider: &P,
    app: &mut App,
    width: u16,
    height: u16,
    theme: Theme,
) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| ui::render_with_theme(frame, app, provider, theme, None))
        .unwrap();
    terminal.backend().buffer().clone()
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

fn find(buffer: &Buffer, needle: &str) -> (u16, u16) {
    (0..buffer.area.height)
        .find_map(|y| {
            let line = (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>();
            line.find(needle).map(|byte| {
                let column = UnicodeWidthStr::width(&line[..byte]);
                (column as u16, y)
            })
        })
        .unwrap_or_else(|| panic!("missing {needle:?} in:\n{}", screen(buffer)))
}

fn assert_role(actual: Style, expected: Style) {
    assert_eq!(actual.fg, expected.fg);
    assert_eq!(actual.add_modifier, expected.add_modifier);
}

fn demo() -> (FixtureProvider, App) {
    let (provider, sources, views) = FixtureProvider::demo();
    (provider, App::new(sources, views, true))
}

#[test]
fn fields_and_details_use_shared_readable_selection_and_action_roles() {
    let theme = Theme::LOVE_LIGHT;
    let styles = DialogStyles::new(theme);
    let (provider, mut app) = demo();
    app.sync_provider(&provider, 10);
    app.handle(Action::OpenFieldPicker, &provider);

    let fields = draw(&provider, &mut app, 88, 24, theme);
    assert_role(fields[find(&fields, "Space")].style(), styles.shortcut);
    assert_role(fields[find(&fields, "pin")].style(), styles.description);
    let selected = app.hit_regions.field_picker_rows[0].0;
    assert_role(fields[(selected.x, selected.y)].style(), styles.selection);
    assert_eq!(
        Some(fields[(selected.x, selected.y)].bg),
        styles.selection.bg
    );
    assert!(
        app.hit_regions
            .field_picker_rows
            .iter()
            .all(|(row, _)| row.bottom() < find(&fields, "Space").1 + 1)
    );

    app.handle(Action::CancelEditor, &provider);
    app.handle(Action::ToggleDetails, &provider);
    let details = draw(&provider, &mut app, 72, 20, theme);
    assert_role(
        details[find(&details, "stable display id:")].style(),
        styles.label,
    );
    assert_role(details[find(&details, "↑/↓")].style(), styles.shortcut);
    assert_role(
        details[find(&details, "scroll")].style(),
        styles.description,
    );
}

#[test]
fn context_keeps_its_raw_anchor_and_help_reflows_with_shared_roles() {
    let (provider, mut app) = demo();
    app.sync_provider(&provider, 8);
    let anchor = app.view_state().unwrap().selected.clone().unwrap();
    app.handle(Action::OpenContext, &provider);
    app.handle(Action::MoveContext(4), &provider);
    let theme = Theme::LOVE_DARK;
    let styles = DialogStyles::new(theme);
    let context = draw(&provider, &mut app, 58, 12, theme);
    assert_eq!(app.context_dialog.as_ref().unwrap().anchor, anchor);
    assert_role(context[find(&context, "Anchor:")].style(), styles.label);
    assert_role(context[find(&context, "g anchor")].style(), styles.shortcut);
    assert!(screen(&context).contains("raw, unfiltered"));

    app.handle(Action::CancelEditor, &provider);
    app.handle(Action::ToggleHelp, &provider);
    for theme in [Theme::LOVE_DARK, Theme::LOVE_LIGHT] {
        let styles = DialogStyles::new(theme);
        for (width, height) in [(54, 12), (120, 30)] {
            let help = draw(&provider, &mut app, width, height, theme);
            let shortcut = &help[find(&help, "Ctrl-P")];
            let description = &help[find(
                &help,
                if width == 54 {
                    "Open the command"
                } else {
                    "Open the command palette"
                },
            )];
            let heading = &help[find(&help, "EVERYWHERE")];
            assert_role(shortcut.style(), styles.shortcut);
            assert_role(description.style(), styles.description);
            assert_role(
                heading.style(),
                styles.label.add_modifier(ratatui::style::Modifier::BOLD),
            );
            assert_eq!(shortcut.bg, theme.dialog_bg);
            assert_eq!(description.bg, theme.dialog_bg);
            assert_eq!(heading.bg, theme.dialog_bg);
            let rendered = screen(&help);
            for obsolete in ["PgUp", "PgDn", "Home", "End", "Enter", "Tab", "Esc"] {
                assert!(
                    !rendered.contains(obsolete),
                    "obsolete Help reminder {obsolete:?} in:\n{rendered}"
                );
            }
            app.help_scroll = 0;
        }
    }
}

#[test]
fn storage_has_real_overflow_and_shared_status_and_selection_roles() {
    let theme = Theme::LOVE_LIGHT;
    let styles = DialogStyles::new(theme);
    let (provider, mut app) = demo();
    app.handle(Action::OpenStorage, &provider);
    let generation = app.take_storage_requests()[0].generation;
    let entries = (0..8)
        .map(|index| StorageEntry {
            category: StorageCategory::Derived,
            label: format!("derived-{index}"),
            bytes: 1024,
            reclaimable: 1024,
            status: "unused, recomputable".into(),
        })
        .collect();
    assert!(app.update_storage(
        generation,
        StorageSnapshot {
            entries,
            total_bytes: 8192,
            reclaimable_bytes: 8192,
            row_cache_bytes: 1,
            row_cache_limit: 2,
            query_index_bytes: 3,
            query_index_limit: 4,
            derived_index_limit_per_source: 5,
            derived_index_limit_total: 6,
            truncated: false,
            errors: vec![("bounded storage diagnostic ".repeat(16))],
        },
        "scan complete".into(),
        true,
    ));
    let storage = draw(&provider, &mut app, 72, 16, theme);
    let selected = app.hit_regions.storage_rows[0].0;
    assert_role(storage[(selected.x, selected.y)].style(), styles.selection);
    assert_eq!(
        Some(storage[(selected.x, selected.y)].bg),
        styles.selection.bg
    );
    assert_role(
        storage[find(&storage, "r refresh")].style(),
        styles.shortcut,
    );
    assert!(
        app.dialog_scroll_limit > 0,
        "long diagnostic must really overflow"
    );
    app.dialog_scroll = app.dialog_scroll_limit;
    let scrolled = draw(&provider, &mut app, 72, 16, theme);
    assert_role(
        scrolled[find(&scrolled, "diagnostic")].style(),
        styles.error,
    );
}
