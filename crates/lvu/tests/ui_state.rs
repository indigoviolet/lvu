use std::{cell::RefCell, collections::HashMap};

use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};
use lvu::{
    Action, App, DisplayRow, Focus, QueryCompletion, QueryRequest, RowId, RowPage, RowProvider,
    ViewportRequest,
    app::{MAX_EDITOR_BYTES, SourceItem, ViewItem, key_to_action},
    fixture::FixtureProvider,
    terminal::{QueryDispatcher, poll_query_completions, submit_query_requests},
    ui,
};
use pretty_assertions::assert_eq;
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};

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

fn demo() -> (FixtureProvider, App) {
    let (provider, sources, views) = FixtureProvider::demo();
    (provider, App::new(sources, views, true))
}

fn render<P: RowProvider>(provider: &P, app: &mut App, width: u16, height: u16) -> String {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("test terminal");
    terminal
        .draw(|frame| ui::render(frame, app, provider))
        .expect("render");
    screen(terminal.backend().buffer())
}

fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

#[test]
fn initial_layout_fills_viewport_and_layout_changes_keep_newest_visible() {
    let (provider, mut app) = demo();
    let initial = render(&provider, &mut app, 88, 24);
    assert!(initial.contains("fixture request 01 completed"));
    assert!(initial.contains("fixture request 16 completed"));
    assert_eq!(app.view_state().expect("state").viewport_height, 19);

    app.handle(Action::ToggleDetails, &provider);
    let details = render(&provider, &mut app, 88, 24);
    assert!(details.contains("fixture request 16 completed"));
    assert!(details.contains("stable display id: api:16"));
    assert_eq!(
        app.view_state().expect("state").selected,
        Some(RowId::new("api", 16))
    );

    let resized = render(&provider, &mut app, 88, 12);
    assert!(resized.contains("fixture request 16 completed"));
    assert!(resized.contains("stable display id: api:16"));
}

#[test]
fn navigation_keeps_stable_selection_and_per_view_state() {
    let (mut provider, mut app) = demo();
    app.sync_provider(&provider, 4);
    app.handle(Action::ToggleFollow, &provider);
    app.handle(Action::MoveLine(-2), &provider);
    let frozen = app.view_state().expect("state").selected.clone();
    let frozen_top = app.view_state().expect("state").top;

    provider.advance();
    app.sync_provider(&provider, 4);
    assert_eq!(app.view_state().expect("state").selected, frozen);
    assert_eq!(app.view_state().expect("state").top, frozen_top);

    app.handle(Action::NextView, &provider);
    app.sync_provider(&provider, 4);
    app.handle(Action::Top, &provider);
    assert_eq!(
        app.view_state().expect("state").selected,
        Some(RowId::new("worker", 4))
    );
    app.handle(Action::PreviousView, &provider);
    assert_eq!(app.view_state().expect("state").selected, frozen);
}

#[test]
fn drafts_and_async_results_are_independent_generation_fenced_and_bounded() {
    let (provider, mut app) = demo();
    app.handle(Action::OpenEditor, &provider);
    app.handle(Action::EditorPaste("first draft".into()), &provider);
    app.handle(Action::CancelEditor, &provider);
    app.handle(Action::OpenEditor, &provider);
    assert_eq!(app.query_state().expect("query").draft, "first draft");

    app.handle(Action::SubmitDraft, &provider);
    app.handle(Action::EditorInput('2'), &provider);
    app.handle(Action::SubmitDraft, &provider);
    let requests = app.take_query_requests();
    assert_eq!(requests.len(), 1, "submissions coalesce per view");
    let newest = requests[0].clone();
    assert_eq!(newest.generation, 2);

    assert!(!app.apply_query_completion(QueryCompletion {
        view_id: "all".into(),
        generation: 1,
        result: Ok("stale".into()),
    }));
    app.handle(Action::CancelEditor, &provider);
    app.handle(Action::NextView, &provider);
    app.handle(Action::OpenEditor, &provider);
    app.handle(Action::EditorPaste("errors draft".into()), &provider);
    assert_eq!(app.query_state().expect("query").draft, "errors draft");

    assert!(app.apply_query_completion(QueryCompletion {
        view_id: newest.view_id,
        generation: newest.generation,
        result: Ok(newest.draft.clone()),
    }));
    assert_eq!(app.active_view_id(), Some("errors"));
    assert_eq!(app.query_state().expect("query").last_applied, "");
    app.handle(Action::CancelEditor, &provider);
    app.handle(Action::PreviousView, &provider);
    assert_eq!(
        app.query_state().expect("query").last_applied,
        "first draft2"
    );

    app.handle(Action::OpenEditor, &provider);
    app.handle(
        Action::EditorPaste("x".repeat(MAX_EDITOR_BYTES + 100)),
        &provider,
    );
    assert_eq!(
        app.query_state().expect("query").draft.len(),
        MAX_EDITOR_BYTES
    );
}

