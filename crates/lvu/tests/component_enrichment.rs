//! Acceptance for the three enrichment layers (docs/component-model.md §6.3
//! step 13). What is asserted here is what the step is *about*:
//!
//! * the model's one real `OpenChild` — the step editor draws over the list it
//!   came from, the list keeps its frame and title under the extra scrim, and
//!   `Close` returns the user to it with the selection intact;
//! * External command reached by `Replace` rather than as a child, because it
//!   draws no parent and closes to the workspace (§6.5);
//! * the `ViewEvent::QueryAccepted` that replaced `App`'s `close_enrichment_step`
//!   flag, fenced on the view the open editor belongs to;
//! * the two product invariants: an invalid step leaves the last applied view
//!   usable, and command enrichment never runs on a save;
//! * a save on a *fixed* view, where the query belongs to a candidate this
//!   layer cannot see: it still reports that it is evaluating, and one draft
//!   still makes exactly one step.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use lvu::{
    Action, App, QueryCompletion, QueryFailure, QueryPurpose, RowProvider, ViewRole,
    app::{CommandEnrichmentRunState, EnrichmentControl, EnrichmentStepControl, ViewItem},
    component::{Component, LayerId, Open, RawEvent},
    fixture::FixtureProvider,
    theme::Theme,
    ui,
};
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer, layout::Rect};

/// The four terminals dialog-system.md quotes.
const SIZES: [(u16, u16); 4] = [(140, 40), (100, 30), (80, 24), (54, 16)];

fn demo() -> (FixtureProvider, App) {
    let (provider, sources, views) = FixtureProvider::demo();
    (provider, App::new(sources, views, true))
}

fn draw<P: RowProvider>(provider: &P, app: &mut App, width: u16, height: u16) -> Buffer {
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

fn alt(app: &mut App, provider: &FixtureProvider, code: KeyCode) {
    app.handle(
        Action::Raw(RawEvent::Key(KeyEvent::new(code, KeyModifiers::ALT))),
        provider,
    );
}

fn paste(app: &mut App, provider: &FixtureProvider, text: &str) {
    app.handle(Action::Raw(RawEvent::Paste(text.into())), provider);
}

fn click(app: &mut App, provider: &FixtureProvider, point: (u16, u16)) {
    app.handle(
        Action::Raw(RawEvent::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: point.0,
            row: point.1,
            modifiers: KeyModifiers::NONE,
        })),
        provider,
    );
}

fn contains(area: Rect, point: (u16, u16)) -> bool {
    point.0 >= area.x && point.0 < area.right() && point.1 >= area.y && point.1 < area.bottom()
}

/// Add one accepted step with `source`, leaving the list on top.
fn accept_step(app: &mut App, provider: &FixtureProvider, source: &str) {
    alt(app, provider, KeyCode::Char('a'));
    paste(app, provider, source);
    key(app, provider, KeyCode::Enter);
    let request = app.take_query_requests().pop().expect("one chain request");
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: request.view_id,
        generation: request.generation,
        revision: request.revision,
        purpose: QueryPurpose::Enrichment,
        result: Ok(()),
    }));
}

#[test]
fn every_recorded_control_was_painted_and_answers_the_hit_test() {
    for (width, height) in SIZES {
        let (provider, mut app) = demo();
        app.handle(Action::Open(Open::Enrichment), &provider);
        accept_step(&mut app, &provider, "one = pl.lit(1)");
        draw(&provider, &mut app, width, height);

        let surface = app.layers.enrichment.surface();
        for (rect, control) in app.layers.enrichment.control_rects() {
            assert!(
                contains(surface.popup, (rect.x, rect.y)),
                "{width}x{height}: {control:?} outside the popup"
            );
            assert_eq!(
                app.layers.enrichment.hit((rect.x, rect.y)),
                Some(lvu::components::enrichment::EnrichmentHit::Control(
                    *control
                ))
            );
        }
        for (rect, index) in app.layers.enrichment.row_rects() {
            assert_eq!(
                app.layers.enrichment.hit((rect.x, rect.y)),
                Some(lvu::components::enrichment::EnrichmentHit::Row(*index))
            );
        }

        alt(&mut app, &provider, KeyCode::Char('e'));
        draw(&provider, &mut app, width, height);
        let surface = app.layers.enrichment_step.surface();
        for (rect, control) in app.layers.enrichment_step.control_rects() {
            assert!(
                contains(surface.popup, (rect.x, rect.y)),
                "{width}x{height}: {control:?} outside the child popup"
            );
        }
    }
}

