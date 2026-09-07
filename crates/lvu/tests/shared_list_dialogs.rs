use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use lvu::{
    Action, App, Focus,
    app::{
        BookmarkDialogControl, RecipeDialogControl, RecipeDialogMode, ViewDialogMode, key_to_action,
    },
    component::{Open, RawEvent},
    components::view::ViewDialogControl,
    fixture::FixtureProvider,
    ui,
};
use ratatui::{Terminal, backend::TestBackend};

fn demo() -> (FixtureProvider, App) {
    let (provider, sources, views) = FixtureProvider::demo();
    (provider, App::new(sources, views, true))
}

fn draw(provider: &FixtureProvider, app: &mut App, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| ui::render(frame, app, provider))
        .unwrap();
    let buffer = terminal.backend().buffer();
    (0..buffer.area.height)
        .map(|y| {
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn click(rect: ratatui::layout::Rect) -> Action {
    Action::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: rect.x,
        row: rect.y,
        modifiers: KeyModifiers::NONE,
    })
}

/// A converted layer owns its keymap, so its input arrives raw (§6.4).
fn raw_click(rect: ratatui::layout::Rect) -> Action {
    Action::Raw(RawEvent::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: rect.x,
        row: rect.y,
        modifiers: KeyModifiers::NONE,
    }))
}

fn raw_press(
    app: &mut App,
    provider: &FixtureProvider,
    code: crossterm::event::KeyCode,
    modifiers: KeyModifiers,
) {
    app.handle(Action::Raw(RawEvent::Key(key(code, modifiers))), provider);
}

fn raw_key(code: KeyCode) -> Action {
    Action::Raw(RawEvent::Key(KeyEvent::new(code, KeyModifiers::NONE)))
}

fn raw_alt(code: KeyCode) -> Action {
    Action::Raw(RawEvent::Key(KeyEvent::new(code, KeyModifiers::ALT)))
}

/// Recipes owns its keymap now, so a test reaches a control the way a user
/// does: Tab until it has focus, then Enter.
fn recipe_activate(app: &mut App, provider: &FixtureProvider, control: RecipeDialogControl) {
    for _ in 0..64 {
        if app.layers.recipes.state().control == control {
            app.handle(raw_key(KeyCode::Enter), provider);
            return;
        }
        app.handle(raw_key(KeyCode::Tab), provider);
    }
    panic!("{control:?} never took focus");
}

fn key(code: crossterm::event::KeyCode, modifiers: KeyModifiers) -> crossterm::event::KeyEvent {
    crossterm::event::KeyEvent::new(code, modifiers)
}

fn press(
    app: &mut App,
    provider: &FixtureProvider,
    code: crossterm::event::KeyCode,
    modifiers: KeyModifiers,
) {
    app.handle(key_to_action(key(code, modifiers), app.focus), provider);
}

#[test]
fn recipes_expose_all_modes_and_keep_q_literal_in_the_focused_input() {
    let (provider, mut app) = demo();
    app.handle(
        Action::Open(Open::Recipes {
            mode: RecipeDialogMode::Browse,
        }),
        &provider,
    );
    // §12.9 retired the row of mode buttons. Every mode is still reachable, so
    // this checks reachability rather than the shape the modes used to take.
    let browse = draw(&provider, &mut app, 84, 20);
    for label in ["Saved recipes", "Save", "Update", "History", "More"] {
        assert!(browse.contains(label), "missing {label}:\n{browse}");
    }
    // Import and Export moved behind the one menu, and are reachable there.
    recipe_activate(&mut app, &provider, RecipeDialogControl::More);
    let menu = draw(&provider, &mut app, 84, 20);
    for label in ["Import", "Export", "Refresh"] {
        assert!(menu.contains(label), "missing {label}:\n{menu}");
    }
    app.handle(raw_key(KeyCode::Esc), &provider);

    app.handle(raw_alt(KeyCode::Char('s')), &provider);
    let saving = draw(&provider, &mut app, 84, 20);
    assert!(saving.contains("Save revision"), "{saving}");
    app.handle(raw_key(KeyCode::Char('q')), &provider);
    assert_eq!(app.layers.recipes.state().name, "q");
    assert_eq!(
        app.layers.recipes.state().control,
        RecipeDialogControl::Input
    );
}

/// The `More ▾` menu is a popup: it owns the arrows and Enter while it is open,
/// and Escape closes it rather than the dialog behind it (§10).
#[test]
fn the_recipe_more_menu_takes_the_keys_while_it_is_open() {
    let (provider, mut app) = demo();
    app.handle(
        Action::Open(Open::Recipes {
            mode: RecipeDialogMode::Browse,
        }),
        &provider,
    );
    draw(&provider, &mut app, 84, 20);
    recipe_activate(&mut app, &provider, RecipeDialogControl::More);
    app.handle(raw_key(KeyCode::Down), &provider);
    assert_eq!(app.layers.recipes.state().menu_selected, 1);
    app.handle(raw_key(KeyCode::Esc), &provider);
    assert!(
        !app.layers.recipes.state().menu_open,
        "Escape closed the menu, not the layer"
    );
    assert!(app.layers.recipes.is_open(), "the layer stays open");
    app.handle(raw_key(KeyCode::Esc), &provider);
    assert!(
        !app.layers.recipes.is_open(),
        "a second Escape closes the layer"
    );
}

