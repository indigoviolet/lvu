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
    // Ask is a layer now (§6.3 step 12), so this is a plain `Replace`: Time
    // leaves the stack and Ask takes its place on it.
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::Time), &provider);
    draw(&provider, &mut app, 100, 30);
    focus(&mut app, &provider, TimeControl::Recognize);
    key(&mut app, &provider, KeyCode::Enter);
    assert_eq!(app.focus, lvu::Focus::Layer);
    assert_eq!(app.layers.top(), Some(lvu::component::LayerId::Ask));
    assert!(!app.layers.time.is_open());
    // The prepared task, not a blank request.
    assert_eq!(
        app.layers.ask.state().and_then(|dialog| dialog.task),
        Some(lvu::app::AskTask::TimestampColumn)
    );
    app.handle(
        Action::Raw(RawEvent::Key(KeyEvent::new(
            KeyCode::Esc,
            KeyModifiers::NONE,
        ))),
        &provider,
    );
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

fn ctrl_key(app: &mut App, provider: &FixtureProvider, code: KeyCode) {
    app.handle(
        Action::Raw(RawEvent::Key(KeyEvent::new(code, KeyModifiers::CONTROL))),
        provider,
    );
}

/// Every planned action/More rect must be full-size (at least its required
/// button width), inside the band, and pairwise disjoint: Ratatui squeezes
/// over-wide fixed Length constraints instead of refusing, so anything less
/// is a clipped label or a dead hitbox masquerading as geometry.
fn assert_action_rects_valid(
    band: Rect,
    buttons: &[(usize, Rect)],
    more: Option<Rect>,
    labels: &[&str],
    tag: &str,
) {
    use lvu::dialog_controls::{MORE_LABEL, button_width};
    let mut seen: Vec<Rect> = Vec::new();
    for (index, rect) in buttons {
        let required = button_width(labels[*index]);
        assert!(
            rect.width >= required,
            "{tag}: button {} paints {} wide, needs {required}",
            labels[*index],
            rect.width,
        );
        assert!(
            rect.x >= band.x
                && rect.right() <= band.right()
                && rect.y >= band.y
                && rect.bottom() <= band.bottom(),
            "{tag}: button {} at {rect:?} escapes band {band:?}",
            labels[*index],
        );
        assert!(
            seen.iter().all(|prior: &Rect| {
                prior.x >= rect.right()
                    || rect.x >= prior.right()
                    || prior.y >= rect.bottom()
                    || rect.y >= prior.bottom()
            }),
            "{tag}: button {} at {rect:?} overlaps {seen:?}",
            labels[*index],
        );
        seen.push(*rect);
    }
    if let Some(rect) = more {
        let required = button_width(MORE_LABEL);
        assert!(
            rect.width >= required,
            "{tag}: More paints {} wide, needs {required}",
            rect.width
        );
        assert!(
            rect.x >= band.x
                && rect.right() <= band.right()
                && rect.y >= band.y
                && rect.bottom() <= band.bottom(),
            "{tag}: More at {rect:?} escapes band {band:?}",
        );
        assert!(
            seen.iter().all(|prior: &Rect| {
                prior.x >= rect.right()
                    || rect.x >= prior.right()
                    || prior.y >= rect.bottom()
                    || rect.y >= prior.bottom()
            }),
            "{tag}: More at {rect:?} overlaps {seen:?}",
        );
    }
}

/// The Time action labels as rendered in these tests (non-ASCII theme).
const TIME_LABELS: [&str; 3] = ["Apply", "&Clear", "🧠 Recognize &timestamp"];

/// An invalid start bound leaves a reported error behind for the menu tests
/// to clear. The dialog stays open with the draft intact.
fn make_time_error(app: &mut App, provider: &FixtureProvider) {
    paste(app, provider, "2026-02-30T00:00:00Z");
    focus(app, provider, TimeControl::Apply);
    key(app, provider, KeyCode::Enter);
    assert!(app.layers.time.is_open());
    assert!(app.take_query_requests().is_empty());
    assert!(app.view_state().unwrap().time_error.is_some());
}

