//! Acceptance for the Ask 🧠 layer as a component (docs/component-model.md
//! §6.3 step 12).
//!
//! Ask is the first layer with a long-running remote stage, so the things worth
//! pinning are the ones the stack cannot see: the outbox and its generation
//! fence, a waiting dialog that is cancellable but not re-submittable, and the
//! proposal hand-off to legacy destinations that keeps the layer up when the
//! shell refuses.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use lvu::{
    Action, App, AskAiKind, AskAiRequest, AskAiStage, Focus,
    app::{AskControl, AskTask},
    component::{Component, LayerId, Open, RawEvent},
    components::ask::{AskHit, AskOpen},
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

fn key(app: &mut App, provider: &FixtureProvider, code: KeyCode) {
    app.handle(
        Action::Raw(RawEvent::Key(KeyEvent::new(code, KeyModifiers::NONE))),
        provider,
    );
}

fn open(app: &mut App, provider: &FixtureProvider, params: AskOpen) {
    app.handle(Action::Open(Open::Ask(params)), provider);
}

/// Tab off the Request field and press the primary. Bounded: a helper that
/// waits on a focus the dialog might never reach must fail rather than spin.
fn submit(app: &mut App, provider: &FixtureProvider) {
    for _ in 0..8 {
        if app.layers.ask.state().map(|dialog| dialog.focus) != Some(AskControl::Prompt) {
            break;
        }
        key(app, provider, KeyCode::Tab);
    }
    assert_ne!(
        app.layers.ask.state().map(|dialog| dialog.focus),
        Some(AskControl::Prompt),
        "focus never left the Request field"
    );
    key(app, provider, KeyCode::Enter);
}

#[test]
fn the_layer_owns_its_request_queue_and_fences_completions_by_generation() {
    let (provider, mut app) = demo();
    open(&mut app, &provider, AskOpen::Generic);
    assert_eq!(app.layers.top(), Some(LayerId::Ask));
    app.handle(
        Action::Raw(RawEvent::Paste("only errors".into())),
        &provider,
    );
    submit(&mut app, &provider);

    let AskAiRequest::Start {
        generation,
        view_id,
        definition_revision,
        instruction,
        ..
    } = app
        .take_ask_ai_requests()
        .pop()
        .expect("the outbox carried it")
    else {
        panic!("start request")
    };
    assert_eq!(instruction, "only errors");
    assert!(app.take_ask_ai_requests().is_empty(), "drained once");

    // A completion for a generation the dialog never issued is ignored, and the
    // waiting stage it is in is left alone.
    assert!(!app.finish_ask_ai(
        generation + 99,
        &view_id,
        definition_revision,
        Ok(("pl.lit(True)".into(), "stale".into())),
    ));
    assert_eq!(
        app.layers.ask.state().unwrap().stage,
        AskAiStage::Snapshot,
        "a stale completion cannot advance the dialog"
    );

    assert!(app.finish_ask_ai(
        generation,
        &view_id,
        definition_revision,
        Ok(("pl.col('level') == 'ERROR'".into(), "because".into())),
    ));
    let dialog = app.layers.ask.state().unwrap();
    assert_eq!(dialog.stage, AskAiStage::Proposal);
    assert_eq!(dialog.focus, AskControl::Apply);
}

#[test]
fn a_waiting_layer_offers_cancel_and_refuses_a_second_request() {
    let (provider, mut app) = demo();
    open(&mut app, &provider, AskOpen::Generic);
    app.handle(Action::Raw(RawEvent::Paste("first".into())), &provider);
    submit(&mut app, &provider);
    assert_eq!(app.layers.ask.state().unwrap().stage, AskAiStage::Snapshot);

    // §12.17: waiting is not a dead end. `Cancel request` is the only action,
    // and submitting again is not one of them.
    let waiting = draw(&provider, &mut app, 120, 30);
    assert!(waiting.contains("[ Cancel request ]"), "{waiting}");
    assert!(!waiting.contains("[ Submit ]"), "{waiting}");
    // Dismissing a waiting layer cancels the turn it started.
    key(&mut app, &provider, KeyCode::Esc);
    assert_eq!(app.layers.top(), None);
    // The Start is still queued behind it, so the cancel joins the outbox
    // rather than replacing it; `lvu-app` drains both in order.
    let queued = app.take_ask_ai_requests();
    assert!(
        queued
            .iter()
            .any(|request| matches!(request, AskAiRequest::Cancel { .. })),
        "{queued:?}"
    );
}

#[test]
fn a_prepared_task_fixes_the_kind_and_keeps_the_request_editable() {
    let (provider, mut app) = demo();
    open(
        &mut app,
        &provider,
        AskOpen::Task(AskTask::RecognizeTimestamp),
    );
    let screen = draw(&provider, &mut app, 120, 30);
    assert!(screen.contains("Recognize timestamp"), "{screen}");
    assert!(!screen.contains("Kind"), "a fixed task offers no choice");
    let dialog = app.layers.ask.state().unwrap();
    assert_eq!(dialog.kind, AskAiKind::Enrichment);
    assert!(!dialog.prompt.is_empty(), "the request is prefilled");

    // The kind cannot be changed out from under the task, by key or by command.
    app.handle(
        Action::Raw(RawEvent::Key(KeyEvent::new(
            KeyCode::Char('f'),
            KeyModifiers::ALT,
        ))),
        &provider,
    );
    assert_eq!(app.layers.ask.state().unwrap().kind, AskAiKind::Enrichment);

    // And the prefilled request is a starting point: editing it works.
    key(&mut app, &provider, KeyCode::Backspace);
    assert!(
        app.layers.ask.state().unwrap().prompt.len()
            < AskTask::RecognizeTimestamp.object().len() + 1500
    );
}

