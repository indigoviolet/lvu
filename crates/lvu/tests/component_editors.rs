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

fn modified_key(app: &mut App, provider: &FixtureProvider, code: KeyCode, modifiers: KeyModifiers) {
    app.handle(
        Action::Raw(RawEvent::Key(KeyEvent::new(code, modifiers))),
        provider,
    );
}

fn paste(app: &mut App, provider: &FixtureProvider, text: &str) {
    app.handle(Action::Raw(RawEvent::Paste(text.into())), provider);
}

fn click(app: &mut App, provider: &FixtureProvider, x: u16, y: u16) {
    app.handle(
        Action::Raw(RawEvent::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        })),
        provider,
    );
}

fn type_text(app: &mut App, provider: &FixtureProvider, text: &str) {
    for character in text.chars() {
        key(app, provider, KeyCode::Char(character));
    }
}

/// Every planned action/More rect must be full-size (at least its required
/// button width), inside the band, and pairwise disjoint: Ratatui squeezes
/// over-wide fixed Length constraints instead of refusing, so anything less
/// is a clipped label or a dead hitbox masquerading as geometry.
fn assert_action_rects_valid(
    band: ratatui::layout::Rect,
    buttons: &[(usize, ratatui::layout::Rect)],
    more: Option<ratatui::layout::Rect>,
    labels: &[&str],
    tag: &str,
) {
    use lvu::dialog_controls::{MORE_LABEL, button_width};
    let mut seen: Vec<ratatui::layout::Rect> = Vec::new();
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
            seen.iter().all(|prior: &ratatui::layout::Rect| {
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
            seen.iter().all(|prior: &ratatui::layout::Rect| {
                prior.x >= rect.right()
                    || rect.x >= prior.right()
                    || prior.y >= rect.bottom()
                    || rect.y >= prior.bottom()
            }),
            "{tag}: More at {rect:?} overlaps {seen:?}",
        );
    }
}

#[test]
fn a_refused_draft_is_kept_and_the_applied_view_stays_on_screen() {
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::Search), &provider);
    assert_eq!(app.focus, Focus::Layer);
    assert_eq!(app.layers.stack, vec![LayerId::Filter]);

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
    assert!(app.layers.filter.is_open(), "a refusal does not close it");
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
        app.layers.filter.is_open(),
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
    assert!(app.layers.filter.surface().text_focus);
    key(&mut app, &provider, KeyCode::Tab);
    assert!(app.layers.filter.completion().is_some());
    let rendered = screen(&draw(&provider, &mut app, 100, 28));
    assert!(rendered.contains("Complete field"), "{rendered}");
    let surface = app.layers.filter.surface();
    assert_eq!(app.hit_regions.selection_modal, Some(surface.interior));
    assert!(!surface.text_focus, "the popup takes q as a dismissal");
    key(&mut app, &provider, KeyCode::Esc);
    assert!(app.layers.filter.completion().is_none());
    assert!(
        app.layers.filter.is_open(),
        "the popup closed, not the dialog"
    );
    key(&mut app, &provider, KeyCode::Esc);
    assert!(!app.layers.filter.is_open());

    // Search has nothing to complete, so Tab hands the keys to the tab
    // control (there is no diagnostics pane to reach) and the field stops
    // taking characters until Tab hands them back.
    app.handle(Action::Open(Open::Search), &provider);
    paste(&mut app, &provider, "kept");
    key(&mut app, &provider, KeyCode::Tab);
    assert!(app.layers.filter.tabs_focused());
    assert!(!app.layers.filter.scroll_focused());
    assert!(app.layers.filter.completion().is_none());
    assert!(!app.layers.filter.surface().text_focus);
    type_text(&mut app, &provider, "xyz");
    key(&mut app, &provider, KeyCode::Backspace);
    assert_eq!(app.search_state().unwrap().draft, "kept");
    key(&mut app, &provider, KeyCode::Tab);
    assert!(!app.layers.filter.tabs_focused());
    type_text(&mut app, &provider, "!");
    assert_eq!(app.search_state().unwrap().draft, "kept!");
}

