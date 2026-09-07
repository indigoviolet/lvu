//! Acceptance for the Help layer as a component (docs/component-model.md §6.3
//! step 6). Help draws no controls, so what is asserted here is the plumbing
//! the conversion replaced: the scroll bound the render recorded is the bound
//! the keymap clamps against, the shell's selection bound is the surface the
//! component published, dismissal restores the focus that was underneath
//! without a `help_return_focus` field, and a mouse event outside the popup is
//! contained instead of reaching the log.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use lvu::{
    Action, App, RowProvider,
    app::Focus,
    component::{Component, LayerId, Open, RawEvent},
    components::help::HelpHit,
    fixture::FixtureProvider,
    theme::Theme,
    ui,
};
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};

fn demo() -> (FixtureProvider, App) {
    let (provider, sources, views) = FixtureProvider::demo();
    (provider, App::new(sources, views, true))
}

fn draw<P: RowProvider>(provider: &P, app: &mut App, width: u16, height: u16) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| ui::render_with_theme(frame, app, provider, Theme::TERMINAL, None))
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

fn key(app: &mut App, provider: &FixtureProvider, code: KeyCode) {
    app.handle(
        Action::Raw(RawEvent::Key(KeyEvent::new(code, KeyModifiers::NONE))),
        provider,
    );
}

fn mouse(app: &mut App, provider: &FixtureProvider, kind: MouseEventKind, point: (u16, u16)) {
    app.handle(
        Action::Raw(RawEvent::Mouse(MouseEvent {
            kind,
            column: point.0,
            row: point.1,
            modifiers: KeyModifiers::NONE,
        })),
        provider,
    );
}

#[test]
fn help_scrolls_within_the_bound_its_own_render_recorded() {
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::Help), &provider);
    assert_eq!(app.focus, Focus::Layer);
    assert_eq!(app.layers.stack, vec![LayerId::Help]);

    let top = screen(&draw(&provider, &mut app, 72, 16));
    assert!(top.contains("EVERYWHERE"), "{top}");
    let limit = app.layers.help.scroll_limit();
    assert!(limit > 0, "a 16-row terminal cannot show every section");

    key(&mut app, &provider, KeyCode::Down);
    key(&mut app, &provider, KeyCode::Char('j'));
    assert_eq!(app.layers.help.scroll(), 2);
    key(&mut app, &provider, KeyCode::Up);
    key(&mut app, &provider, KeyCode::Char('k'));
    key(&mut app, &provider, KeyCode::Char('k'));
    assert_eq!(app.layers.help.scroll(), 0, "the top is the floor");

    for _ in 0..limit + 20 {
        key(&mut app, &provider, KeyCode::Down);
    }
    assert_eq!(app.layers.help.scroll(), limit, "the bound is the ceiling");
    let bottom = screen(&draw(&provider, &mut app, 72, 16));
    assert!(bottom.contains("Alt-N"), "{bottom}");
    assert!(!bottom.contains("EVERYWHERE"), "{bottom}");

    // A wider terminal fits everything, so the recorded bound collapses and
    // the offset is clamped by the render that discovered it.
    let wide = screen(&draw(&provider, &mut app, 160, 70));
    assert_eq!(app.layers.help.scroll_limit(), 0);
    assert_eq!(app.layers.help.scroll(), 0);
    assert!(
        wide.contains("EVERYWHERE") && wide.contains("ASSISTANCE"),
        "{wide}"
    );
}

#[test]
fn the_shell_bounds_selection_and_the_mouse_by_the_published_surface() {
    let (provider, mut app) = demo();
    app.handle(Action::Top, &provider);
    app.handle(Action::MoveLine(2), &provider);
    draw(&provider, &mut app, 80, 24);
    let selected = app.view_state().unwrap().selected.clone();

    app.handle(Action::Open(Open::Help), &provider);
    draw(&provider, &mut app, 80, 24);
    let surface = app.layers.help.surface();
    assert_eq!(app.hit_regions.selection_modal, Some(surface.interior));
    assert!(surface.scrollable && !surface.text_focus && surface.caret.is_none());

    // Everything clickable is the body, and only inside the popup.
    let inside = (surface.interior.x + 2, surface.interior.y + 1);
    assert_eq!(app.layers.help.hit(inside), Some(HelpHit::Body));
    assert_eq!(app.layers.help.hit((0, 0)), None);

    // §5.2 containment: a wheel outside the popup neither scrolls Help nor
    // reaches the log behind it.
    mouse(&mut app, &provider, MouseEventKind::ScrollDown, (1, 1));
    assert_eq!(app.layers.help.scroll(), 0);
    assert_eq!(app.view_state().unwrap().selected, selected);

    mouse(&mut app, &provider, MouseEventKind::ScrollDown, inside);
    assert_eq!(app.layers.help.scroll(), 1);
    assert_eq!(app.view_state().unwrap().selected, selected);

    mouse(
        &mut app,
        &provider,
        MouseEventKind::Down(MouseButton::Left),
        inside,
    );
    assert_eq!(app.focus, Focus::Logs);

    assert!(!app.layers.help.is_open());
    assert!(app.layers.stack.is_empty());
}

#[test]
fn every_dismissal_returns_to_the_focus_underneath() {
    let (provider, mut app) = demo();
    for (code, opened_from) in [
        (KeyCode::Esc, Focus::Logs),
        (KeyCode::Char('q'), Focus::Logs),
        (KeyCode::Char('?'), Focus::Logs),
        (KeyCode::Esc, Focus::Details),
    ] {
        app.focus = opened_from;
        if opened_from == Focus::Details {
            app.show_details = true;
        }
        app.handle(Action::Open(Open::Help), &provider);
        assert_eq!(app.focus, Focus::Layer);
        draw(&provider, &mut app, 80, 24);
        key(&mut app, &provider, code);
        // Stack discipline replaced `help_return_focus`: popping the last
        // layer resumes the base focus the push captured.
        assert!(
            app.layers.stack.is_empty(),
            "{code:?} did not pop the layer"
        );
        assert_eq!(app.focus, opened_from, "{code:?}");
        assert!(!app.layers.help.is_open());
        // Reopening starts at the top, as `Action::ToggleHelp` did.
        assert_eq!(app.layers.help.scroll(), 0);
    }
}
