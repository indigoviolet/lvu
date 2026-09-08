//! Acceptance for the Investigation 🧠 layer as a component
//! (docs/component-model.md §6.3 step 12).
//!
//! Investigation is the first layer whose remote stage is a *conversation*, so
//! the things worth pinning are the ones the stack cannot see: two fences
//! rather than one, a saved list that outlives the layer, the resume that
//! returns to the transcript, and palette rows whose availability the layer —
//! not `terminal.rs` — answers for.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use lvu::{
    Action, App, Focus,
    app::{InvestigationControl, InvestigationItem, InvestigationRequest, InvestigationStage},
    command_palette::CommandId,
    component::{Component, LayerId, Open, RawEvent},
    components::investigation::InvestigationHit,
    fixture::FixtureProvider,
    theme::Theme,
    ui,
};
use ratatui::{Terminal, backend::TestBackend};

fn demo() -> (FixtureProvider, App) {
    let (provider, sources, views) = FixtureProvider::demo();
    (provider, App::new(sources, views, true))
}

fn draw(provider: &FixtureProvider, app: &mut App, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| ui::render_with_theme(frame, app, provider, Theme::TERMINAL, None))
        .unwrap();
    let buffer = terminal.backend().buffer().clone();
    (0..buffer.area.height)
        .map(|y| {
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The terminal caret, which is what a real terminal shows. `Surface.caret` is
/// the layer's record of it; the two must agree.
fn caret(provider: &FixtureProvider, app: &mut App, width: u16, height: u16) -> (u16, u16) {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| ui::render_with_theme(frame, app, provider, Theme::TERMINAL, None))
        .unwrap();
    let position = terminal.backend().cursor_position();
    (position.x, position.y)
}

fn key(app: &mut App, provider: &FixtureProvider, code: KeyCode) {
    app.handle(
        Action::Raw(RawEvent::Key(KeyEvent::new(code, KeyModifiers::NONE))),
        provider,
    );
}

fn open(app: &mut App, provider: &FixtureProvider) {
    app.handle(Action::Open(Open::Investigation), provider);
}

/// Tab to the primary and press it. Bounded, and loud when the bound is hit:
/// pressing Enter on whatever happens to have focus would submit something
/// else and read as a product bug rather than a helper that missed.
fn reach_submit(app: &mut App, provider: &FixtureProvider) {
    for _ in 0..8 {
        if app.layers.investigation.state().map(|dialog| dialog.focus)
            == Some(InvestigationControl::Submit)
        {
            return;
        }
        key(app, provider, KeyCode::Tab);
    }
    panic!("the primary never took focus in 8 tabs");
}

fn ask(app: &mut App, provider: &FixtureProvider, question: &str) {
    app.handle(Action::Raw(RawEvent::Paste(question.to_owned())), provider);
    reach_submit(app, provider);
    key(app, provider, KeyCode::Enter);
}

fn item(view_id: String, id: &str, session: &str, question: &str) -> InvestigationItem {
    InvestigationItem {
        id: id.to_owned(),
        view_id,
        session_id: session.to_owned(),
        snapshot_dir: format!("/tmp/{id}"),
        manifest_path: format!("/tmp/{id}/manifest.json"),
        question: question.to_owned(),
    }
}

/// §2.4. Two fences, not one: a stage change is matched by the generation the
/// request carried, and a transcript line by the session it came from. A
/// completion for a superseded generation must not touch a reopened dialog.
#[test]
fn the_layer_fences_stages_by_generation_and_transcript_lines_by_session() {
    let (provider, mut app) = demo();
    open(&mut app, &provider);
    ask(&mut app, &provider, "why did the requests fail");
    let InvestigationRequest::Start {
        generation,
        view_id,
        ..
    } = app
        .take_investigation_requests()
        .pop()
        .expect("start request")
    else {
        panic!("start request")
    };

    assert!(!app.update_investigation_progress(
        generation + 7,
        InvestigationStage::StartingSession,
        "stale".into(),
        None,
        None,
        None,
    ));
    assert!(app.update_investigation_progress(
        generation,
        InvestigationStage::StartingSession,
        "starting the local agent".into(),
        None,
        None,
        None,
    ));
    assert_eq!(
        app.layers.investigation.state().unwrap().stage,
        InvestigationStage::StartingSession
    );

    // Before the session exists nothing can arrive for it.
    assert!(!app.append_investigation_output("session-1", "early".into()));
    assert!(app.investigation_ready(generation, item(view_id, "i1", "session-1", "why")));
    assert!(!app.append_investigation_output("other-session", "not ours".into()));
    assert!(app.push_investigation_event("session-1", "Agent: a timeout".into(), Ok(())));
    let dialog = app.layers.investigation.state().unwrap();
    assert_eq!(dialog.stage, InvestigationStage::Conversation);
    assert!(
        dialog
            .messages
            .iter()
            .any(|line| line.contains("a timeout"))
    );
    assert!(!dialog.messages.iter().any(|line| line.contains("not ours")));
}

/// The saved list is the component's and outlives the layer: the disk scan runs
/// whether or not the dialog is up, and a session created while it ran must
/// survive the merge.
#[test]
fn the_saved_list_survives_a_close_and_merges_by_identity() {
    let (provider, mut app) = demo();
    let view_id = app.active_view_id().unwrap().to_owned();
    open(&mut app, &provider);
    ask(&mut app, &provider, "in memory");
    let InvestigationRequest::Start { generation, .. } = app
        .take_investigation_requests()
        .pop()
        .expect("start request")
    else {
        panic!("start request")
    };
    assert!(app.investigation_ready(
        generation,
        item(view_id.clone(), "live", "live-session", "in memory"),
    ));
    key(&mut app, &provider, KeyCode::Esc);
    assert!(app.layers.stack_ids().is_empty());

    // The scan finishes after the layer closed, and reports a session it could
    // not have seen plus one it did.
    app.set_investigations(vec![
        item(view_id.clone(), "live", "live-session", "in memory"),
        item(view_id, "old", "old-session", "from disk"),
    ]);
    open(&mut app, &provider);
    let screen = draw(&provider, &mut app, 120, 30);
    assert!(screen.contains("in memory"), "{screen}");
    assert!(screen.contains("from disk"), "{screen}");
    assert_eq!(app.layers.investigation.state().unwrap().items.len(), 2);
}

/// The fix this conversion had to carry forward: picking a saved session
/// returns to the transcript, because that is where its replies appear.
#[test]
fn resuming_a_saved_session_returns_to_the_transcript() {
    let (provider, mut app) = demo();
    let view_id = app.active_view_id().unwrap().to_owned();
    app.set_investigations(vec![item(
        view_id,
        "old",
        "old-session",
        "earlier question",
    )]);
    open(&mut app, &provider);
    // With saved sessions and nothing in flight, the list is what there is to
    // act on, so the dialog opens on it.
    assert!(app.layers.investigation.state().unwrap().saved_mode);

    // An empty question means "resume the selected one".
    reach_submit(&mut app, &provider);
    key(&mut app, &provider, KeyCode::Enter);
    assert!(matches!(
        app.take_investigation_requests().as_slice(),
        [InvestigationRequest::Resume { item, .. }] if item.session_id == "old-session"
    ));
    assert!(!app.layers.investigation.state().unwrap().saved_mode);
    assert!(draw(&provider, &mut app, 120, 30).contains("Transcript"));
}

/// §5.1/§5.2. The layer owns its geometry and its modal bound, and a turn in
/// flight is cancelled on the way out rather than abandoned.
#[test]
fn geometry_containment_and_a_cancelled_turn_come_from_the_layer() {
    let (provider, mut app) = demo();
    open(&mut app, &provider);
    draw(&provider, &mut app, 120, 30);
    let surface = app.layers.investigation.surface();
    assert_eq!(app.hit_regions.selection_modal, Some(surface.interior));
    assert!(surface.text_focus, "the Question field opens focused");
    assert_eq!(app.focus, Focus::Layer);
    // A focused text field shows a real terminal caret, not only a painted
    // cell: `Surface.caret` records where the layer put it, and the terminal
    // has to have been told.
    assert_eq!(Some(caret(&provider, &mut app, 120, 30)), surface.caret);

    let mut found = false;
    for y in surface.popup.y..surface.popup.bottom() {
        for x in surface.popup.x..surface.popup.right() {
            if app.layers.investigation.hit((x, y))
                == Some(InvestigationHit::Control(InvestigationControl::Submit))
            {
                found = true;
            }
        }
    }
    assert!(found, "Start is clickable where it is drawn");
    assert_eq!(app.layers.investigation.hit((0, 0)), None);

    ask(&mut app, &provider, "why");
    let InvestigationRequest::Start { generation, .. } = app
        .take_investigation_requests()
        .pop()
        .expect("start request")
    else {
        panic!("start request")
    };
    key(&mut app, &provider, KeyCode::Esc);
    assert!(app.layers.stack_ids().is_empty());
    assert!(matches!(
        app.take_investigation_requests().as_slice(),
        [InvestigationRequest::Cancel { generation: value }] if *value == generation
    ));
}

/// §4.3. The three catalog rows used to have their availability computed in
/// `terminal.rs` from a peek at the dialog's private state. The layer reports
/// it, and the rows do what they say.
#[test]
fn the_palette_rows_report_their_own_availability_and_act() {
    let (provider, mut app) = demo();
    let view_id = app.active_view_id().unwrap().to_owned();
    let reason = |app: &App, id: CommandId| {
        app.layer_commands()
            .into_iter()
            .find(|(layer, entry)| *layer == LayerId::Investigation && entry.spec.id == id)
            .map(|(_, entry)| entry.unavailable_reason)
            .expect("the layer contributes the row")
    };

    assert_eq!(
        reason(&app, CommandId::NewInvestigation),
        Some("open Investigations first")
    );
    app.set_investigations(vec![item(view_id, "old", "old-session", "earlier")]);
    open(&mut app, &provider);
    assert_eq!(reason(&app, CommandId::NewInvestigation), None);
    // An empty question over a selected saved session is exactly what "resume"
    // means, so the row is live.
    assert_eq!(reason(&app, CommandId::ResumeInvestigation), None);
    assert_eq!(
        reason(&app, CommandId::InvestigationFollowup),
        Some("open an active investigation and enter a follow-up first")
    );

    app.handle(
        Action::Command(LayerId::Investigation, CommandId::ResumeInvestigation),
        &provider,
    );
    assert!(matches!(
        app.take_investigation_requests().as_slice(),
        [InvestigationRequest::Resume { .. }]
    ));
    assert_eq!(
        reason(&app, CommandId::ResumeInvestigation),
        Some("open Investigations and select a saved session first"),
        "nothing to resume while one is being resumed"
    );
}
