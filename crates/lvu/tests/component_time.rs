//! Acceptance for the Time layer as a component (docs/component-model.md §6.2).
//!
//! Time is the first layer to cross the two hard seams, so what is asserted
//! here is the seams and the contract, not the drawing: drafts stay in the
//! view and survive an invalid submission, a valid one goes through
//! `Views::submit_capture_time` and closes the layer, and the geometry
//! `render` recorded — including the anchored dropdown that extends past the
//! dialog — is the geometry `hit()` answers with.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use lvu::{
    Action, App, CaptureTimeRange,
    component::{Component, LayerId, Open, RawEvent},
    components::time::{TimeControl, TimeDialog},
    fixture::FixtureProvider,
    terminal::QueryDispatcher,
    theme::Theme,
    ui,
};
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer, layout::Rect};

/// The four terminals dialog-system.md quotes; 54x16 is where Time's body
/// overflows and its scrollbar appears.
const SIZES: [(u16, u16); 4] = [(140, 40), (100, 30), (80, 24), (54, 16)];

fn demo() -> (FixtureProvider, App) {
    let (provider, sources, views) = FixtureProvider::demo();
    (provider, App::new(sources, views, true))
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

fn key(app: &mut App, provider: &FixtureProvider, code: KeyCode) {
    app.handle(
        Action::Raw(RawEvent::Key(KeyEvent::new(code, KeyModifiers::NONE))),
        provider,
    );
}

fn paste(app: &mut App, provider: &FixtureProvider, text: &str) {
    app.handle(Action::Raw(RawEvent::Paste(text.into())), provider);
}

fn click(app: &mut App, provider: &FixtureProvider, point: (u16, u16)) {
    app.handle(
        Action::Raw(RawEvent::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: point.0,
            row: point.1,
            modifiers: KeyModifiers::NONE,
        })),
        provider,
    );
}

fn focus(app: &mut App, provider: &FixtureProvider, control: TimeControl) {
    for _ in 0..64 {
        if app.layers.time.state().focus == control {
            return;
        }
        key(app, provider, KeyCode::Tab);
    }
    panic!("{control:?} never took focus");
}

fn contains(area: Rect, point: (u16, u16)) -> bool {
    point.0 >= area.x && point.0 < area.right() && point.1 >= area.y && point.1 < area.bottom()
}

#[test]
fn every_recorded_control_was_painted_and_answers_the_hit_test() {
    for (width, height) in SIZES {
        let (provider, mut app) = demo();
        app.handle(Action::Open(Open::Time), &provider);
        let rendered = screen(&draw(&provider, &mut app, width, height));
        assert!(rendered.contains("Time window"), "at {width}x{height}");

        let surface = app.layers.time.surface();
        // §3/§5.1: the shell's selection bound is what the component published.
        assert_eq!(
            app.hit_regions.selection_modal,
            Some(surface.interior),
            "at {width}x{height}"
        );
        let controls: Vec<(Rect, TimeControl)> = app.layers.time.control_rects().to_vec();
        assert!(!controls.is_empty(), "at {width}x{height}");
        for (rect, control) in controls {
            assert!(
                surface.interior.union(rect) == surface.interior,
                "{control:?} at {rect:?} escapes {:?} at {width}x{height}",
                surface.interior
            );
            assert_eq!(
                app.layers.time.hit((rect.x, rect.y)),
                Some(lvu::components::time::TimeHit::Control(control)),
                "at {width}x{height}"
            );
        }
    }
}