#[test]
fn a_completion_is_fenced_against_the_draft_and_caret_it_was_offered_for() {
    let (provider, mut app) = demo();
    draw(&provider, &mut app, 100, 28);
    app.handle(Action::Open(Open::Advanced), &provider);
    key(&mut app, &provider, KeyCode::Tab);
    let first = app.layers.filter.completion().unwrap().items[0]
        .insertion
        .clone();

    // Clicking a drawn row selects it, and Enter inserts it at the caret
    // without applying anything.
    let rendered = draw(&provider, &mut app, 100, 28);
    assert!(screen(&rendered).contains("Complete field"));
    let (rect, index) = app.layers.filter.completion_rects()[0];
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
    assert!(app.layers.filter.completion().is_none());

    // A draft that moved on invalidates the offer rather than inserting it
    // somewhere it no longer fits.
    key(&mut app, &provider, KeyCode::Tab);
    assert!(app.layers.filter.completion().is_some());
    type_text(&mut app, &provider, "x");
    assert!(app.layers.filter.completion().is_none());
}

#[test]
fn each_editor_keeps_its_own_scroll_caret_and_draft() {
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::Search), &provider);
    paste(&mut app, &provider, "needle");
    key(&mut app, &provider, KeyCode::Esc);

    app.handle(Action::Open(Open::Grouping), &provider);
    // The normal control opens on Run with a blank column: Run is the
    // primary configured path and legacy Auto is never the default.
    assert_eq!(
        app.view_state().unwrap().grouping.draft,
        lvu::grouping::run_rule(""),
        "grouping opens on Run with a blank column"
    );
    for code in [KeyCode::Home, KeyCode::Left, KeyCode::End] {
        key(&mut app, &provider, code);
        assert_eq!(
            app.view_state().unwrap().grouping.draft,
            lvu::grouping::run_rule("")
        );
    }
    modified_key(
        &mut app,
        &provider,
        KeyCode::Char('k'),
        KeyModifiers::CONTROL,
    );
    assert!(app.view_state().unwrap().grouping.draft.is_empty());

    // The segmented control reaches Run, Filter, Legacy and Off in either
    // direction. The legacy Auto token itself is never typed on screen: the
    // Legacy tab renders its static paragraph instead.
    key(&mut app, &provider, KeyCode::Tab);
    key(&mut app, &provider, KeyCode::Right);
    assert_eq!(
        app.view_state().unwrap().grouping.draft,
        lvu::grouping::run_rule("")
    );
    key(&mut app, &provider, KeyCode::Right);
    assert_eq!(
        app.view_state().unwrap().grouping.draft,
        lvu::grouping::filter_rule("")
    );
    key(&mut app, &provider, KeyCode::Right);
    assert_eq!(
        app.view_state().unwrap().grouping.draft,
        lvu::grouping::AUTO_GROUPING_TOKEN
    );
    assert!(!screen(&draw(&provider, &mut app, 100, 30)).contains("(?lvu:auto:"));
    key(&mut app, &provider, KeyCode::Right);
    assert!(app.view_state().unwrap().grouping.draft.is_empty());
    key(&mut app, &provider, KeyCode::Left);
    assert_eq!(
        app.view_state().unwrap().grouping.draft,
        lvu::grouping::AUTO_GROUPING_TOKEN
    );
    key(&mut app, &provider, KeyCode::BackTab);
    // The caret is the bank's and per draft, so moving it in one editor does
    // not disturb another's. Pasting into a blank Filter slot names the
    // column in place, keeping a well-formed rule.
    key(&mut app, &provider, KeyCode::Tab);
    key(&mut app, &provider, KeyCode::Left);
    key(&mut app, &provider, KeyCode::BackTab);
    paste(&mut app, &provider, "is_start");
    assert_eq!(
        app.view_state().unwrap().grouping.draft,
        lvu::grouping::filter_rule("is_start")
    );
    assert_eq!(app.search_state().unwrap().draft, "needle");

    // Clicking the already-selected Filter mode is idempotent, and temporary
    // Run/Off choices retain this view's exact legacy Auto draft without
    // applying any of them.
    draw(&provider, &mut app, 100, 30);
    let modes = app.layers.grouping.tab_rects().to_vec();
    assert_eq!(modes.len(), 4);
    let applied = app.view_state().unwrap().grouping.applied.clone();
    click(&mut app, &provider, modes[1].x + 1, modes[1].y);
    assert_eq!(
        app.view_state().unwrap().grouping.draft,
        lvu::grouping::filter_rule("is_start")
    );
    click(&mut app, &provider, modes[2].x + 1, modes[2].y);
    assert_eq!(
        app.view_state().unwrap().grouping.draft,
        lvu::grouping::AUTO_GROUPING_TOKEN
    );
    click(&mut app, &provider, modes[0].x + 1, modes[0].y);
    assert_eq!(
        app.view_state().unwrap().grouping.draft,
        lvu::grouping::run_rule("")
    );
    click(&mut app, &provider, modes[3].x + 1, modes[3].y);
    assert!(app.view_state().unwrap().grouping.draft.is_empty());
    click(&mut app, &provider, modes[2].x + 1, modes[2].y);
    assert_eq!(
        app.view_state().unwrap().grouping.draft,
        lvu::grouping::AUTO_GROUPING_TOKEN
    );
    assert_eq!(app.view_state().unwrap().grouping.applied, applied);
    key(&mut app, &provider, KeyCode::Esc);

    // Reopening restores the draft and the caret it was left at.
    app.handle(Action::Open(Open::Search), &provider);
    type_text(&mut app, &provider, "s");
    assert_eq!(app.search_state().unwrap().draft, "needles");
    assert_eq!(app.layers.filter.purpose(), QueryPurpose::Search);
    assert_eq!(app.layers.grouping.purpose(), QueryPurpose::Grouping);
}

