//! docs/dialog-system.md §8.14: command steps are ordered steps of the
//! enrichment chain. The list shows them in place, `Edit` opens the External
//! command dialog on one, `External command…` inserts after the selection,
//! Alt-Up/Down reorder through the same validated chain change, and a recipe
//! that needs a program the machine lacks says so when it is applied.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use lvu::{
    Action, App, QueryCompletion, QueryPurpose, RecipeConfig, RowProvider,
    app::{CommandEnrichmentField, CommandEnrichmentRunState},
    component::{LayerId, Open, RawEvent},
    fixture::FixtureProvider,
    theme::Theme,
    ui,
};
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};
use std::collections::BTreeMap;

fn demo() -> (FixtureProvider, App) {
    let (provider, sources, views) = FixtureProvider::demo();
    (provider, App::new(sources, views, true))
}

fn draw(provider: &FixtureProvider, app: &mut App, width: u16, height: u16) -> Buffer {
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

fn press(app: &mut App, provider: &FixtureProvider, code: KeyCode, modifiers: KeyModifiers) {
    app.handle(
        Action::Raw(RawEvent::Key(KeyEvent::new(code, modifiers))),
        provider,
    );
}

fn key(app: &mut App, provider: &FixtureProvider, code: KeyCode) {
    press(app, provider, code, KeyModifiers::NONE);
}

fn alt(app: &mut App, provider: &FixtureProvider, code: KeyCode) {
    press(app, provider, code, KeyModifiers::ALT);
}

fn paste(app: &mut App, provider: &FixtureProvider, text: &str) {
    app.handle(Action::Raw(RawEvent::Paste(text.into())), provider);
}

/// Accepts the one query the last action queued, whatever its purpose.
fn accept(app: &mut App) -> lvu::QueryRequest {
    let requests = app.take_query_requests();
    assert_eq!(requests.len(), 1, "{requests:?}");
    let request = requests.into_iter().next().unwrap();
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: request.view_id.clone(),
        generation: request.generation,
        revision: request.revision,
        purpose: request.purpose,
        result: Ok(()),
    }));
    request
}

fn accept_step(app: &mut App, provider: &FixtureProvider, source: &str) {
    alt(app, provider, KeyCode::Char('a'));
    paste(app, provider, source);
    key(app, provider, KeyCode::Enter);
    let request = accept(app);
    assert_eq!(request.purpose, QueryPurpose::Enrichment);
}

/// Saves a command with `program` in the open External command dialog.
fn save_command(app: &mut App, provider: &FixtureProvider, program: &str) {
    paste(app, provider, program);
    press(app, provider, KeyCode::Char('s'), KeyModifiers::CONTROL);
    assert_eq!(
        app.layers.external_command.state().unwrap().run_state,
        CommandEnrichmentRunState::Saving
    );
    accept(app);
    assert_eq!(
        app.layers.external_command.state().unwrap().run_state,
        CommandEnrichmentRunState::Unrun
    );
}

fn chain(app: &App, view_id: &str) -> Vec<(String, bool)> {
    app.views
        .state(view_id)
        .unwrap()
        .enrichments
        .iter()
        .map(|step| (step.source.clone(), step.is_command()))
        .collect()
}