#[test]
fn the_step_editor_is_a_child_and_close_returns_to_the_list() {
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::Enrichment), &provider);
    accept_step(&mut app, &provider, "one = pl.lit(1)");
    accept_step(&mut app, &provider, "two = pl.lit(2)");
    assert_eq!(app.view_state().unwrap().enrichment_selected, 1);

    alt(&mut app, &provider, KeyCode::Char('e'));
    assert_eq!(
        app.layers.stack_ids(),
        vec![LayerId::Enrichment, LayerId::EnrichmentStep],
        "the parent stays on the stack underneath its child (§5.3)"
    );

    // §10: the parent keeps its title under the child, and the child is inset
    // inside the parent's frame at anything but a compact size.
    let step = screen(&draw(&provider, &mut app, 100, 30));
    assert!(step.contains("Enrichment › Edit step"), "{step}");
    assert!(step.contains(" Enrichment "), "{step}");
    let parent = app.layers.enrichment.surface().popup;
    let child = app.layers.enrichment_step.surface().popup;
    assert!(
        child.width + 4 <= parent.width,
        "{child:?} inside {parent:?}"
    );

    // Escape pops exactly one layer, and the selection it was opened on is
    // still there.
    key(&mut app, &provider, KeyCode::Esc);
    assert_eq!(app.layers.top(), Some(LayerId::Enrichment));
    assert_eq!(app.view_state().unwrap().enrichment_selected, 1);
    key(&mut app, &provider, KeyCode::Esc);
    assert!(app.layers.top().is_none());
}

#[test]
fn a_saved_step_closes_only_the_editor_that_owns_the_accepted_draft() {
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::Enrichment), &provider);
    alt(&mut app, &provider, KeyCode::Char('a'));
    paste(&mut app, &provider, "kind = pl.lit('ok')");
    key(&mut app, &provider, KeyCode::Enter);
    let request = app.take_query_requests().pop().unwrap();

    // A completion for another view is not this editor's: the layer stays open
    // because the event is fenced on the view it holds.
    assert!(!app.apply_query_completion(QueryCompletion {
        view_id: "worker".into(),
        generation: request.generation,
        revision: request.revision,
        purpose: QueryPurpose::Enrichment,
        result: Ok(()),
    }));
    assert_eq!(app.layers.top(), Some(LayerId::EnrichmentStep));

    assert!(app.apply_query_completion(QueryCompletion {
        view_id: request.view_id.clone(),
        generation: request.generation,
        revision: request.revision,
        purpose: QueryPurpose::Enrichment,
        result: Ok(()),
    }));
    assert_eq!(
        app.layers.top(),
        Some(LayerId::Enrichment),
        "an accepted step returns the user to the list"
    );
    assert!(app.view_state().unwrap().enrichment.draft.is_empty());
}

#[test]
fn a_rejected_step_keeps_its_layer_its_draft_and_the_applied_view() {
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::Enrichment), &provider);
    accept_step(&mut app, &provider, "good = pl.lit(1)");
    let applied = app.view_state().unwrap().enrichments.clone();

    alt(&mut app, &provider, KeyCode::Char('a'));
    paste(&mut app, &provider, "broken = pl.col(");
    key(&mut app, &provider, KeyCode::Enter);
    let request = app.take_query_requests().pop().unwrap();
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: request.view_id,
        generation: request.generation,
        revision: request.revision,
        purpose: QueryPurpose::Enrichment,
        result: Err(QueryFailure {
            purpose: QueryPurpose::Enrichment,
            message: "unbalanced parenthesis".into(),
        }),
    }));
    // The invariant: the last applied view is untouched and still usable.
    assert_eq!(app.view_state().unwrap().enrichments, applied);
    assert_eq!(
        app.layers.top(),
        Some(LayerId::EnrichmentStep),
        "a refusal keeps the editor open on the draft that caused it"
    );
    assert_eq!(
        app.view_state().unwrap().enrichment.draft,
        "broken = pl.col("
    );

    // The reaffirmation the shell sends after a failure must not close it.
    let reaffirm = app.take_query_requests().pop().unwrap();
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: reaffirm.view_id,
        generation: reaffirm.generation,
        revision: reaffirm.revision,
        purpose: QueryPurpose::Enrichment,
        result: Ok(()),
    }));
    assert_eq!(app.layers.top(), Some(LayerId::EnrichmentStep));
    assert_eq!(
        app.view_state().unwrap().enrichment.error.as_deref(),
        Some("unbalanced parenthesis")
    );
}