#[test]
fn grouping_column_cycling_names_a_carried_column_and_keeps_a_valid_rule() {
    let (provider, sources, views) = FixtureProvider::json_demo();
    let mut app = App::new(sources, views, true);
    app.handle(Action::Open(Open::Grouping), &provider);
    assert_eq!(
        app.view_state().unwrap().grouping.draft,
        lvu::grouping::run_rule("")
    );
    // Down names the carried enrichment-like column in place; the rule stays
    // well-formed and applicable rather than becoming pasted text.
    key(&mut app, &provider, KeyCode::Down);
    assert_eq!(
        app.view_state().unwrap().grouping.draft,
        lvu::grouping::run_rule("request_id")
    );
    // A single offered column wraps onto itself instead of clearing.
    key(&mut app, &provider, KeyCode::Down);
    assert_eq!(
        app.view_state().unwrap().grouping.draft,
        lvu::grouping::run_rule("request_id")
    );
    key(&mut app, &provider, KeyCode::Up);
    assert_eq!(
        app.view_state().unwrap().grouping.draft,
        lvu::grouping::run_rule("request_id")
    );
    assert!(app.view_state().unwrap().grouping.applied.is_empty());
    key(&mut app, &provider, KeyCode::Esc);
}

#[test]
fn grouping_legacy_custom_text_survives_temporary_mode_choices() {
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::Grouping), &provider);
    // Reach Legacy Auto, then type: the token clears into a Custom draft.
    key(&mut app, &provider, KeyCode::Tab);
    key(&mut app, &provider, KeyCode::Right);
    key(&mut app, &provider, KeyCode::Right);
    assert_eq!(
        app.view_state().unwrap().grouping.draft,
        lvu::grouping::AUTO_GROUPING_TOKEN
    );
    key(&mut app, &provider, KeyCode::BackTab);
    type_text(&mut app, &provider, "^x");
    assert_eq!(app.view_state().unwrap().grouping.draft, "^x");
    // Temporary Run/Off choices retain the exact custom text unapplied.
    key(&mut app, &provider, KeyCode::Tab);
    key(&mut app, &provider, KeyCode::Left);
    assert_eq!(
        app.view_state().unwrap().grouping.draft,
        lvu::grouping::filter_rule("")
    );
    key(&mut app, &provider, KeyCode::Left);
    assert_eq!(
        app.view_state().unwrap().grouping.draft,
        lvu::grouping::run_rule("")
    );
    key(&mut app, &provider, KeyCode::Right);
    key(&mut app, &provider, KeyCode::Right);
    assert_eq!(app.view_state().unwrap().grouping.draft, "^x");
    assert!(app.view_state().unwrap().grouping.applied.is_empty());
    key(&mut app, &provider, KeyCode::Esc);
}

#[test]
fn grouping_off_click_applies_empty_and_ungroups() {
    use lvu::QueryCompletion;

    let (provider, mut app) = demo();
    // Apply a Run rule first so Off has something to clear.
    app.handle(Action::Open(Open::Grouping), &provider);
    paste(&mut app, &provider, "service");
    assert_eq!(
        app.view_state().unwrap().grouping.draft,
        lvu::grouping::run_rule("service")
    );
    key(&mut app, &provider, KeyCode::Enter);
    let request = app.take_query_requests().pop().unwrap();
    assert_eq!(request.purpose, QueryPurpose::Grouping);
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: request.view_id,
        generation: request.generation,
        revision: request.revision,
        purpose: request.purpose,
        result: Ok(()),
    }));
    assert_eq!(
        app.view_state().unwrap().grouping.applied,
        lvu::grouping::run_rule("service")
    );
    key(&mut app, &provider, KeyCode::Esc);

    // Clicking Off empties the draft; Enter applies the empty rule, which is
    // no grouping rather than a failed draft.
    app.handle(Action::Open(Open::Grouping), &provider);
    draw(&provider, &mut app, 100, 30);
    let modes = app.layers.grouping.tab_rects().to_vec();
    assert_eq!(modes.len(), 4);
    click(&mut app, &provider, modes[3].x + 1, modes[3].y);
    assert!(app.view_state().unwrap().grouping.draft.is_empty());
    key(&mut app, &provider, KeyCode::Enter);
    let request = app.take_query_requests().pop().unwrap();
    assert_eq!(request.purpose, QueryPurpose::Grouping);
    assert!(request.constraints.grouping.is_none());
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: request.view_id,
        generation: request.generation,
        revision: request.revision,
        purpose: request.purpose,
        result: Ok(()),
    }));
    assert!(app.view_state().unwrap().grouping.applied.is_empty());
    key(&mut app, &provider, KeyCode::Esc);
}

