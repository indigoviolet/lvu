//! Acceptance for the three query editors as components (docs/component-model.md
//! §6.3 step 7). This is the first conversion whose drafts are view-owned, so
//! what is asserted here is the seam and the two product invariants it carries:
//! an invalid draft leaves the applied view usable, and a canonical view forks
//! instead of filtering in place.

use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use lvu::{
    Action, App, QueryCompletion, QueryFailure, QueryPurpose, RowProvider,
    app::{Focus, SEARCH_DEBOUNCE},
    component::{Component, LayerId, Open, RawEvent},
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

fn paste(app: &mut App, provider: &FixtureProvider, text: &str) {
    app.handle(Action::Raw(RawEvent::Paste(text.into())), provider);
}

fn type_text(app: &mut App, provider: &FixtureProvider, text: &str) {
    for character in text.chars() {
        key(app, provider, KeyCode::Char(character));
    }
}

#[test]
fn a_refused_draft_is_kept_and_the_applied_view_stays_on_screen() {
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::Search), &provider);
    assert_eq!(app.focus, Focus::Layer);
    assert_eq!(app.layers.stack, vec![LayerId::Search]);

    paste(&mut app, &provider, "accepted");
    key(&mut app, &provider, KeyCode::Enter);
    let request = app.take_query_requests().pop().unwrap();
    assert_eq!(request.purpose, QueryPurpose::Search);
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: request.view_id,
        generation: request.generation,
        revision: request.revision,
        purpose: request.purpose,
        result: Ok(()),
    }));
    assert_eq!(app.search_state().unwrap().applied, "accepted");

    // A draft the worker refuses keeps both halves: the draft to correct, and
    // the accepted filter that is still describing the rows on screen.
    paste(&mut app, &provider, " and more");
    key(&mut app, &provider, KeyCode::Enter);
    let request = app.take_query_requests().pop().unwrap();
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: request.view_id,
        generation: request.generation,
        revision: request.revision,
        purpose: request.purpose,
        result: Err(QueryFailure {
            purpose: request.purpose,
            message: "invalid expression".into(),
        }),
    }));
    let editor = app.search_state().unwrap();
    assert_eq!(editor.draft, "accepted and more");
    assert_eq!(editor.applied, "accepted", "the applied view is untouched");
    assert_eq!(editor.error.as_deref(), Some("invalid expression"));
    let rendered = screen(&draw(&provider, &mut app, 120, 20));
    assert!(rendered.contains("Error"), "{rendered}");
    assert!(rendered.contains("invalid expression"), "{rendered}");
    assert!(
        rendered.contains("last accepted accepted"),
        "the message row still names the filter the rows on screen came from:\n{rendered}"
    );
    assert!(app.layers.search.is_open(), "a refusal does not close it");
}

#[test]
fn the_debounce_is_armed_by_typing_and_never_forks_a_canonical_view() {
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::Search), &provider);

    // The layer only arms the debounce; the shell fires it, so nothing is
    // queried until the deadline passes.
    type_text(&mut app, &provider, "err");
    assert!(!app.flush_debounced_searches(Instant::now()));
    assert!(app.take_query_requests().is_empty());
    assert!(app.flush_debounced_searches(Instant::now() + SEARCH_DEBOUNCE));
    let request = app.take_query_requests().pop().unwrap();
    assert_eq!(request.purpose, QueryPurpose::Search);

    // Applying explicitly supersedes an armed debounce rather than firing a
    // second identical query behind it.
    type_text(&mut app, &provider, "or");
    key(&mut app, &provider, KeyCode::Enter);
    assert_eq!(app.take_query_requests().len(), 1);
    assert!(!app.flush_debounced_searches(Instant::now() + SEARCH_DEBOUNCE * 4));
    assert!(app.take_query_requests().is_empty());
}

#[test]
fn applying_on_a_canonical_view_forks_and_leaves_the_editor_open() {
    let (provider, mut app) = demo();
    let canonical = app.active_view_id().unwrap().to_owned();
    app.set_view_role(&canonical, lvu::ViewRole::Canonical);
    let views_before = app.views().len();

    app.handle(Action::Open(Open::Search), &provider);
    // Typing on All events is only typing: the live search settles into
    // nothing rather than accumulating one derived view per pause.
    type_text(&mut app, &provider, "request");
    assert!(app.flush_debounced_searches(Instant::now() + SEARCH_DEBOUNCE));
    assert!(app.take_view_fork_requests().is_empty());
    assert!(app.take_query_requests().is_empty());
    assert_eq!(app.views().len(), views_before);
    assert_eq!(app.search_state().unwrap().draft, "request");

    // Applying it is what asks for a view, and it leaves the user in the
    // editor rather than dropping them back on the log.
    key(&mut app, &provider, KeyCode::Enter);
    assert_eq!(app.take_view_fork_requests().len(), 1);
    assert!(
        app.layers.search.is_open(),
        "an accepted submission that forked keeps the layer open"
    );
    assert_eq!(app.focus, Focus::Layer);
    assert!(
        app.search_state().unwrap().applied.is_empty(),
        "All events is never filtered in place"
    );
}