#[test]
fn external_command_replaces_the_list_and_closes_to_the_workspace() {
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::Enrichment), &provider);
    // §6.5: `Replace`, not `OpenChild` — it draws no parent behind it, and its
    // Escape went to the log rather than back to the step list.
    alt(&mut app, &provider, KeyCode::Char('c'));
    assert_eq!(app.layers.stack_ids(), vec![LayerId::ExternalCommand]);
    let output = screen(&draw(&provider, &mut app, 100, 30));
    assert!(output.contains("Enrichment › External command"), "{output}");
    assert!(!output.contains(" Steps "), "{output}");
    key(&mut app, &provider, KeyCode::Esc);
    assert!(app.layers.top().is_none());
}

#[test]
fn saving_a_command_never_runs_it_and_the_state_vocabulary_is_unchanged() {
    let (provider, mut app) = demo();
    let view_id = app.active_view_id().unwrap().to_owned();
    app.handle(
        Action::Open(Open::ExternalCommand {
            stage: None,
            insert_at: usize::MAX,
        }),
        &provider,
    );
    paste(&mut app, &provider, "/usr/bin/enrich");
    app.handle(
        Action::Raw(RawEvent::Key(KeyEvent::new(
            KeyCode::Char('s'),
            KeyModifiers::CONTROL,
        ))),
        &provider,
    );
    assert!(
        app.take_command_enrichment_requests().is_empty(),
        "a save is a chain change through the query seam, never an execution"
    );
    let request = app.take_query_requests().pop().expect("one chain request");
    assert_eq!(request.purpose, QueryPurpose::Enrichment);
    assert_eq!(
        app.layers.external_command.state().unwrap().run_state,
        CommandEnrichmentRunState::Saving
    );
    // The same guarantee the step editor needed, in the dialog that already
    // had it: the state word says a save is in flight, and a second Ctrl-S
    // while it is showing submits nothing.
    let saving = screen(&draw(&provider, &mut app, 100, 30));
    assert!(saving.contains("Saving"), "{saving}");
    app.handle(
        Action::Raw(RawEvent::Key(KeyEvent::new(
            KeyCode::Char('s'),
            KeyModifiers::CONTROL,
        ))),
        &provider,
    );
    assert!(
        app.take_query_requests().is_empty(),
        "a second save with one outstanding changes the chain twice"
    );
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: request.view_id,
        generation: request.generation,
        revision: request.revision,
        purpose: QueryPurpose::Enrichment,
        result: Ok(()),
    }));
    let state = app.views.state(&view_id).unwrap();
    assert_eq!(state.enrichments.len(), 1);
    assert!(state.enrichments[0].is_command());
    assert_eq!(state.enrichments[0].source, "command");
    assert_eq!(state.command_revision("command-1"), 1);
    let output = screen(&draw(&provider, &mut app, 100, 30));
    assert!(output.contains("Unrun"), "{output}");
    assert!(
        output.contains("New records stay pending until you run it again."),
        "{output}"
    );
    assert!(
        output.contains("Saving or restoring never starts this command."),
        "{output}"
    );
}