#[test]
fn a_refused_apply_keeps_the_layer_and_its_proposal() {
    let (provider, mut app) = demo();
    open(&mut app, &provider, AskOpen::Generic);
    app.handle(
        Action::Raw(RawEvent::Paste("only errors".into())),
        &provider,
    );
    submit(&mut app, &provider);
    let AskAiRequest::Start {
        generation,
        view_id,
        definition_revision,
        ..
    } = app.take_ask_ai_requests().pop().unwrap()
    else {
        panic!("start request")
    };
    assert!(app.finish_ask_ai(
        generation,
        &view_id,
        definition_revision,
        Ok(("pl.col('level') == 'ERROR'".into(), "because".into())),
    ));

    // Applying a proposal for a view that has moved on is refused *in* the
    // dialog: the layer stays and says so.
    app.handle(Action::NextView, &provider);
    key(&mut app, &provider, KeyCode::Enter);
    assert_eq!(app.layers.top(), Some(LayerId::Ask));
    let dialog = app.layers.ask.state().unwrap();
    assert_eq!(dialog.stage, AskAiStage::Error);
    assert!(dialog.progress.contains("view changed"), "{dialog:?}");
}

#[test]
fn geometry_and_containment_come_from_the_layer() {
    let (provider, mut app) = demo();
    open(&mut app, &provider, AskOpen::Generic);
    draw(&provider, &mut app, 120, 30);
    let surface = app.layers.ask.surface();
    assert_eq!(app.hit_regions.selection_modal, Some(surface.interior));
    assert!(surface.text_focus, "the Request field opens focused");

    // Every action the dialog draws resolves through the component's own hit
    // test, and a point outside its surface resolves to nothing.
    let mut found = None;
    for y in surface.popup.y..surface.popup.bottom() {
        for x in surface.popup.x..surface.popup.right() {
            if app.layers.ask.hit((x, y)) == Some(AskHit::Control(AskControl::Submit)) {
                found = Some((x, y));
            }
        }
    }
    assert!(found.is_some(), "Submit is clickable where it is drawn");
    assert_eq!(app.layers.ask.hit((0, 0)), None);
    assert_eq!(app.focus, Focus::Layer);
}

/// §4.3. The two kind rows are the layer's, not the shell's: they were
/// `Action::SelectAskAiKind` gated on `Focus::AskAi` before the conversion, and
/// routing them to a layer that ignored them would have made them silently do
/// nothing from the palette.
#[test]
fn the_palette_kind_rows_are_taken_over_by_the_layer_and_say_when_they_cannot_be() {
    use lvu::command_palette::CommandId;
    let (provider, mut app) = demo();

    let closed = app.layer_commands();
    let filter = closed
        .iter()
        .find(|(_, entry)| entry.spec.id == CommandId::AskAiFilter)
        .expect("the layer contributes the filter row");
    assert_eq!(filter.0, LayerId::Ask);
    assert_eq!(filter.1.unavailable_reason, Some("open Ask agent first"));

    open(&mut app, &provider, AskOpen::Generic);
    assert!(
        app.layer_commands()
            .iter()
            .filter(|(_, entry)| matches!(
                entry.spec.id,
                CommandId::AskAiFilter | CommandId::AskAiEnrichment
            ))
            .all(|(_, entry)| entry.unavailable_reason.is_none()),
        "both kinds are choosable while the request is still being written"
    );
    app.handle(
        Action::Command(LayerId::Ask, CommandId::AskAiEnrichment),
        &provider,
    );
    assert_eq!(app.layers.ask.state().unwrap().kind, AskAiKind::Enrichment);
    app.handle(
        Action::Command(LayerId::Ask, CommandId::AskAiFilter),
        &provider,
    );
    assert_eq!(app.layers.ask.state().unwrap().kind, AskAiKind::Filter);

    // A prepared task has already decided; the rows say so rather than
    // pretending to work.
    app.handle(Action::CancelEditor, &provider);
    open(
        &mut app,
        &provider,
        AskOpen::Task(AskTask::RecognizeTimestamp),
    );
    assert!(
        app.layer_commands()
            .iter()
            .filter(|(_, entry)| matches!(
                entry.spec.id,
                CommandId::AskAiFilter | CommandId::AskAiEnrichment
            ))
            .all(|(_, entry)| entry.unavailable_reason == Some("this request already has a kind")),
    );
    app.handle(
        Action::Command(LayerId::Ask, CommandId::AskAiFilter),
        &provider,
    );
    assert_eq!(
        app.layers.ask.state().unwrap().kind,
        AskAiKind::Enrichment,
        "the task's kind is not overridden from the palette"
    );
}
