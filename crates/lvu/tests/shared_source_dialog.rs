use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use lvu::app::SourceControl;
use lvu::{
    Action, App, DiscoveryItem, DisplayRow, RowId, RowPage, RowProvider, SourceDialogMode,
    SourceKind, ViewportRequest, ui,
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

fn press(app: &mut App, code: KeyCode) -> Action {
    let action = app.key_to_action(KeyEvent::new(code, KeyModifiers::NONE));
    app.handle(action.clone(), &EmptyProvider);
    action
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

    assert_eq!(
        press(&mut app, KeyCode::Char('q')),
        Action::SourceInput('q')
    );
    assert!(app.source_dialog.as_ref().unwrap().path_completion.scanning);
    assert_eq!(
        press(&mut app, KeyCode::Char('q')),
        Action::SourceInput('q'),
        "q must remain input while automatic completion is scanning"
    );
    complete(&mut app, &["qq-result.log"]);
    assert_eq!(
        press(&mut app, KeyCode::Char('q')),
        Action::SourceInput('q'),
        "q must remain input while completion candidates are visible"
    );
    assert_eq!(app.source_dialog.as_ref().unwrap().draft, "qqq");

    assert_eq!(press(&mut app, KeyCode::Esc), Action::CancelEditor);
    let dialog = app.source_dialog.as_ref().expect("Source remains open");
    assert!(dialog.path_completion.candidates.is_empty());
    assert_eq!(dialog.draft, "qqq");

    assert_eq!(
        press(&mut app, KeyCode::Tab),
        Action::ToggleSourceControlFocus
    );
    let dialog = app.source_dialog.as_ref().expect("Source remains open");
    assert_eq!(dialog.control, SourceControl::Manual);
    assert!(dialog.controls_focused);
    assert_eq!(
        app.key_to_action(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE)),
        Action::CancelEditor,
        "q on a non-input Source control retains one-layer dismissal"
    );
}

#[test]
fn input_arrows_select_automatic_file_results_and_enter_opens_the_file() {
    let mut app = App::new(vec![], vec![], false);
    assert_eq!(
        press(&mut app, KeyCode::Char('a')),
        Action::SourceInput('a')
    );
    assert_eq!(
        press(&mut app, KeyCode::Char('l')),
        Action::SourceInput('l')
    );
    assert!(
        app.take_path_completion_requests().is_empty(),
        "rapid edits remain inside the short debounce window"
    );
    complete(&mut app, &["alpha/", "alpine.log"]);
    assert_eq!(app.source_dialog.as_ref().unwrap().draft, "al");

    assert_eq!(
        press(&mut app, KeyCode::Down),
        Action::MovePathCompletion(1)
    );
    assert_eq!(
        app.source_dialog.as_ref().unwrap().path_completion.selected,
        1
    );
    assert_eq!(
        app.key_to_action(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE)),
        Action::TextMoveLeft
    );
    assert_eq!(
        press(&mut app, KeyCode::Enter),
        Action::ActivateSourceControl
    );
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
    let second = app.hit_regions.path_completion_rows[1].0;
    app.handle(
        Action::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: second.x + 1,
            row: second.y,
            modifiers: KeyModifiers::NONE,
        }),
        &EmptyProvider,
    );
    assert_eq!(
        app.source_dialog.as_ref().unwrap().path_completion.selected,
        1
    );
    press(&mut app, KeyCode::Enter);
    assert!(app.take_source_requests().is_empty());
    assert_eq!(app.source_dialog.as_ref().unwrap().draft, "nested spare/");
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
    assert_eq!(
        press(&mut app, KeyCode::Enter),
        Action::ActivateSourceControl
    );
    assert!(app.take_source_requests().is_empty());
    assert_eq!(app.source_dialog.as_ref().unwrap().draft, "nested/");
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
    app.handle(Action::ToggleDiscovery, &EmptyProvider);
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
    assert_eq!(
        app.source_dialog.as_ref().unwrap().mode,
        SourceDialogMode::Discovery
    );
    app.source_dialog.as_mut().unwrap().control = lvu::app::SourceControl::Refresh;
    app.source_dialog.as_mut().unwrap().controls_focused = true;
    let mut terminal = Terminal::new(TestBackend::new(90, 22)).unwrap();
    terminal
        .draw(|frame| ui::render(frame, &mut app, &EmptyProvider))
        .unwrap();
    let first_row = app.hit_regions.discovery_rows[0].0;
    app.handle(
        Action::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: first_row.x + 1,
            row: first_row.y,
            modifiers: KeyModifiers::NONE,
        }),
        &EmptyProvider,
    );
    assert_eq!(
        app.source_dialog.as_ref().unwrap().control,
        lvu::app::SourceControl::Input
    );
    assert!(!app.source_dialog.as_ref().unwrap().controls_focused);
    for character in "worker".chars() {
        press(&mut app, KeyCode::Char(character));
    }
    assert_eq!(press(&mut app, KeyCode::Down), Action::MoveDiscovery(1));
    assert_eq!(
        press(&mut app, KeyCode::Enter),
        Action::ActivateSourceControl
    );
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
    app.handle(Action::ToggleDiscovery, &EmptyProvider);
    app.dialog_scroll_focused = true;
    if let Some(dialog) = &mut app.source_dialog {
        dialog.discovery.status_scroll_limit = 4;
    }
    let action = app.key_to_action(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    assert_eq!(action, Action::ModalVertical(1));
    app.handle(action, &EmptyProvider);
    assert_eq!(
        app.source_dialog.as_ref().unwrap().discovery.status_scroll,
        1
    );
    assert_eq!(app.source_dialog.as_ref().unwrap().discovery.selected, 0);
}

