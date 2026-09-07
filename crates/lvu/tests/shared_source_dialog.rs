use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use lvu::component::{LayerId, RawEvent};
use lvu::components::source::{SourceControl, SourceDialogMode};
use lvu::{
    Action, App, DiscoveryItem, DisplayRow, RowId, RowPage, RowProvider, SourceKind,
    ViewportRequest, ui,
};
use ratatui::{Terminal, backend::TestBackend};
use std::time::Duration;

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

/// Source owns its keymap now, so a test sends the key rather than the `Action`
/// the retired base table produced for it.
fn press(app: &mut App, code: KeyCode) {
    app.handle(
        Action::Raw(RawEvent::Key(KeyEvent::new(code, KeyModifiers::NONE))),
        &EmptyProvider,
    );
}

fn press_with(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    app.handle(
        Action::Raw(RawEvent::Key(KeyEvent::new(code, modifiers))),
        &EmptyProvider,
    );
}

fn click(app: &mut App, column: u16, row: u16) {
    app.handle(
        Action::Raw(RawEvent::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::NONE,
        })),
        &EmptyProvider,
    );
}

/// Tab until `control` has focus, the way a user reaches it.
fn focus(app: &mut App, control: SourceControl) {
    for _ in 0..16 {
        if app.layers.source.state().control == control {
            return;
        }
        press(app, KeyCode::Tab);
    }
    panic!("{control:?} never took focus");
}

fn complete(app: &mut App, candidates: &[&str]) {
    std::thread::sleep(Duration::from_millis(45));
    let request = app
        .take_path_completion_requests()
        .pop()
        .expect("automatic completion request");
    assert!(
        app.apply_path_completion_result(
            request.generation,
            &request.draft,
            None,
            candidates
                .iter()
                .map(|candidate| (*candidate).into())
                .collect(),
            None,
        )
    );
}

#[test]
fn q_remains_literal_in_source_input_during_scanning_and_ready_completion() {
    let mut app = App::new(vec![], vec![], false);

    press(&mut app, KeyCode::Char('q'));
    assert!(app.layers.source.state().path_completion.scanning);
    press(&mut app, KeyCode::Char('q'));
    assert_eq!(
        app.layers.source.state().draft,
        "qq",
        "q must remain input while automatic completion is scanning"
    );
    complete(&mut app, &["qq-result.log"]);
    press(&mut app, KeyCode::Char('q'));
    assert_eq!(
        app.layers.source.state().draft,
        "qqq",
        "q must remain input while completion candidates are visible"
    );

    // §5.3: the completion list is the innermost thing, so Escape closes it and
    // leaves the layer open on the draft.
    press(&mut app, KeyCode::Esc);
    let dialog = app.layers.source.state();
    assert!(dialog.path_completion.candidates.is_empty());
    assert_eq!(dialog.draft, "qqq");
    assert!(app.layers.source.is_open());

    press(&mut app, KeyCode::Tab);
    let dialog = app.layers.source.state();
    assert_eq!(dialog.control, SourceControl::Manual);
    assert!(dialog.controls_focused);
    // `q` on a non-input control is still a character the layer swallows, not a
    // dismissal: the surface reports a focused text field only for Input.
    let mut terminal = Terminal::new(TestBackend::new(90, 22)).unwrap();
    terminal
        .draw(|frame| ui::render(frame, &mut app, &EmptyProvider))
        .unwrap();
    press(&mut app, KeyCode::Char('q'));
    assert!(
        !app.layers.source.is_open(),
        "q on a non-input Source control retains one-layer dismissal"
    );
}

#[test]
fn input_arrows_select_automatic_file_results_and_enter_opens_the_file() {
    let mut app = App::new(vec![], vec![], false);
    press(&mut app, KeyCode::Char('a'));
    press(&mut app, KeyCode::Char('l'));
    assert!(
        app.take_path_completion_requests().is_empty(),
        "rapid edits remain inside the short debounce window"
    );
    complete(&mut app, &["alpha/", "alpine.log"]);
    assert_eq!(app.layers.source.state().draft, "al");

    press(&mut app, KeyCode::Down);
    assert_eq!(app.layers.source.state().path_completion.selected, 1);
    press(&mut app, KeyCode::Enter);
    let launch = app
        .take_source_requests()
        .pop()
        .expect("selected file launch");
    assert_eq!(launch.kind, SourceKind::File);
    assert_eq!(launch.text, "alpine.log");
}

