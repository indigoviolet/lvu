//! Acceptance for the Source layer (docs/component-model.md §6.3 step 11).
//!
//! What is asserted here is the seam the step introduces — four kinds of
//! background work over one `SourceRequest` outbox, drained by kind and with
//! the path-completion debounce travelling with the requests it gates — and the
//! contract every layer owes: the geometry `render` recorded is the geometry
//! `hit()` answers with, the shell's selection bound is the surface the
//! component published, and a click outside the popup is contained.

use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use lvu::{
    Action, App, DisplayRow, RowId, RowPage, RowProvider, SourceKind, ViewportRequest,
    component::{Component, LayerId, RawEvent},
    components::source::{SourceControl, SourceDialogMode, SourceHit},
    theme::Theme,
    ui,
};
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer, layout::Rect};

/// The four terminals dialog-system.md quotes; 54x16 is where the agent
/// proposal must still expose every field before an irreversible launch.
const SIZES: [(u16, u16); 4] = [(140, 40), (100, 30), (80, 24), (54, 16)];

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

fn draw(app: &mut App, width: u16, height: u16) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| ui::render_with_theme(frame, app, &EmptyProvider, Theme::TERMINAL, None))
        .unwrap();
    terminal.backend().buffer().clone()
}

fn key(app: &mut App, code: KeyCode) {
    app.handle(
        Action::Raw(RawEvent::Key(KeyEvent::new(code, KeyModifiers::NONE))),
        &EmptyProvider,
    );
}

fn key_with(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    app.handle(
        Action::Raw(RawEvent::Key(KeyEvent::new(code, modifiers))),
        &EmptyProvider,
    );
}

fn paste(app: &mut App, text: &str) {
    app.handle(Action::Raw(RawEvent::Paste(text.into())), &EmptyProvider);
}

fn click(app: &mut App, point: (u16, u16)) {
    app.handle(
        Action::Raw(RawEvent::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: point.0,
            row: point.1,
            modifiers: KeyModifiers::NONE,
        })),
        &EmptyProvider,
    );
}

fn contains(area: Rect, point: (u16, u16)) -> bool {
    point.0 >= area.x && point.0 < area.right() && point.1 >= area.y && point.1 < area.bottom()
}

/// An empty workspace opens on Add source, as it always has.
fn empty() -> App {
    App::new(Vec::new(), Vec::new(), false)
}

#[test]
fn an_empty_workspace_opens_on_the_source_layer() {
    let app = empty();
    assert_eq!(app.layers.stack_ids(), vec![LayerId::Source]);
    assert!(app.layers.source.is_open());
}

#[test]
fn every_recorded_control_was_painted_and_answers_the_hit_test() {
    for (width, height) in SIZES {
        let mut app = empty();
        paste(&mut app, "logs/");
        std::thread::sleep(Duration::from_millis(45));
        let request = app
            .take_path_completion_requests()
            .pop()
            .expect("typing schedules a scan");
        assert!(app.apply_path_completion_result(
            request.generation,
            &request.draft,
            None,
            vec!["logs/one.log".into(), "logs/two.log".into()],
            None,
        ));
        draw(&mut app, width, height);
        let surface = app.layers.source.surface();
        for (rect, control) in app.layers.source.control_rects() {
            assert!(
                contains(surface.popup, (rect.x, rect.y)),
                "{control:?} at {width}x{height} is outside what the layer drew"
            );
        }
        for (rect, index) in app.layers.source.path_completion_rects() {
            let point = (rect.x, rect.y);
            assert!(contains(surface.popup, point));
            assert_eq!(
                app.layers.source.hit(point),
                Some(SourceHit::PathCompletion(*index)),
                "a drawn suggestion row must answer its own hit test"
            );
        }
        assert_eq!(
            app.hit_regions.selection_modal,
            Some(surface.interior),
            "the shell's selection bound is the surface the component published"
        );
    }
}

/// §6.3 folds four request types into one queue. Each consumer drains its own
/// kind and leaves the others alone, which is what lets `lvu-app` keep four
/// independent loops (§8).
#[test]
fn one_outbox_serves_four_consumers_without_crossing_them() {
    let mut app = empty();
    // A launch and a discovery scan, queued together.
    paste(&mut app, "one.log");
    key(&mut app, KeyCode::Enter);
    key_with(&mut app, KeyCode::Char('d'), KeyModifiers::CONTROL);

    let discovery = app.take_discovery_requests();
    assert_eq!(discovery.len(), 1, "only the discovery request is drained");
    let launches = app.take_source_requests();
    assert_eq!(launches.len(), 1, "the launch was still queued");
    assert_eq!(launches[0].text, "one.log");
    assert!(app.take_source_requests().is_empty());
    assert!(app.take_discovery_requests().is_empty());
}