#[test]
fn an_editor_declines_to_open_without_a_view_and_dismisses_to_the_base_focus() {
    let (provider, sources, _) = FixtureProvider::demo();
    let mut app = App::new(sources, Vec::new(), true);
    for (open, layer) in [
        (Open::Search, LayerId::Filter),
        (Open::Advanced, LayerId::Filter),
        (Open::Grouping, LayerId::Grouping),
    ] {
        app.handle(Action::Open(open), &provider);
        // An empty workspace already has Add source on the stack, so what this
        // asserts is that the editor did not join it.
        assert!(!app.layers.stack.contains(&layer), "no view, no editor");
    }

    let (provider, mut app) = demo();
    for (open, layer) in [
        (Open::Search, LayerId::Filter),
        (Open::Advanced, LayerId::Filter),
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

#[test]
fn an_unbound_alt_chord_is_ignored_rather_than_typed() {
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::Search), &provider);
    paste(&mut app, &provider, "kept");

    // A terminal encodes Alt-<key> as ESC followed by the key's byte, so a
    // dismissal whose Esc lands in the same read as the next key arrives here
    // as one Alt chord. Inserting the bare character would swallow the Esc and
    // type the shortcut the user pressed — `test_search_race_pty` watched the
    // Search editor type `t` instead of closing so Time could open.
    app.handle(
        Action::Raw(RawEvent::Key(KeyEvent::new(
            KeyCode::Char('t'),
            KeyModifiers::ALT,
        ))),
        &provider,
    );
    assert_eq!(app.search_state().unwrap().draft, "kept");

    // Shift is not a chord: it is how the character was capitalized.
    app.handle(
        Action::Raw(RawEvent::Key(KeyEvent::new(
            KeyCode::Char('T'),
            KeyModifiers::SHIFT,
        ))),
        &provider,
    );
    assert_eq!(app.search_state().unwrap().draft, "keptT");
}

#[test]
fn completion_popup_recomputes_on_resize_and_never_goes_stale() {
    let (provider, mut app) = demo();
    draw(&provider, &mut app, 100, 28);
    app.handle(Action::Open(Open::Advanced), &provider);
    key(&mut app, &provider, KeyCode::Tab);
    assert!(app.layers.filter.completion().is_some());
    draw(&provider, &mut app, 100, 28);
    let roomy_popup = app.layers.filter.completion_popup();
    assert!(!roomy_popup.is_empty());

    // Shrink to a frame where neither band around the field holds even the
    // shrunken popup minimum with its one-row gap: the shared placement would
    // have to slide the popup over the field to stay in-area, so nothing is
    // painted instead. The fenced offer survives, no stale rect from the
    // roomy size leaks through, and no fabricated hitbox swallows clicks —
    // while the dialog itself stays usable.
    draw(&provider, &mut app, 20, 6);
    assert!(
        app.layers.filter.completion().is_some(),
        "the fenced offer survives; only gap-violating paint is refused"
    );
    assert!(
        !app.layers.filter.field_rect().is_empty(),
        "the dialog field still paints at 20x6"
    );
    assert!(
        app.layers.filter.completion_popup().is_empty(),
        "no popup that cannot keep its gap"
    );
    assert!(
        app.layers.filter.completion_rects().is_empty(),
        "no row hitboxes without a painted popup"
    );
    for x in 0..20 {
        for y in 0..6 {
            assert!(
                !matches!(
                    app.layers.filter.hit((x, y)),
                    Some(lvu::components::editors::EditorHit::Completion(_))
                ),
                "fabricated completion hit at ({x}, {y})"
            );
        }
    }

    // Growing back restores the roomy placement from the repainted field.
    draw(&provider, &mut app, 100, 28);
    assert_eq!(app.layers.filter.completion_popup(), roomy_popup);
}

