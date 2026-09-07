//! Acceptance for the Recipes layer and its History child (docs/component-model.md
//! §6.3 step 9). What is asserted here is the two seams the step introduces —
//! `Views::apply_recipe` and the `RecipeRequest` outbox with its
//! `RecipeRequestMeta` fence — and the first parent/child stack (§5.3): the
//! parent keeps its list and keeps rendering underneath, the child's requests
//! cannot be answered by the parent's fence, and dismissal pops one layer.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use lvu::{
    Action, App, RecipeRequest, RowProvider,
    app::{RecipeConfig, RecipeDialogControl, RecipeDialogMode, RecipeItem, RecipeSuggestion},
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

fn item(name: &str) -> RecipeItem {
    RecipeItem {
        id: format!("id-{name}"),
        revision: format!("rev-{name}"),
        name: name.to_owned(),
        config: RecipeConfig {
            advanced: "pl.col('level') == 'ERROR'".into(),
            ..RecipeConfig::default()
        },
        incompatibility: None,
    }
}

/// Answers whatever request the top slot has outstanding.
fn deliver(app: &mut App, items: Vec<RecipeItem>, suggestions: Vec<RecipeSuggestion>) {
    let meta = app
        .take_recipe_requests()
        .into_iter()
        .rev()
        .find_map(|request| match request {
            RecipeRequest::List { meta } | RecipeRequest::History { meta, .. } => Some(meta),
            _ => None,
        })
        .expect("a list or history request is outstanding");
    app.set_recipes_with_suggestions(meta, items, suggestions, None);
}

fn open_with(provider: &FixtureProvider, app: &mut App, items: Vec<RecipeItem>) {
    app.handle(
        Action::Open(Open::Recipes {
            mode: RecipeDialogMode::Browse,
        }),
        provider,
    );
    deliver(app, items, Vec::new());
}

#[test]
fn every_recorded_control_was_painted_and_answers_the_hit_test() {
    for (width, height) in SIZES {
        let (provider, mut app) = demo();
        open_with(&provider, &mut app, vec![item("one"), item("two")]);
        // Open the anchored menu too: §10 lets it extend past the dialog, and
        // its rows are the component's geometry rather than a second layer.
        for _ in 0..16 {
            if app.layers.recipes.state().control == RecipeDialogControl::More {
                break;
            }
            key(&mut app, &provider, KeyCode::Tab);
        }
        key(&mut app, &provider, KeyCode::Enter);
        draw(&provider, &mut app, width, height);
        let surface = app.layers.recipes.surface();
        for (rect, control) in app.layers.recipes.control_rects() {
            assert!(
                contains(surface.popup, (rect.x, rect.y)),
                "{control:?} at {width}x{height} is outside what the layer drew"
            );
        }
        for (rect, index) in app.layers.recipes.menu_rects() {
            let point = (rect.x, rect.y);
            assert!(
                contains(surface.popup, point),
                "menu row {index} at {width}x{height} escapes the popup"
            );
            assert_eq!(
                app.layers.recipes.hit(point),
                Some(lvu::components::recipes::RecipeHit::Menu(*index)),
                "a drawn menu row must answer its own hit test"
            );
        }
        assert_eq!(
            app.hit_regions.selection_modal,
            Some(surface.interior),
            "the shell's selection bound is the surface the component published"
        );
    }
}

#[test]
fn applying_a_recipe_goes_through_the_view_seam_and_closes_the_layer() {
    let (provider, mut app) = demo();
    open_with(&provider, &mut app, vec![item("errors")]);
    let view = app.active_view_id().unwrap().to_owned();
    let before = app.view_interaction_revision(&view).unwrap();
    key(&mut app, &provider, KeyCode::Enter);
    assert!(app.layers.top().is_none(), "applying closes the layer");
    let request = app.take_query_requests().pop().expect("one query");
    assert_eq!(
        request.constraints.advanced_polars.as_deref(),
        Some("pl.col('level') == 'ERROR'")
    );
    assert!(app.view_interaction_revision(&view).unwrap() > before);
}

/// A recipe is a whole definition, so a refusal leaves the applied view exactly
/// as it was — the draft is not partially written.
#[test]
fn a_refused_recipe_leaves_the_applied_view_untouched() {
    let (provider, mut app) = demo();
    open_with(&provider, &mut app, vec![item("errors")]);
    // Fill the submission queue so the seam refuses.
    for _ in 0..64 {
        app.handle(Action::OpenSearch, &provider);
        app.handle(Action::EditorPaste("x".into()), &provider);
        app.handle(Action::SubmitDraft, &provider);
        app.handle(Action::CancelEditor, &provider);
    }
    let applied = app.advanced_state().unwrap().applied.clone();
    app.handle(
        Action::Open(Open::Recipes {
            mode: RecipeDialogMode::Browse,
        }),
        &provider,
    );
    deliver(&mut app, vec![item("errors")], Vec::new());
    key(&mut app, &provider, KeyCode::Enter);
    assert_eq!(
        app.advanced_state().unwrap().applied,
        applied,
        "the accepted filter survives a refusal"
    );
}