#[test]
fn command_steps_are_rows_of_the_list_that_edit_and_insert_in_place() {
    let (provider, mut app) = demo();
    let view_id = app.active_view_id().unwrap().to_owned();
    app.handle(Action::Open(Open::Enrichment), &provider);
    accept_step(&mut app, &provider, "one = pl.lit(1)");
    // External command… on an expression row inserts after it.
    alt(&mut app, &provider, KeyCode::Char('c'));
    assert_eq!(app.layers.stack_ids(), vec![LayerId::ExternalCommand]);
    let dialog = app.layers.external_command.state().unwrap();
    assert_eq!(dialog.name, "command");
    assert_eq!(dialog.insert_at, 1);
    assert!(dialog.accepted.is_none());
    save_command(&mut app, &provider, "/usr/bin/true");
    assert_eq!(
        chain(&app, &view_id),
        vec![
            ("one = pl.lit(1)".to_owned(), false),
            ("command".to_owned(), true)
        ]
    );
    assert_eq!(
        app.views
            .state(&view_id)
            .unwrap()
            .command_revision("command-1"),
        1
    );
    assert!(app.action_notice.as_deref().unwrap().contains("not run"));
    key(&mut app, &provider, KeyCode::Esc);

    // The list shows the command step in chain order with its run state,
    // and the message row names what is unrun (§7.4).
    app.handle(Action::Open(Open::Enrichment), &provider);
    let output = screen(&draw(&provider, &mut app, 100, 30));
    assert!(
        output.contains("⚙ command · unrun · /usr/bin/true"),
        "{output}"
    );
    assert!(
        output.contains("2 steps active · 1 unrun: command"),
        "{output}"
    );
    assert!(!output.contains("Not configured"), "{output}");

    // Edit on the command row opens the External command dialog on it.
    let state = app.views.state(&view_id).unwrap();
    assert_eq!(state.enrichment_selected, 1, "a saved step is selected");
    alt(&mut app, &provider, KeyCode::Char('e'));
    assert_eq!(app.layers.stack_ids(), vec![LayerId::ExternalCommand]);
    let dialog = app.layers.external_command.state().unwrap();
    assert_eq!(dialog.stage_id, "command-1");
    assert_eq!(dialog.program, "/usr/bin/true");
    assert!(dialog.accepted.is_some());
    let notes = screen(&draw(&provider, &mut app, 120, 34));
    assert!(notes.contains("step 2 of 2"), "{notes}");
    assert!(notes.contains("later steps may read"), "{notes}");
    key(&mut app, &provider, KeyCode::Esc);

    // A second command inserted after the first expression takes the next
    // free name and lands between the two.
    app.handle(Action::Open(Open::Enrichment), &provider);
    key(&mut app, &provider, KeyCode::Up);
    alt(&mut app, &provider, KeyCode::Char('c'));
    let dialog = app.layers.external_command.state().unwrap();
    assert_eq!(dialog.name, "command2");
    assert_eq!(dialog.stage_id, "command-2");
    save_command(&mut app, &provider, "/usr/bin/env");
    assert_eq!(
        chain(&app, &view_id),
        vec![
            ("one = pl.lit(1)".to_owned(), false),
            ("command2".to_owned(), true),
            ("command".to_owned(), true),
        ]
    );
    key(&mut app, &provider, KeyCode::Esc);

    // Removing a command step from the list drops its run state with it.
    app.handle(Action::Open(Open::Enrichment), &provider);
    if let Some(state) = app.views.state_mut(&view_id) {
        state.enrichment_selected = 1;
    }
    alt(&mut app, &provider, KeyCode::Char('r'));
    accept(&mut app);
    assert_eq!(
        chain(&app, &view_id),
        vec![
            ("one = pl.lit(1)".to_owned(), false),
            ("command".to_owned(), true)
        ]
    );
    let state = app.views.state(&view_id).unwrap();
    assert!(!state.command_steps.contains_key("command-2"));
    assert_eq!(state.command_revision("command-1"), 1);
}

