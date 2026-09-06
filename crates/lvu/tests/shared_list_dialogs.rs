use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use lvu::{
    Action, App, Focus,
    app::{
        BookmarkDialogControl, RecipeDialogControl, RecipeDialogMode, ViewDialogControl,
        ViewDialogMode, key_to_action,
    },
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
    app.handle(Action::OpenRecipes, &provider);
    app.handle(Action::SelectRecipeMode(RecipeDialogMode::Save), &provider);
    let screen = draw(&provider, &mut app, 84, 20);
    for label in [
        "Browse",
        "Save",
        "Import",
        "Export",
        "History",
        "Update",
        "Save revision",
    ] {
        assert!(screen.contains(label), "missing {label}:\n{screen}");
    }
    app.handle(Action::RecipeInput('q'), &provider);
    assert_eq!(app.recipe_dialog.as_ref().unwrap().name, "q");
    assert_eq!(
        app.recipe_dialog.as_ref().unwrap().control,
        RecipeDialogControl::Input
    );
}

#[test]
fn view_membership_has_transactional_apply_button_and_clickable_modes() {
    let (provider, mut app) = demo();
    app.handle(Action::OpenViewDialog, &provider);
    app.handle(
        Action::SelectViewDialogMode(ViewDialogMode::Sources),
        &provider,
    );
    let screen = draw(&provider, &mut app, 94, 22);
    assert!(screen.contains("Apply membership"));
    assert!(screen.contains("captures are shared"));
    let clone = app
        .hit_regions
        .view_dialog_controls
        .iter()
        .find(|(_, control)| *control == ViewDialogControl::Mode(ViewDialogMode::Clone))
        .unwrap()
        .0;
    app.handle(click(clone), &provider);
    assert_eq!(
        app.view_dialog.as_ref().unwrap().mode,
        ViewDialogMode::Clone
    );
    assert_eq!(
        app.view_dialog.as_ref().unwrap().control,
        ViewDialogControl::Input
    );
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
    app.handle(Action::OpenRecipes, &provider);
    app.handle(Action::SelectRecipeMode(RecipeDialogMode::Save), &provider);
    press(&mut app, &provider, KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(
        app.recipe_dialog.as_ref().unwrap().control,
        RecipeDialogControl::Apply
    );
    press(&mut app, &provider, KeyCode::BackTab, KeyModifiers::SHIFT);
    assert_eq!(
        app.recipe_dialog.as_ref().unwrap().control,
        RecipeDialogControl::Input
    );
    draw(&provider, &mut app, 84, 20);

    app.handle(Action::CancelEditor, &provider);
    app.handle(Action::OpenViewDialog, &provider);
    press(&mut app, &provider, KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(
        app.view_dialog.as_ref().unwrap().control,
        ViewDialogControl::Apply
    );
    press(&mut app, &provider, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(app.take_view_requests().len(), 1);

    app.handle(Action::CancelEditor, &provider);
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
                ui::render_bookmarks(frame, &mut app, area, lvu::theme::Theme::TERMINAL);
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