#[test]
fn an_anchored_dropdown_is_the_components_geometry_and_stays_reachable() {
    // §5.3: the list may extend past the dialog it belongs to, so §5.2's
    // containment rect has to be everything the layer drew, not the frame.
    for (width, height) in SIZES {
        let (provider, mut app) = demo();
        app.handle(Action::Open(Open::Time), &provider);
        draw(&provider, &mut app, width, height);
        focus(&mut app, &provider, TimeControl::Window);
        key(&mut app, &provider, KeyCode::Enter);
        draw(&provider, &mut app, width, height);

        let surface = app.layers.time.surface();
        let rows: Vec<(Rect, usize)> = app.layers.time.choice_rects().to_vec();
        assert!(!rows.is_empty(), "at {width}x{height}");
        for (rect, index) in &rows {
            assert!(
                contains(surface.popup, (rect.x, rect.y)),
                "row {index} at {rect:?} is outside the containment rect {:?} at {width}x{height}",
                surface.popup
            );
            assert_eq!(
                app.layers.time.hit((rect.x, rect.y)),
                Some(lvu::components::time::TimeHit::Choice(*index)),
                "at {width}x{height}"
            );
        }
        // Clicking the `Absolute` row commits it; the list closes.
        let absolute = rows
            .iter()
            .find_map(|(rect, index)| (*index == 1).then_some(*rect))
            .expect("Absolute is always offered");
        click(&mut app, &provider, (absolute.x, absolute.y));
        assert!(app.layers.time.state().dropdown.is_none());
        assert_eq!(
            app.view_state().unwrap().time_window_draft,
            lvu::app::TimeWindowChoice::Absolute,
            "at {width}x{height}"
        );
    }
}

#[test]
fn an_invalid_draft_is_kept_reported_and_leaves_the_applied_window_alone() {
    let (provider, mut app) = demo();
    app.sync_provider(&provider, 8);
    app.handle(Action::Open(Open::Time), &provider);
    draw(&provider, &mut app, 100, 30);

    // A good window first, so there is something to preserve.
    paste(&mut app, &provider, "1970-01-01T00:00:01Z");
    app.layers.time.switch_field();
    paste(&mut app, &provider, "1970-01-01T00:00:03Z");
    focus(&mut app, &provider, TimeControl::Apply);
    key(&mut app, &provider, KeyCode::Enter);
    let request = app.take_query_requests().pop().unwrap();
    assert_eq!(
        request.constraints.capture_time,
        Some(CaptureTimeRange {
            start_unix_nanos: 1_000_000_000,
            end_unix_nanos: 3_000_000_000,
        })
    );
    assert!(!app.layers.time.is_open(), "a submitted window closes Time");
    let mut dispatcher = provider.query_dispatcher();
    dispatcher.submit(request).unwrap();
    app.apply_query_completion(dispatcher.poll().unwrap());
    let applied = app.view_state().unwrap().applied_capture_time;
    assert!(applied.is_some());

    // Now an impossible date. The layer stays open, says why, and the applied
    // window is untouched: pending is not a failed predicate (AGENTS.md).
    app.handle(Action::Open(Open::Time), &provider);
    draw(&provider, &mut app, 100, 30);
    paste(&mut app, &provider, "2026-02-30T00:00:00Z");
    focus(&mut app, &provider, TimeControl::Apply);
    key(&mut app, &provider, KeyCode::Enter);
    assert!(
        app.layers.time.is_open(),
        "a rejected draft keeps the layer"
    );
    assert!(app.take_query_requests().is_empty());
    assert_eq!(app.view_state().unwrap().applied_capture_time, applied);
    assert!(app.view_state().unwrap().time_error.is_some());
    let rendered = screen(&draw(&provider, &mut app, 100, 30));
    assert!(
        rendered.contains("2026-02-30"),
        "the draft survives:\n{rendered}"
    );
}

