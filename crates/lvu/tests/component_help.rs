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
    assert!(bottom.contains("Restart the selected source"), "{bottom}");
    assert!(!bottom.contains("EVERYWHERE"), "{bottom}");

    // A wider terminal fits everything, so the recorded bound collapses and
    // the offset is clamped by the render that discovered it.
    let wide = screen(&draw(&provider, &mut app, 160, 70));
    assert_eq!(app.layers.help.scroll_limit(), 0);
    assert_eq!(app.layers.help.scroll(), 0);
    assert!(
        wide.contains("EVERYWHERE") && wide.contains("SOURCES"),
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
fn responsive_frame_is_policy_stable_with_body_owning_surplus_and_no_actions() {
    use lvu::dialog_layout::{PresentationKind, policy_size};

    // LongContent policy owns the frame at every size; Help has no header/
    // message/help/actions bands, so the body owns all surplus and scrolls.
    // Frame is identical at the top and bottom of the document.
    for (width, height) in [(240u16, 80u16), (140, 40), (80, 24), (54, 16), (20, 6)] {
        let (provider, mut app) = demo();
        app.handle(Action::Open(Open::Help), &provider);
        let top_buffer = draw(&provider, &mut app, width, height);
        let top = screen(&top_buffer);
        let surface = app.layers.help.surface();
        let (want_w, want_h) = policy_size(
            ratatui::layout::Rect::new(0, 0, width, height),
            PresentationKind::LongContent,
        );
        assert_eq!(
            (surface.popup.width, surface.popup.height),
            (want_w, want_h),
            "{width}x{height} frame must be policy"
        );
        if (width, height) != (20, 6) {
            assert!(
                surface.popup.width < width || surface.popup.height < height,
                "{width}x{height} became full frame"
            );
        }
        assert!(top.contains("Help"), "{top}");
        // No action row: Enter is inert, no default button is drawn. Help body
        // itself documents keys like "[ / ]", so assert the absence of action
        // verbs rather than brackets.
        assert!(
            !top.contains("[ Open ]") && !top.contains("[ Apply ]"),
            "{width}x{height} must draw no action buttons:\n{top}"
        );
        // Scroll to the bottom: frame identical, content moved, selection bound
        // still the published interior, wheel and keys share it.
        for _ in 0..app.layers.help.scroll_limit() + 5 {
            key(&mut app, &provider, KeyCode::Down);
        }
        let bottom_buffer = draw(&provider, &mut app, width, height);
        let bottom = screen(&bottom_buffer);
        assert_eq!(
            app.layers.help.surface().popup,
            surface.popup,
            "{width}x{height} frame must not move with scroll"
        );
        assert_eq!(
            app.hit_regions.selection_modal,
            Some(app.layers.help.surface().interior)
        );
        // Wide/combining text never breaks columns: two-column layout only at
        // content width >= 88, single column otherwise, both scroll.
        if (width, height) == (80, 24) {
            assert_ne!(top, bottom, "80x24 help must scroll");
        }
        // The wheel is wanted exactly when the reference overflows: roomy
        // 240x80 fits it, 80x24 does not.
        if (width, height) == (240, 80) {
            assert!(
                !surface.scrollable,
                "240x80 fits the reference and must not want the wheel"
            );
        } else if (width, height) == (80, 24) {
            assert!(
                surface.scrollable,
                "80x24 overflows the reference and must want the wheel"
            );
        }
    }

    // Below the floor the tiny fallback owns the frame.
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::Help), &provider);
    let tiny = screen(&draw(&provider, &mut app, 19, 5));
    assert!(tiny.contains("terminal too small"), "{tiny}");

    // At the floor Help still renders with a scrollbar when overflowing.
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::Help), &provider);
    let floor = screen(&draw(&provider, &mut app, 20, 6));
    assert!(floor.contains("Help"), "{floor}");
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