/// §6.5: History is its own layer, reached and left by `Replace`. One layer is
/// on the stack at a time — nothing is scrimmed twice — and Escape closes the
/// dialog rather than stepping back through a stack.
#[test]
fn history_is_reached_and_left_by_replace() {
    let (provider, mut app) = demo();
    open_with(&provider, &mut app, vec![item("errors")]);
    alt(&mut app, &provider, KeyCode::Char('h'));
    assert_eq!(app.layers.stack_ids(), vec![LayerId::RecipeHistory]);
    let RecipeRequest::History { recipe_id, .. } = app
        .take_recipe_requests()
        .into_iter()
        .find(|request| matches!(request, RecipeRequest::History { .. }))
        .expect("History asks for revisions")
    else {
        unreachable!()
    };
    assert_eq!(recipe_id, "id-errors");
    assert_eq!(app.layers.recipes.state().mode, RecipeDialogMode::History);
    let rendered = screen(&draw(&provider, &mut app, 100, 30));
    assert!(
        rendered.contains("history"),
        "History renders its breadcrumb"
    );

    // Alt-B replaces back into Browse and re-lists, as the mode switch did.
    alt(&mut app, &provider, KeyCode::Char('b'));
    assert_eq!(app.layers.stack_ids(), vec![LayerId::Recipes]);
    assert!(app.layers.recipes.state().loading);
    assert!(
        app.take_recipe_requests()
            .iter()
            .any(|request| matches!(request, RecipeRequest::List { .. }))
    );

    key(&mut app, &provider, KeyCode::Esc);
    assert!(app.layers.top().is_none(), "Escape closes the dialog");
}

/// A mode reached from History acts on the revision the user selected there:
/// the `Replace` carries the list, the selection and the fence id across, which
/// is what it means for the two surfaces to be one dialog (§6.5).
#[test]
fn a_mode_entered_from_history_still_acts_on_the_selected_revision() {
    let (provider, mut app) = demo();
    open_with(&provider, &mut app, vec![item("errors")]);
    alt(&mut app, &provider, KeyCode::Char('h'));
    deliver(&mut app, vec![item("older"), item("newer")], Vec::new());
    key(&mut app, &provider, KeyCode::Down);
    let state = app.layers.recipes.state();
    let selected = state.items[state.selected].clone();

    alt(&mut app, &provider, KeyCode::Char('u'));
    assert_eq!(app.layers.stack_ids(), vec![LayerId::Recipes]);
    assert_eq!(app.layers.recipes.state().mode, RecipeDialogMode::Update);
    assert_eq!(app.layers.recipes.state().name, selected.name);
    key(&mut app, &provider, KeyCode::Enter);
    let RecipeRequest::Save { update, .. } = app
        .take_recipe_requests()
        .into_iter()
        .find(|request| matches!(request, RecipeRequest::Save { .. }))
        .expect("update request")
    else {
        unreachable!()
    };
    assert_eq!(update, Some((selected.id, selected.revision)));
}

/// A stale answer to the request the dialog abandoned cannot replace the one it
/// is waiting for: one dialog, one generation, one pending request id.
#[test]
fn a_stale_history_answer_cannot_replace_a_live_browse_request() {
    let (provider, mut app) = demo();
    open_with(&provider, &mut app, vec![item("errors")]);
    alt(&mut app, &provider, KeyCode::Char('h'));
    let history_meta = app
        .take_recipe_requests()
        .into_iter()
        .find_map(|request| match request {
            RecipeRequest::History { meta, .. } => Some(meta),
            _ => None,
        })
        .expect("history request");
    alt(&mut app, &provider, KeyCode::Char('b'));
    app.set_recipes(history_meta, vec![item("revision")], None);
    assert!(
        app.layers.recipes.state().loading,
        "the abandoned history answer is ignored and the list stays pending"
    );
}

/// A suggestion the user rejects is reported once and stops being offered; it
/// never deletes the saved recipe.
#[test]
fn rejecting_a_suggestion_reports_it_and_keeps_the_recipe() {
    let (provider, mut app) = demo();
    app.handle(
        Action::Open(Open::Recipes {
            mode: RecipeDialogMode::Browse,
        }),
        &provider,
    );
    let entry = item("errors");
    deliver(
        &mut app,
        vec![entry.clone()],
        vec![RecipeSuggestion {
            recipe_id: entry.id.clone(),
            evidence: vec!["same project".into()],
            missing_fields: Vec::new(),
        }],
    );
    key(&mut app, &provider, KeyCode::Char('x'));
    let outcome = app
        .take_recipe_requests()
        .into_iter()
        .find_map(|request| match request {
            RecipeRequest::Outcome(outcome) => Some(outcome),
            _ => None,
        })
        .expect("the rejection is reported");
    assert!(!outcome.accepted);
    assert_eq!(outcome.recipe_id, entry.id);
    assert!(app.layers.recipes.state().suggestions.is_empty());
    assert_eq!(app.layers.recipes.state().items, vec![entry]);
}

#[test]
fn clicks_outside_the_popup_are_contained() {
    let (provider, mut app) = demo();
    open_with(&provider, &mut app, vec![item("errors")]);
    draw(&provider, &mut app, 140, 40);
    let popup = app.layers.recipes.surface().popup;
    let selected = app.layers.recipes.state().selected;
    click(&mut app, &provider, (0, 0));
    assert!(!contains(popup, (0, 0)));
    assert_eq!(app.layers.top(), Some(LayerId::Recipes));
    assert_eq!(app.layers.recipes.state().selected, selected);
}
