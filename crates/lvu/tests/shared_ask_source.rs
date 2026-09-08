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
    app.handle(Action::Open(Open::Investigation), &provider);
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
        app.handle(raw_key(key), &provider);
    }
    assert_eq!(
        app.layers.investigation.state().unwrap().input,
        "wide 界\nn"
    );
    app.handle(raw_key(KeyCode::Tab), &provider);
    assert_eq!(
        app.layers.investigation.state().unwrap().focus,
        InvestigationControl::Submit
    );
    let rendered = render(&provider, &mut app, 48, 14);
    assert!(rendered.contains("[ Start ]"), "{rendered}");
}

/// Drives the transcript through the real seams — start, ready, agent events —
/// rather than reaching into the dialog, which the layer no longer permits.
fn conversation(app: &mut App, provider: &FixtureProvider, messages: &[String]) {
    app.handle(Action::Open(Open::Investigation), provider);
    app.handle(
        Action::Raw(lvu::component::RawEvent::Paste("why".into())),
        provider,
    );
    app.handle(raw_key(KeyCode::Tab), provider);
    app.handle(raw_key(KeyCode::Enter), provider);
    let request = app
        .take_investigation_requests()
        .pop()
        .expect("start request");
    let lvu::app::InvestigationRequest::Start {
        generation,
        view_id,
        ..
    } = request
    else {
        panic!("start request")
    };
    assert!(app.investigation_ready(
        generation,
        lvu::app::InvestigationItem {
            id: "investigation-1".into(),
            view_id,
            session_id: "session-1".into(),
            snapshot_dir: "/tmp/investigation-1".into(),
            manifest_path: "/tmp/investigation-1/manifest.json".into(),
            question: "why".into(),
        },
    ));
    for message in messages {
        assert!(app.push_investigation_event("session-1", message.clone(), Ok(())));
    }
}

/// Tab around to the transcript, which is focusable only while it overflows —
/// and only *after* a frame has found it overflowing, because the scroll limit
/// the control list keys off is settled by `render`.
///
/// Bounded on purpose. An earlier `while focus != More { Tab }` here spun a
/// test binary at 100% of a core indefinitely: before the first render the
/// transcript is not in the control list, so Tab cycles the other controls for
/// ever and the condition can never be met. A wait for a state the loop cannot
/// itself bring about has to be bounded and has to fail loudly.
fn focus_transcript(app: &mut App, provider: &FixtureProvider) {
    let controls = 8;
    for _ in 0..=controls {
        if app.layers.investigation.state().unwrap().focus == InvestigationControl::More {
            return;
        }
        app.handle(raw_key(KeyCode::Tab), provider);
    }
    panic!(
        "the transcript never took focus in {controls} tabs; render before \
         tabbing, or it is not overflowing and is not in the control list"
    );
}

#[test]
fn investigation_more_exists_only_for_real_overflow_and_normalizes_same_frame() {
    let (provider, mut app) = demo();
    let messages: Vec<String> = (0..12)
        .map(|index| format!("activity {index}: {}", "wide detail ".repeat(10)))
        .collect();
    conversation(&mut app, &provider, &messages);
    // The transcript is focusable only once a frame has found it overflowing.
    render(&provider, &mut app, 48, 14);
    focus_transcript(&mut app, &provider);
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
        app.layers
            .investigation
            .state()
            .unwrap()
            .review_scroll_limit
            > 0
    );

    // A short conversation in a wide terminal does not overflow, so the pane
    // is not focusable and a focus left on it is normalised in the same frame.
    let (provider, mut app) = demo();
    let short: Vec<String> = (0..4)
        .map(|index| format!("reply {index}: {}", "detail ".repeat(13)))
        .collect();
    conversation(&mut app, &provider, &short);
    let narrow = render(&provider, &mut app, 48, 14);
    assert!(narrow.contains('▼'), "it overflows when narrow:\n{narrow}");
    focus_transcript(&mut app, &provider);
    render(&provider, &mut app, 48, 14);
    let wide = render(&provider, &mut app, 120, 30);
    assert!(
        !wide.contains('▼'),
        "nothing to scroll, so no scrollbar:\n{wide}"
    );
    assert_eq!(
        app.layers.investigation.state().unwrap().focus,
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