#[test]
fn diagnostic_scroll_reveals_the_field_before_completion_opens() {
    let (provider, mut app) = demo();
    draw(&provider, &mut app, 54, 12);
    app.handle(Action::Open(Open::Advanced), &provider);
    paste(&mut app, &provider, "broken draft");
    key(&mut app, &provider, KeyCode::Enter);
    let request = app.take_query_requests().pop().unwrap();
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: request.view_id,
        generation: request.generation,
        revision: request.revision,
        purpose: request.purpose,
        result: Err(QueryFailure {
            purpose: request.purpose,
            message: format!("invalid expression: {}", "details ".repeat(40)),
        }),
    }));
    draw(&provider, &mut app, 54, 12);
    assert!(
        app.layers.filter.scroll_limit() > 0,
        "the long diagnostic overflows into a scrollable pane"
    );

    // Drive the diagnostics to the bottom: the field (body row 0) scrolls out
    // and genuinely paints nothing — the scrolled-out field this suite guards.
    // Tab walks Field → completion → completion → Tabs first, offering (and
    // dropping) popups along the way; only the fourth Tab reaches the pane.
    key(&mut app, &provider, KeyCode::Tab);
    key(&mut app, &provider, KeyCode::Tab);
    key(&mut app, &provider, KeyCode::Tab);
    assert!(app.layers.filter.tabs_focused());
    key(&mut app, &provider, KeyCode::Tab);
    assert!(app.layers.filter.scroll_focused());
    assert!(app.layers.filter.completion().is_none());
    for _ in 0..64 {
        key(&mut app, &provider, KeyCode::Down);
    }
    draw(&provider, &mut app, 54, 12);
    assert!(
        app.layers.filter.field_rect().is_empty(),
        "a fully scrolled pane paints no field rect"
    );

    // Tabbing back hands the keys to the field first, which the shared body
    // scroll reveals before anything may anchor to it; only then does the
    // next Tab offer completion against the freshly painted field.
    key(&mut app, &provider, KeyCode::Tab);
    draw(&provider, &mut app, 54, 12);
    let field = app.layers.filter.field_rect();
    assert!(!field.is_empty(), "returning to the field reveals it");
    assert!(app.layers.filter.completion().is_none());
    key(&mut app, &provider, KeyCode::Tab);
    assert!(app.layers.filter.completion().is_some());
    draw(&provider, &mut app, 54, 12);
    let popup = app.layers.filter.completion_popup();
    let field = app.layers.filter.field_rect();
    assert!(!popup.is_empty());
    let below = popup.y == field.bottom().saturating_add(1);
    let above = popup.bottom().saturating_add(1) == field.y;
    assert!(
        below || above,
        "popup {popup:?} has no one-row gap to revealed field {field:?}"
    );
    assert!(
        popup.right() <= 54 && popup.bottom() <= 12,
        "popup {popup:?} escapes the area"
    );
}

#[test]
fn wheel_interest_follows_real_overflow_not_an_unconditional_claim() {
    // A plain field with no diagnostics overflows nothing: no wheel target.
    let (provider, mut app) = demo();
    draw(&provider, &mut app, 100, 28);
    app.handle(Action::Open(Open::Search), &provider);
    draw(&provider, &mut app, 100, 28);
    assert!(!app.layers.filter.surface().scrollable);

    // A long rejected diagnostic overflows the body: wheel wanted.
    let (provider, mut app) = demo();
    draw(&provider, &mut app, 54, 12);
    app.handle(Action::Open(Open::Advanced), &provider);
    paste(&mut app, &provider, "broken draft");
    key(&mut app, &provider, KeyCode::Enter);
    let request = app.take_query_requests().pop().unwrap();
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: request.view_id,
        generation: request.generation,
        revision: request.revision,
        purpose: request.purpose,
        result: Err(QueryFailure {
            purpose: request.purpose,
            message: format!("invalid expression: {}", "details ".repeat(40)),
        }),
    }));
    draw(&provider, &mut app, 54, 12);
    assert!(app.layers.filter.scroll_limit() > 0);
    assert!(app.layers.filter.surface().scrollable);

    // An open completion whose rows all fit adds no wheel interest of its own.
    let (provider, mut app) = demo();
    draw(&provider, &mut app, 100, 28);
    app.handle(Action::Open(Open::Advanced), &provider);
    key(&mut app, &provider, KeyCode::Tab);
    draw(&provider, &mut app, 100, 28);
    assert!(app.layers.filter.completion().is_some());
    assert!(!app.layers.filter.completion_popup().is_empty());
    assert!(!app.layers.filter.surface().scrollable);

    // Squeeze the same open popup until it must scroll: interest returns.
    draw(&provider, &mut app, 20, 6);
    assert!(app.layers.filter.surface().scrollable);
}