#[test]
fn overflow_menu_activates_every_hidden_verb_at_the_floor() {
    // At 20x6 Clear and Recognize hide behind More; Apply stays painted.
    // The menu itself scrolls: one row fits above the action band.
    let (provider, mut app) = demo();
    draw(&provider, &mut app, 20, 6);
    app.handle(Action::Open(Open::Time), &provider);
    draw(&provider, &mut app, 20, 6);
    make_time_error(&mut app, &provider);
    draw(&provider, &mut app, 20, 6);
    assert!(screen(&draw(&provider, &mut app, 20, 6)).contains("Time window"));
    let more = app.layers.time.more_button().expect("More paints at 20x6");
    assert!(
        !app.layers
            .time
            .control_rects()
            .iter()
            .any(|(_, control)| { matches!(control, TimeControl::Clear | TimeControl::Recognize) })
    );
    assert!(app.layers.time.more_rows().is_empty());

    // Hidden-focus Enter opens the menu on that verb instead of running it
    // blind: the error survives the first Enter (menu opened with Clear
    // highlighted), and only the second Enter runs Clear through
    // `press_action`, closing the dialog like its button. The kept two-row
    // band holds a full Apply plus a full More: no squeezed Lengths.
    focus(&mut app, &provider, TimeControl::Clear);
    key(&mut app, &provider, KeyCode::Enter);
    draw(&provider, &mut app, 20, 6);
    assert_eq!(app.layers.time.action_band().height, 2);
    assert_action_rects_valid(
        app.layers.time.action_band(),
        &[(
            0,
            app.layers
                .time
                .control_rects()
                .iter()
                .find_map(|(rect, control)| (*control == TimeControl::Apply).then_some(*rect))
                .expect("Apply paints"),
        )],
        app.layers.time.more_button(),
        &TIME_LABELS,
        "20x6 Time floor",
    );
    let floor = screen(&draw(&provider, &mut app, 20, 6));
    assert!(floor.contains("[ Apply ]"), "{floor}");
    assert!(floor.contains("[ More ▾ ]"), "{floor}");
    let rows = app.layers.time.more_rows().to_vec();
    assert!(!rows.is_empty(), "the menu paints its first row");
    assert_eq!(rows[0].1, 1, "menu rows carry original action indices");
    for (rect, _) in &rows {
        assert!(rect.right() <= 20 && rect.bottom() <= 6);
    }
    // One-row gap against the More button that anchored the menu.
    let menu_top = rows[0].0.y.saturating_sub(1);
    let menu_bottom = rows.last().map(|(rect, _)| rect.y.saturating_add(2));
    assert!(
        menu_top == more.bottom().saturating_add(1)
            || menu_bottom.is_some_and(|bottom| bottom.saturating_add(1) == more.y),
        "menu keeps its one-row gap to More {more:?}"
    );
    assert!(app.view_state().unwrap().time_error.is_some());
    key(&mut app, &provider, KeyCode::Enter);
    assert!(!app.layers.time.is_open());
    assert_eq!(app.layers.time.state().start_date, "");

    // Mouse: the More button opens; a row click activates immediately.
    // Clear is the visible row here.
    let (provider, mut app) = demo();
    draw(&provider, &mut app, 20, 6);
    app.handle(Action::Open(Open::Time), &provider);
    draw(&provider, &mut app, 20, 6);
    make_time_error(&mut app, &provider);
    draw(&provider, &mut app, 20, 6);
    let more = app.layers.time.more_button().expect("More paints");
    click(&mut app, &provider, (more.x, more.y));
    draw(&provider, &mut app, 20, 6);
    let rows = app.layers.time.more_rows().to_vec();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].1, 1);
    click(&mut app, &provider, (rows[0].0.x, rows[0].0.y));
    assert!(!app.layers.time.is_open());
    assert_eq!(app.layers.time.state().start_date, "");

    // Keyboard nav across the scrolled menu: Down reveals Recognize, Enter
    // hands off to the assistant — all by key.
    let (provider, mut app) = demo();
    draw(&provider, &mut app, 20, 6);
    app.handle(Action::Open(Open::Time), &provider);
    draw(&provider, &mut app, 20, 6);
    make_time_error(&mut app, &provider);
    draw(&provider, &mut app, 20, 6);
    let more = app.layers.time.more_button().expect("More paints");
    click(&mut app, &provider, (more.x, more.y));
    draw(&provider, &mut app, 20, 6);
    key(&mut app, &provider, KeyCode::Down);
    draw(&provider, &mut app, 20, 6);
    let rows = app.layers.time.more_rows().to_vec();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].1, 2, "Down scrolled the menu to Recognize");
    key(&mut app, &provider, KeyCode::Enter);
    assert_eq!(app.layers.top(), Some(lvu::component::LayerId::Ask));

    // Mouse on the scrolled row works the same way.
    let (provider, mut app) = demo();
    draw(&provider, &mut app, 20, 6);
    app.handle(Action::Open(Open::Time), &provider);
    draw(&provider, &mut app, 20, 6);
    make_time_error(&mut app, &provider);
    draw(&provider, &mut app, 20, 6);
    let more = app.layers.time.more_button().expect("More paints");
    click(&mut app, &provider, (more.x, more.y));
    draw(&provider, &mut app, 20, 6);
    key(&mut app, &provider, KeyCode::Down);
    draw(&provider, &mut app, 20, 6);
    let rows = app.layers.time.more_rows().to_vec();
    let recognize = rows
        .iter()
        .find_map(|(rect, index)| (*index == 2).then_some(*rect))
        .expect("Recognize painted after scroll");
    click(&mut app, &provider, (recognize.x, recognize.y));
    assert_eq!(app.layers.top(), Some(lvu::component::LayerId::Ask));

    // Escape closes the menu and keeps the dialog with its error.
    let (provider, mut app) = demo();
    draw(&provider, &mut app, 20, 6);
    app.handle(Action::Open(Open::Time), &provider);
    draw(&provider, &mut app, 20, 6);
    make_time_error(&mut app, &provider);
    draw(&provider, &mut app, 20, 6);
    let more = app.layers.time.more_button().expect("More paints");
    click(&mut app, &provider, (more.x, more.y));
    draw(&provider, &mut app, 20, 6);
    assert!(!app.layers.time.more_rows().is_empty());
    key(&mut app, &provider, KeyCode::Esc);
    draw(&provider, &mut app, 20, 6);
    assert!(app.layers.time.more_rows().is_empty());
    assert!(app.layers.time.is_open());
    assert!(app.view_state().unwrap().time_error.is_some());

    // Roomy sizes fit every verb with a one-row band: no overflow invented.
    for (width, height) in [(80u16, 24u16), (54, 16)] {
        let (provider, mut app) = demo();
        draw(&provider, &mut app, width, height);
        app.handle(Action::Open(Open::Time), &provider);
        draw(&provider, &mut app, width, height);
        assert!(
            app.layers.time.more_button().is_none(),
            "{width}x{height}: no overflow invented"
        );
        assert_eq!(
            app.layers.time.action_band().height,
            1,
            "{width}x{height}: one action row, no dead row"
        );
        let mut painted = Vec::new();
        for (control, index) in [
            (TimeControl::Apply, 0),
            (TimeControl::Clear, 1),
            (TimeControl::Recognize, 2),
        ] {
            let rect = app
                .layers
                .time
                .control_rects()
                .iter()
                .find_map(|(rect, painted)| (*painted == control).then_some(*rect))
                .unwrap_or_else(|| panic!("{width}x{height}: {control:?} paints directly"));
            painted.push((index, rect));
        }
        assert_action_rects_valid(
            app.layers.time.action_band(),
            &painted,
            None,
            &TIME_LABELS,
            &format!("{width}x{height} roomy"),
        );
        let rendered = screen(&draw(&provider, &mut app, width, height));
        assert!(rendered.contains("[ Apply ]"), "{rendered}");
        assert!(rendered.contains("[ Clear ]"), "{rendered}");
        assert!(rendered.contains("Recognize"), "{rendered}");
    }
}