/// "All events is never filtered in place" holds for a time window too. The
/// seam stages the derived view itself and reports the revision, so the layer
/// closes on an accepted submission and never learns a fork happened
/// (component-model.md §2.3).
#[test]
fn a_window_applied_to_the_canonical_view_forks_instead_of_filtering_it() {
    let (provider, mut app) = demo();
    let canonical = app.active_view_id().unwrap().to_owned();
    app.set_view_role(&canonical, lvu::ViewRole::Canonical);
    let views_before = app.views().len();

    app.handle(Action::Open(Open::Time), &provider);
    draw(&provider, &mut app, 100, 30);
    paste(&mut app, &provider, "1970-01-01T00:00:01Z");
    app.layers.time.switch_field();
    paste(&mut app, &provider, "1970-01-01T00:00:03Z");
    focus(&mut app, &provider, TimeControl::Apply);
    key(&mut app, &provider, KeyCode::Enter);

    // Nothing was queried against the canonical view and its definition is
    // untouched, but a candidate view was proposed.
    assert!(
        app.take_query_requests().is_empty(),
        "an edit to All events never queries All events"
    );
    assert_eq!(app.views().len(), views_before, "no view is visible yet");
    assert_eq!(app.view_state().unwrap().applied_capture_time, None);
    let forks = app.take_view_fork_requests();
    assert_eq!(forks.len(), 1, "{forks:?}");
    assert_eq!(forks[0].origin_view_id, canonical);
    // The layer closed, as it does on any accepted submission.
    assert!(!app.layers.time.is_open());
    assert_eq!(app.focus, lvu::Focus::Logs);
}

#[test]
fn drafts_live_in_the_view_so_they_survive_close_and_reopen() {
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::Time), &provider);
    draw(&provider, &mut app, 100, 30);
    paste(&mut app, &provider, "1999-12-31T23:59:58Z");
    let draft = app.view_state().unwrap().time_start_draft.clone();
    assert!(draft.starts_with("1999-12-31"));
    key(&mut app, &provider, KeyCode::Esc);
    assert!(!app.layers.time.is_open());
    assert_eq!(app.focus, lvu::Focus::Logs);
    // Nothing was submitted, and the draft is still the view's.
    assert!(app.take_query_requests().is_empty());
    app.handle(Action::Open(Open::Time), &provider);
    assert_eq!(app.view_state().unwrap().time_start_draft, draft);
    assert_eq!(app.layers.time.state().start_date, "1999-12-31");
}

#[test]
fn q_types_into_a_segment_and_dismisses_from_a_button() {
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::Time), &provider);
    draw(&provider, &mut app, 100, 30);
    focus(&mut app, &provider, TimeControl::StartDate);
    // `text_focus` is published by render, like every other part of `Surface`.
    draw(&provider, &mut app, 100, 30);
    assert!(app.layers.time.surface().text_focus);
    key(&mut app, &provider, KeyCode::Char('q'));
    assert!(app.layers.time.is_open(), "q is a character in a segment");
    assert!(app.layers.time.state().start_date.contains('q'));

    focus(&mut app, &provider, TimeControl::Apply);
    draw(&provider, &mut app, 100, 30);
    assert!(!app.layers.time.surface().text_focus);
    key(&mut app, &provider, KeyCode::Char('q'));
    assert!(!app.layers.time.is_open(), "q dismisses from a button");
    assert!(!app.should_quit, "q never quits out of a dialog");
}

#[test]
fn escape_closes_the_dropdown_before_the_layer() {
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::Time), &provider);
    draw(&provider, &mut app, 100, 30);
    focus(&mut app, &provider, TimeControl::Basis);
    key(&mut app, &provider, KeyCode::Enter);
    assert!(app.layers.time.state().dropdown.is_some());
    key(&mut app, &provider, KeyCode::Esc);
    assert!(app.layers.time.state().dropdown.is_none());
    assert!(app.layers.time.is_open(), "§5.3: innermost thing first");
    key(&mut app, &provider, KeyCode::Esc);
    assert!(!app.layers.time.is_open());
}

#[test]
fn the_recognizer_reports_through_the_outbox_and_is_generation_fenced() {
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::Time), &provider);
    let request = app.layers.time.outbox.take().pop().unwrap();
    assert_eq!(request.view_id, app.active_view_id().unwrap());

    let recognition = lvu::app::TimeRecognition {
        sampled_records: 4,
        ..Default::default()
    };
    assert!(
        !app.layers
            .time
            .complete_recognition(request.generation + 1, recognition.clone()),
        "a stale report is dropped"
    );
    assert!(
        app.layers
            .time
            .complete_recognition(request.generation, recognition.clone())
    );

    // A report for a layer that has closed lands nowhere.
    key(&mut app, &provider, KeyCode::Esc);
    assert!(
        !app.layers
            .time
            .complete_recognition(request.generation, recognition)
    );
}

