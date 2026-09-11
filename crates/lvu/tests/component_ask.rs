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

/// Where the layer draws `control`, so a click can be aimed at it.
fn layer_rect(app: &App, control: AskControl) -> (u16, u16) {
    let popup = app.layers.ask.surface().popup;
    for y in popup.y..popup.bottom() {
        for x in popup.x..popup.right() {
            if app.layers.ask.hit((x, y)) == Some(AskHit::Control(control)) {
                return (x, y);
            }
        }
    }
    panic!("no cell hits {control:?}");
}

fn raw_alt(code: KeyCode) -> Action {
    Action::Raw(RawEvent::Key(KeyEvent::new(code, KeyModifiers::ALT)))
}

fn mouse_down(column: u16, row: u16) -> crossterm::event::MouseEvent {
    crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column,
        row,
        modifiers: KeyModifiers::NONE,
    }
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
    open(&mut app, &provider, AskOpen::Task(AskTask::TimestampColumn));
    let screen = draw(&provider, &mut app, 120, 30);
    assert!(screen.contains("Timestamp column"), "{screen}");
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
            < AskTask::TimestampColumn.object().len() + 1500
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
    open(&mut app, &provider, AskOpen::Task(AskTask::TimestampColumn));
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

/// §12.17 and `docs/larger-ask-sample.md`. The sample the preparation admitted
/// is shown whether or not anything was left out, an answer keeps the sample it
/// was built from, and the wider re-run is offered only when there is something
/// wider to find.
#[test]
fn a_thin_sample_is_shown_and_offers_one_wider_re_run() {
    use lvu::app::{AskSample, AskSampleTier};
    let (provider, mut app) = demo();
    let view_id = app.active_view_id().unwrap().to_owned();
    open(&mut app, &provider, AskOpen::Generic);
    app.handle(
        Action::Raw(RawEvent::Paste("why do these fail".into())),
        &provider,
    );
    submit(&mut app, &provider);
    let AskAiRequest::Start {
        generation,
        definition_revision,
        wider,
        ..
    } = app.take_ask_ai_requests().pop().expect("start request")
    else {
        panic!("start request")
    };
    assert!(!wider, "the first turn uses the standard tier");

    let thin = AskSample {
        used: 128,
        available: 4_201_993,
        sources: 3,
        tier: AskSampleTier::Standard,
    };
    assert!(app.record_ask_sample(generation, thin));
    // Stale generations cannot rewrite what a request was measured at.
    assert!(!app.record_ask_sample(generation + 9, thin));
    assert!(app.finish_ask_ai(
        generation,
        &view_id,
        definition_revision,
        Ok(("pl.col('level') == 'ERROR'".into(), "keeps errors".into())),
    ));

    let screen = draw(&provider, &mut app, 130, 34);
    assert!(
        screen.contains("128 of 4201993 rows · 3 sources · standard"),
        "the sample is on screen:\n{screen}"
    );
    assert!(
        screen.contains("wider sample"),
        "a thin sample offers the re-run:\n{screen}"
    );
    let answer = app.layers.ask.state().unwrap().answer_sample.unwrap();
    assert_eq!(answer, thin, "the answer records the sample it used");

    // Taking the offer re-runs the same request at the wider tier, under a new
    // generation so the first answer's completions cannot land on it.
    let before = app.layers.ask.state().unwrap().generation;
    let (x, y) = layer_rect(&app, AskControl::Widen);
    app.handle(Action::Raw(RawEvent::Mouse(mouse_down(x, y))), &provider);
    let AskAiRequest::Start {
        generation: widened,
        instruction,
        wider,
        ..
    } = app.take_ask_ai_requests().pop().expect("widened request")
    else {
        panic!("start request")
    };
    assert!(wider, "the re-run asks for the wider sample");
    assert_eq!(
        instruction, "why do these fail",
        "same request, wider sample"
    );
    assert_ne!(widened, before, "a new turn, fenced on its own generation");
    let previous = app
        .layers
        .ask
        .state()
        .unwrap()
        .previous_answer
        .as_ref()
        .unwrap();
    assert_eq!(previous.expression, "pl.col('level') == 'ERROR'");
    assert_eq!(previous.sample, thin);

    let wide = AskSample {
        used: 512,
        available: 4_201_993,
        sources: 3,
        tier: AskSampleTier::Wider,
    };
    assert!(app.record_ask_sample(widened, wide));
    assert!(app.finish_ask_ai(
        widened,
        &view_id,
        definition_revision,
        Ok(("pl.col('level') == 'WARN'".into(), "wider answer".into())),
    ));
    let screen = draw(&provider, &mut app, 130, 34);
    assert!(
        screen.contains("standard answer · 128 of 4201993"),
        "{screen}"
    );
    assert!(screen.contains("wider answer · 512 of 4201993"), "{screen}");
    assert!(screen.contains("pl.col('level') == 'ERROR'"), "{screen}");
    assert!(screen.contains("pl.col('level') == 'WARN'"), "{screen}");

    // Apply reads the current wider candidate, never the historical standard
    // answer. The shell places that exact expression in the Advanced editor.
    key(&mut app, &provider, KeyCode::Enter);
    let filter = draw(&provider, &mut app, 130, 34);
    assert!(filter.contains("pl.col('level') == 'WARN'"), "{filter}");
    assert!(!filter.contains("pl.col('level') == 'ERROR'"), "{filter}");
}