#[test]
fn inspector_frame_is_stable_across_clean_error_and_open_dropdown_states() {
    let (provider, mut app) = demo();
    draw(&provider, &mut app, 100, 30);
    app.handle(Action::Open(Open::Time), &provider);
    draw(&provider, &mut app, 100, 30);
    let anchor = app.shell.context_anchor.expect("anchor frozen at open");
    let clean_popup = app.layers.time.surface().popup;
    let clean_controls = app.layers.time.control_rects().to_vec();

    // An open dropdown joins the containment union but must not move the
    // frame or any control it hangs from.
    focus(&mut app, &provider, TimeControl::Window);
    key(&mut app, &provider, KeyCode::Enter);
    assert!(app.layers.time.state().dropdown.is_some());
    draw(&provider, &mut app, 100, 30);
    assert_eq!(
        app.layers.time.control_rects(),
        clean_controls.as_slice(),
        "an open dropdown moves no control"
    );
    // Dismiss the dropdown (Escape closes it before the layer) for the error
    // half below.
    key(&mut app, &provider, KeyCode::Esc);
    assert!(app.layers.time.state().dropdown.is_none());

    // A rejected draft keeps the layer with an error row; the frame is the
    // same policy frame the clean dialog used.
    paste(&mut app, &provider, "2026-02-30T00:00:00Z");
    focus(&mut app, &provider, TimeControl::Apply);
    key(&mut app, &provider, KeyCode::Enter);
    assert!(
        app.layers.time.is_open(),
        "a rejected draft keeps the layer"
    );
    assert!(app.take_query_requests().is_empty());
    assert!(app.view_state().unwrap().time_error.is_some());
    let rendered = screen(&draw(&provider, &mut app, 100, 30));
    assert!(rendered.contains("Error"), "{rendered}");
    assert_eq!(
        app.layers.time.surface().popup,
        clean_popup,
        "error state moved the frame"
    );
    assert_eq!(
        app.shell.context_anchor,
        Some(anchor),
        "frames never recapture the anchor"
    );
}