#[test]
fn the_shared_completion_belongs_to_the_step_editor_and_absorbs_one_dismissal() {
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::Enrichment), &provider);
    alt(&mut app, &provider, KeyCode::Char('a'));
    paste(&mut app, &provider, "copied = ");
    app.handle(
        Action::Raw(RawEvent::Key(KeyEvent::new(
            KeyCode::Char(' '),
            KeyModifiers::CONTROL,
        ))),
        &provider,
    );
    draw(&provider, &mut app, 100, 30);
    assert!(app.layers.enrichment_step.completion().is_some());
    assert!(!app.layers.enrichment_step.completion_rects().is_empty());
    // §5.3: the innermost thing goes first.
    key(&mut app, &provider, KeyCode::Esc);
    assert!(app.layers.enrichment_step.completion().is_none());
    assert_eq!(app.layers.top(), Some(LayerId::EnrichmentStep));
}

#[test]
fn below_floor_uses_the_tiny_fallback() {
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::Enrichment), &provider);
    accept_step(&mut app, &provider, "one = pl.lit(1)");

    // Each of the three layers leaves the frame to the tiny fallback below
    // the 20x6 floor, and the layers stay open underneath it.
    let tiny = screen(&draw(&provider, &mut app, 19, 5));
    assert!(tiny.contains("terminal too small"), "{tiny}");
    assert!(!tiny.contains(" Enrichment "), "{tiny}");

    alt(&mut app, &provider, KeyCode::Char('e'));
    let tiny = screen(&draw(&provider, &mut app, 19, 5));
    assert!(tiny.contains("terminal too small"), "{tiny}");
    assert!(!tiny.contains("Edit step"), "{tiny}");
    key(&mut app, &provider, KeyCode::Esc);

    alt(&mut app, &provider, KeyCode::Char('c'));
    let tiny = screen(&draw(&provider, &mut app, 19, 5));
    assert!(tiny.contains("terminal too small"), "{tiny}");
    assert!(!tiny.contains("External command"), "{tiny}");
    // Replace closes to the workspace, not to the list (§6.5).
    key(&mut app, &provider, KeyCode::Esc);
    assert_eq!(app.layers.top(), None);

    // A roomy redraw restores the list on the same selection.
    app.handle(Action::Open(Open::Enrichment), &provider);
    let roomy = screen(&draw(&provider, &mut app, 80, 24));
    assert!(roomy.contains("Enrichment"), "{roomy}");
    assert!(roomy.contains("one = pl.lit(1)"), "{roomy}");
}

#[test]
fn clicking_a_completion_row_selects_it_through_the_shared_hitbox() {
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::Enrichment), &provider);
    alt(&mut app, &provider, KeyCode::Char('a'));
    paste(&mut app, &provider, "copied = ");
    app.handle(
        Action::Raw(RawEvent::Key(KeyEvent::new(
            KeyCode::Char(' '),
            KeyModifiers::CONTROL,
        ))),
        &provider,
    );
    draw(&provider, &mut app, 80, 24);
    let rows = app.layers.enrichment_step.completion_rects().to_vec();
    assert!(rows.len() > 1, "completion paints selectable rows");
    // The painted rows answer the hit test with their own index, so click
    // and paint share one geometry.
    for (rect, index) in &rows {
        assert_eq!(
            app.layers.enrichment_step.hit((rect.x, rect.y)),
            Some(lvu::components::enrichment_step::StepHit::Completion(
                *index
            ))
        );
    }
    click(&mut app, &provider, (rows[1].0.x, rows[1].0.y));
    assert_eq!(
        app.layers.enrichment_step.completion().unwrap().selected,
        rows[1].1
    );
}

#[test]
fn removing_from_the_editor_returns_to_the_list_and_revalidates_the_chain() {
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::Enrichment), &provider);
    accept_step(&mut app, &provider, "one = pl.lit(1)");
    accept_step(&mut app, &provider, "two = pl.lit(2)");

    alt(&mut app, &provider, KeyCode::Char('e'));
    for _ in 0..8 {
        if app.layers.enrichment_step.control() == EnrichmentStepControl::Remove {
            break;
        }
        key(&mut app, &provider, KeyCode::Tab);
    }
    key(&mut app, &provider, KeyCode::Enter);
    assert_eq!(app.layers.top(), Some(LayerId::Enrichment));
    let request = app
        .take_query_requests()
        .pop()
        .expect("a chain revalidation");
    assert_eq!(request.constraints.enrichments.len(), 1);
    assert_eq!(request.constraints.enrichments[0].source, "one = pl.lit(1)");
}

