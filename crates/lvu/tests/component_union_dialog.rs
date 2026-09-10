//! Acceptance for the Union dialog layer: open, toggle, create, dismiss.
//!
//! Drives the real `App` shell through the real palette-open action against
//! the fixture provider and asserts screens plus the outbox the shell drains.
//! The merge itself is covered in `lvu-view`; this pins the layer contract:
//! origin pre-selection, checklist toggling, default-action Create, and a
//! draft that survives rejection.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use lvu::{
    Action, App,
    component::{LayerId, Open, RawEvent},
    fixture::FixtureProvider,
    theme::Theme,
    ui,
};
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};

fn demo() -> (FixtureProvider, App) {
    let (provider, sources, views) = FixtureProvider::demo();
    let mut app = App::new(sources, views, true);
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

fn key(app: &mut App, provider: &FixtureProvider, code: KeyCode) {
    app.handle(
        Action::Raw(RawEvent::Key(KeyEvent::new(code, KeyModifiers::NONE))),
        provider,
    );
}

#[test]
fn union_dialog_lists_views_with_origin_preselected() {
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::Union(lvu::UnionOpen::Plain)), &provider);
    assert_eq!(app.layers.top(), Some(LayerId::Union));
    let text = screen(&draw(&provider, &mut app, 100, 24));
    assert!(text.contains("Union views"), "{text}");
    assert!(text.contains("All events"), "{text}");
    assert!(text.contains("Errors only"), "{text}");
    // The active view arrives pre-selected: one of two checked.
    assert!(text.contains("1 of 2 views selected"), "{text}");
    assert!(text.contains("[ Create union ]"), "{text}");
}

#[test]
fn toggle_then_create_queues_inputs_in_order() {
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::Union(lvu::UnionOpen::Plain)), &provider);
    key(&mut app, &provider, KeyCode::Down);
    key(&mut app, &provider, KeyCode::Char(' '));
    let text = screen(&draw(&provider, &mut app, 100, 24));
    assert!(text.contains("2 of 2 views selected"), "{text}");
    key(&mut app, &provider, KeyCode::Enter);
    let requests = app.layers.union.take_requests();
    assert_eq!(requests.len(), 1);
    let lvu::UnionDialogRequest::Create { inputs, shared_key } = &requests[0];
    assert!(shared_key.is_none());
    assert_eq!(inputs, &vec!["all".to_owned(), "errors".to_owned()]);
    // Creation stays open until the shell reports back: a second Enter must
    // not queue a duplicate candidate behind the first.
    key(&mut app, &provider, KeyCode::Enter);
    assert!(app.layers.union.take_requests().is_empty());
}

#[test]
fn create_with_one_input_reports_without_closing() {
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::Union(lvu::UnionOpen::Plain)), &provider);
    // Deselect the pre-selected origin: back to zero of two.
    key(&mut app, &provider, KeyCode::Char(' '));
    let text = screen(&draw(&provider, &mut app, 100, 24));
    assert!(text.contains("0 of 2 views selected"), "{text}");
    key(&mut app, &provider, KeyCode::Enter);
    assert_eq!(app.layers.top(), Some(LayerId::Union));
    assert!(app.layers.union.take_requests().is_empty());
    let text = screen(&draw(&provider, &mut app, 100, 24));
    assert!(text.contains("at least two"), "{text}");
}

#[test]
fn escape_closes_without_queueing() {
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::Union(lvu::UnionOpen::Plain)), &provider);
    assert_eq!(app.layers.top(), Some(LayerId::Union));
    app.handle(
        Action::Raw(RawEvent::Key(KeyEvent::new(
            KeyCode::Esc,
            KeyModifiers::NONE,
        ))),
        &provider,
    );
    assert_ne!(app.layers.top(), Some(LayerId::Union));
    assert!(app.layers.union.take_requests().is_empty());
}

#[test]
fn short_dialog_scrolls_highlight_and_mouse_uses_visible_global_index() {
    let (provider, mut app) = demo();
    let source_id = app.views()[0].source_id.clone();
    for index in 0..12 {
        app.add_view(lvu::ViewItem {
            id: format!("extra-{index}"),
            source_id: source_id.clone(),
            name: format!("Extra view {index}"),
        });
    }
    app.select_view("all");
    app.handle(Action::Open(Open::Union(lvu::UnionOpen::Plain)), &provider);
    for _ in 0..9 {
        key(&mut app, &provider, KeyCode::Down);
    }
    let buffer = draw(&provider, &mut app, 80, 12);
    let rendered = screen(&buffer);
    assert!(rendered.contains("Extra view 7"), "{rendered}");
    assert!(
        !rendered.contains("Errors only"),
        "viewport did not scroll: {rendered}"
    );
    let selected_bg = Theme::TERMINAL.selection_bg;
    assert!(
        (0..buffer.area.height).any(|y| {
            let line = (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>();
            line.contains("Extra view 7")
                && (0..buffer.area.width).any(|x| buffer[(x, y)].bg == selected_bg)
        }),
        "highlighted row is not visibly selected: {rendered}"
    );
    key(&mut app, &provider, KeyCode::Char(' '));

    let buffer = draw(&provider, &mut app, 80, 12);
    let (mouse_x, mouse_y) = (0..buffer.area.height)
        .find_map(|y| {
            let line = (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>();
            line.find("Extra view 7")
                .map(|x| (u16::try_from(x).unwrap(), y))
        })
        .expect("scrolled highlighted candidate");
    app.handle(
        Action::Raw(RawEvent::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: mouse_x,
            row: mouse_y,
            modifiers: KeyModifiers::NONE,
        })),
        &provider,
    );
    let after_mouse = screen(&draw(&provider, &mut app, 80, 12));
    assert!(
        after_mouse.contains("1 of 14 views selected"),
        "mouse hit must use the scrolled global index: {after_mouse}"
    );
    key(&mut app, &provider, KeyCode::Char(' '));
    key(&mut app, &provider, KeyCode::Enter);
    let requests = app.layers.union.take_requests();
    let lvu::UnionDialogRequest::Create { inputs, .. } = &requests[0];
    assert!(inputs.contains(&"extra-7".to_owned()));
}