#[test]
fn inspector_avoids_the_frozen_selected_row_with_a_one_row_gap() {
    for (width, height) in [(240u16, 80u16), (140, 40), (80, 24), (54, 16)] {
        let tag = format!("{width}x{height}");
        let (provider, mut app) = demo();
        draw(&provider, &mut app, width, height);
        app.handle(Action::Open(Open::Time), &provider);
        let anchor = app.shell.context_anchor.expect("anchor frozen at open");
        assert!(
            !anchor.row.is_empty(),
            "{tag}: the opening selection names a row"
        );
        draw(&provider, &mut app, width, height);
        let frame = app.layers.time.surface().popup;
        assert!(
            frame.right() <= width && frame.bottom() <= height,
            "{tag}: frame {frame:?} escapes the area"
        );
        assert!(
            frame.bottom() <= anchor.row.y || frame.y >= anchor.row.bottom(),
            "{tag}: frame {frame:?} covers referent row {:?}",
            anchor.row
        );
        let below = frame.y == anchor.row.bottom().saturating_add(1);
        let above = frame.bottom().saturating_add(1) == anchor.row.y;
        assert!(
            below || above,
            "{tag}: frame {frame:?} keeps no one-row gap to row {:?}",
            anchor.row
        );
    }

    // The 20x6 floor still draws the full dialog; below it the existing tiny
    // fallback owns the screen instead.
    let (provider, mut app) = demo();
    draw(&provider, &mut app, 20, 6);
    app.handle(Action::Open(Open::Time), &provider);
    draw(&provider, &mut app, 20, 6);
    assert!(screen(&draw(&provider, &mut app, 20, 6)).contains("Time window"));
    let tiny = screen(&draw(&provider, &mut app, 19, 5));
    assert!(tiny.contains("terminal too small"), "{tiny}");
    assert!(!tiny.contains("Time window"), "{tiny}");
}

