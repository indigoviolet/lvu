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
        saved_at_unix_nanos: None,
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
        app.handle(Action::Open(Open::Search), &provider);
        app.handle(Action::Raw(RawEvent::Paste("x".into())), &provider);
        key(&mut app, &provider, KeyCode::Enter);
        key(&mut app, &provider, KeyCode::Esc);
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

/// §12.9: History dates each revision, because "which of these is recent" is
/// the question a list of revisions raises and the revision id answers none.
///
/// The rows go through the same list rendering the browse mode uses, so this
/// also pins that a revision carries its *own* date rather than the recipe's.
#[test]
fn history_dates_every_revision_it_lists() {
    let (provider, mut app) = demo();
    open_with(&provider, &mut app, vec![item("errors")]);
    alt(&mut app, &provider, KeyCode::Char('h'));
    assert_eq!(app.layers.stack_ids(), vec![LayerId::RecipeHistory]);

    // 2026-09-06T12:00:00Z and a day later.
    let saved = 1_788_696_000_000_000_000i64;
    let newest = RecipeItem {
        revision: "rev-newest".into(),
        saved_at_unix_nanos: Some(saved + 86_400_000_000_000),
        ..item("errors")
    };
    let oldest = RecipeItem {
        revision: "rev-oldest".into(),
        saved_at_unix_nanos: Some(saved),
        ..item("errors")
    };
    // And one written before the document carried a date.
    let undated = RecipeItem {
        revision: "rev-undated".into(),
        saved_at_unix_nanos: None,
        ..item("errors")
    };
    deliver(&mut app, vec![newest, oldest, undated], Vec::new());

    let rendered = screen(&draw(&provider, &mut app, 100, 30));
    let rows = rendered
        .lines()
        // The breadcrumb title names the recipe too; the rows are the ones
        // that also carry a summary.
        .filter(|line| line.contains("errors") && line.contains("advanced filter"))
        .collect::<Vec<_>>();
    assert!(rows.len() >= 3, "three revisions are listed\n{rendered}");
    assert!(rows[0].contains("2026-09-07"), "{rendered}");
    assert!(rows[1].contains("2026-09-06"), "{rendered}");
    assert!(
        rows[2].contains('—'),
        "an undated revision says so\n{rendered}"
    );
}

fn scroll(app: &mut App, provider: &FixtureProvider, point: (u16, u16), down: bool) {
    app.handle(
        Action::Raw(RawEvent::Mouse(MouseEvent {
            kind: if down {
                MouseEventKind::ScrollDown
            } else {
                MouseEventKind::ScrollUp
            },
            column: point.0,
            row: point.1,
            modifiers: KeyModifiers::NONE,
        })),
        provider,
    );
}

fn focus_control(app: &mut App, provider: &FixtureProvider, control: RecipeDialogControl) {
    for _ in 0..32 {
        if app.layers.recipes.state().control == control {
            return;
        }
        key(app, provider, KeyCode::Tab);
    }
    panic!("{control:?} never took focus");
}

fn open_more(app: &mut App, provider: &FixtureProvider) {
    focus_control(app, provider, RecipeDialogControl::More);
    key(app, provider, KeyCode::Enter);
    assert!(app.layers.recipes.state().menu_open);
}

