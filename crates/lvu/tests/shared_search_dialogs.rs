use lvu::{
    Action, App, QueryCompletion, QueryFailure, QueryPurpose,
    dialog_layout::{DialogClass, dialog_rect_for_class},
    fixture::FixtureProvider,
    theme::Theme,
    ui,
};
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer, layout::Rect};

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
    app.handle(Action::OpenSearch, &provider);
    assert!(
        draw(&provider, &mut app, 88, 20)
            .0
            .contains("No filter every record is shown"),
        "the empty state is still explicit"
    );

    app.handle(Action::EditorPaste("request".into()), &provider);
    app.handle(Action::SubmitDraft, &provider);
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

    app.handle(Action::EditorPaste(" [".into()), &provider);
    app.handle(Action::SubmitDraft, &provider);
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
    app.handle(Action::OpenAdvanced, &provider);
    app.handle(Action::EditorPaste("東京e\u{301}".into()), &provider);
    let (_, cursor) = draw(&provider, &mut app, 88, 20);
    let popup = dialog_rect_for_class(Rect::new(0, 0, 88, 20), DialogClass::S);
    let input_left = popup.x + 2;
    assert_eq!(
        cursor.x,
        input_left + 5,
        "wide and combining characters use terminal columns"
    );

    app.handle(Action::ToggleEditorCompletion, &provider);
    let (rendered, completion_cursor) = draw(&provider, &mut app, 88, 20);
    assert!(rendered.contains("↑/↓ select"), "{rendered}");
    assert_ne!(
        completion_cursor, cursor,
        "completion layer must own focus instead of repainting the editor caret"
    );
}