#[test]
fn source_mode_controls_honor_ascii_agent_label() {
    let mut app = App::new(vec![], vec![], false);
    app.ascii = true;
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
    app.handle(Action::ToggleDiscovery, &EmptyProvider);
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
    app.handle(Action::MoveDiscovery(19), &EmptyProvider);

    let mut terminal = Terminal::new(TestBackend::new(70, 18)).unwrap();
    terminal
        .draw(|frame| ui::render(frame, &mut app, &EmptyProvider))
        .unwrap();
    let row = app
        .hit_regions
        .discovery_rows
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
    app.handle(Action::ToggleDiscovery, &EmptyProvider);
    let mut terminal = Terminal::new(TestBackend::new(70, 18)).unwrap();
    terminal
        .draw(|frame| ui::render(frame, &mut app, &EmptyProvider))
        .unwrap();
    let area = app.hit_regions.dialog_scroll.expect("diagnostics surface");
    let heading = (area.x, area.y);
    let body = (area.x + 2, area.y + 1);
    let unfocused_body = terminal.backend().buffer()[body].fg;
    let unfocused_heading = terminal.backend().buffer()[heading].fg;

    app.dialog_scroll_focused = true;
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
        app.handle(Action::ToggleSourceAi, &EmptyProvider);
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| ui::render(frame, &mut app, &EmptyProvider))
            .unwrap();
        assert!(buffer_text(terminal.backend().buffer()).contains("Request"));
        assert!(app.hit_regions.dialog_scroll.is_none());
        assert!(
            app.hit_regions
                .source_controls
                .iter()
                .any(|(area, control)| *control == SourceControl::Input
                    && area.width > 0
                    && area.height > 0)
        );
        app.source_dialog.as_mut().unwrap().ai.generation = 17;
        assert!(
            app.finish_source_ai(
                17,
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
            assert!(app.hit_regions.dialog_scroll.is_some());
            observed.push_str(&screen);
            if app.source_dialog.as_ref().unwrap().ai.preview_scroll
                == app.source_dialog.as_ref().unwrap().ai.preview_scroll_limit
            {
                break;
            }
            assert_eq!(press(&mut app, KeyCode::Down), Action::ModalVertical(1));
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
            .hit_regions
            .source_controls
            .iter()
            .find_map(|(area, control)| (*control == SourceControl::Input).then_some(*area))
            .expect("visible reviewed-source confirmation");
        assert!(popup.contains((action.x, action.y).into()));
        assert!(popup.contains((action.right() - 1, action.bottom() - 1).into()));
        app.handle(
            Action::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: action.x,
                row: action.y,
                modifiers: KeyModifiers::NONE,
            }),
            &EmptyProvider,
        );
        assert!(matches!(
            app.take_source_ai_requests().as_slice(),
            [SourceAiRequest::Apply { generation: 17 }]
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