/// Policy/stable budgets alone determine the frame and sticky tail origins:
/// pending, empty, populated and error states share one outer frame and one
/// action-band origin at every responsive size. Loading/results/errors only
/// move the scroll extent, never the frame.
#[test]
fn responsive_frame_and_tail_are_stable_across_async_states() {
    for (width, height) in [(240u16, 80u16), (140, 40), (80, 24), (54, 16), (20, 6)] {
        // Pending: opening starts a list request with no rows yet.
        let (provider, mut pending) = demo();
        pending.handle(
            Action::Open(Open::Recipes {
                mode: RecipeDialogMode::Browse,
            }),
            &provider,
        );
        let pending_popup = draw(&provider, &mut pending, width, height);
        let pending_surface = pending.layers.recipes.surface();
        let pending_apply = pending
            .layers
            .recipes
            .control_rects()
            .iter()
            .find(|(_, c)| *c == RecipeDialogControl::Apply)
            .map(|(r, _)| *r);

        // Empty: fresh dialog, one answered list request.
        let (provider, mut empty_app) = demo();
        empty_app.handle(
            Action::Open(Open::Recipes {
                mode: RecipeDialogMode::Browse,
            }),
            &provider,
        );
        deliver(&mut empty_app, Vec::new(), Vec::new());
        draw(&provider, &mut empty_app, width, height);
        let empty_surface = empty_app.layers.recipes.surface();
        let empty_apply = empty_app
            .layers
            .recipes
            .control_rects()
            .iter()
            .find(|(_, c)| *c == RecipeDialogControl::Apply)
            .map(|(r, _)| *r);

        // Populated: fresh dialog, one answered list request with rows.
        let (provider, mut full_app) = demo();
        full_app.handle(
            Action::Open(Open::Recipes {
                mode: RecipeDialogMode::Browse,
            }),
            &provider,
        );
        deliver(
            &mut full_app,
            vec![item("one"), item("two"), item("three")],
            Vec::new(),
        );
        draw(&provider, &mut full_app, width, height);
        let full_surface = full_app.layers.recipes.surface();
        let full_apply = full_app
            .layers
            .recipes
            .control_rects()
            .iter()
            .find(|(_, c)| *c == RecipeDialogControl::Apply)
            .map(|(r, _)| *r);

        // Error (same empty list, failed status).
        let (provider, mut failed) = demo();
        failed.handle(
            Action::Open(Open::Recipes {
                mode: RecipeDialogMode::Browse,
            }),
            &provider,
        );
        let meta = failed
            .take_recipe_requests()
            .into_iter()
            .rev()
            .find_map(|request| match request {
                RecipeRequest::List { meta } | RecipeRequest::History { meta, .. } => Some(meta),
                _ => None,
            })
            .expect("list request outstanding");
        failed.set_recipes_with_suggestions(
            meta,
            Vec::new(),
            Vec::new(),
            Some("workspace busy".into()),
        );
        draw(&provider, &mut failed, width, height);
        let error_surface = failed.layers.recipes.surface();

        for (name, surface) in [
            ("pending", pending_surface),
            ("empty", empty_surface),
            ("populated", full_surface),
            ("error", error_surface),
        ] {
            assert_eq!(
                surface.popup, pending_surface.popup,
                "{name} frame moved at {width}x{height}: {surface:?} vs {pending_surface:?}"
            );
            assert_eq!(
                surface.interior, pending_surface.interior,
                "{name} interior moved at {width}x{height}"
            );
        }
        assert_eq!(
            empty_apply, pending_apply,
            "tail moved (empty) at {width}x{height}"
        );
        assert_eq!(
            full_apply, pending_apply,
            "tail moved (populated) at {width}x{height}"
        );
        let _ = pending_popup;
    }
}

/// Below the 20x6 floor the existing tiny fallback owns the frame: no dialog
/// popup, no stale hitboxes, Escape still closes the layer.
#[test]
fn below_floor_uses_the_tiny_fallback() {
    let (provider, mut app) = demo();
    open_with(&provider, &mut app, vec![item("one")]);
    let buffer = draw(&provider, &mut app, 19, 5);
    assert!(
        screen(&buffer).contains("terminal too small"),
        "19x5 must take the tiny fallback"
    );
    assert!(app.layers.recipes.row_rects().is_empty());
    assert!(app.layers.recipes.control_rects().is_empty());
    assert!(app.layers.recipes.menu_rects().is_empty());
}