#[test]
fn completion_popup_anchors_to_the_painted_field_with_a_one_row_gap() {
    let (provider, mut app) = demo();
    draw(&provider, &mut app, 100, 28);
    app.handle(Action::Open(Open::Advanced), &provider);
    key(&mut app, &provider, KeyCode::Tab);
    let items = app.layers.filter.completion().unwrap().items.len();
    assert!(items > 0, "the fixture offers completions to place");

    for (width, height) in [(100u16, 28u16), (80, 24), (54, 16)] {
        draw(&provider, &mut app, width, height);
        let popup = app.layers.filter.completion_popup();
        let field = app.layers.filter.field_rect();
        let rows = app.layers.filter.completion_rects().to_vec();
        let tag = format!("{width}x{height}");
        assert!(!field.is_empty(), "{tag}: the field paints");
        assert!(!popup.is_empty(), "{tag}: the popup paints");
        assert!(!rows.is_empty(), "{tag}: rows paint");
        // Frame-bounded: the whole popup, and every row, stays inside the
        // render area even at narrow widths.
        assert!(
            popup.right() <= width && popup.bottom() <= height,
            "{tag}: popup {popup:?} escapes the area"
        );
        for (rect, _) in &rows {
            assert!(
                rect.right() <= width && rect.bottom() <= height,
                "{tag}: row {rect:?} escapes the area"
            );
        }
        // Max-eight rule: the popup never shows more than eight item rows no
        // matter how many the sampler offered.
        assert!(rows.len() <= 8, "{tag}: {} rows", rows.len());
        assert!(rows.len() <= items, "{tag}: more rows than items");
        // One-row gap against the actual painted field: below preferred,
        // above when below cannot fit.
        let below = popup.y == field.bottom().saturating_add(1);
        let above = popup.bottom().saturating_add(1) == field.y;
        assert!(
            below || above,
            "{tag}: popup {popup:?} has no one-row gap to field {field:?}"
        );
        // Rows tile the popup viewport contiguously, one row per item, starting
        // directly inside the border — the same rects paint and hit-testing share.
        for (offset, (rect, _)) in rows.iter().enumerate() {
            assert_eq!(
                (rect.x, rect.y),
                (popup.x + 1, popup.y + 1 + offset as u16),
                "{tag}: row {offset} not tiled from the popup origin"
            );
        }
        // Every painted row hit-tests back to its own item.
        for (rect, index) in &rows {
            assert_eq!(
                app.layers.filter.hit((rect.x, rect.y)),
                Some(lvu::components::editors::EditorHit::Completion(*index)),
                "{tag}: row {rect:?} does not hit-test to item {index}"
            );
        }
    }

    // Wide: the popup shares the field's column while room allows, proving the
    // anchor is the painted field rect rather than a viewport-centered guess.
    draw(&provider, &mut app, 100, 28);
    assert_eq!(
        app.layers.filter.completion_popup().x,
        app.layers.filter.field_rect().x,
        "popup left-aligned with the field at roomy sizes"
    );
}

