use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use lvu::{Action, App, RowProvider, app::InvestigationControl, fixture::FixtureProvider, ui};
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};

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

fn render<P: RowProvider>(provider: &P, app: &mut App, width: u16, height: u16) -> String {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("terminal");
    terminal
        .draw(|frame| ui::render(frame, app, provider))
        .expect("render");
    screen(terminal.backend().buffer())
}

#[test]
fn investigation_prompt_keeps_spaces_and_newlines_then_reaches_start() {
    let (provider, mut app) = demo();
    app.handle(Action::OpenInvestigation, &provider);
    for key in [
        KeyCode::Char('w'),
        KeyCode::Char('i'),
        KeyCode::Char('d'),
        KeyCode::Char('e'),
        KeyCode::Char(' '),
        KeyCode::Char('界'),
        KeyCode::Enter,
        KeyCode::Char('n'),
    ] {
        let action = app.key_to_action(KeyEvent::new(key, KeyModifiers::NONE));
        app.handle(action, &provider);
    }
    assert_eq!(
        app.investigation_dialog.as_ref().unwrap().input,
        "wide 界\nn"
    );
    app.handle(
        app.key_to_action(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)),
        &provider,
    );
    assert_eq!(
        app.investigation_dialog.as_ref().unwrap().focus,
        InvestigationControl::Submit
    );
    let rendered = render(&provider, &mut app, 48, 14);
    assert!(rendered.contains("[ Start ]"), "{rendered}");
}
#[test]
fn investigation_more_exists_only_for_real_overflow_and_normalizes_same_frame() {
    let (provider, mut app) = demo();
    app.handle(Action::OpenInvestigation, &provider);
    let dialog = app.investigation_dialog.as_mut().unwrap();
    dialog.stage = lvu::InvestigationStage::Conversation;
    dialog.focus = InvestigationControl::More;
    for index in 0..12 {
        dialog
            .messages
            .push_back(format!("activity {index}: {}", "wide detail ".repeat(10)));
    }
    let narrow = render(&provider, &mut app, 48, 14);
    assert!(narrow.contains("[ More ]"), "{narrow}");
    assert!(
        app.investigation_dialog
            .as_ref()
            .unwrap()
            .review_scroll_limit
            > 0
    );

    let dialog = app.investigation_dialog.as_mut().unwrap();
    dialog.messages.clear();
    dialog.items.clear();
    dialog.session_id = None;
    dialog.snapshot_dir = None;
    dialog.focus = InvestigationControl::More;
    let wide = render(&provider, &mut app, 120, 30);
    assert!(!wide.contains("[ More ]"), "{wide}");
    assert_eq!(
        app.investigation_dialog.as_ref().unwrap().focus,
        InvestigationControl::Submit
    );
}

#[test]
fn ask_kind_dropdown_q_and_escape_close_only_the_dropdown() {
    let (provider, mut app) = demo();
    app.handle(Action::OpenAskAi, &provider);
    app.handle(Action::EditorPaste("keep this prompt".into()), &provider);

    for code in [KeyCode::Char('q'), KeyCode::Esc] {
        app.handle(Action::OpenAskKind, &provider);
        assert!(app.ask_ai_dialog.as_ref().unwrap().kind_dropdown);
        for (modified, modifiers) in [
            (KeyCode::Up, KeyModifiers::SHIFT),
            (KeyCode::Down, KeyModifiers::CONTROL),
            (KeyCode::Enter, KeyModifiers::ALT),
        ] {
            assert_eq!(
                app.key_to_action(KeyEvent::new(modified, modifiers)),
                Action::None
            );
        }
        let dismiss = app.key_to_action(KeyEvent::new(code, KeyModifiers::NONE));
        assert_eq!(dismiss, Action::CancelEditor);
        app.handle(dismiss, &provider);
        let dialog = app.ask_ai_dialog.as_ref().unwrap();
        assert!(!dialog.kind_dropdown);
        assert_eq!(dialog.prompt, "keep this prompt");
    }
}