#[test]
fn every_dropdown_hangs_from_its_painted_field_with_a_one_row_gap() {
    for (width, height) in [(140u16, 40u16), (100, 30), (80, 24), (54, 16)] {
        let tag = format!("{width}x{height}");
        let (provider, mut app) = demo();
        draw(&provider, &mut app, width, height);
        app.handle(Action::Open(Open::Time), &provider);
        draw(&provider, &mut app, width, height);
        focus(&mut app, &provider, TimeControl::Window);
        key(&mut app, &provider, KeyCode::Enter);
        draw(&provider, &mut app, width, height);

        let rows = app.layers.time.choice_rects().to_vec();
        assert!(!rows.is_empty(), "{tag}: the Window dropdown paints");
        let field = app
            .layers
            .time
            .control_rects()
            .iter()
            .find_map(|(rect, control)| (*control == TimeControl::Window).then_some(*rect))
            .expect("the Window field paints");
        for (rect, _) in &rows {
            assert!(
                rect.right() <= width && rect.bottom() <= height,
                "{tag}: row {rect:?} escapes the area"
            );
        }
        // Max-eight rule and shared tiling: one row per item from the popup
        // origin, the same rects paint and hit-testing share.
        assert!(rows.len() <= 8, "{tag}: {} rows", rows.len());
        for (offset, (rect, _)) in rows.iter().enumerate() {
            assert_eq!(
                (rect.x, rect.y),
                (rows[0].0.x, rows[0].0.y + offset as u16),
                "{tag}: row {offset} not tiled"
            );
            assert_eq!(
                app.layers.time.hit((rect.x, rect.y)),
                Some(lvu::components::time::TimeHit::Choice(rows[offset].1)),
                "{tag}: row {rect:?} does not hit-test to its choice"
            );
        }
        // The rows start directly inside the popup border, so the gap assertion
        // below reads against the painted field without a popup accessor.
        let popup_top = rows[0].0.y.saturating_sub(1);
        let popup_bottom = rows.last().map(|(rect, _)| rect.y.saturating_add(2));
        let below = popup_top == field.bottom().saturating_add(1);
        let above = popup_bottom.is_some_and(|bottom| bottom.saturating_add(1) == field.y);
        assert!(
            below || above,
            "{tag}: dropdown rows {rows:?} keep no one-row gap to field {field:?}"
        );
    }

    // At the 20x6 floor a gap-honoring dropdown may be impossible; then the
    // dialog paints without it and the open dropdown survives for regrow,
    // rather than covering its own field.
    let (provider, mut app) = demo();
    draw(&provider, &mut app, 20, 6);
    app.handle(Action::Open(Open::Time), &provider);
    draw(&provider, &mut app, 20, 6);
    focus(&mut app, &provider, TimeControl::Window);
    key(&mut app, &provider, KeyCode::Enter);
    draw(&provider, &mut app, 20, 6);
    assert!(app.layers.time.state().dropdown.is_some());
    let rows = app.layers.time.choice_rects().to_vec();
    if !rows.is_empty() {
        let field = app
            .layers
            .time
            .control_rects()
            .iter()
            .find_map(|(rect, control)| (*control == TimeControl::Window).then_some(*rect))
            .expect("the Window field paints at 20x6");
        let popup_top = rows[0].0.y.saturating_sub(1);
        let popup_bottom = rows.last().map(|(rect, _)| rect.y.saturating_add(2));
        assert!(
            popup_top == field.bottom().saturating_add(1)
                || popup_bottom.is_some_and(|bottom| bottom.saturating_add(1) == field.y),
            "20x6: painted rows {rows:?} violate the gap to {field:?}"
        );
    }
    assert!(screen(&draw(&provider, &mut app, 20, 6)).contains("Time window"));
}