#[test]
fn wider_failure_and_retry_preserve_one_answer_but_a_fresh_request_clears_it() {
    use lvu::app::{AskSample, AskSampleTier};
    let (provider, mut app) = demo();
    let view_id = app.active_view_id().unwrap().to_owned();
    open(&mut app, &provider, AskOpen::Generic);
    app.handle(
        Action::Raw(RawEvent::Paste("same request".into())),
        &provider,
    );
    submit(&mut app, &provider);
    let AskAiRequest::Start {
        generation,
        definition_revision,
        ..
    } = app.take_ask_ai_requests().pop().unwrap()
    else {
        panic!()
    };
    let standard = AskSample {
        used: 1,
        available: 2,
        sources: 1,
        tier: AskSampleTier::Standard,
    };
    assert!(app.record_ask_sample(generation, standard));
    assert!(app.finish_ask_ai(
        generation,
        &view_id,
        definition_revision,
        Ok(("pl.lit(True)".into(), "standard answer".into()))
    ));
    draw(&provider, &mut app, 120, 30);
    let (x, y) = layer_rect(&app, AskControl::Widen);
    app.handle(Action::Raw(RawEvent::Mouse(mouse_down(x, y))), &provider);
    let AskAiRequest::Start {
        generation: wider,
        wider: is_wider,
        ..
    } = app.take_ask_ai_requests().pop().unwrap()
    else {
        panic!()
    };
    assert!(is_wider);
    assert!(app.finish_ask_ai(
        wider,
        &view_id,
        definition_revision,
        Err("failed wider".into())
    ));
    assert_eq!(
        app.layers
            .ask
            .state()
            .unwrap()
            .previous_answer
            .as_ref()
            .unwrap()
            .explanation,
        "standard answer"
    );
    assert!(
        app.top_layer_action_labels(&provider)
            .iter()
            .all(|label| !label.contains("Apply")),
        "a failed wider request cannot apply its historical answer"
    );
    assert!(!app.finish_ask_ai(
        generation,
        &view_id,
        definition_revision,
        Ok(("pl.lit(False)".into(), "stale".into()))
    ));

    submit(&mut app, &provider);
    let AskAiRequest::Start {
        wider: retry_is_wider,
        generation: retry,
        ..
    } = app.take_ask_ai_requests().pop().unwrap()
    else {
        panic!()
    };
    assert!(retry_is_wider);
    assert!(app.layers.ask.state().unwrap().previous_answer.is_some());
    assert!(app.finish_ask_ai(
        retry,
        &view_id,
        definition_revision,
        Err("failed again".into())
    ));

    app.handle(Action::Raw(RawEvent::Paste(" changed".into())), &provider);
    submit(&mut app, &provider);
    let AskAiRequest::Start {
        wider: fresh_is_wider,
        ..
    } = app.take_ask_ai_requests().pop().unwrap()
    else {
        panic!()
    };
    assert!(!fresh_is_wider);
    assert!(app.layers.ask.state().unwrap().previous_answer.is_none());
}