#[test]
fn directory_enter_navigates_and_mouse_uses_rendered_suggestion_rows() {
    let mut app = App::new(vec![], vec![], false);
    press(&mut app, KeyCode::Char('n'));
    complete(&mut app, &["nested space/", "nested spare/"]);
    let mut terminal = Terminal::new(TestBackend::new(90, 22)).unwrap();
    terminal
        .draw(|frame| ui::render(frame, &mut app, &EmptyProvider))
        .unwrap();
    let second = app.layers.source.path_completion_rects()[1].0;
    click(&mut app, second.x + 1, second.y);
    assert_eq!(app.layers.source.state().path_completion.selected, 1);
    press(&mut app, KeyCode::Enter);
    assert!(app.take_source_requests().is_empty());
    assert_eq!(app.layers.source.state().draft, "nested spare/");
    std::thread::sleep(Duration::from_millis(45));
    assert_eq!(
        app.take_path_completion_requests()
            .pop()
            .expect("directory navigation refresh")
            .draft,
        "nested spare/"
    );
}

#[test]
fn fully_typed_directory_enter_waits_for_pending_children_instead_of_launching() {
    let mut app = App::new(vec![], vec![], false);
    for character in "nested/".chars() {
        press(&mut app, KeyCode::Char(character));
    }
    press(&mut app, KeyCode::Enter);
    assert!(app.take_source_requests().is_empty());
    assert_eq!(app.layers.source.state().draft, "nested/");
    std::thread::sleep(Duration::from_millis(45));
    assert_eq!(
        app.take_path_completion_requests()
            .pop()
            .expect("pending directory children request")
            .draft,
        "nested/"
    );
}

#[test]
fn discovery_input_arrows_select_and_enter_admits_without_open_focus() {
    let mut app = App::new(vec![], vec![], false);
    press_with(&mut app, KeyCode::Char('d'), KeyModifiers::CONTROL);
    let generation = match app.take_discovery_requests().pop().unwrap() {
        lvu::DiscoveryUiRequest::Scan { generation } => generation,
        other => panic!("unexpected request: {other:?}"),
    };
    assert!(app.apply_discovery_result(
        generation,
        vec![
            DiscoveryItem {
                key: "api".into(),
                label: "api.log".into(),
                detail: "/tmp/api.log".into(),
                status: "available".into(),
            },
            DiscoveryItem {
                key: "worker-a".into(),
                label: "worker-a.log".into(),
                detail: "/tmp/worker-a.log".into(),
                status: "available".into(),
            },
            DiscoveryItem {
                key: "worker-b".into(),
                label: "worker-b.log".into(),
                detail: "/tmp/worker-b.log".into(),
                status: "available".into(),
            },
        ],
        "complete".into(),
    ));
    assert_eq!(app.layers.source.state().mode, SourceDialogMode::Discovery);
    focus(&mut app, SourceControl::Refresh);
    let mut terminal = Terminal::new(TestBackend::new(90, 22)).unwrap();
    terminal
        .draw(|frame| ui::render(frame, &mut app, &EmptyProvider))
        .unwrap();
    let first_row = app.layers.source.discovery_rects()[0].0;
    click(&mut app, first_row.x + 1, first_row.y);
    assert_eq!(app.layers.source.state().control, SourceControl::Input);
    assert!(!app.layers.source.state().controls_focused);
    for character in "worker".chars() {
        press(&mut app, KeyCode::Char(character));
    }
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Enter);
    assert_eq!(
        app.take_discovery_requests(),
        vec![lvu::DiscoveryUiRequest::Select {
            generation,
            key: "worker-b".into(),
        }]
    );
}