#[test]
fn alt_up_and_down_reorder_through_a_validated_chain_change() {
    let (provider, mut app) = demo();
    let view_id = app.active_view_id().unwrap().to_owned();
    app.handle(Action::Open(Open::Enrichment), &provider);
    accept_step(&mut app, &provider, "one = pl.lit(1)");
    alt(&mut app, &provider, KeyCode::Char('c'));
    save_command(&mut app, &provider, "/usr/bin/true");
    key(&mut app, &provider, KeyCode::Esc);
    app.handle(Action::Open(Open::Enrichment), &provider);
    assert_eq!(app.views.state(&view_id).unwrap().enrichment_selected, 1);
    alt(&mut app, &provider, KeyCode::Up);
    let request = accept(&mut app);
    assert_eq!(request.purpose, QueryPurpose::Enrichment);
    assert_eq!(
        chain(&app, &view_id),
        vec![
            ("command".to_owned(), true),
            ("one = pl.lit(1)".to_owned(), false)
        ]
    );
    let state = app.views.state(&view_id).unwrap();
    assert_eq!(
        state.enrichment_selected, 0,
        "the selection follows the step"
    );
    assert_eq!(
        state.command_revision("command-1"),
        1,
        "moving a step does not change its definition"
    );
    // At the top, Alt-Up is a no-op rather than a query.
    alt(&mut app, &provider, KeyCode::Up);
    assert!(app.take_query_requests().is_empty());
    // A rejected reorder keeps the accepted order and says why in the list.
    alt(&mut app, &provider, KeyCode::Down);
    let request = app.take_query_requests().pop().unwrap();
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: request.view_id,
        generation: request.generation,
        revision: request.revision,
        purpose: QueryPurpose::Enrichment,
        result: Err(lvu::QueryFailure {
            purpose: QueryPurpose::Enrichment,
            message: "step one reads command.x before command step command runs".into(),
        }),
    }));
    assert_eq!(
        chain(&app, &view_id),
        vec![
            ("command".to_owned(), true),
            ("one = pl.lit(1)".to_owned(), false)
        ]
    );
    let output = screen(&draw(&provider, &mut app, 100, 30));
    assert!(output.contains("before command step"), "{output}");
}

#[test]
fn a_command_step_name_is_a_valid_unique_prefix() {
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::Enrichment), &provider);
    alt(&mut app, &provider, KeyCode::Char('c'));
    paste(&mut app, &provider, "/usr/bin/true");
    // Shift-Tab from Program is the Name field.
    key(&mut app, &provider, KeyCode::BackTab);
    assert_eq!(
        app.layers.external_command.state().unwrap().selected_field,
        CommandEnrichmentField::Name
    );
    for _ in 0.."command".len() {
        key(&mut app, &provider, KeyCode::Backspace);
    }
    for (name, message) in [("raw", "output prefix"), ("1st", "output prefix")] {
        paste(&mut app, &provider, name);
        press(
            &mut app,
            &provider,
            KeyCode::Char('s'),
            KeyModifiers::CONTROL,
        );
        assert!(app.take_query_requests().is_empty(), "{name} was accepted");
        let error = app.layers.external_command.state().unwrap().error.clone();
        assert!(error.as_deref().unwrap().contains(message), "{error:?}");
        for _ in 0..name.len() {
            key(&mut app, &provider, KeyCode::Backspace);
        }
    }
    paste(&mut app, &provider, "geo");
    press(
        &mut app,
        &provider,
        KeyCode::Char('s'),
        KeyModifiers::CONTROL,
    );
    let request = accept(&mut app);
    assert_eq!(request.constraints.enrichments[0].source, "geo");
    assert_eq!(app.layers.external_command.state().unwrap().name, "geo");
    key(&mut app, &provider, KeyCode::Esc);

    // A second step may not take a name the chain already uses.
    app.handle(Action::Open(Open::Enrichment), &provider);
    alt(&mut app, &provider, KeyCode::Char('a'));
    paste(&mut app, &provider, "two = pl.lit(2)");
    key(&mut app, &provider, KeyCode::Enter);
    accept(&mut app);
    alt(&mut app, &provider, KeyCode::Char('c'));
    paste(&mut app, &provider, "/usr/bin/true");
    key(&mut app, &provider, KeyCode::BackTab);
    for _ in 0.."command".len() {
        key(&mut app, &provider, KeyCode::Backspace);
    }
    paste(&mut app, &provider, "geo");
    press(
        &mut app,
        &provider,
        KeyCode::Char('s'),
        KeyModifiers::CONTROL,
    );
    assert!(app.take_query_requests().is_empty());
    assert!(
        app.layers
            .external_command
            .state()
            .unwrap()
            .error
            .as_deref()
            .unwrap()
            .contains("already named geo")
    );
}