/// Long Unicode names/paths clip by display width with a visible ellipsis,
/// never split a wide glyph or start with an orphaned combining mark, and
/// every painted row/field/menu cell still answers its own hit test with the
/// caret inside the field.
#[test]
fn long_unicode_clips_by_display_width_with_exact_hitboxes_and_caret() {
    for (width, height) in [(140u16, 40u16), (80, 24), (54, 16)] {
        let (provider, mut app) = demo();
        let names = [
            "portable caf\u{e9} \u{65e5}\u{672c}\u{8a9e} \u{1f469}\u{200d}\u{1f4bb} e\u{301}xpansion",
            "\u{65e5}\u{672c}\u{8a9e}\u{65e5}\u{672c}\u{8a9e}\u{65e5}\u{672c}\u{8a9e}\u{65e5}\u{672c}\u{8a9e} tail",
            "combining e\u{301}\u{301}\u{301} and wide \u{6f22}\u{5b57} mix",
        ];
        let items = names.iter().map(|name| item(name)).collect::<Vec<_>>();
        open_with(&provider, &mut app, items);
        // Put a Unicode path in the field via Export mode.
        alt(&mut app, &provider, KeyCode::Char('e'));
        for ch in "portable caf\u{e9} \u{65e5}\u{672c}.toml".chars() {
            key(&mut app, &provider, KeyCode::Char(ch));
        }
        // Focus the field so the caret draws; the Unicode tail must stay
        // intact with the caret inside its own rect.
        focus_control(&mut app, &provider, RecipeDialogControl::Input);
        draw(&provider, &mut app, width, height);
        let surface = app.layers.recipes.surface();
        for (rect, _) in app.layers.recipes.row_rects() {
            assert!(
                contains(surface.popup, (rect.x, rect.y)),
                "unicode row escapes popup at {width}x{height}"
            );
            assert!(
                app.layers.recipes.hit((rect.x, rect.y)).is_some(),
                "unicode row not hit-testable at {width}x{height}"
            );
        }
        for (rect, _) in app.layers.recipes.control_rects() {
            assert!(
                contains(surface.popup, (rect.x, rect.y)),
                "control escapes popup at {width}x{height}"
            );
            assert!(
                app.layers.recipes.hit((rect.x, rect.y)).is_some(),
                "control not hit-testable at {width}x{height}"
            );
        }
        let field = app
            .layers
            .recipes
            .control_rects()
            .iter()
            .find(|(_, c)| *c == RecipeDialogControl::Input)
            .map(|(r, _)| *r)
            .expect("field must be painted");
        let caret = app.layers.recipes.surface().caret.expect("caret must draw");
        assert!(
            contains(field, caret),
            "unicode caret {caret:?} outside field {field:?} at {width}x{height}"
        );
        let rendered = screen(&draw(&provider, &mut app, width, height));
        assert!(
            rendered.contains("caf") && rendered.contains("Recipes"),
            "unicode dialog must render at {width}x{height}\n{rendered}"
        );
    }
}

/// More is a shared Anchored popup: below when room, above when the action
/// band sits at the viewport bottom, clamped in x, at most eight rows with
/// display-width sizing, and the same rects paint, select and hit-test.
#[test]
fn more_popup_is_anchored_below_above_and_clamped_with_shared_hitboxes() {
    for (width, height) in [(240u16, 80u16), (140, 40), (80, 24), (54, 16), (20, 6)] {
        let (provider, mut app) = demo();
        open_with(&provider, &mut app, vec![item("one"), item("two")]);
        open_more(&mut app, &provider);
        draw(&provider, &mut app, width, height);
        let surface = app.layers.recipes.surface();
        let menu = app.layers.recipes.menu_rects().to_vec();
        assert!(!menu.is_empty(), "menu must paint at {width}x{height}");
        assert!(
            menu.len() <= 8,
            "anchored popup shows at most eight rows at {width}x{height}"
        );
        // Frame-bounded (terminal frame, not the dialog): every menu cell is
        // inside the viewport and inside the published popup union.
        for (rect, index) in &menu {
            assert!(
                rect.x + rect.width <= width && rect.y + rect.height <= height,
                "menu row {index} escapes {width}x{height}: {rect:?}"
            );
            assert!(
                contains(surface.popup, (rect.x, rect.y)),
                "menu row {index} escapes popup at {width}x{height}"
            );
            assert_eq!(
                app.layers.recipes.hit((rect.x, rect.y)),
                Some(lvu::components::recipes::RecipeHit::Menu(*index)),
                "menu paint/hit disagree at {width}x{height}"
            );
        }
        // Display-width sizing: all menu rows share one width that fits the
        // longest label plus chrome (border adds two plus one scrollbar when
        // the merged menu scrolls), clamped to the frame.
        let menu_width = menu[0].0.width;
        assert!(
            menu_width >= 8,
            "anchored popup too narrow at {width}x{height}: row {menu_width}"
        );
        for (rect, _) in &menu {
            assert_eq!(
                rect.width, menu_width,
                "menu rows share width at {width}x{height}"
            );
        }
        // Selection follows Down and stays on a painted row.
        let before = app.layers.recipes.state().menu_selected;
        key(&mut app, &provider, KeyCode::Down);
        draw(&provider, &mut app, width, height);
        let after = app.layers.recipes.state().menu_selected;
        assert_ne!(
            before, after,
            "menu selection must move at {width}x{height}"
        );
        let selected_rect = app
            .layers
            .recipes
            .menu_rects()
            .iter()
            .find(|(_, idx)| *idx == after)
            .expect("selected menu row must stay painted via shared reveal");
        assert!(
            contains(surface.popup, (selected_rect.0.x, selected_rect.0.y)),
            "selected menu row must stay painted at {width}x{height}"
        );
    }
}

