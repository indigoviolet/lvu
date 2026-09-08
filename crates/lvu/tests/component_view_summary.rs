//! Acceptance for the View summary layer (docs/dialog-system.md §12.22): one
//! read-only stack of everything applied to the active view. What is asserted
//! is the contract the layer exists for — every kind of applied state is on
//! the stack, worded as its owning dialog words it; a row for an operation
//! that is not applied reads `—` so the list keeps its shape; and Enter on a
//! row replaces the summary with the dialog that owns that row, with the
//! row's item selected.

use std::collections::BTreeMap;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use lvu::{
    Action, App, CaptureTimePolicy, ColorRule, EnrichmentDefinition, RowProvider, RuleColor,
    TimeBasis, ViewDialogMode, ViewRole,
    app::{CommandStepState, Focus},
    component::{Component, LayerId, Open, RawEvent},
    components::view_summary::{SummaryControl, SummaryHit, SummaryRow, summary_rows},
    fixture::FixtureProvider,
    provider::ViewOrder,
    theme::Theme,
    ui,
};
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};

fn demo() -> (FixtureProvider, App) {
    let (provider, sources, views) = FixtureProvider::demo();
    let mut app = App::new(sources, views, true);
    app.sync_provider(&provider, 8);
    (provider, app)
}

fn draw<P: RowProvider>(provider: &P, app: &mut App, width: u16, height: u16) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| ui::render_with_theme(frame, app, provider, Theme::LOVE_DARK, None))
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