#[test]
fn a_recipe_with_a_missing_program_applies_unrun_and_says_so() {
    let (provider, mut app) = demo();
    let _ = provider.revision("");
    let view_id = app.active_view_id().unwrap().to_owned();
    let missing = "/nonexistent/lvu-enricher-for-this-test";
    let config = RecipeConfig {
        enrichments: vec![
            lvu::EnrichmentDefinition::expression("one".to_owned(), "one = pl.lit(1)".to_owned()),
            lvu::EnrichmentDefinition::command(
                "command-1".to_owned(),
                "geo".to_owned(),
                lvu_core::CommandDefinition {
                    program: lvu_core::CommandProgram::Exec {
                        executable: missing.into(),
                        args: vec!["--json".into()],
                    },
                    cwd: None,
                    environment: BTreeMap::new(),
                    restart: lvu_core::RestartPolicy::Never,
                },
            ),
        ],
        ..RecipeConfig::default()
    };
    app.views.apply_recipe(&view_id, config, 0).unwrap();
    let request = accept(&mut app);
    assert_eq!(request.constraints.enrichments.len(), 2);
    let notice = app.action_notice.clone().unwrap_or_default();
    assert!(notice.contains("not on this machine"), "{notice}");
    assert!(notice.contains(&format!("geo needs {missing}")), "{notice}");
    assert!(notice.contains("saved unrun"), "{notice}");
    let state = app.views.state(&view_id).unwrap();
    assert_eq!(chain(&app, &view_id)[1], ("geo".to_owned(), true));
    assert_eq!(state.command_revision("command-1"), 1);
    assert!(state.command_steps["command-1"].publication.is_none());
    // The step editor's draft is the last expression, not the command's name.
    assert_eq!(state.enrichment.draft, "one = pl.lit(1)");
    // The applied config carries the command step back out for a recipe.
    let captured = app.views.applied_recipe_config(&view_id).unwrap();
    assert!(captured.enrichments[1].is_command());
    assert!(lvu::app::program_available(std::path::Path::new("sh")));
    assert!(!lvu::app::program_available(std::path::Path::new(missing)));
}

#[test]
fn applying_a_recipe_from_the_dialog_reports_a_missing_program() {
    let (provider, mut app) = demo();
    let view_id = app.active_view_id().unwrap().to_owned();
    let missing = "/nonexistent/lvu-enricher-for-this-test";
    app.handle(
        Action::Open(Open::Recipes {
            mode: lvu::RecipeDialogMode::Browse,
        }),
        &provider,
    );
    let lvu::RecipeRequest::List { meta } = app.take_recipe_requests().pop().unwrap() else {
        panic!("recipe list")
    };
    app.set_recipes(
        meta,
        vec![lvu::RecipeItem {
            id: "chain".into(),
            revision: "one".into(),
            name: "Chain".into(),
            config: RecipeConfig {
                enrichments: vec![lvu::EnrichmentDefinition::command(
                    "command-1".to_owned(),
                    "geo".to_owned(),
                    lvu_core::CommandDefinition {
                        program: lvu_core::CommandProgram::Exec {
                            executable: missing.into(),
                            args: vec![],
                        },
                        cwd: None,
                        environment: BTreeMap::new(),
                        restart: lvu_core::RestartPolicy::Never,
                    },
                )],
                ..RecipeConfig::default()
            },
            incompatibility: None,
            saved_at_unix_nanos: None,
        }],
        None,
    );
    let layer = app.layers.top().expect("recipes open");
    app.handle(
        Action::Command(layer, lvu::command_palette::CommandId::RecipeApply),
        &provider,
    );
    let requests = app.take_query_requests();
    assert_eq!(requests.len(), 1, "{requests:?}");
    let request = requests.into_iter().next().unwrap();
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: request.view_id.clone(),
        generation: request.generation,
        revision: request.revision,
        purpose: request.purpose,
        result: Ok(()),
    }));
    let notice = app.action_notice.clone().unwrap_or_default();
    assert!(notice.contains("not on this machine"), "{notice}");
    assert_eq!(chain(&app, &view_id), vec![("geo".to_owned(), true)]);
    let output = screen(&draw(&provider, &mut app, 120, 30));
    assert!(output.contains("not on this machine"), "{output}");
}