#[derive(Default)]
struct DelayedDispatcher {
    submitted: Vec<QueryRequest>,
    ready: Vec<QueryCompletion>,
}

impl QueryDispatcher for DelayedDispatcher {
    fn submit(&mut self, request: QueryRequest) -> Result<(), String> {
        self.submitted.push(request);
        Ok(())
    }

    fn poll(&mut self) -> Option<QueryCompletion> {
        if self.ready.is_empty() {
            None
        } else {
            Some(self.ready.remove(0))
        }
    }
}

#[test]
fn delayed_dispatcher_does_not_block_actions_and_late_results_are_fenced() {
    let (provider, mut app) = demo();
    app.sync_provider(&provider, 5);
    app.handle(Action::OpenEditor, &provider);
    app.handle(Action::EditorPaste("slow".into()), &provider);
    app.handle(Action::SubmitDraft, &provider);
    let mut dispatcher = DelayedDispatcher::default();
    assert!(submit_query_requests(&mut app, &mut dispatcher));
    assert_eq!(dispatcher.submitted.len(), 1);
    assert!(!poll_query_completions(&mut app, &mut dispatcher));

    app.handle(Action::CancelEditor, &provider);
    app.handle(Action::Resize(55, 9), &provider);
    app.handle(Action::MoveLine(-1), &provider);
    assert_eq!(app.terminal_size, (55, 9));
    assert!(!app.should_quit);

    app.handle(Action::OpenEditor, &provider);
    app.handle(Action::EditorInput('2'), &provider);
    app.handle(Action::SubmitDraft, &provider);
    let stale = dispatcher.submitted[0].clone();
    submit_query_requests(&mut app, &mut dispatcher);
    dispatcher.ready.push(QueryCompletion {
        view_id: stale.view_id,
        generation: stale.generation,
        result: Err("late error".into()),
    });
    assert!(!poll_query_completions(&mut app, &mut dispatcher));
    assert!(app.query_state().expect("query").error.is_none());
    app.handle(Action::Quit, &provider);
    assert!(app.should_quit);
}

#[test]
fn pending_submission_queue_has_a_hard_limit() {
    let source = SourceItem {
        id: "source".into(),
        name: "Source".into(),
        health: "ok".into(),
    };
    let views = (0..33)
        .map(|index| ViewItem {
            id: format!("view-{index}"),
            source_id: "source".into(),
            name: format!("View {index}"),
        })
        .collect();
    let provider = EmptyProvider;
    let mut app = App::new(vec![source], views, false);
    app.handle(Action::OpenEditor, &provider);
    for index in 0..33 {
        app.handle(Action::SubmitDraft, &provider);
        if index < 32 {
            app.handle(Action::NextView, &provider);
        }
    }
    assert_eq!(app.take_query_requests().len(), 32);
    assert_eq!(
        app.query_state().expect("query").error.as_deref(),
        Some("query submission queue is full; draft was preserved")
    );
}

#[test]
fn empty_startup_is_actionable_and_navigation_safe() {
    let provider = EmptyProvider;
    let mut app = App::new(vec![], vec![], false);
    app.handle(Action::NextView, &provider);
    app.handle(Action::Top, &provider);
    app.handle(Action::ToggleFollow, &provider);
    let output = render(&provider, &mut app, 80, 18);
    assert!(output.contains("No view selected"));
    assert!(output.contains("add or discover a source"));
    assert_eq!(app.active_view_id(), None);
}

struct EmptyProvider;

impl RowProvider for EmptyProvider {
    fn page(&self, _: &str, _: ViewportRequest) -> RowPage {
        RowPage {
            total: 0,
            rows: vec![],
        }
    }
    fn row_by_id(&self, _: &str, _: &RowId) -> Option<DisplayRow> {
        None
    }
    fn index_of_id(&self, _: &str, _: &RowId) -> Option<usize> {
        None
    }
    fn revision(&self, _: &str) -> u64 {
        0
    }
}