#[test]
fn prompt_frames_cover_the_size_matrix_without_invented_overflow() {
    for (width, height) in [(240u16, 80u16), (140, 40), (80, 24), (54, 16)] {
        let tag = format!("{width}x{height}");
        // Filter Search: title, frame and both verbs directly painted.
        let (provider, mut app) = demo();
        draw(&provider, &mut app, width, height);
        app.handle(Action::Open(Open::Search), &provider);
        draw(&provider, &mut app, width, height);
        let rendered = screen(&draw(&provider, &mut app, width, height));
        assert!(rendered.contains("Filter"), "{tag}:\n{rendered}");
        let frame = app.layers.filter.surface().popup;
        assert!(
            frame.right() <= width && frame.bottom() <= height,
            "{tag}: frame {frame:?} escapes the area"
        );
        assert!(
            app.layers.filter.more_button().is_none(),
            "{tag}: no overflow invented while both verbs fit"
        );
        assert_eq!(
            app.layers.filter.action_band().height,
            1,
            "{tag}: one action row, no dead row"
        );
        assert_action_rects_valid(
            app.layers.filter.action_band(),
            &[
                (0, app.layers.filter.action_rects()[0]),
                (1, app.layers.filter.action_rects()[1]),
            ],
            None,
            &["Apply", "&Clear"],
            &format!("{tag} Filter"),
        );
        assert!(rendered.contains("[ Apply ]"), "{tag}:\n{rendered}");
        assert!(rendered.contains("[ Clear ]"), "{tag}:\n{rendered}");
        for (rect, hit) in [
            (
                app.layers.filter.action_rects()[0],
                lvu::components::editors::EditorHit::Apply,
            ),
            (
                app.layers.filter.action_rects()[1],
                lvu::components::editors::EditorHit::Clear,
            ),
        ] {
            assert!(!rect.is_empty(), "{tag}: action {hit:?} paints");
            assert_eq!(app.layers.filter.hit((rect.x, rect.y)), Some(hit));
        }
        // Multiline grouping: title, frame and its single verb, no overflow.
        let (provider, mut app) = demo();
        draw(&provider, &mut app, width, height);
        app.handle(Action::Open(Open::Grouping), &provider);
        draw(&provider, &mut app, width, height);
        let rendered = screen(&draw(&provider, &mut app, width, height));
        assert!(
            rendered.contains("Multiline grouping"),
            "{tag}:\n{rendered}"
        );
        let frame = app.layers.grouping.surface().popup;
        assert!(
            frame.right() <= width && frame.bottom() <= height,
            "{tag}: frame {frame:?} escapes the area"
        );
        assert!(
            app.layers.grouping.more_button().is_none(),
            "{tag}: one verb never overflows"
        );
        let apply = app.layers.grouping.action_rects()[0];
        assert_action_rects_valid(
            app.layers.grouping.action_band(),
            &[(0, apply)],
            None,
            &["Apply"],
            &format!("{tag} Grouping"),
        );
        assert!(!apply.is_empty(), "{tag}: Apply paints");
        assert_eq!(
            app.layers.grouping.hit((apply.x, apply.y)),
            Some(lvu::components::editors::EditorHit::Apply)
        );
    }

    // The 20x6 floor keeps both Filter verbs directly painted: the floor
    // budgets (header1 + body1 + actions2, message/help dropped) hold a
    // two-row band, so Apply and Clear never need the overflow menu here.
    // Grouping's single verb still fits directly.
    let (provider, mut app) = demo();
    draw(&provider, &mut app, 20, 6);
    app.handle(Action::Open(Open::Search), &provider);
    draw(&provider, &mut app, 20, 6);
    assert!(screen(&draw(&provider, &mut app, 20, 6)).contains("Filter"));
    assert_eq!(app.layers.filter.action_band().height, 2);
    assert!(app.layers.filter.more_button().is_none());
    assert_action_rects_valid(
        app.layers.filter.action_band(),
        &[
            (0, app.layers.filter.action_rects()[0]),
            (1, app.layers.filter.action_rects()[1]),
        ],
        None,
        &["Apply", "&Clear"],
        "20x6 Filter floor",
    );
    let (provider, mut app) = demo();
    draw(&provider, &mut app, 20, 6);
    app.handle(Action::Open(Open::Grouping), &provider);
    draw(&provider, &mut app, 20, 6);
    // The 20-char title cannot spell itself out in a 20-wide frame, so assert
    // structure instead of text: the dialog resolves, Apply paints, no overflow.
    assert_eq!(
        app.layers.grouping.surface().popup,
        ratatui::layout::Rect::new(0, 0, 20, 6)
    );
    let apply = app.layers.grouping.action_rects()[0];
    assert!(!apply.is_empty(), "Grouping Apply paints at 20x6");
    assert_eq!(
        app.layers.grouping.hit((apply.x, apply.y)),
        Some(lvu::components::editors::EditorHit::Apply)
    );
    assert!(app.layers.grouping.more_button().is_none());

    // Below the floor the existing tiny fallback owns the screen instead.
    let (provider, mut app) = demo();
    draw(&provider, &mut app, 19, 5);
    app.handle(Action::Open(Open::Search), &provider);
    let tiny = screen(&draw(&provider, &mut app, 19, 5));
    assert!(tiny.contains("terminal too small"), "{tiny}");
    assert!(!tiny.contains("Filter"), "{tiny}");
}