#[test]
fn discovery_arrows_preserve_separate_diagnostics_scroll_focus() {
    let mut app = App::new(vec![], vec![], false);
    press_with(&mut app, KeyCode::Char('d'), KeyModifiers::CONTROL);
    let generation = match app.take_discovery_requests().pop().unwrap() {
        lvu::DiscoveryUiRequest::Scan { generation } => generation,
        other => panic!("unexpected request: {other:?}"),
    };
    // A report long enough to overflow the pane is what gives it a scroll
    // limit; the renderer records both the limit and the pane's rect.
    assert!(app.apply_discovery_result(
        generation,
        Vec::new(),
        "a bounded scan report long enough to overflow the diagnostics pane ".repeat(6),
    ));
    let mut terminal = Terminal::new(TestBackend::new(70, 18)).unwrap();
    terminal
        .draw(|frame| ui::render(frame, &mut app, &EmptyProvider))
        .unwrap();
    // Clicking the pane hands it the arrows, as `hit_regions.dialog_scroll` did.
    let pane = app
        .layers
        .source
        .scroll_rect()
        .expect("diagnostics surface");
    click(&mut app, pane.x, pane.y);
    press(&mut app, KeyCode::Down);
    assert_eq!(app.layers.source.state().discovery.status_scroll, 1);
    assert_eq!(app.layers.source.state().discovery.selected, 0);
}

