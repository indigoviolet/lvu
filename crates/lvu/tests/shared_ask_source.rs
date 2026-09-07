use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use lvu::component::Open;
use lvu::components::ask::AskOpen;
use lvu::{
    Action, App, RowProvider, app::InvestigationControl, component::Component,
    fixture::FixtureProvider, ui,
};
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};

fn demo() -> (FixtureProvider, App) {
    let (provider, sources, views) = FixtureProvider::demo();
    (provider, App::new(sources, views, true))
}

fn raw_key(code: KeyCode) -> Action {
    Action::Raw(lvu::component::RawEvent::Key(KeyEvent::new(
        code,
        KeyModifiers::NONE,
    )))
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
    // §12.18 retired the `More` button: the transcript is a pane, so it shows
    // a scrollbar and takes focus to be scrolled. The invariant is unchanged —
    // an overflowing transcript is reachable.
    assert!(
        narrow.contains('▼'),
        "the transcript shows a scrollbar:\n{narrow}"
    );
    assert!(!narrow.contains("[ More ]"), "{narrow}");
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
    assert!(
        !wide.contains('▼'),
        "nothing to scroll, so no scrollbar:\n{wide}"
    );
    assert_eq!(
        app.investigation_dialog.as_ref().unwrap().focus,
        InvestigationControl::Submit
    );
}

#[test]
fn ask_kind_dropdown_q_and_escape_close_only_the_dropdown() {
    // §10: dismissal reaches the innermost surface first. The kind list closes
    // and the layer stays, with the request the user typed intact.
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::Ask(AskOpen::Generic)), &provider);
    app.handle(
        Action::Raw(lvu::component::RawEvent::Paste("keep this prompt".into())),
        &provider,
    );
    render(&provider, &mut app, 120, 30);

    for code in [KeyCode::Char('q'), KeyCode::Esc] {
        // Walk to the Kind field rather than assuming where focus was left.
        for _ in 0..4 {
            if app.layers.ask.state().unwrap().focus == lvu::app::AskControl::Kind {
                break;
            }
            app.handle(raw_key(KeyCode::Tab), &provider);
        }
        app.handle(raw_key(KeyCode::Enter), &provider);
        assert!(app.layers.ask.state().unwrap().kind_dropdown);
        // An open list never takes a modified key as one of its own.
        for (modified, modifiers) in [
            (KeyCode::Up, KeyModifiers::SHIFT),
            (KeyCode::Down, KeyModifiers::CONTROL),
            (KeyCode::Enter, KeyModifiers::ALT),
        ] {
            app.handle(
                Action::Raw(lvu::component::RawEvent::Key(KeyEvent::new(
                    modified, modifiers,
                ))),
                &provider,
            );
            assert!(app.layers.ask.state().unwrap().kind_dropdown);
        }
        // The dropdown reports no text focus, so `q` dismisses it rather than
        // being typed into the request.
        render(&provider, &mut app, 120, 30);
        assert!(!app.layers.ask.text_focus());
        app.handle(raw_key(code), &provider);
        let dialog = app.layers.ask.state().expect("the layer stays open");
        assert!(!dialog.kind_dropdown);
        assert_eq!(dialog.prompt, "keep this prompt");
        assert_eq!(app.focus, lvu::Focus::Layer);
    }
}
