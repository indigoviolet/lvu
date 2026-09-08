//! docs/raw-context-as-jump.md: `o` is a jump to the record in its source's
//! All events view, and `o` again is the way back — to the view, the record,
//! and the dialog it was pressed in.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use lvu::{
    Action, App, ViewRole,
    app::{Focus, RawContextOrigin, key_to_action},
    command_palette::{CommandId, Palette, PaletteContext},
    component::{LayerId, Open, RawEvent},
    fixture::FixtureProvider,
    theme::Theme,
    ui,
};
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};

/// The filtered view active, its second record selected, `all` canonical.
fn opened() -> (FixtureProvider, App) {
    let (provider, sources, views) = FixtureProvider::raw_context_demo();
    let mut app = App::new(sources, views, true);
    app.set_view_role("all", ViewRole::Canonical);
    app.select_view("filtered");
    app.sync_provider(&provider, 10);
    app.handle(Action::Top, &provider);
    app.handle(Action::MoveLine(1), &provider);
    app.sync_provider(&provider, 10);
    (provider, app)
}

fn draw(provider: &FixtureProvider, app: &mut App, width: u16, height: u16) -> Buffer {
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

fn press(app: &mut App, provider: &FixtureProvider, code: KeyCode) {
    app.handle(
        Action::Raw(RawEvent::Key(KeyEvent::new(code, KeyModifiers::NONE))),
        provider,
    );
}

fn key(app: &mut App, provider: &FixtureProvider, code: KeyCode) {
    let action = app.key_to_action(KeyEvent::new(code, KeyModifiers::NONE));
    app.handle(action, provider);
}

#[test]
fn o_jumps_to_the_record_in_all_events_and_o_again_returns() {
    let (provider, mut app) = opened();
    let anchor = app.view_state().unwrap().selected.clone().unwrap();
    assert_eq!(anchor.sequence, 10);
    assert_eq!(
        key_to_action(
            KeyEvent::new(KeyCode::Char('o'), KeyModifiers::NONE),
            Focus::Logs
        ),
        Action::RawContext {
            anchor: None,
            layer: None
        }
    );
    key(&mut app, &provider, KeyCode::Char('o'));
    assert_eq!(app.active_view_id(), Some("all"));
    assert_eq!(app.focus, Focus::Logs);
    assert_eq!(
        app.raw_context_origin(),
        Some(&RawContextOrigin {
            view_id: "filtered".into(),
            raw_view_id: "all".into(),
            anchor: anchor.clone(),
            layer: None,
        })
    );
    // The fixture answers at once, so the first frame already says how to
    // get back (the `locating…` state is the readiness test below).
    app.sync_provider(&provider, 6);
    let landed = screen(&draw(&provider, &mut app, 100, 12));
    assert!(
        landed.contains("raw of Warnings · #10 · o back"),
        "{landed}"
    );
    let state = app.view_state().unwrap();
    assert_eq!(state.selected.as_ref(), Some(&anchor));
    assert!(!state.follow);
    assert!(
        state.top > 0 && state.top < 9,
        "centred, not at the top: top={}",
        state.top
    );
    // Neighbours the filter hid are on screen.
    assert!(landed.contains("fixture request 09 completed"), "{landed}");
    assert!(landed.contains("fixture request 11 completed"), "{landed}");

    key(&mut app, &provider, KeyCode::Char('o'));
    assert_eq!(app.active_view_id(), Some("filtered"));
    assert_eq!(app.raw_context_origin(), None);
    app.sync_provider(&provider, 10);
    assert_eq!(app.view_state().unwrap().selected.as_ref(), Some(&anchor));
    let back = screen(&draw(&provider, &mut app, 100, 12));
    assert!(!back.contains("raw of"), "{back}");
    assert!(
        !back.contains("fixture request 09"),
        "the filter is back: {back}"
    );
}

#[test]
fn leaving_the_raw_view_retires_the_origin_and_o_on_the_raw_stream_says_so() {
    let (provider, mut app) = opened();
    key(&mut app, &provider, KeyCode::Char('o'));
    assert!(app.raw_context_origin().is_some());
    // Switching views is a deliberate departure: nothing to return to.
    app.handle(Action::NextView, &provider);
    app.handle(Action::PreviousView, &provider);
    assert_eq!(app.active_view_id(), Some("all"));
    assert_eq!(app.raw_context_origin(), None);
    key(&mut app, &provider, KeyCode::Char('o'));
    assert_eq!(
        app.active_view_id(),
        Some("all"),
        "no jump from the raw stream"
    );
    assert_eq!(
        app.action_notice.as_deref(),
        Some("this is the raw stream · o returns nowhere")
    );
    app.handle(Action::ReturnFromRawContext, &provider);
    assert_eq!(app.action_notice.as_deref(), Some("nothing to return to"));
}

#[test]
fn fields_closes_on_the_jump_and_is_re_pushed_on_the_same_record_on_return() {
    let (provider, mut app) = opened();
    let anchor = app.view_state().unwrap().selected.clone().unwrap();
    app.handle(Action::Open(Open::Fields), &provider);
    assert_eq!(app.layers.stack_ids(), vec![LayerId::Fields]);
    press(&mut app, &provider, KeyCode::Down);
    press(&mut app, &provider, KeyCode::Char('o'));
    assert!(
        app.layers.stack_ids().is_empty(),
        "Fields closed on the jump"
    );
    assert_eq!(app.active_view_id(), Some("all"));
    assert_eq!(app.focus, Focus::Logs);
    assert_eq!(
        app.raw_context_origin().map(|origin| origin.layer.clone()),
        Some(Some(Open::Fields))
    );
    app.sync_provider(&provider, 6);
    key(&mut app, &provider, KeyCode::Char('o'));
    assert_eq!(app.active_view_id(), Some("filtered"));
    assert_eq!(
        app.layers.stack_ids(),
        vec![LayerId::Fields],
        "Fields is back"
    );
    assert_eq!(app.focus, Focus::Layer);
    assert_eq!(
        app.view_state().unwrap().field_picker_row.as_ref(),
        Some(&anchor),
        "on the record it was opened for"
    );
}

#[test]
fn bookmarks_closes_on_the_jump_and_is_re_pushed_on_the_same_bookmark_on_return() {
    let (provider, mut app) = opened();
    app.handle(Action::ToggleBookmark, &provider);
    app.handle(Action::MoveLine(1), &provider);
    app.handle(Action::ToggleBookmark, &provider);
    let second = app.view_state().unwrap().selected.clone().unwrap();
    app.handle(Action::Open(Open::Bookmarks), &provider);
    draw(&provider, &mut app, 100, 30);
    press(&mut app, &provider, KeyCode::Down);
    assert_eq!(app.layers.bookmarks.state().selected, 1);
    // Tab to the Raw context button and press it.
    for _ in 0..8 {
        if app.layers.bookmarks.state().control == lvu::app::BookmarkDialogControl::Context {
            break;
        }
        press(&mut app, &provider, KeyCode::Tab);
    }
    press(&mut app, &provider, KeyCode::Enter);
    assert!(
        app.layers.stack_ids().is_empty(),
        "Bookmarks closed on the jump"
    );
    assert_eq!(app.active_view_id(), Some("all"));
    assert_eq!(
        app.raw_context_origin().map(|origin| origin.anchor.clone()),
        Some(second.clone())
    );
    app.sync_provider(&provider, 6);
    assert_eq!(app.view_state().unwrap().selected.as_ref(), Some(&second));
    key(&mut app, &provider, KeyCode::Char('o'));
    assert_eq!(
        app.layers.stack_ids(),
        vec![LayerId::Bookmarks],
        "Bookmarks is back"
    );
    assert_eq!(
        app.layers.bookmarks.state().selected,
        1,
        "on the same bookmark"
    );
    assert_eq!(app.view_state().unwrap().bookmark_selected, 1);
}

#[test]
fn a_record_the_raw_view_cannot_address_stops_being_chased_and_says_so() {
    let (provider, mut app) = opened();
    // Hide every row of `all` from the provider: the anchor never resolves.
    provider.hide_view_rows("all");
    key(&mut app, &provider, KeyCode::Char('o'));
    assert_eq!(app.active_view_id(), Some("all"));
    let locating = screen(&draw(&provider, &mut app, 100, 12));
    assert!(
        locating.contains("raw of Warnings · #10 · locating…"),
        "{locating}"
    );
    for _ in 0..300 {
        app.sync_provider(&provider, 6);
        if !app.jump_pending() {
            break;
        }
    }
    assert!(!app.jump_pending(), "the chase is bounded");
    assert_eq!(
        app.action_notice.as_deref(),
        Some("record #10 is not addressable in this view yet")
    );
    assert!(app.raw_context_origin().is_some(), "o still returns");
    key(&mut app, &provider, KeyCode::Char('o'));
    assert_eq!(app.active_view_id(), Some("filtered"));
}

#[test]
fn the_palette_lists_the_jump_and_the_return_with_the_chord_that_works() {
    let (provider, mut app) = opened();
    let mut palette = Palette::new();
    let mut context = PaletteContext::new(Focus::Logs, true);
    context.has_selected_row = true;
    palette.open(context.clone());
    let jump = palette
        .results()
        .find(|command| command.id == CommandId::Context)
        .expect("the jump is runnable in a filtered view");
    assert_eq!(jump.shortcut, Some("o"));
    assert!(
        !palette
            .results()
            .any(|command| command.id == CommandId::ReturnFromRawContext),
        "nothing to return to yet"
    );
    key(&mut app, &provider, KeyCode::Char('o'));
    let mut held = PaletteContext::new(Focus::Logs, true);
    held.has_selected_row = true;
    held.raw_context_held = true;
    held.in_raw_view = true;
    palette.open(held);
    let back = palette
        .results()
        .find(|command| command.id == CommandId::ReturnFromRawContext)
        .expect("the return is runnable while an origin is held");
    assert_eq!(back.shortcut, Some("o"));
    let jump = palette
        .commands()
        .iter()
        .find(|command| command.id == CommandId::Context)
        .unwrap();
    assert_eq!(jump.unavailable_reason, Some("return with o first"));
    app.handle(back.action.clone(), &provider);
    assert_eq!(app.active_view_id(), Some("filtered"));
}
