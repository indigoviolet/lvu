use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use lvu::{
    Action, App, RowProvider, StorageCategory, StorageEntry, StorageSnapshot,
    component::{Open, RawEvent},
    dialog_controls::{ButtonRole, DialogStyles, role_style},
    fixture::FixtureProvider,
    theme::Theme,
    ui,
};

fn raw_key(code: KeyCode) -> Action {
    Action::Raw(RawEvent::Key(KeyEvent::new(code, KeyModifiers::NONE)))
}
use ratatui::{
    Terminal,
    backend::TestBackend,
    buffer::Buffer,
    style::{Modifier, Style},
};
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
    app.handle(Action::Open(Open::Fields), &provider);

    let fields = draw(&provider, &mut app, 88, 24, theme);
    // §11 retired the key footer; the affordances are buttons and a help row,
    // and both still use the shared roles.
    // §7.3 headings are the label role in bold; `Field` also appears in the
    // title, so anchor on the other column heading.
    assert_role(
        fields[find(&fields, "Value")].style(),
        styles.label.add_modifier(Modifier::BOLD),
    );
    assert_role(
        fields[find(&fields, "Pinned fields become")].style(),
        styles.description,
    );
    let selected = app.layers.fields.row_rects()[0].0;
    assert_role(fields[(selected.x, selected.y)].style(), styles.selection);
    assert_eq!(
        Some(fields[(selected.x, selected.y)].bg),
        styles.selection.bg
    );
    // The rows stay above the action row that acts on them.
    let actions = app
        .layers
        .fields
        .control_rects()
        .first()
        .expect("the Fields dialog draws its actions")
        .0;
    assert!(
        app.layers
            .fields
            .row_rects()
            .iter()
            .all(|(row, _)| row.bottom() <= actions.y),
        "a field row overlapped the action row"
    );

    app.handle(raw_key(KeyCode::Esc), &provider);
    app.handle(Action::ToggleDetails, &provider);
    let details = draw(&provider, &mut app, 72, 20, theme);
    assert_role(
        details[find(&details, "stable display id:")].style(),
        styles.label,
    );
    // §8.10: the pane's last row is content, never a key footer.
    assert!(!screen(&details).contains("↑/↓"), "{}", screen(&details));
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
    // §11 retired the key list: `g` is still the accelerator but the dialog
    // presents the action as a button instead of printing the binding.
    let rendered = screen(&context);
    assert!(rendered.contains("Anchor:"), "{rendered}");
    assert!(rendered.contains("[ Back to anchor ]"), "{rendered}");
    assert!(!rendered.contains("g anchor"), "{rendered}");
    assert!(rendered.contains("raw, unfiltered"), "{rendered}");
    assert_role(
        context[find(&context, "Anchor:")].style(),
        styles.description,
    );

    app.handle(Action::CancelEditor, &provider);
    app.handle(Action::Open(Open::Help), &provider);
    for theme in [Theme::LOVE_DARK, Theme::LOVE_LIGHT] {
        let styles = DialogStyles::new(theme);
        for (width, height) in [(54, 12), (120, 30)] {
            let help = draw(&provider, &mut app, width, height, theme);
            let shortcut = &help[find(&help, "Ctrl-P")];
            let description = &help[find(&help, "Command palette")];
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
            // §8.10: Help states the routine keys once, in CONVENTIONS, and
            // never as the old `Enter apply` / `Esc close` reminders.
            for obsolete in [
                "PgUp",
                "PgDn",
                "Home/End",
                "Enter apply",
                "Enter activate",
                "Esc close",
                "Tab next",
            ] {
                assert!(
                    !rendered.contains(obsolete),
                    "obsolete Help reminder {obsolete:?} in:\n{rendered}"
                );
            }
            assert_eq!(
                rendered.matches("CONVENTIONS").count(),
                1,
                "the conventions are stated once:\n{rendered}"
            );
        }
    }
}

#[test]
fn storage_has_real_overflow_and_shared_status_and_selection_roles() {
    let theme = Theme::LOVE_LIGHT;
    let styles = DialogStyles::new(theme);
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::Storage), &provider);
    let generation = app.layers.storage.outbox.take()[0].generation;
    let entries = (0..8)
        .map(|index| StorageEntry {
            category: StorageCategory::Derived,
            label: format!("derived-{index}"),
            bytes: 1024,
            reclaimable: 1024,
            status: "unused, recomputable".into(),
        })
        .collect();
    assert!(app.layers.storage.complete(
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
    let selected = app.layers.storage.row_rects()[0].0;
    assert_role(storage[(selected.x, selected.y)].style(), styles.selection);
    assert_eq!(
        Some(storage[(selected.x, selected.y)].bg),
        styles.selection.bg
    );
    // §3 replaces the key-reminder footer with an action row; §8.9 makes
    // `Refresh` the default, so it carries the accent *fill* that marks the
    // one button Enter presses.
    let refresh = storage[find(&storage, "[ Refresh ]")].style();
    assert_role(refresh, role_style(theme, ButtonRole::Default, false));
    assert_eq!(refresh.bg, Some(theme.accent));
    let limit = app.layers.storage.scroll_limit();
    assert!(limit > 0, "long diagnostic must really overflow");
    // Tab hands the arrows to the diagnostics pane; the component owns both
    // the offset and the keymap that moves it.
    app.handle(raw_key(KeyCode::Tab), &provider);
    for _ in 0..limit {
        app.handle(raw_key(KeyCode::Down), &provider);
    }
    assert_eq!(app.layers.storage.scroll(), limit);
    let scrolled = draw(&provider, &mut app, 72, 16, theme);
    assert_role(
        scrolled[find(&scrolled, "diagnostic")].style(),
        styles.error,
    );
}