#[test]
fn the_palette_entries_are_the_components_and_reach_it_as_a_command() {
    let (provider, mut app) = demo();
    let time_entries = |app: &App| {
        app.layer_commands()
            .into_iter()
            .filter(|(layer, _)| *layer == LayerId::Time)
            .collect::<Vec<_>>()
    };
    // Listed but muted while the layer is closed, exactly as before.
    let closed = time_entries(&app);
    assert_eq!(closed.len(), 8);
    assert!(
        closed
            .iter()
            .all(|(_, entry)| entry.unavailable_reason == Some("open Time window first"))
    );

    app.handle(Action::Open(Open::Time), &provider);
    draw(&provider, &mut app, 100, 30);
    let open = time_entries(&app);
    let recent_five = open
        .iter()
        .find(|(_, entry)| entry.spec.id == lvu::command_palette::CommandId::TimeRecentFive)
        .expect("the five-minute preset is Time's");
    assert!(recent_five.1.unavailable_reason.is_none());
    assert_eq!(recent_five.1.spec.shortcut, Some("Alt-5"));

    app.handle(
        Action::Command(recent_five.0, recent_five.1.spec.id),
        &provider,
    );
    let request = app.take_query_requests().pop().unwrap();
    assert!(request.constraints.capture_time.is_some());
    assert_eq!(
        app.view_state().unwrap().time_window_draft,
        lvu::app::TimeWindowChoice::Recent(300)
    );
    assert!(!app.layers.time.is_open());
}

#[test]
fn the_recognize_button_hands_off_to_the_assistant_and_leaves_the_stack_clean() {
    // §6.4 `Outcome::Legacy`: Ask is converted last, so Time pops itself and
    // opens the legacy dialog rather than pushing a layer over it.
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::Time), &provider);
    draw(&provider, &mut app, 100, 30);
    focus(&mut app, &provider, TimeControl::Recognize);
    key(&mut app, &provider, KeyCode::Enter);
    assert_eq!(app.focus, lvu::Focus::AskAi);
    assert!(!app.layers.time.is_open());
    // Storage can still be opened and closed without a stale Time underneath.
    app.handle(Action::Open(Open::Storage), &provider);
    assert_eq!(app.focus, lvu::Focus::Layer);
    app.handle(
        Action::Raw(RawEvent::Key(KeyEvent::new(
            KeyCode::Esc,
            KeyModifiers::NONE,
        ))),
        &provider,
    );
    assert_eq!(app.focus, lvu::Focus::Logs);
}

/// The layer must not swallow the shell's navigation keys just because it has
/// focus; before the conversion this was asserted against `key_to_action`.
#[test]
fn time_does_not_claim_global_navigation_keys() {
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::Time), &provider);
    draw(&provider, &mut app, 100, 30);
    let before = app.layers.time.state().clone();
    for code in [
        KeyCode::PageUp,
        KeyCode::PageDown,
        KeyCode::Home,
        KeyCode::End,
    ] {
        for modifiers in [KeyModifiers::NONE, KeyModifiers::CONTROL] {
            app.handle(
                Action::Raw(RawEvent::Key(KeyEvent::new(code, modifiers))),
                &provider,
            );
        }
    }
    assert_eq!(app.layers.time.state(), &before);
    assert!(app.layers.time.is_open());
}

#[test]
fn a_default_time_dialog_contributes_nothing_and_answers_no_hit() {
    let time = TimeDialog::default();
    assert!(!time.is_open());
    assert!(time.hit((0, 0)).is_none());
    assert!(
        time.commands(&lvu::app::Views::default())
            .iter()
            .all(|entry| entry.unavailable_reason.is_some())
    );
}