/// At 54x16 and 20x6 the field and the default action stay painted and the
/// wheel moves the shared list window with hitboxes that match the paint.
#[test]
fn tiny_pressure_keeps_field_and_actions_reachable_with_matching_wheel() {
    for (width, height) in [(54u16, 16u16), (20, 6)] {
        let (provider, mut app) = demo();
        let items = (0..20)
            .map(|n| item(&format!("recipe-{n:02}")))
            .collect::<Vec<_>>();
        open_with(&provider, &mut app, items);
        // List focus: the selection is painted and hit-testable.
        focus_control(&mut app, &provider, RecipeDialogControl::List);
        draw(&provider, &mut app, width, height);
        let surface = app.layers.recipes.surface();
        assert!(
            !app.layers.recipes.row_rects().is_empty(),
            "list must paint at {width}x{height}"
        );
        for (rect, index) in app.layers.recipes.row_rects() {
            assert!(
                contains(surface.popup, (rect.x, rect.y)),
                "list row escapes at {width}x{height}"
            );
            assert_eq!(
                app.layers.recipes.hit((rect.x, rect.y)),
                Some(lvu::components::recipes::RecipeHit::Row(*index)),
                "list paint/hit disagree at {width}x{height}"
            );
        }
        // Wheel over the painted viewport moves the shared window; the new
        // selection is painted and hit-testable (no second geometry).
        let center = {
            let (rect, _) = app.layers.recipes.row_rects()[0];
            (rect.x, rect.y)
        };
        let before = app.layers.recipes.state().selected;
        scroll(&mut app, &provider, center, true);
        draw(&provider, &mut app, width, height);
        assert_ne!(
            before,
            app.layers.recipes.state().selected,
            "wheel must scroll the list at {width}x{height}"
        );
        let (rect, index) = app.layers.recipes.row_rects()[0];
        assert_eq!(
            app.layers.recipes.hit((rect.x, rect.y)),
            Some(lvu::components::recipes::RecipeHit::Row(index)),
            "wheel paint/hit disagree at {width}x{height}"
        );
        // Scrollability is real overflow only: a populated list with a
        // painted scrollbar scrolls while list-focused.
        assert!(
            app.layers.recipes.surface().scrollable,
            "populated list must be scrollable while list-focused at {width}x{height}"
        );
        // Field focus: the field and its caret are painted and hit-testable,
        // and the filled default action never leaves the sticky band.
        focus_control(&mut app, &provider, RecipeDialogControl::Input);
        draw(&provider, &mut app, width, height);
        let field = app
            .layers
            .recipes
            .control_rects()
            .iter()
            .find(|(_, c)| *c == RecipeDialogControl::Input)
            .map(|(r, _)| *r)
            .expect("field must stay reachable at tiny pressure");
        assert!(contains(surface.popup, (field.x, field.y)));
        assert_eq!(
            app.layers.recipes.hit((field.x, field.y)),
            Some(lvu::components::recipes::RecipeHit::Control(
                RecipeDialogControl::Input
            )),
            "field paint/hit disagree at {width}x{height}"
        );
        if let Some(caret) = app.layers.recipes.surface().caret {
            assert!(
                contains(field, caret),
                "caret must stay in field at {width}x{height}"
            );
        }
        let apply = app
            .layers
            .recipes
            .control_rects()
            .iter()
            .find(|(_, c)| *c == RecipeDialogControl::Apply)
            .map(|(r, _)| *r)
            .expect("default action must survive tiny pressure");
        assert!(contains(surface.popup, (apply.x, apply.y)));
    }
    // Empty list claims no list overflow (menu closed): not scrollable.
    let (provider, mut empty) = demo();
    open_with(&provider, &mut empty, Vec::new());
    draw(&provider, &mut empty, 54, 16);
    assert!(
        !empty.layers.recipes.surface().scrollable,
        "empty list must not claim scrollability"
    );
}