/// The debounce travels with the requests it gates: a scan is only handed over
/// once typing has paused, which is what keeps automatic completion from asking
/// on every keystroke.
#[test]
fn path_completion_is_debounced_and_generation_fenced() {
    let mut app = empty();
    paste(&mut app, "logs/a");
    assert!(
        app.take_path_completion_requests().is_empty(),
        "a scan inside the debounce window is withheld"
    );
    std::thread::sleep(Duration::from_millis(45));
    let first = app
        .take_path_completion_requests()
        .pop()
        .expect("the pause releases it");
    assert_eq!(first.draft, "logs/a");

    // A newer keystroke retires the request in flight.
    key(&mut app, KeyCode::Char('b'));
    assert_ne!(
        app.active_path_completion_generation(),
        Some(first.generation)
    );
    assert!(
        !app.apply_path_completion_result(
            first.generation,
            &first.draft,
            None,
            vec!["stale".into()],
            None,
        ),
        "a stale answer cannot land on a newer draft"
    );
    assert!(
        app.layers
            .source
            .state()
            .path_completion
            .candidates
            .is_empty()
    );
}

/// A command source never asks for paths: the scan is a file affordance, and
/// switching kind retires whatever was queued.
#[test]
fn a_command_source_asks_for_no_paths() {
    let mut app = empty();
    paste(&mut app, "journalctl -f");
    key_with(&mut app, KeyCode::Char('c'), KeyModifiers::ALT);
    assert_eq!(app.layers.source.state().kind, SourceKind::Command);
    std::thread::sleep(Duration::from_millis(45));
    assert!(app.take_path_completion_requests().is_empty());
    key(&mut app, KeyCode::Char('x'));
    std::thread::sleep(Duration::from_millis(45));
    assert!(app.take_path_completion_requests().is_empty());
}

/// §5.3: the innermost thing closes first. The suggestion list goes, then the
/// layer — and closing cancels the scan it started rather than leaking it.
#[test]
fn dismissal_closes_the_suggestions_then_the_layer_and_cancels_the_scan() {
    let mut app = empty();
    key_with(&mut app, KeyCode::Char('d'), KeyModifiers::CONTROL);
    assert_eq!(app.layers.source.state().mode, SourceDialogMode::Discovery);
    let scan = app.take_discovery_requests();
    assert_eq!(scan.len(), 1);
    key(&mut app, KeyCode::Esc);
    assert!(!app.layers.source.is_open());
    assert!(
        matches!(
            app.take_discovery_requests().as_slice(),
            [lvu::DiscoveryUiRequest::Cancel { .. }]
        ),
        "closing cancels the scan it started"
    );
    assert!(app.layers.top().is_none());
}

#[test]
fn clicks_outside_the_popup_are_contained() {
    let mut app = empty();
    paste(&mut app, "one.log");
    draw(&mut app, 140, 40);
    let popup = app.layers.source.surface().popup;
    let control = app.layers.source.state().control;
    click(&mut app, (0, 0));
    assert!(!contains(popup, (0, 0)));
    assert_eq!(app.layers.top(), Some(LayerId::Source));
    assert_eq!(app.layers.source.state().control, control);
    assert_eq!(app.layers.source.state().draft, "one.log");
}

/// A source the runtime refuses reopens the dialog on the draft that failed,
/// wherever the user had gone.
#[test]
fn a_refused_launch_reopens_the_dialog_on_the_draft_that_failed() {
    let mut app = empty();
    paste(&mut app, "missing.log");
    key(&mut app, KeyCode::Enter);
    let request = app.take_source_requests().pop().expect("launch");
    // The in-flight scan is the innermost thing, so it takes the first Escape.
    key(&mut app, KeyCode::Esc);
    assert!(app.layers.source.is_open());
    key(&mut app, KeyCode::Esc);
    assert!(!app.layers.source.is_open());

    app.source_request_failed(request, "no such file".into());
    assert_eq!(app.layers.top(), Some(LayerId::Source));
    assert_eq!(app.layers.source.state().draft, "missing.log");
    assert_eq!(
        app.layers.source.state().error.as_deref(),
        Some("no such file")
    );
}

/// Focus on the scrollable pane takes the field out of editing, which is what
/// hands it the arrows and makes `q` a dismissal there rather than a character.
#[test]
fn the_scroll_pane_takes_focus_from_the_field() {
    let mut app = empty();
    key_with(&mut app, KeyCode::Char('d'), KeyModifiers::CONTROL);
    let generation = match app.take_discovery_requests().pop().unwrap() {
        lvu::DiscoveryUiRequest::Scan { generation } => generation,
        other => panic!("unexpected request: {other:?}"),
    };
    assert!(app.apply_discovery_result(
        generation,
        Vec::new(),
        "a bounded scan report long enough to overflow the diagnostics pane ".repeat(6),
    ));
    draw(&mut app, 100, 28);
    assert!(app.layers.source.surface().text_focus);

    let pane = app
        .layers
        .source
        .scroll_rect()
        .expect("diagnostics surface");
    click(&mut app, (pane.x, pane.y));
    draw(&mut app, 100, 28);
    assert!(!app.layers.source.surface().text_focus);
    key(&mut app, KeyCode::Down);
    assert_eq!(app.layers.source.state().discovery.status_scroll, 1);
    assert_eq!(app.layers.source.state().discovery.selected, 0);

    // Clicking anything else gives the field its arrows back.
    let input = app
        .layers
        .source
        .control_rects()
        .iter()
        .find_map(|(rect, control)| (*control == SourceControl::Input).then_some(*rect))
        .expect("the field is clickable");
    click(&mut app, (input.x, input.y));
    draw(&mut app, 100, 28);
    assert!(app.layers.source.surface().text_focus);
}