#[test]
fn the_list_opens_nothing_without_an_active_view() {
    // `Open::needs_active_view` carries the precondition each legacy `Open*`
    // arm carried itself (§7.7); an empty workspace opens Add source instead.
    let (provider, _) = demo();
    let mut app = App::new(Vec::new(), Vec::<ViewItem>::new(), true);
    let opening = app.layers.stack_ids();
    app.handle(Action::Open(Open::Enrichment), &provider);
    app.handle(
        Action::Open(Open::ExternalCommand {
            stage: None,
            insert_at: usize::MAX,
        }),
        &provider,
    );
    assert_eq!(app.layers.stack_ids(), opening);
    assert!(!app.layers.enrichment.is_open());
    assert!(!app.layers.external_command.is_open());
}

#[test]
fn clicking_a_step_selects_it_and_clicking_edit_opens_the_child() {
    for (width, height) in SIZES {
        let (provider, mut app) = demo();
        app.handle(Action::Open(Open::Enrichment), &provider);
        accept_step(&mut app, &provider, "one = pl.lit(1)");
        accept_step(&mut app, &provider, "two = pl.lit(2)");
        draw(&provider, &mut app, width, height);

        let (rect, index) = app.layers.enrichment.row_rects()[0];
        click(&mut app, &provider, (rect.x, rect.y));
        assert_eq!(app.view_state().unwrap().enrichment_selected, index);
        assert_eq!(
            app.view_state().unwrap().enrichment_control,
            EnrichmentControl::Steps
        );

        let edit = app
            .layers
            .enrichment
            .control_rects()
            .iter()
            .find(|(_, control)| *control == EnrichmentControl::Edit)
            .copied()
            .unwrap_or_else(|| panic!("{width}x{height} has no Edit hitbox"));
        click(&mut app, &provider, (edit.0.x, edit.0.y));
        assert_eq!(app.layers.top(), Some(LayerId::EnrichmentStep));
        assert_eq!(
            app.view_state().unwrap().enrichment.draft,
            "one = pl.lit(1)"
        );
    }
}

/// Add one step to a canonical view, which forks, and hand back the candidate
/// the seam staged plus the query it wants run.
fn stage_step(
    app: &mut App,
    provider: &FixtureProvider,
    source: &str,
) -> (String, lvu::app::QueryRequest) {
    alt(app, provider, KeyCode::Char('a'));
    paste(app, provider, source);
    key(app, provider, KeyCode::Enter);
    let staged = app.take_view_fork_requests();
    assert_eq!(staged.len(), 1, "one candidate per save: {staged:?}");
    let candidate = staged[0].candidate_view_id.clone();
    assert!(app.begin_fork_query(&candidate));
    let query = app
        .take_query_requests()
        .into_iter()
        .find(|request| request.view_id == candidate)
        .expect("the candidate queries for itself");
    (candidate, query)
}

#[test]
fn a_save_on_a_fixed_view_says_so_and_one_draft_makes_one_step() {
    let (provider, mut app) = demo();
    let origin = app.active_view_id().unwrap().to_owned();
    app.set_view_role(&origin, ViewRole::Canonical);
    app.handle(Action::Open(Open::Enrichment), &provider);
    let (candidate, query) = stage_step(&mut app, &provider, "name4 = pl.col('time')");

    // A fixed view forks, so the query is the candidate's and the origin's
    // editor holds no `pending_generation`. Without a marker of its own the
    // editor said nothing at all, which is what made a user press Save again.
    assert_eq!(app.layers.top(), Some(LayerId::EnrichmentStep));
    assert!(app.views.state(&origin).unwrap().enrichment.fork_pending);
    let output = screen(&draw(&provider, &mut app, 100, 30));
    assert!(output.contains("Updating"), "{output}");
    assert!(output.contains("Evaluating this step"), "{output}");

    // The second Save on the same draft: it restaged the candidate, which
    // appended the same step to its chain a second time and produced
    // `duplicate enrichment output field`. Now it does nothing.
    key(&mut app, &provider, KeyCode::Enter);
    assert!(
        app.take_view_fork_requests().is_empty(),
        "a save with an outcome outstanding must not restage the candidate"
    );
    assert!(
        app.take_query_requests().is_empty(),
        "the same draft was submitted twice"
    );

    assert!(app.apply_query_completion(QueryCompletion {
        view_id: candidate.clone(),
        generation: query.generation,
        revision: query.revision,
        purpose: QueryPurpose::Enrichment,
        result: Ok(()),
    }));
    assert_eq!(app.take_ready_forks().len(), 1);
    assert!(app.install_fork(&candidate));

    // Acceptance returns the user to the list, on the view the save created,
    // with the step it made selected.
    assert_eq!(app.layers.top(), Some(LayerId::Enrichment));
    let state = app.views.state(&candidate).unwrap();
    assert_eq!(state.enrichments.len(), 1, "one draft, one step");
    assert_eq!(state.enrichment_selected, 0);
    assert!(!state.enrichment.fork_pending);
}

