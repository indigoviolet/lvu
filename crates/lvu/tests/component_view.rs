//! Acceptance for the View layer as a component (docs/component-model.md §6.3
//! step 8). This is the first conversion that introduces an outbox for a
//! *view mutation* and the first that closes on a broadcast `ViewEvent`, so
//! what is asserted here is the seam rather than the drawing: a submission
//! leaves through the outbox and nowhere else, a refusal keeps the draft, the
//! shell's `SourcesChanged` broadcast is what closes the layer, and the
//! geometry `render` recorded is the geometry `hit()` answers with.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use lvu::{
    Action, App, RowProvider, SourceItem, ViewDialogMode,
    app::Focus,
    component::{Component, LayerId, Open, RawEvent},
    components::view::{ViewDialogControl, ViewHit},
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

fn alt(app: &mut App, provider: &FixtureProvider, code: KeyCode) {
    app.handle(
        Action::Raw(RawEvent::Key(KeyEvent::new(code, KeyModifiers::ALT))),
        provider,
    );
}

fn ctrl(app: &mut App, provider: &FixtureProvider, code: KeyCode) {
    app.handle(
        Action::Raw(RawEvent::Key(KeyEvent::new(code, KeyModifiers::CONTROL))),
        provider,
    );
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

fn type_name(app: &mut App, provider: &FixtureProvider, name: &str) {
    for _ in 0..64 {
        key(app, provider, KeyCode::Backspace);
    }
    for character in name.chars() {
        key(app, provider, KeyCode::Char(character));
    }
}

#[test]
fn a_submission_leaves_only_through_the_outbox_and_a_refusal_keeps_the_draft() {
    let (provider, mut app) = demo();
    let view = app.active_view_id().unwrap().to_owned();
    app.handle(Action::Open(Open::View), &provider);
    assert_eq!(app.focus, Focus::Layer);
    assert_eq!(app.layers.stack, vec![LayerId::View]);
    draw(&provider, &mut app, 90, 24);

    // An empty name never reaches the worker; it is refused where it was typed.
    type_name(&mut app, &provider, "   ");
    key(&mut app, &provider, KeyCode::Enter);
    assert!(app.layers.view.outbox.take().is_empty());
    assert_eq!(app.layers.view.error(), Some("view name cannot be empty"));
    assert!(app.layers.view.is_open(), "a refusal does not close it");

    type_name(&mut app, &provider, "Errors");
    key(&mut app, &provider, KeyCode::Enter);
    let requests = app.layers.view.outbox.take();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].name, "Errors");
    assert_eq!(requests[0].view_id, view);
    assert_eq!(requests[0].mode, ViewDialogMode::Clone);
    // Submitting is not closing: the layer waits for the worker's answer.
    assert!(app.layers.view.is_open());
    assert_eq!(app.layers.stack, vec![LayerId::View]);

    // The worker refuses. The draft survives so the name can be corrected.
    app.view_request_failed("a view with that name already exists".into());
    assert_eq!(
        app.layers.view.error(),
        Some("a view with that name already exists")
    );
    assert_eq!(app.layers.view.draft(), "Errors");
    assert!(app.layers.view.is_open());

    // The worker accepts. §4.2: the shell broadcasts what happened to the view
    // and the layer closes itself.
    app.view_request_succeeded(&view);
    assert!(!app.layers.view.is_open());
    assert!(app.layers.stack.is_empty());
    assert_eq!(app.focus, Focus::Logs);
}

#[test]
fn the_name_field_is_dialog_owned_and_only_it_takes_the_editing_chords() {
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::View), &provider);
    draw(&provider, &mut app, 90, 24);
    assert_eq!(app.layers.view.control(), ViewDialogControl::Input);
    assert!(
        app.layers.view.surface().text_focus,
        "a focused name field takes q as a character"
    );

    type_name(&mut app, &provider, "abc");
    ctrl(&mut app, &provider, KeyCode::Char('a'));
    key(&mut app, &provider, KeyCode::Char('X'));
    assert_eq!(app.layers.view.draft(), "Xabc");
    ctrl(&mut app, &provider, KeyCode::Char('e'));
    key(&mut app, &provider, KeyCode::Char('Z'));
    assert_eq!(app.layers.view.draft(), "XabcZ");
    // `q` is text while the field has focus, not a dismissal.
    key(&mut app, &provider, KeyCode::Char('q'));
    assert_eq!(app.layers.view.draft(), "XabcZq");
    assert!(app.layers.view.is_open());

    // Tab moves to a button; the chords are then inert and `q` dismisses.
    key(&mut app, &provider, KeyCode::Tab);
    assert_eq!(app.layers.view.control(), ViewDialogControl::Apply);
    draw(&provider, &mut app, 90, 24);
    assert!(!app.layers.view.surface().text_focus);
    ctrl(&mut app, &provider, KeyCode::Char('k'));
    assert_eq!(app.layers.view.draft(), "XabcZq", "a button does not edit");
    key(&mut app, &provider, KeyCode::Char('n'));
    assert_eq!(app.layers.view.draft(), "XabcZq");

    // A mode switch reseeds the draft from the view, so the caret follows it
    // rather than pointing into text that no longer exists.
    alt(&mut app, &provider, KeyCode::Char('b'));
    assert_eq!(app.layers.view.mode(), ViewDialogMode::Blank);
    assert_eq!(app.layers.view.draft(), "New view");
    key(&mut app, &provider, KeyCode::Char('!'));
    assert_eq!(app.layers.view.draft(), "New view!");
}