#[test]
fn tab_reaches_completion_on_advanced_and_the_diagnostics_pane_elsewhere() {
    let (provider, mut app) = demo();
    draw(&provider, &mut app, 100, 28);

    // Advanced completes; the popup owns the modal bound and `q` while it is
    // open, and Escape closes it before the dialog.
    app.handle(Action::Open(Open::Advanced), &provider);
    assert!(app.layers.advanced.surface().text_focus);
    key(&mut app, &provider, KeyCode::Tab);
    assert!(app.layers.advanced.completion().is_some());
    let rendered = screen(&draw(&provider, &mut app, 100, 28));
    assert!(rendered.contains("Complete field"), "{rendered}");
    let surface = app.layers.advanced.surface();
    assert_eq!(app.hit_regions.selection_modal, Some(surface.interior));
    assert!(!surface.text_focus, "the popup takes q as a dismissal");
    key(&mut app, &provider, KeyCode::Esc);
    assert!(app.layers.advanced.completion().is_none());
    assert!(
        app.layers.advanced.is_open(),
        "the popup closed, not the dialog"
    );
    key(&mut app, &provider, KeyCode::Esc);
    assert!(!app.layers.advanced.is_open());

    // Search has nothing to complete, so Tab hands the arrows to the pane and
    // the field stops taking characters until Tab hands them back.
    app.handle(Action::Open(Open::Search), &provider);
    paste(&mut app, &provider, "kept");
    key(&mut app, &provider, KeyCode::Tab);
    assert!(app.layers.search.scroll_focused());
    assert!(app.layers.search.completion().is_none());
    assert!(!app.layers.search.surface().text_focus);
    type_text(&mut app, &provider, "xyz");
    key(&mut app, &provider, KeyCode::Backspace);
    assert_eq!(app.search_state().unwrap().draft, "kept");
    key(&mut app, &provider, KeyCode::Tab);
    assert!(!app.layers.search.scroll_focused());
    type_text(&mut app, &provider, "!");
    assert_eq!(app.search_state().unwrap().draft, "kept!");
}

#[test]
fn a_completion_is_fenced_against_the_draft_and_caret_it_was_offered_for() {
    let (provider, mut app) = demo();
    draw(&provider, &mut app, 100, 28);
    app.handle(Action::Open(Open::Advanced), &provider);
    key(&mut app, &provider, KeyCode::Tab);
    let first = app.layers.advanced.completion().unwrap().items[0]
        .insertion
        .clone();

    // Clicking a drawn row selects it, and Enter inserts it at the caret
    // without applying anything.
    let rendered = draw(&provider, &mut app, 100, 28);
    assert!(screen(&rendered).contains("Complete field"));
    let (rect, index) = app.layers.advanced.completion_rects()[0];
    assert_eq!(index, 0);
    app.handle(
        Action::Raw(RawEvent::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: rect.x,
            row: rect.y,
            modifiers: KeyModifiers::NONE,
        })),
        &provider,
    );
    key(&mut app, &provider, KeyCode::Enter);
    assert_eq!(app.advanced_state().unwrap().draft, first);
    assert!(
        app.take_query_requests().is_empty(),
        "completing is not applying"
    );
    assert!(app.layers.advanced.completion().is_none());

    // A draft that moved on invalidates the offer rather than inserting it
    // somewhere it no longer fits.
    key(&mut app, &provider, KeyCode::Tab);
    assert!(app.layers.advanced.completion().is_some());
    type_text(&mut app, &provider, "x");
    assert!(app.layers.advanced.completion().is_none());
}

#[test]
fn each_editor_keeps_its_own_scroll_caret_and_draft() {
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::Search), &provider);
    paste(&mut app, &provider, "needle");
    key(&mut app, &provider, KeyCode::Esc);

    app.handle(Action::Open(Open::Grouping), &provider);
    assert_eq!(
        app.view_state().unwrap().grouping.draft,
        r"^(\s+|Caused by:)",
        "grouping opens on the rule that matches indented continuations"
    );
    // The caret is the bank's and per draft, so moving it in one editor does
    // not disturb another's.
    key(&mut app, &provider, KeyCode::Left);
    type_text(&mut app, &provider, "!");
    assert!(app.view_state().unwrap().grouping.draft.ends_with("!)"));
    assert_eq!(app.search_state().unwrap().draft, "needle");
    key(&mut app, &provider, KeyCode::Esc);

    // Reopening restores the draft and the caret it was left at.
    app.handle(Action::Open(Open::Search), &provider);
    type_text(&mut app, &provider, "s");
    assert_eq!(app.search_state().unwrap().draft, "needles");
    assert_eq!(app.layers.search.purpose(), QueryPurpose::Search);
    assert_eq!(app.layers.grouping.purpose(), QueryPurpose::Grouping);
}

#[test]
fn an_editor_declines_to_open_without_a_view_and_dismisses_to_the_base_focus() {
    let (provider, sources, _) = FixtureProvider::demo();
    let mut app = App::new(sources, Vec::new(), true);
    for (open, layer) in [
        (Open::Search, LayerId::Search),
        (Open::Advanced, LayerId::Advanced),
        (Open::Grouping, LayerId::Grouping),
    ] {
        app.handle(Action::Open(open), &provider);
        // An empty workspace already has Add source on the stack, so what this
        // asserts is that the editor did not join it.
        assert!(!app.layers.stack.contains(&layer), "no view, no editor");
    }

    let (provider, mut app) = demo();
    for (open, layer) in [
        (Open::Search, LayerId::Search),
        (Open::Advanced, LayerId::Advanced),
        (Open::Grouping, LayerId::Grouping),
    ] {
        app.focus = Focus::Selector;
        app.handle(Action::Open(open), &provider);
        assert_eq!(app.layers.stack, vec![layer]);
        draw(&provider, &mut app, 80, 24);
        key(&mut app, &provider, KeyCode::Esc);
        assert!(app.layers.stack.is_empty());
        assert_eq!(app.focus, Focus::Selector, "the push captured the sidebar");
    }
}
