use lvu::{
    Action, App, QueryCompletion, QueryFailure, QueryPurpose,
    component::{Open, RawEvent},
    fixture::FixtureProvider,
    theme::Theme,
    ui,
};
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};

/// A converted layer owns its keymap, so its input arrives raw (§6.4).
fn raw_key(code: crossterm::event::KeyCode) -> Action {
    Action::Raw(RawEvent::Key(crossterm::event::KeyEvent::new(
        code,
        crossterm::event::KeyModifiers::NONE,
    )))
}

fn demo() -> (FixtureProvider, App) {
    let (provider, sources, views) = FixtureProvider::demo();
    (provider, App::new(sources, views, true))
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

fn draw(
    provider: &FixtureProvider,
    app: &mut App,
    width: u16,
    height: u16,
) -> (String, ratatui::layout::Position) {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| ui::render_with_theme(frame, app, provider, Theme::LOVE_DARK, None))
        .unwrap();
    (
        screen(terminal.backend().buffer()),
        terminal.backend().cursor_position(),
    )
}

#[test]
fn search_distinguishes_applied_pending_error_and_retains_last_good() {
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::Search), &provider);
    assert!(
        draw(&provider, &mut app, 88, 20)
            .0
            .contains("No filter every record is shown"),
        "the empty state is still explicit"
    );

    app.handle(Action::Raw(RawEvent::Paste("request".into())), &provider);
    app.handle(raw_key(crossterm::event::KeyCode::Enter), &provider);
    let accepted = app.take_query_requests().pop().unwrap();
    assert!(draw(&provider, &mut app, 88, 20).0.contains("Updating"));
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: accepted.view_id.clone(),
        generation: accepted.generation,
        revision: accepted.revision,
        purpose: QueryPurpose::Search,
        result: Ok(()),
    }));
    assert!(
        draw(&provider, &mut app, 88, 20)
            .0
            .contains("Applied   request")
    );

    app.handle(Action::Raw(RawEvent::Paste(" [".into())), &provider);
    app.handle(raw_key(crossterm::event::KeyCode::Enter), &provider);
    let rejected = app.take_query_requests().pop().unwrap();
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: rejected.view_id,
        generation: rejected.generation,
        revision: rejected.revision,
        purpose: QueryPurpose::Search,
        result: Err(QueryFailure {
            purpose: QueryPurpose::Search,
            message: "invalid search regex".into(),
        }),
    }));
    let rendered = draw(&provider, &mut app, 72, 16).0;
    assert!(
        rendered.contains("Error     invalid search regex"),
        "{rendered}"
    );
    // The accepted filter still has to survive a failing draft; it is now part
    // of the single message sentence rather than a second status line.
    assert!(rendered.contains("last accepted request"), "{rendered}");
    assert!(
        !rendered.contains("Scroll status"),
        "short status must not claim overflow: {rendered}"
    );
    assert_eq!(
        app.hit_regions.dialog_scroll, None,
        "non-overflowing status must not expose a dead scroll hitbox"
    );
}

#[test]
fn unicode_search_caret_uses_display_columns_and_completion_owns_it() {
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::Advanced), &provider);
    app.handle(
        Action::Raw(RawEvent::Paste("東京e\u{301}".into())),
        &provider,
    );
    let (_, cursor) = draw(&provider, &mut app, 88, 20);
    let input_left = app.layers.filter.field_rect().x;
    assert_eq!(
        cursor.x,
        input_left + 5,
        "wide and combining characters use terminal columns"
    );

    app.handle(raw_key(crossterm::event::KeyCode::Tab), &provider);
    let (rendered, completion_cursor) = draw(&provider, &mut app, 88, 20);
    // §8.10: the completion popup carries no key footer; arrows are routine.
    assert!(!rendered.contains("↑/↓ select"), "{rendered}");
    assert_ne!(
        completion_cursor, cursor,
        "completion layer must own focus instead of repainting the editor caret"
    );
}
