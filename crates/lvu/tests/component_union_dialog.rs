//! Acceptance for the Union dialog layer: open, toggle, create, dismiss.
//!
//! Drives the real `App` shell through the real palette-open action against
//! the fixture provider and asserts screens plus the outbox the shell drains.
//! The merge itself is covered in `lvu-view`; this pins the layer contract:
//! origin pre-selection, checklist toggling, default-action Create, and a
//! draft that survives rejection.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
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
    app.handle(Action::Open(Open::Union), &provider);
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
    app.handle(Action::Open(Open::Union), &provider);
    key(&mut app, &provider, KeyCode::Down);
    key(&mut app, &provider, KeyCode::Char(' '));
    let text = screen(&draw(&provider, &mut app, 100, 24));
    assert!(text.contains("2 of 2 views selected"), "{text}");
    key(&mut app, &provider, KeyCode::Enter);
    let requests = app.layers.union.take_requests();
    assert_eq!(requests.len(), 1);
    let lvu::UnionDialogRequest::Create { inputs } = &requests[0];
    assert_eq!(inputs, &vec!["all".to_owned(), "errors".to_owned()]);
    // Creation stays open until the shell reports back: a second Enter must
    // not queue a duplicate candidate behind the first.
    key(&mut app, &provider, KeyCode::Enter);
    assert!(app.layers.union.take_requests().is_empty());
}

#[test]
fn create_with_one_input_reports_without_closing() {
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::Union), &provider);
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
    app.handle(Action::Open(Open::Union), &provider);
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