#[test]
fn a_rejected_fork_keeps_the_editor_and_lets_the_draft_be_saved_again() {
    let (provider, mut app) = demo();
    let origin = app.active_view_id().unwrap().to_owned();
    app.set_view_role(&origin, ViewRole::Canonical);
    app.handle(Action::Open(Open::Enrichment), &provider);
    let (candidate, query) = stage_step(&mut app, &provider, "broken = pl.col(");
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: candidate,
        generation: query.generation,
        revision: query.revision,
        purpose: QueryPurpose::Enrichment,
        result: Err(QueryFailure {
            purpose: QueryPurpose::Enrichment,
            message: "unbalanced parenthesis".into(),
        }),
    }));

    assert_eq!(
        app.layers.top(),
        Some(LayerId::EnrichmentStep),
        "a refusal keeps the editor open on the draft that caused it"
    );
    let editor = &app.views.state(&origin).unwrap().enrichment;
    assert_eq!(editor.error.as_deref(), Some("unbalanced parenthesis"));
    assert_eq!(editor.draft, "broken = pl.col(");
    assert!(
        !editor.fork_pending,
        "a known outcome releases the draft, or it could never be saved again"
    );

    // Which is the point of releasing it: the correction is submittable.
    paste(&mut app, &provider, "1)");
    key(&mut app, &provider, KeyCode::Enter);
    assert_eq!(app.take_view_fork_requests().len(), 1);
}

#[test]
fn one_enter_per_draft_on_a_derived_view_for_both_add_and_edit() {
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::Enrichment), &provider);

    // Add: the view applies in place, so the guard is the editor's own
    // `pending_generation` rather than a fork's.
    alt(&mut app, &provider, KeyCode::Char('a'));
    paste(&mut app, &provider, "name4 = pl.col('time')");
    key(&mut app, &provider, KeyCode::Enter);
    key(&mut app, &provider, KeyCode::Enter);
    let requests = app.take_query_requests();
    assert_eq!(requests.len(), 1, "one draft, one query: {requests:?}");
    let output = screen(&draw(&provider, &mut app, 100, 30));
    assert!(output.contains("Evaluating this step"), "{output}");
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: requests[0].view_id.clone(),
        generation: requests[0].generation,
        revision: requests[0].revision,
        purpose: QueryPurpose::Enrichment,
        result: Ok(()),
    }));
    assert_eq!(app.layers.top(), Some(LayerId::Enrichment));
    assert_eq!(app.view_state().unwrap().enrichments.len(), 1);

    // Edit replaces one stage rather than appending, but a second Enter is
    // still a second query for a draft whose outcome is not known.
    alt(&mut app, &provider, KeyCode::Char('e'));
    assert_eq!(app.layers.top(), Some(LayerId::EnrichmentStep));
    paste(&mut app, &provider, " ");
    key(&mut app, &provider, KeyCode::Enter);
    key(&mut app, &provider, KeyCode::Enter);
    let edits = app.take_query_requests();
    assert_eq!(edits.len(), 1, "one edited draft, one query: {edits:?}");
}