#[test]
fn focus_and_hit_regions_route_sidebar_log_and_modal_mouse() {
    let (provider, mut app) = demo();
    render(&provider, &mut app, 88, 24);
    app.handle(Action::CycleFocus, &provider);
    assert_eq!(app.focus, Focus::Selector);
    app.handle(Action::SelectSidebar(1), &provider);
    assert_eq!(app.active_view_id(), Some("errors"));

    let all_region = app.hit_regions.sidebar_views[0].0;
    app.handle(
        Action::Mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            all_region.x,
            all_region.y,
        )),
        &provider,
    );
    assert_eq!(app.active_view_id(), Some("all"));
    render(&provider, &mut app, 88, 24);
    let rows = app.hit_regions.log_rows.expect("log rows");
    app.handle(
        Action::Mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            rows.x,
            rows.y,
        )),
        &provider,
    );
    assert_eq!(
        app.view_state().expect("state").selected,
        Some(RowId::new("api", 1))
    );

    let selected = app.view_state().expect("state").selected.clone();
    app.handle(
        Action::Mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            rows.x,
            rows.y - 1,
        )),
        &provider,
    );
    assert_eq!(
        app.view_state().expect("state").selected,
        selected,
        "table header is not a row"
    );
    app.handle(Action::ToggleHelp, &provider);
    app.handle(
        Action::Mouse(mouse(MouseEventKind::ScrollDown, rows.x, rows.y)),
        &provider,
    );
    assert_eq!(
        app.view_state().expect("state").selected,
        selected,
        "modal captures mouse"
    );
}

#[test]
fn small_dimensions_unicode_and_help_render() {
    let (provider, mut app) = demo();
    assert!(render(&provider, &mut app, 18, 4).contains("terminal too small"));
    render(&provider, &mut app, 70, 16);
    app.handle(Action::Top, &provider);
    app.handle(Action::MoveLine(2), &provider);
    let unicode = render(&provider, &mut app, 70, 16);
    assert!(unicode.contains("Unicode 東"), "{unicode}");
    assert!(unicode.contains("café e\u{301}"), "{unicode}");
    assert_eq!(ui::clipped_width("a東京b", 5), "a東京");
    app.handle(Action::ToggleHelp, &provider);
    assert!(render(&provider, &mut app, 70, 16).contains("left click exact row/view"));
}

#[test]
fn release_keys_are_ignored_and_selector_arrows_do_not_move_logs() {
    let released = KeyEvent {
        code: KeyCode::Down,
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Release,
        state: KeyEventState::NONE,
    };
    assert_eq!(key_to_action(released, Focus::Logs), Action::None);
    assert_eq!(
        key_to_action(
            KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
            Focus::Selector
        ),
        Action::SelectSidebar(1)
    );
}

struct CountingProvider {
    rows: HashMap<String, Vec<DisplayRow>>,
    requests: RefCell<Vec<ViewportRequest>>,
}

impl RowProvider for CountingProvider {
    fn page(&self, view_id: &str, request: ViewportRequest) -> RowPage {
        self.requests.borrow_mut().push(request);
        let rows = &self.rows[view_id];
        let start = request.start.min(rows.len());
        let end = start.saturating_add(request.len).min(rows.len());
        RowPage {
            total: rows.len(),
            rows: rows[start..end].to_vec(),
        }
    }
    fn row_by_id(&self, view_id: &str, id: &RowId) -> Option<DisplayRow> {
        self.rows[view_id].iter().find(|row| &row.id == id).cloned()
    }
    fn index_of_id(&self, view_id: &str, id: &RowId) -> Option<usize> {
        self.rows[view_id].iter().position(|row| &row.id == id)
    }
    fn revision(&self, _: &str) -> u64 {
        1
    }
}

#[test]
fn renderer_requests_only_viewport_rows() {
    let rows = (0..10_000)
        .map(|sequence| DisplayRow {
            id: RowId::new("large", sequence),
            timestamp: "00:00:00".into(),
            level: "INFO".into(),
            text: format!("row {sequence}"),
            details: vec![],
        })
        .collect();
    let provider = CountingProvider {
        rows: HashMap::from([("large-view".into(), rows)]),
        requests: RefCell::new(Vec::new()),
    };
    let mut app = App::new(
        vec![SourceItem {
            id: "large".into(),
            name: "Large".into(),
            health: "ok".into(),
        }],
        vec![ViewItem {
            id: "large-view".into(),
            source_id: "large".into(),
            name: "All".into(),
        }],
        false,
    );
    render(&provider, &mut app, 80, 20);
    let height = app.view_state().expect("state").viewport_height;
    assert!(
        provider
            .requests
            .borrow()
            .iter()
            .all(|request| request.len <= height.max(1))
    );
    assert!(
        provider
            .requests
            .borrow()
            .iter()
            .any(|request| request.len == height)
    );
}