#[test]
fn widening_after_a_standard_failure_does_not_fabricate_history() {
    use lvu::app::{AskSample, AskSampleTier};
    let (provider, mut app) = demo();
    let view_id = app.active_view_id().unwrap().to_owned();
    open(&mut app, &provider, AskOpen::Generic);
    app.handle(
        Action::Raw(RawEvent::Paste("missing rows".into())),
        &provider,
    );
    submit(&mut app, &provider);
    let AskAiRequest::Start {
        generation,
        definition_revision,
        ..
    } = app.take_ask_ai_requests().pop().unwrap()
    else {
        panic!()
    };
    assert!(app.record_ask_sample(
        generation,
        AskSample {
            used: 1,
            available: 9,
            sources: 1,
            tier: AskSampleTier::Standard,
        }
    ));
    assert!(app.finish_ask_ai(
        generation,
        &view_id,
        definition_revision,
        Err("no answer".into()),
    ));
    draw(&provider, &mut app, 120, 30);
    let (x, y) = layer_rect(&app, AskControl::Widen);
    app.handle(Action::Raw(RawEvent::Mouse(mouse_down(x, y))), &provider);
    assert!(app.layers.ask.state().unwrap().previous_answer.is_none());
}

#[test]
fn recipe_history_is_an_immutable_snapshot_of_the_standard_candidate() {
    use lvu::{EnrichmentDefinition, RecipeConfig};
    let (provider, mut app) = demo();
    open(
        &mut app,
        &provider,
        AskOpen::Recipe {
            config: Box::new(RecipeConfig::default()),
            outcome: lvu::app::RecipeOutcome {
                source_id: String::new(),
                recipe_id: String::new(),
                revision: String::new(),
                accepted: true,
            },
            prompt: "adapt recipe".into(),
        },
    );
    submit(&mut app, &provider);
    let AskAiRequest::Start {
        generation,
        view_id,
        definition_revision,
        ..
    } = app.take_ask_ai_requests().pop().unwrap()
    else {
        panic!()
    };
    let standard = lvu::AskSample {
        used: 2,
        available: 3,
        sources: 1,
        tier: lvu::AskSampleTier::Standard,
    };
    assert!(app.record_ask_sample(generation, standard));
    assert!(app.finish_recipe_ai(
        generation,
        &view_id,
        definition_revision,
        Ok((
            "pl.lit(True)".into(),
            "standard recipe".into(),
            Some(vec![EnrichmentDefinition::expression(
                "standard",
                "old = pl.lit(1)"
            )])
        ))
    ));
    draw(&provider, &mut app, 130, 34);
    let (x, y) = layer_rect(&app, AskControl::Widen);
    app.handle(Action::Raw(RawEvent::Mouse(mouse_down(x, y))), &provider);
    let AskAiRequest::Start {
        generation: wider, ..
    } = app.take_ask_ai_requests().pop().unwrap()
    else {
        panic!()
    };
    assert!(app.record_ask_sample(
        wider,
        lvu::AskSample {
            tier: lvu::AskSampleTier::Wider,
            ..standard
        }
    ));
    assert!(app.finish_recipe_ai(
        wider,
        &view_id,
        definition_revision,
        Ok((
            "pl.lit(False)".into(),
            "wider recipe".into(),
            Some(vec![EnrichmentDefinition::expression(
                "wider",
                "new = pl.lit(2)"
            )])
        ))
    ));
    let previous = app
        .layers
        .ask
        .state()
        .unwrap()
        .previous_answer
        .as_ref()
        .unwrap();
    assert_eq!(
        previous.recipe.as_ref().unwrap().enrichments[0].id.0,
        "standard"
    );
    assert_eq!(
        app.layers
            .ask
            .state()
            .unwrap()
            .recipe
            .as_ref()
            .unwrap()
            .enrichments[0]
            .id
            .0,
        "wider"
    );
}