#[test]
fn view_membership_has_transactional_apply_button_and_clickable_modes() {
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::View), &provider);
    raw_press(
        &mut app,
        &provider,
        crossterm::event::KeyCode::Char('m'),
        KeyModifiers::ALT,
    );
    let screen = draw(&provider, &mut app, 94, 22);
    assert!(screen.contains("Apply membership"));
    // dialog-system.md §7.4 replaced the free-form status line with the shared
    // message row; the promise it makes is unchanged.
    assert!(
        screen.contains("changing membership keeps every capture"),
        "{screen}"
    );
    let clone = app
        .layers
        .view
        .control_rects()
        .iter()
        .find(|(_, control)| *control == ViewDialogControl::Mode(ViewDialogMode::Clone))
        .unwrap()
        .0;
    app.handle(raw_click(clone), &provider);
    assert_eq!(app.layers.view.mode(), ViewDialogMode::Clone);
    assert_eq!(app.layers.view.control(), ViewDialogControl::Input);
}

#[test]
fn bookmark_note_buttons_preserve_the_fixed_record_identity() {
    let (provider, mut app) = demo();
    draw(&provider, &mut app, 100, 22);
    app.handle(Action::ToggleBookmark, &provider);
    app.handle(Action::OpenBookmarks, &provider);
    draw(&provider, &mut app, 100, 22);
    let edit = app
        .hit_regions
        .bookmark_controls
        .iter()
        .find(|(_, control)| *control == BookmarkDialogControl::Edit)
        .unwrap()
        .0;
    let id = app.bookmarks_for_view(app.active_view_id().unwrap())[0]
        .id
        .clone();
    app.handle(click(edit), &provider);
    assert_eq!(app.focus, Focus::Bookmarks);
    assert_eq!(
        app.bookmark_dialog.as_ref().unwrap().editing.as_ref(),
        Some(&id)
    );
    app.handle(Action::BookmarkInput('q'), &provider);
    assert_eq!(app.bookmark_dialog.as_ref().unwrap().draft, "q");
    let screen = draw(&provider, &mut app, 52, 12);
    assert!(screen.contains("Save note"));
}

#[test]
fn actual_tab_and_enter_keys_route_through_each_dialog_control_model() {
    use crossterm::event::KeyCode;

    let (provider, mut app) = demo();
    app.handle(
        Action::Open(Open::Recipes {
            mode: RecipeDialogMode::Browse,
        }),
        &provider,
    );
    app.handle(raw_alt(KeyCode::Char('s')), &provider);
    // A converted layer takes the key itself, so the test sends the key rather
    // than the `Action` the base table used to produce for it.
    app.handle(raw_key(KeyCode::Tab), &provider);
    assert_eq!(
        app.layers.recipes.state().control,
        RecipeDialogControl::Apply
    );
    app.handle(
        Action::Raw(RawEvent::Key(KeyEvent::new(
            KeyCode::BackTab,
            KeyModifiers::SHIFT,
        ))),
        &provider,
    );
    assert_eq!(
        app.layers.recipes.state().control,
        RecipeDialogControl::Input
    );
    draw(&provider, &mut app, 84, 20);

    app.handle(raw_key(KeyCode::Esc), &provider);
    app.handle(Action::Open(Open::View), &provider);
    raw_press(&mut app, &provider, KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(app.layers.view.control(), ViewDialogControl::Apply);
    raw_press(&mut app, &provider, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(app.layers.view.outbox.take().len(), 1);

    raw_press(&mut app, &provider, KeyCode::Esc, KeyModifiers::NONE);
    draw(&provider, &mut app, 100, 22);
    app.handle(Action::ToggleBookmark, &provider);
    app.handle(Action::OpenBookmarks, &provider);
    press(&mut app, &provider, KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(
        app.bookmark_dialog.as_ref().unwrap().control,
        BookmarkDialogControl::Context
    );
    draw(&provider, &mut app, 100, 22);
    press(&mut app, &provider, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(app.focus, Focus::Context);
}

#[test]
fn bookmark_geometry_is_safe_and_emits_no_invalid_tiny_hitboxes() {
    let (provider, mut app) = demo();
    draw(&provider, &mut app, 100, 22);
    app.handle(Action::ToggleBookmark, &provider);
    app.handle(Action::OpenBookmarks, &provider);

    for (width, height) in [(0, 0), (1, 1), (2, 1)] {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| {
                let area = frame.area();
                ui::render_bookmarks(
                    frame,
                    &mut app,
                    &provider,
                    area,
                    lvu::theme::Theme::TERMINAL,
                );
            })
            .unwrap();
        let bounds = ratatui::layout::Rect::new(0, 0, width, height);
        for rect in app
            .hit_regions
            .bookmark_rows
            .iter()
            .map(|(rect, _)| rect)
            .chain(
                app.hit_regions
                    .bookmark_controls
                    .iter()
                    .map(|(rect, _)| rect),
            )
        {
            assert!(
                !rect.is_empty(),
                "{width}x{height} emitted empty hitbox {rect:?}"
            );
            assert!(
                rect.x >= bounds.x
                    && rect.y >= bounds.y
                    && rect.right() <= bounds.right()
                    && rect.bottom() <= bounds.bottom(),
                "{width}x{height} emitted out-of-bounds hitbox {rect:?}"
            );
        }
    }

    // The ordinary renderer intentionally takes its tiny fallback, and must also
    // clear every stale modal hitbox without reaching the direct dialog seam.
    for (width, height) in [(0, 0), (1, 1), (2, 1)] {
        draw(&provider, &mut app, width, height);
        assert!(app.hit_regions.bookmark_rows.is_empty());
        assert!(app.hit_regions.bookmark_controls.is_empty());
    }
}