#[test]
fn source_mode_controls_honor_ascii_agent_label() {
    let mut app = App::new(vec![], vec![], false);
    app.appearance.ascii = true;
    let mut terminal = Terminal::new(TestBackend::new(70, 18)).unwrap();
    terminal
        .draw(|frame| ui::render(frame, &mut app, &EmptyProvider))
        .unwrap();
    let rendered = (0..terminal.backend().buffer().area.height)
        .map(|y| {
            (0..terminal.backend().buffer().area.width)
                .map(|x| terminal.backend().buffer()[(x, y)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(rendered.contains("Agent"), "{rendered}");
    assert!(!rendered.contains("🧠"), "{rendered}");
}

#[test]
fn last_discovery_candidate_stays_visible_and_has_its_exact_row_hitbox() {
    let mut app = App::new(vec![], vec![], false);
    press_with(&mut app, KeyCode::Char('d'), KeyModifiers::CONTROL);
    let generation = match app.take_discovery_requests().pop().unwrap() {
        lvu::DiscoveryUiRequest::Scan { generation } => generation,
        other => panic!("unexpected request: {other:?}"),
    };
    let items = (0..20)
        .map(|index| DiscoveryItem {
            key: format!("candidate-{index:02}"),
            label: format!("candidate-{index:02}.log"),
            detail: format!("/tmp/candidate-{index:02}.log"),
            status: "available".into(),
        })
        .collect();
    assert!(app.apply_discovery_result(generation, items, "complete".into()));
    for _ in 0..19 {
        press(&mut app, KeyCode::Down);
    }

    let mut terminal = Terminal::new(TestBackend::new(70, 18)).unwrap();
    terminal
        .draw(|frame| ui::render(frame, &mut app, &EmptyProvider))
        .unwrap();
    let row = app
        .layers
        .source
        .discovery_rects()
        .iter()
        .find_map(|(row, index)| (*index == 19).then_some(*row))
        .expect("selected last candidate has a visible hitbox");
    let rendered_row = (row.x..row.right())
        .map(|x| terminal.backend().buffer()[(x, row.y)].symbol())
        .collect::<String>();
    assert!(rendered_row.contains("candidate-19.log"), "{rendered_row}");
}

#[test]
fn diagnostics_focus_changes_the_heading_without_recoloring_readable_body_text() {
    // dialog-system.md §8.7 retires the box around a pane, so focus is now
    // signalled on the pane heading instead of on a border. The invariant is
    // unchanged: focus must be visible and must not recolour the body text the
    // user has to read.
    let mut app = App::new(vec![], vec![], false);
    press_with(&mut app, KeyCode::Char('d'), KeyModifiers::CONTROL);
    let mut terminal = Terminal::new(TestBackend::new(70, 18)).unwrap();
    terminal
        .draw(|frame| ui::render(frame, &mut app, &EmptyProvider))
        .unwrap();
    let area = app
        .layers
        .source
        .scroll_rect()
        .expect("diagnostics surface");
    let heading = (area.x, area.y);
    let body = (area.x + 2, area.y + 1);
    let unfocused_body = terminal.backend().buffer()[body].fg;
    let unfocused_heading = terminal.backend().buffer()[heading].fg;

    // Clicking the pane is what hands it focus, as it always was.
    click(&mut app, area.x, area.y);
    terminal
        .draw(|frame| ui::render(frame, &mut app, &EmptyProvider))
        .unwrap();
    assert_eq!(terminal.backend().buffer()[body].fg, unfocused_body);
    assert_ne!(terminal.backend().buffer()[heading].fg, unfocused_heading);
}

#[test]
fn source_ai_review_scrolls_every_launch_detail_before_mouse_confirmation() {
    use lvu::{SourceAiPreview, SourceAiRequest};

    for (width, height) in [(140, 28), (54, 16)] {
        let mut app = App::new(vec![], vec![], false);
        app.handle(
            Action::Command(
                LayerId::Source,
                lvu::command_palette::CommandId::AskAiSource,
            ),
            &EmptyProvider,
        );
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| ui::render(frame, &mut app, &EmptyProvider))
            .unwrap();
        assert!(buffer_text(terminal.backend().buffer()).contains("Request"));
        assert!(app.layers.source.scroll_rect().is_none());
        assert!(
            app.layers
                .source
                .control_rects()
                .iter()
                .any(|(area, control)| *control == SourceControl::Input
                    && area.width > 0
                    && area.height > 0)
        );
        // The generation is the component's; a real submission produces it.
        for character in "follow the api service".chars() {
            press(&mut app, KeyCode::Char(character));
        }
        press(&mut app, KeyCode::Enter);
        let generation = match app.take_source_ai_requests().pop().expect("start request") {
            SourceAiRequest::Start { generation, .. } => generation,
            other => panic!("unexpected request: {other:?}"),
        };
        assert!(
            app.finish_source_ai(
                generation,
                Ok(SourceAiPreview {
                    name: "reviewed source".into(),
                    kind: "command".into(),
                    launch: "journalctl --follow --unit api.service".into(),
                    effective_path_or_cwd: "/srv/controlled application".into(),
                    restart: "on-failure with bounded delay".into(),
                    environment: (0..10)
                        .map(|index| format!("CONTROLLED_KEY_{index}=value-{index}"))
                        .collect(),
                    explanation: "selected from bounded local service discovery evidence".into(),
                })
            )
        );

        let mut observed = String::new();
        for _ in 0..32 {
            terminal
                .draw(|frame| ui::render(frame, &mut app, &EmptyProvider))
                .unwrap();
            let screen = buffer_text(terminal.backend().buffer());
            assert!(screen.contains("↑/↓"), "{width}x{height}\n{screen}");
            assert!(app.layers.source.scroll_rect().is_some());
            observed.push_str(&screen);
            if app.layers.source.state().ai.preview_scroll
                == app.layers.source.state().ai.preview_scroll_limit
            {
                break;
            }
            press(&mut app, KeyCode::Down);
        }
        for expected in [
            "Start reviewed",
            "Launch:",
            "journalctl",
            "Effective path/cwd:",
            "/srv/controlled",
            "Restart:",
            "CONTROLLED_KEY_0",
            "CONTROLLED_KEY_9",
            "Why:",
            "bounded local service",
        ] {
            assert!(
                observed.contains(expected),
                "missing {expected} at {width}x{height}: {observed}"
            );
        }
        assert!(app.take_source_requests().is_empty());
        assert!(app.take_source_ai_requests().is_empty());

        terminal
            .draw(|frame| ui::render(frame, &mut app, &EmptyProvider))
            .unwrap();
        let popup = app.hit_regions.selection_modal.expect("Source popup");
        let action = app
            .layers
            .source
            .control_rects()
            .iter()
            .find_map(|(area, control)| (*control == SourceControl::Input).then_some(*area))
            .expect("visible reviewed-source confirmation");
        assert!(popup.contains((action.x, action.y).into()));
        assert!(popup.contains((action.right() - 1, action.bottom() - 1).into()));
        click(&mut app, action.x, action.y);
        assert!(matches!(
            app.take_source_ai_requests().as_slice(),
            [SourceAiRequest::Apply { generation: applied }] if *applied == generation
        ));
    }
}

fn buffer_text(buffer: &ratatui::buffer::Buffer) -> String {
    (0..buffer.area.height)
        .map(|y| {
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}