/// A complete sample offers nothing to widen to, however large the capture.
#[test]
fn a_complete_sample_is_stated_and_offers_no_re_run() {
    use lvu::app::{AskSample, AskSampleTier};
    let (provider, mut app) = demo();
    let view_id = app.active_view_id().unwrap().to_owned();
    open(&mut app, &provider, AskOpen::Generic);
    app.handle(Action::Raw(RawEvent::Paste("why".into())), &provider);
    submit(&mut app, &provider);
    let AskAiRequest::Start {
        generation,
        definition_revision,
        ..
    } = app.take_ask_ai_requests().pop().expect("start request")
    else {
        panic!("start request")
    };
    assert!(app.record_ask_sample(
        generation,
        AskSample {
            used: 512,
            available: 512,
            sources: 1,
            tier: AskSampleTier::Standard,
        },
    ));
    assert!(app.finish_ask_ai(
        generation,
        &view_id,
        definition_revision,
        Ok(("pl.col('level') == 'ERROR'".into(), "keeps errors".into())),
    ));
    let screen = draw(&provider, &mut app, 130, 34);
    assert!(
        screen.contains("512 of 512 rows · 1 source · standard"),
        "a complete sample still says so:\n{screen}"
    );
    assert!(!screen.contains("wider sample"), "{screen}");
}

/// The agent's own verdict is the second signal: a sample can be complete and
/// still be the wrong data to answer with.
#[test]
fn an_agent_that_asks_for_more_offers_the_re_run_on_a_complete_sample() {
    use lvu::app::{AskSample, AskSampleTier};
    let (provider, mut app) = demo();
    let view_id = app.active_view_id().unwrap().to_owned();
    open(&mut app, &provider, AskOpen::Generic);
    app.handle(Action::Raw(RawEvent::Paste("why".into())), &provider);
    submit(&mut app, &provider);
    let AskAiRequest::Start {
        generation,
        definition_revision,
        ..
    } = app.take_ask_ai_requests().pop().expect("start request")
    else {
        panic!("start request")
    };
    assert!(app.record_ask_sample(
        generation,
        AskSample {
            used: 512,
            available: 512,
            sources: 1,
            tier: AskSampleTier::Standard,
        },
    ));
    assert!(app.ask_needs_more_data(generation));
    assert!(app.finish_ask_ai(
        generation,
        &view_id,
        definition_revision,
        Ok(("pl.col('level') == 'ERROR'".into(), "keeps errors".into())),
    ));
    let screen = draw(&provider, &mut app, 130, 34);
    assert!(screen.contains("wider sample"), "{screen}");
}