#[test]
fn floor_action_pair_paints_directly_with_kept_two_row_band() {
    // At 20x6 the floor budgets (header1 + body1 + actions2, message/help
    // dropped) hold a two-row band, so Apply and Clear both paint full-size
    // with no overflow menu at all. The message row is gone, but the dialog
    // stays fully operable by mouse and key.
    let (provider, mut app) = demo();
    draw(&provider, &mut app, 20, 6);
    app.handle(Action::Open(Open::Search), &provider);
    type_text(&mut app, &provider, "needle");
    draw(&provider, &mut app, 20, 6);
    let rendered = screen(&draw(&provider, &mut app, 20, 6));
    assert!(rendered.contains("Filter"), "{rendered}");
    assert_eq!(app.layers.filter.action_band().height, 2);
    assert!(app.layers.filter.more_button().is_none());
    assert!(app.layers.filter.more_rows().is_empty());
    assert_action_rects_valid(
        app.layers.filter.action_band(),
        &[
            (0, app.layers.filter.action_rects()[0]),
            (1, app.layers.filter.action_rects()[1]),
        ],
        None,
        &["Apply", "&Clear"],
        "20x6 Filter floor",
    );
    assert!(rendered.contains("[ Apply ]"), "{rendered}");
    assert!(rendered.contains("[ Clear ]"), "{rendered}");
    // Both verbs work by mouse: Clear empties the draft, Apply submits it.
    let apply = app.layers.filter.action_rects()[0];
    let clear = app.layers.filter.action_rects()[1];
    click(&mut app, &provider, clear.x, clear.y);
    assert_eq!(app.search_state().unwrap().draft, "");
    type_text(&mut app, &provider, "needle");
    click(&mut app, &provider, apply.x, apply.y);
    assert_eq!(app.take_query_requests().len(), 1);
    assert!(app.layers.filter.is_open());
    // Keyboard mnemonics still resolve without any menu in the picture.
    key(&mut app, &provider, KeyCode::Esc);
    assert!(!app.layers.filter.is_open());
}

#[test]
fn action_budget_flips_exactly_where_the_pair_stops_fitting() {
    // One column changes the policy content width from 19 to 20 cells —
    // exactly the Apply+Clear two-button width — flipping the kept band
    // height with nothing else moving. This pins the area-aware budget
    // against hard-coded subtraction: a 2-cell estimate drift would put
    // both viewports on the same side of the boundary.
    for (width, tag, rows) in [(25u16, "narrow", 2), (26u16, "fits", 1)] {
        let (provider, mut app) = demo();
        draw(&provider, &mut app, width, 28);
        app.handle(Action::Open(Open::Search), &provider);
        draw(&provider, &mut app, width, 28);
        assert_eq!(
            app.layers.filter.action_band().height,
            rows,
            "{tag}: kept band height at {width}x28"
        );
        assert!(
            app.layers.filter.more_button().is_none(),
            "{tag}: no overflow"
        );
        assert_action_rects_valid(
            app.layers.filter.action_band(),
            &[
                (0, app.layers.filter.action_rects()[0]),
                (1, app.layers.filter.action_rects()[1]),
            ],
            None,
            &["Apply", "&Clear"],
            &format!("{tag} {width}x28"),
        );
        let rendered = screen(&draw(&provider, &mut app, width, 28));
        assert!(rendered.contains("[ Apply ]"), "{tag}:\n{rendered}");
        assert!(rendered.contains("[ Clear ]"), "{tag}:\n{rendered}");
    }
}

#[test]
fn floor_budgets_drop_message_help_only_where_they_must() {
    // 22x10 (content 16x6): the pair needs two rows, so message/help drop
    // and both verbs still paint directly. 22x12 (content 16x8): the full
    // chrome fits, so the help sentence paints while the band stays two.
    let (provider, mut app) = demo();
    draw(&provider, &mut app, 22, 10);
    app.handle(Action::Open(Open::Search), &provider);
    draw(&provider, &mut app, 22, 10);
    let floor = screen(&draw(&provider, &mut app, 22, 10));
    assert!(floor.contains("[ Apply ]"), "{floor}");
    assert!(floor.contains("[ Clear ]"), "{floor}");
    assert!(!floor.contains("Examples"), "help drops at 22x10:\n{floor}");
    assert_eq!(app.layers.filter.action_band().height, 2);

    let (provider, mut app) = demo();
    draw(&provider, &mut app, 22, 12);
    app.handle(Action::Open(Open::Search), &provider);
    draw(&provider, &mut app, 22, 12);
    let roomy = screen(&draw(&provider, &mut app, 22, 12));
    assert!(roomy.contains("[ Apply ]"), "{roomy}");
    assert!(roomy.contains("[ Clear ]"), "{roomy}");
    assert!(
        roomy.contains("Examples"),
        "help survives at 22x12:\n{roomy}"
    );
    assert_eq!(app.layers.filter.action_band().height, 2);
}