#[test]
fn every_focus_stop_paints_through_shared_projection_at_small_sizes() {
    // Tab order, paint and hit-test agree at sizes where the body scrolls. A
    // focused control either paints its own hitbox, or — for an action hidden
    // behind `More ▾` — paints the More ring instead: Tab never lands
    // invisibly.
    use ratatui::style::Modifier;

    let order = [
        TimeControl::Basis,
        TimeControl::Window,
        TimeControl::Gap,
        TimeControl::StartDate,
        TimeControl::StartClock,
        TimeControl::StartZoneMenu,
        TimeControl::EndDate,
        TimeControl::EndClock,
        TimeControl::EndZoneMenu,
        TimeControl::Apply,
        TimeControl::Clear,
        TimeControl::Recognize,
    ];
    // At 54x16 every action fits: no overflow is invented.
    let (provider, mut app) = demo();
    draw(&provider, &mut app, 54, 16);
    app.handle(Action::Open(Open::Time), &provider);
    draw(&provider, &mut app, 54, 16);
    assert!(
        app.layers.time.more_button().is_none(),
        "54x16 fits every action: no More button"
    );

    for (width, height) in [(54u16, 16u16), (20, 6)] {
        let tag = format!("{width}x{height}");
        let (provider, mut app) = demo();
        draw(&provider, &mut app, width, height);
        app.handle(Action::Open(Open::Time), &provider);
        draw(&provider, &mut app, width, height);
        let mut visited = vec![app.layers.time.state().focus];
        for _ in 0..order.len() {
            let buffer = draw(&provider, &mut app, width, height);
            let focused = app.layers.time.state().focus;
            let painted = app
                .layers
                .time
                .control_rects()
                .iter()
                .any(|(_, control)| *control == focused);
            if painted {
                // Segments show their caret once revealed; dropdowns and
                // buttons paint their own affordance instead.
                if matches!(
                    focused,
                    TimeControl::StartDate
                        | TimeControl::StartClock
                        | TimeControl::EndDate
                        | TimeControl::EndClock
                ) {
                    assert!(
                        app.layers.time.surface().caret.is_some(),
                        "{tag}: focused {focused:?} shows no caret"
                    );
                }
            } else {
                // Only an overflow-hidden action may lack its own hitbox, and
                // then the More button must carry the focus ring instead.
                let hidden_action = matches!(
                    focused,
                    TimeControl::Apply | TimeControl::Clear | TimeControl::Recognize
                );
                assert!(hidden_action, "{tag}: focused {focused:?} paints no hitbox");
                let more = app
                    .layers
                    .time
                    .more_button()
                    .expect("a hidden focused action implies a painted More button");
                let cell = &buffer[(more.x, more.y)];
                assert!(
                    cell.modifier.contains(Modifier::BOLD),
                    "{tag}: More carries no focus ring for hidden {focused:?}"
                );
            }
            key(&mut app, &provider, KeyCode::Tab);
            visited.push(app.layers.time.state().focus);
        }
        for control in order {
            assert!(
                visited.contains(&control),
                "{tag}: {control:?} never took focus"
            );
        }
        // Hidden overflow actions never trap Tab, but their mnemonics stay
        // live: Alt-T reaches Recognize even where its button cannot paint.
        if (width, height) == (20, 6) {
            assert!(
                !app.layers
                    .time
                    .control_rects()
                    .iter()
                    .any(|(_, control)| *control == TimeControl::Recognize),
                "Recognize paints no button at 20x6"
            );
            app.handle(
                Action::Raw(RawEvent::Key(KeyEvent::new(
                    KeyCode::Char('t'),
                    KeyModifiers::ALT,
                ))),
                &provider,
            );
            assert_eq!(app.layers.top(), Some(lvu::component::LayerId::Ask));
        }
    }
}

#[test]
fn wide_segment_text_keeps_exact_bytes_and_a_display_width_caret() {
    let (provider, mut app) = demo();
    draw(&provider, &mut app, 80, 24);
    app.handle(Action::Open(Open::Time), &provider);
    draw(&provider, &mut app, 80, 24);
    focus(&mut app, &provider, TimeControl::StartDate);
    // Empty the segment, then type across it: ASCII, wide CJK, combining.
    ctrl_key(&mut app, &provider, KeyCode::Char('a'));
    ctrl_key(&mut app, &provider, KeyCode::Char('k'));
    assert_eq!(app.layers.time.state().start_date, "");
    let mut columns = Vec::new();
    for character in ['a', '東', 'e', '́', 'x'] {
        key(&mut app, &provider, KeyCode::Char(character));
        draw(&provider, &mut app, 80, 24);
        columns.push(app.layers.time.surface().caret.expect("caret paints").0);
    }
    assert_eq!(
        app.layers.time.state().start_date,
        "a東éx",
        "segment bytes are preserved, not normalized"
    );
    let deltas: Vec<u16> = columns
        .windows(2)
        .map(|pair| pair[1].saturating_sub(pair[0]))
        .collect();
    assert_eq!(
        deltas,
        vec![2, 1, 0, 1],
        "segment caret advances by display width: {columns:?}"
    );
}