/// §8.10. Ask's action row changes with its stage, so the letters it marks
/// change too. Every row it can draw must mark each letter at most once, and
/// the marked letter must press that button — the inventory in
/// `tests/mnemonics.rs` only ever sees the row the dialog opens with.
#[test]
fn every_row_ask_can_draw_marks_each_letter_once_and_the_letter_presses_it() {
    use lvu::app::{AskSample, AskSampleTier};
    use lvu::dialog_controls::mnemonic_key;
    let (provider, mut app) = demo();
    let view_id = app.active_view_id().unwrap().to_owned();

    let letters = |app: &mut App| -> Vec<char> {
        app.top_layer_action_labels(&provider)
            .iter()
            .filter_map(|label| mnemonic_key(label))
            .collect()
    };
    let unique = |name: &str, found: &[char]| {
        let mut sorted = found.to_vec();
        sorted.sort_unstable();
        let before = sorted.len();
        sorted.dedup();
        assert_eq!(
            before,
            sorted.len(),
            "{name} claims a letter twice: {found:?}"
        );
    };

    open(&mut app, &provider, AskOpen::Generic);
    draw(&provider, &mut app, 130, 34);
    unique("input", &letters(&mut app));

    app.handle(Action::Raw(RawEvent::Paste("why".into())), &provider);
    submit(&mut app, &provider);
    let AskAiRequest::Start {
        generation,
        definition_revision,
        ..
    } = app.take_ask_ai_requests().pop().expect("start request")
    else {
        panic!("start request")
    };
    draw(&provider, &mut app, 130, 34);
    unique("waiting", &letters(&mut app));

    assert!(app.record_ask_sample(
        generation,
        AskSample {
            used: 8,
            available: 900,
            sources: 1,
            tier: AskSampleTier::Standard,
        },
    ));
    assert!(app.finish_ask_ai(
        generation,
        &view_id,
        definition_revision,
        Ok(("pl.col('level') == 'ERROR'".into(), "keeps errors".into())),
    ));
    draw(&provider, &mut app, 130, 34);
    let proposal = letters(&mut app);
    unique("proposal with a wider re-run", &proposal);
    assert!(
        proposal.contains(&'a'),
        "Apply marks a letter: {proposal:?}"
    );
    assert!(
        proposal.contains(&'w'),
        "the wider re-run marks one: {proposal:?}"
    );

    // The marked letter presses that button and does not move the focus ring.
    let focused = app.layers.ask.state().unwrap().focus;
    app.handle(raw_alt(KeyCode::Char('w')), &provider);
    assert!(
        matches!(
            app.take_ask_ai_requests().as_slice(),
            [AskAiRequest::Start { wider: true, .. }]
        ),
        "Alt-W ran the wider re-run"
    );
    assert_eq!(
        app.layers.ask.state().unwrap().focus,
        focused,
        "an accelerator fires a verb; it does not move focus"
    );
}

fn propose(app: &mut App, provider: &FixtureProvider, prompt: &str) -> (String, u64) {
    let view_id = app.active_view_id().unwrap().to_owned();
    open(app, provider, AskOpen::Generic);
    app.handle(Action::Raw(RawEvent::Paste(prompt.into())), provider);
    submit(app, provider);
    let lvu::AskAiRequest::Start { generation, .. } =
        app.take_ask_ai_requests().pop().expect("start request")
    else {
        panic!("start request")
    };
    (view_id, generation)
}

fn finish_proposal(app: &mut App, view_id: &str, generation: u64, expression: &str) {
    let revision = app
        .layers
        .ask
        .state()
        .map(|dialog| dialog.definition_revision)
        .unwrap_or_default();
    assert!(app.finish_ask_ai(
        generation,
        view_id,
        revision,
        Ok((expression.into(), "keeps the matching records".into())),
    ));
}

/// The responsive frame and sticky tail do not move across the dialog's
/// states: input, waiting, proposal, error and the wider re-run share one
/// popup, one interior and one primary-action origin at both normal sizes.
#[test]
fn responsive_frame_and_tail_are_stable_across_request_states() {
    use lvu::app::{AskSample, AskSampleTier};
    for (width, height) in [(80u16, 24u16), (54, 16)] {
        // Input.
        let (provider, mut input) = demo();
        open(&mut input, &provider, AskOpen::Generic);
        input.handle(
            Action::Raw(RawEvent::Paste("why do these fail".into())),
            &provider,
        );
        draw(&provider, &mut input, width, height);
        let first = input.layers.ask.surface();
        assert!(!first.popup.is_empty(), "no frame at {width}x{height}");
        let primary = first_action_origin(&input);

        // Waiting on the remote turn: cancellable, not re-submittable.
        let (provider, mut waiting) = demo();
        let (_, generation) = propose(&mut waiting, &provider, "why do these fail");
        let _ = generation;
        draw(&provider, &mut waiting, width, height);

        // Proposal with a thin standard sample, offering the wider re-run.
        let (provider, mut proposal) = demo();
        let (view_id, generation) = propose(&mut proposal, &provider, "why do these fail");
        let thin = AskSample {
            used: 128,
            available: 4_201_993,
            sources: 3,
            tier: AskSampleTier::Standard,
        };
        assert!(proposal.record_ask_sample(generation, thin));
        assert!(
            proposal.finish_ask_ai(
                generation,
                &view_id,
                proposal
                    .layers
                    .ask
                    .state()
                    .map(|dialog| dialog.definition_revision)
                    .unwrap_or_default(),
                Ok(("pl.col('level') == 'ERROR'".into(), "keeps errors".into())),
            )
        );
        draw(&provider, &mut proposal, width, height);

        // Error keeps the layer and the prompt.
        let (provider, mut failed) = demo();
        open(&mut failed, &provider, AskOpen::Generic);
        submit(&mut failed, &provider);
        draw(&provider, &mut failed, width, height);
        assert_eq!(
            failed.layers.ask.state().map(|dialog| dialog.stage),
            Some(AskAiStage::Error)
        );

        for (name, app) in [
            ("input", &input),
            ("waiting", &waiting),
            ("proposal", &proposal),
            ("failed", &failed),
        ] {
            let surface = app.layers.ask.surface();
            assert_eq!(
                surface.popup, first.popup,
                "{name} frame moved at {width}x{height}"
            );
            assert_eq!(
                surface.interior, first.interior,
                "{name} interior moved at {width}x{height}"
            );
            assert_eq!(
                first_action_origin(app),
                primary,
                "{name} tail moved at {width}x{height}"
            );
        }
        let screen = draw(&provider, &mut input, width, height);
        assert!(screen.contains("Ask"), "{screen}");
    }
}