#[test]
fn membership_reads_the_shared_sources_and_hit_testing_matches_what_was_drawn() {
    let (provider, mut app) = demo();
    let view = app.active_view_id().unwrap().to_owned();
    let primary = app.view_source_ids(&view)[0].clone();
    for index in 0..4 {
        app.sources.push(SourceItem {
            id: format!("extra-{index}"),
            name: format!("extra source {index}"),
            health: "open".into(),
        });
    }
    app.handle(Action::Open(Open::View), &provider);
    alt(&mut app, &provider, KeyCode::Char('m'));
    let rendered = screen(&draw(&provider, &mut app, 94, 22));
    assert!(rendered.contains("Apply membership"), "{rendered}");
    assert!(rendered.contains("extra source 3"), "{rendered}");
    assert!(
        !app.layers.view.surface().text_focus,
        "a list is not a field"
    );

    // Every drawn row is hit-testable at the rect it was drawn in.
    for (rect, index) in app.layers.view.source_rects().to_vec() {
        assert_eq!(
            app.layers.view.hit((rect.x, rect.y)),
            Some(ViewHit::Source(index))
        );
    }
    let (rect, index) = *app.layers.view.source_rects().last().unwrap();
    click(&mut app, &provider, (rect.x, rect.y));
    assert_eq!(app.layers.view.selected_source(), index);

    key(&mut app, &provider, KeyCode::Char(' '));
    assert_eq!(
        app.layers.view.source_ids(),
        [primary.clone(), "extra-3".to_string()]
    );
    alt(&mut app, &provider, KeyCode::Up);
    assert_eq!(
        app.layers.view.source_ids(),
        ["extra-3".to_string(), primary.clone()]
    );

    // The owning source is not removable, and saying so is the dialog's job.
    app.layers.view.outbox.take();
    let owning = app
        .layers
        .view
        .source_rects()
        .iter()
        .find(|(_, index)| app.sources[*index].id == primary)
        .map(|(rect, _)| (rect.x, rect.y))
        .expect("the owning source is on screen");
    click(&mut app, &provider, owning);
    key(&mut app, &provider, KeyCode::Char(' '));
    assert_eq!(
        app.layers.view.error(),
        Some("the owning source stays in this view")
    );

    key(&mut app, &provider, KeyCode::Enter);
    let request = app.layers.view.outbox.take().pop().unwrap();
    assert_eq!(request.mode, ViewDialogMode::Sources);
    assert_eq!(request.source_ids, ["extra-3".to_string(), primary]);
}

#[test]
fn the_shell_contains_the_modal_and_a_button_click_acts_where_a_field_only_focuses() {
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::View), &provider);
    draw(&provider, &mut app, 90, 24);
    let surface = app.layers.view.surface();
    assert_eq!(app.hit_regions.selection_modal, Some(surface.interior));
    assert!(
        surface.caret.is_some(),
        "the focused name field draws a caret"
    );

    // §5.2: a click outside the popup never reaches the log behind it.
    let selected = app.view_state().unwrap().selected.clone();
    click(&mut app, &provider, (0, 0));
    assert!(app.layers.view.is_open());
    assert_eq!(app.view_state().unwrap().selected, selected);

    // A mode button acts on the click that focuses it; the field does not.
    let sources_button = app
        .layers
        .view
        .control_rects()
        .iter()
        .find(|(_, control)| *control == ViewDialogControl::Mode(ViewDialogMode::Sources))
        .map(|(rect, _)| (rect.x, rect.y))
        .expect("the Sources mode button is drawn");
    click(&mut app, &provider, sources_button);
    assert_eq!(app.layers.view.mode(), ViewDialogMode::Sources);
    assert_eq!(app.layers.view.control(), ViewDialogControl::Sources);
    assert!(
        app.layers.view.outbox.take().is_empty(),
        "choosing a mode is not submitting"
    );

    key(&mut app, &provider, KeyCode::Esc);
    assert!(!app.layers.view.is_open());
    assert_eq!(app.focus, Focus::Logs);
}

#[test]
fn a_layer_that_edits_the_active_view_does_not_open_without_one() {
    let (provider, sources, _) = FixtureProvider::demo();
    let mut app = App::new(sources, Vec::new(), true);
    assert!(app.active_view_id().is_none());
    app.handle(Action::Open(Open::View), &provider);
    // An empty workspace already has Add source on the stack, so what this
    // asserts is that View did not join it.
    assert!(
        !app.layers.stack.contains(&LayerId::View),
        "nothing to edit, nothing opens"
    );
    assert!(!app.layers.view.is_open());
}