fn key(app: &mut App, provider: &FixtureProvider, code: KeyCode, modifiers: KeyModifiers) {
    app.handle(
        Action::Raw(RawEvent::Key(KeyEvent::new(code, modifiers))),
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

fn command_step(name: &str) -> EnrichmentDefinition {
    EnrichmentDefinition::command(
        format!("command-{name}"),
        name.to_owned(),
        lvu_core::CommandDefinition {
            program: lvu_core::CommandProgram::Exec {
                executable: "/usr/bin/enricher".into(),
                args: vec!["--json".into()],
            },
            cwd: None,
            environment: BTreeMap::new(),
            restart: lvu_core::RestartPolicy::Never,
        },
    )
}

/// Every kind of state the summary lists, applied directly to the active view
/// the way the shell's completions would leave it.
fn apply_everything(app: &mut App) {
    let view_id = app.active_view_id().unwrap().to_owned();
    app.set_view_role(&view_id, ViewRole::Derived);
    let state = app.views.active_mut().unwrap();
    state.source_ids = vec!["api".into(), "worker".into()];
    state.applied_capture_time_policy = Some(CaptureTimePolicy::Recent { seconds: 300 });
    state.applied_time_basis = TimeBasis::Event;
    state.enrichments = vec![
        EnrichmentDefinition::expression("step-1", "name4 = pl.col('time')"),
        command_step("geo"),
    ];
    state.command_steps.insert(
        "command-geo".into(),
        CommandStepState {
            revision: 1,
            publication: None,
        },
    );
    state.search.applied = "retry".into();
    state.advanced.applied = "pl.col('level') >= 40".into();
    state.grouping.applied = "^\\s+at ".into();
    state.fold_enabled = true;
    state.fold_key_column = Some("daemonVersion".into());
    state.fold_minimum_run = 3;
    state.fold_lookback = 0;
    state.pinned_columns = vec!["level".into(), "module".into()];
    state.color_field = Some("level".into());
    state.color_rules = vec![
        ColorRule {
            predicate: "level:error".into(),
            color: RuleColor::Red,
        },
        ColorRule {
            predicate: "/retry/i".into(),
            color: RuleColor::Orange,
        },
    ];
}

fn open_summary(app: &mut App, provider: &FixtureProvider) {
    app.handle(Action::Open(Open::ViewSummary), provider);
    assert_eq!(app.layers.stack, vec![LayerId::ViewSummary]);
    assert_eq!(app.focus, Focus::Layer);
}

#[test]
fn every_kind_of_applied_state_is_on_the_stack_in_evaluation_order() {
    let (provider, mut app) = demo();
    apply_everything(&mut app);
    let rows = summary_rows(&app.views, &app.sources, None, false);
    let values: Vec<(SummaryRow, &str)> = rows
        .iter()
        .map(|entry| (entry.row, entry.value.as_str()))
        .collect();
    assert_eq!(
        values,
        vec![
            (SummaryRow::Role, "derived from API fixture"),
            (
                SummaryRow::Sources,
                "API fixture, Worker fixture (merged, capture (arrival))"
            ),
            (SummaryRow::Time, "rolling last 5m · basis: Recognized"),
            (
                SummaryRow::Enrichment,
                "2 steps: name4 = pl.col('time'), ⚙ geo · unrun"
            ),
            (SummaryRow::Search, "retry"),
            (SummaryRow::Filter, "pl.col('level') >= 40"),
            (SummaryRow::Grouping, "^\\s+at"),
            (SummaryRow::Fold, "by daemonVersion · adjacent · min 3"),
            (SummaryRow::Columns, "pinned: level, module"),
            (
                SummaryRow::Colour,
                "by level · 2 rules: 1 level:error → red, 2 /retry/i → orange"
            ),
            (SummaryRow::Readiness, "1 command step unrun: geo"),
        ]
    );
    assert!(rows.iter().all(|entry| entry.applied));

    // The provider's merged-order report replaces the old hard-coded capture
    // claim, including the chosen basis and whether every source is ordered.
    let ordered = summary_rows(
        &app.views,
        &app.sources,
        Some(ViewOrder {
            basis: TimeBasis::Event,
            sources: 2,
            out_of_order: 0,
            interleaved: true,
        }),
        false,
    );
    assert_eq!(
        ordered[1].value,
        "API fixture, Worker fixture (merged, recognized · merged)"
    );

    // Drawn, every row is on screen with its label, and the title names the
    // view. 100x30 is the size the dialog fits at without scrolling.
    open_summary(&mut app, &provider);
    let rendered = screen(&draw(&provider, &mut app, 100, 30));
    assert!(rendered.contains("View summary · All events"), "{rendered}");
    for (row, _) in &values {
        assert!(
            rendered.contains(row.label()),
            "{} missing:\n{rendered}",
            row.label()
        );
    }
    // Values are asserted exactly above. The fixed-width list deliberately
    // truncates long values, so rendering asserts the meaningful visible
    // prefixes rather than requiring the untruncated backing strings.
    assert!(rendered.contains("merged, capture (arri…"), "{rendered}");
    assert!(
        rendered.contains("2 rules: 1 level:error → red"),
        "{rendered}"
    );
    assert!(rendered.contains("1 of 11"), "{rendered}");
    assert!(
        rendered.contains("Updating") && rendered.contains("8 of 8 operations applied"),
        "{rendered}"
    );
    assert!(rendered.contains("[ Open ]"), "{rendered}");
}

#[test]
fn a_view_with_nothing_applied_keeps_the_shape_and_reads_a_dash_per_operation() {
    let (provider, mut app) = demo();
    let view_id = app.active_view_id().unwrap().to_owned();
    app.set_view_role(&view_id, ViewRole::Canonical);
    let rows = summary_rows(&app.views, &app.sources, None, false);
    assert_eq!(rows.len(), SummaryRow::ALL.len());
    for entry in &rows {
        if entry.row.is_operation() {
            assert_eq!(entry.value, "—", "{:?}", entry.row);
            assert!(!entry.applied);
        }
    }
    assert_eq!(rows[0].value, "All events of API fixture · fixed");
    assert_eq!(rows[1].value, "API fixture");
    assert_eq!(rows.last().unwrap().value, "ready");

    open_summary(&mut app, &provider);
    let rendered = screen(&draw(&provider, &mut app, 100, 30));
    assert!(rendered.contains("no operation applied"), "{rendered}");
    assert_eq!(rendered.matches(" · —").count(), 8, "{rendered}");

    // The time basis alone is an applied operation: a window of all times
    // under a non-default basis is still a choice the Time dialog owns.
    app.views.active_mut().unwrap().applied_time_basis = TimeBasis::Extracted;
    let rows = summary_rows(&app.views, &app.sources, None, false);
    assert_eq!(rows[2].value, "all times · basis: Extracted");

    // ASCII: the dash and the separator have fallbacks, like every glyph.
    let ascii = summary_rows(&app.views, &app.sources, None, true);
    assert_eq!(ascii[4].value, "-");
}

#[test]
fn enter_on_each_row_replaces_the_summary_with_the_owning_layer_and_its_item() {
    let (provider, mut app) = demo();
    apply_everything(&mut app);
    let expected: [(SummaryRow, LayerId); 11] = [
        (SummaryRow::Role, LayerId::View),
        (SummaryRow::Sources, LayerId::View),
        (SummaryRow::Time, LayerId::Time),
        (SummaryRow::Enrichment, LayerId::Enrichment),
        (SummaryRow::Search, LayerId::Filter),
        (SummaryRow::Filter, LayerId::Filter),
        (SummaryRow::Grouping, LayerId::Grouping),
        (SummaryRow::Fold, LayerId::Folding),
        (SummaryRow::Columns, LayerId::Fields),
        (SummaryRow::Colour, LayerId::ColorRules),
        (SummaryRow::Readiness, LayerId::Enrichment),
    ];
    for (index, (row, layer)) in expected.iter().enumerate() {
        open_summary(&mut app, &provider);
        draw(&provider, &mut app, 100, 30);
        for _ in 0..index {
            key(&mut app, &provider, KeyCode::Down, KeyModifiers::NONE);
        }
        assert_eq!(app.layers.view_summary.selected_row(), *row);
        key(&mut app, &provider, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(
            app.layers.stack,
            vec![*layer],
            "{row:?}: Enter replaces the summary with its owner"
        );
        assert!(!app.layers.view_summary.is_open());
        match row {
            SummaryRow::Role => assert_eq!(app.layers.view.mode(), ViewDialogMode::Clone),
            SummaryRow::Sources => assert_eq!(app.layers.view.mode(), ViewDialogMode::Sources),
            SummaryRow::Search => {
                assert_eq!(app.layers.filter.purpose(), lvu::QueryPurpose::Search);
            }
            SummaryRow::Filter => {
                assert_eq!(app.layers.filter.purpose(), lvu::QueryPurpose::Advanced);
            }
            SummaryRow::Enrichment => {
                assert_eq!(app.view_state().unwrap().enrichment_selected, 0);
            }
            // Readiness names the unrun command step, so Enrichment opens on it.
            SummaryRow::Readiness => {
                assert_eq!(app.view_state().unwrap().enrichment_selected, 1);
            }
            SummaryRow::Columns => {
                // The demo record's fields are its `level`, `message`, … pairs
                // in record order; the list opens on the named one.
                let rendered = screen(&draw(&provider, &mut app, 100, 30));
                assert!(rendered.contains("Fields"), "{rendered}");
                let selected = app.view_state().unwrap().field_picker_selected;
                let row = lvu::components::fields::anchored_row(&app.views, &provider).unwrap();
                let fields = lvu::components::fields::field_rows(&row, &Default::default());
                assert_eq!(fields[selected].path, "level", "{row:?}");
            }
            SummaryRow::Colour => assert!(app.layers.color_rules.is_open()),
            _ => {}
        }
        // Back to the base screen for the next row.
        key(&mut app, &provider, KeyCode::Esc, KeyModifiers::NONE);
        assert!(
            app.layers.stack.is_empty(),
            "{row:?}: Escape closes the owner"
        );
    }
}

#[test]
fn the_button_its_mnemonic_and_the_mouse_open_the_selected_row_too() {
    let (provider, mut app) = demo();
    apply_everything(&mut app);

    // `o` is the button's mnemonic (§8.10): bare, because nothing here is a
    // text field, and with Alt.
    open_summary(&mut app, &provider);
    key(&mut app, &provider, KeyCode::Down, KeyModifiers::NONE);
    key(&mut app, &provider, KeyCode::Down, KeyModifiers::NONE);
    key(&mut app, &provider, KeyCode::Char('o'), KeyModifiers::ALT);
    assert_eq!(app.layers.stack, vec![LayerId::Time]);
    key(&mut app, &provider, KeyCode::Esc, KeyModifiers::NONE);
    open_summary(&mut app, &provider);
    key(&mut app, &provider, KeyCode::Down, KeyModifiers::NONE);
    key(&mut app, &provider, KeyCode::Char('o'), KeyModifiers::NONE);
    assert_eq!(app.layers.stack, vec![LayerId::View]);
    assert_eq!(app.layers.view.mode(), ViewDialogMode::Sources);
    key(&mut app, &provider, KeyCode::Esc, KeyModifiers::NONE);

    // Tab reaches the button; Enter on it runs the same verb (§8.9).
    open_summary(&mut app, &provider);
    key(&mut app, &provider, KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(app.layers.view_summary.control(), SummaryControl::Open);
    key(&mut app, &provider, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(app.layers.stack, vec![LayerId::View]);
    key(&mut app, &provider, KeyCode::Esc, KeyModifiers::NONE);

    // A click selects a row; a click on the button opens it. The rects are
    // the ones the last render recorded.
    open_summary(&mut app, &provider);
    let buffer = draw(&provider, &mut app, 100, 30);
    let rendered = screen(&buffer);
    let fold_y = rendered
        .lines()
        .position(|line| line.contains("Fold "))
        .unwrap() as u16;
    let point = (buffer.area.width / 2, fold_y);
    assert_eq!(
        app.layers.view_summary.hit(point),
        Some(SummaryHit::Row(7)),
        "{rendered}"
    );
    click(&mut app, &provider, point);
    assert_eq!(app.layers.view_summary.selected_row(), SummaryRow::Fold);
    let button_y = rendered
        .lines()
        .position(|line| line.contains("[ Open ]"))
        .unwrap() as u16;
    let button_x = rendered
        .lines()
        .nth(usize::from(button_y))
        .unwrap()
        .find("[ Open ]")
        .unwrap() as u16;
    assert_eq!(
        app.layers.view_summary.hit((button_x + 2, button_y)),
        Some(SummaryHit::Control(SummaryControl::Open))
    );
    click(&mut app, &provider, (button_x + 2, button_y));
    assert_eq!(app.layers.stack, vec![LayerId::Folding]);
}

#[test]
fn the_list_scrolls_at_54x16_and_the_selection_stays_visible() {
    let (provider, mut app) = demo();
    apply_everything(&mut app);
    open_summary(&mut app, &provider);
    let first = screen(&draw(&provider, &mut app, 54, 16));
    assert!(first.contains("View summary"), "{first}");
    assert!(first.contains("1 of 11"), "{first}");
    assert!(first.contains("› View"), "{first}");
    for _ in 0..10 {
        key(&mut app, &provider, KeyCode::Down, KeyModifiers::NONE);
    }
    let last = screen(&draw(&provider, &mut app, 54, 16));
    assert!(last.contains("11 of 11"), "{last}");
    assert!(last.contains("› Readiness"), "{last}");
    assert!(last.contains("[ Open ]"), "{last}");
    // Nothing prints a routine key (§8.10).
    for banned in ["Enter", "Esc", "↑/↓", "Tab"] {
        assert!(!last.contains(banned), "{banned}:\n{last}");
    }
}