fn first_action_origin(app: &App) -> Option<(u16, u16)> {
    find_action(app).map(|(rect, _)| (rect.x, rect.y))
}

fn find_action(app: &App) -> Option<(ratatui::layout::Rect, AskControl)> {
    let surface = app.layers.ask.surface();
    let mut found = None;
    for y in surface.popup.y..surface.popup.bottom() {
        for x in surface.popup.x..surface.popup.right() {
            if let Some(AskHit::Control(control)) = app.layers.ask.hit((x, y))
                && matches!(
                    control,
                    AskControl::Submit | AskControl::Apply | AskControl::Cancel
                )
            {
                found.get_or_insert((ratatui::layout::Rect::new(x, y, 1, 1), control));
            }
        }
    }
    found
}

/// At 54x16 the Kind choice, the Request field, the scrolling panes and the
/// actions are all reachable: the dropdown opens from its field and answers
/// its own hit test, typing lands in the request, the wheel scrolls an
/// overflowing proposal, and Submit fires by mouse.
#[test]
fn compact_form_panes_and_actions_stay_reachable() {
    let (provider, mut app) = demo();
    open(&mut app, &provider, AskOpen::Generic);
    draw(&provider, &mut app, 54, 16);
    // The Kind field opens its anchored list and the rows answer hits.
    let (kx, ky) = layer_rect(&app, AskControl::Kind);
    app.handle(Action::Raw(RawEvent::Mouse(mouse_down(kx, ky))), &provider);
    assert!(app.layers.ask.state().unwrap().kind_dropdown);
    draw(&provider, &mut app, 54, 16);
    let surface = app.layers.ask.surface();
    let mut choice = None;
    for y in surface.popup.y..surface.popup.bottom() {
        for x in surface.popup.x..surface.popup.right() {
            if let Some(AskHit::KindChoice(index)) = app.layers.ask.hit((x, y)) {
                choice.get_or_insert(((x, y), index));
            }
        }
    }
    let ((cx, cy), _) = choice.expect("kind rows must paint at 54x16");
    app.handle(Action::Raw(RawEvent::Mouse(mouse_down(cx, cy))), &provider);
    assert!(!app.layers.ask.state().unwrap().kind_dropdown);
    // Typing still lands in the request after the dropdown closes.
    let (px, py) = layer_rect(&app, AskControl::Prompt);
    app.handle(Action::Raw(RawEvent::Mouse(mouse_down(px, py))), &provider);
    app.handle(
        Action::Raw(RawEvent::Paste("show only errors".into())),
        &provider,
    );
    assert_eq!(app.layers.ask.state().unwrap().prompt, "show only errors");
    // A long proposal overflows the panes and scrolls under the fixed form,
    // then Submit fires by mouse on a fresh dialog.
    let (provider, mut scrolling) = demo();
    let (view_id, generation) = propose(&mut scrolling, &provider, "why do these fail");
    finish_proposal(
        &mut scrolling,
        &view_id,
        generation,
        &format!("pl.col('level') == 'ERROR' and {}", "very ".repeat(30)),
    );
    draw(&provider, &mut scrolling, 54, 16);
    let before = scrolling
        .layers
        .ask
        .state()
        .map(|dialog| dialog.review_scroll)
        .unwrap_or_default();
    let surface = scrolling.layers.ask.surface();
    let mut body_cell = None;
    for y in surface.popup.y..surface.popup.bottom() {
        for x in surface.popup.x..surface.popup.right() {
            if scrolling.layers.ask.hit((x, y)) == Some(AskHit::Body) {
                body_cell.get_or_insert((x, y));
            }
        }
    }
    if let Some((x, y)) = body_cell {
        scrolling.handle(
            Action::Raw(RawEvent::Mouse(crossterm::event::MouseEvent {
                kind: crossterm::event::MouseEventKind::ScrollDown,
                column: x,
                row: y,
                modifiers: KeyModifiers::NONE,
            })),
            &provider,
        );
    }
    let after = scrolling
        .layers
        .ask
        .state()
        .map(|dialog| dialog.review_scroll)
        .unwrap_or_default();
    let limit = scrolling
        .layers
        .ask
        .state()
        .map(|dialog| dialog.review_scroll_limit)
        .unwrap_or_default();
    assert!(limit > 0, "the long proposal must overflow at 54x16");
    assert_eq!(after, before.saturating_add(1).min(limit));

    let (provider, mut app) = demo();
    open(&mut app, &provider, AskOpen::Generic);
    app.handle(
        Action::Raw(RawEvent::Paste("show only errors".into())),
        &provider,
    );
    draw(&provider, &mut app, 54, 16);
    let (sx, sy) = layer_rect(&app, AskControl::Submit);
    app.handle(Action::Raw(RawEvent::Mouse(mouse_down(sx, sy))), &provider);
    assert!(
        matches!(
            app.take_ask_ai_requests().pop(),
            Some(AskAiRequest::Start { .. })
        ),
        "Submit fires by mouse at 54x16"
    );
}

/// Below the floor the tiny fallback owns the frame with no stale hitboxes.
#[test]
fn below_floor_uses_the_tiny_fallback() {
    let (provider, mut app) = demo();
    open(&mut app, &provider, AskOpen::Generic);
    let screen = draw(&provider, &mut app, 19, 5);
    assert!(screen.contains("terminal too small"), "{screen}");
    assert_eq!(app.layers.ask.hit((10, 2)), None);
}

/// Wide and combining characters clip by display width without splitting a
/// glyph: every painted control answers its own hit test inside the popup,
/// and the unicode prompt echoes back intact.
#[test]
fn long_unicode_clips_with_exact_hitboxes() {
    for (width, height) in [(80u16, 24u16), (54, 16)] {
        let (provider, mut app) = demo();
        let (view_id, generation) = propose(&mut app, &provider, "café 日本語 éxpansion 漢字 mix");
        finish_proposal(
            &mut app,
            &view_id,
            generation,
            "pl.col('café-日本語') == '👩‍💻-é'",
        );
        draw(&provider, &mut app, width, height);
        let surface = app.layers.ask.surface();
        assert!(!surface.popup.is_empty());
        let mut controls = 0;
        for y in surface.popup.y..surface.popup.bottom() {
            for x in surface.popup.x..surface.popup.right() {
                if app.layers.ask.hit((x, y)).is_some() {
                    controls += 1;
                }
            }
        }
        assert!(controls > 0, "controls must paint at {width}x{height}");
        assert_eq!(app.layers.ask.hit((0, 0)), None);
        let screen = draw(&provider, &mut app, width, height);
        assert!(screen.contains("café"), "{screen}");
    }
}
