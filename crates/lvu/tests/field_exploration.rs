//! Acceptance for docs/dialog-system.md §8.11–§8.13: a JSON record is a tree
//! in Details and Fields with expansion remembered per view and per path,
//! scalars are the record's own bytes, the Value pane describes a path over
//! a bounded sample, the one-key actions act on the selected value, and the
//! editors' picker offers nested paths.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use lvu::{
    Action, App, QueryCompletion, QueryPurpose, RowProvider,
    app::{FieldPickerControl, Focus},
    command_palette::CommandId,
    component::{Component, Open, RawEvent},
    components::fields::{FieldShape, field_rows, value_predicate},
    field_stats::{ValueType, field_stats},
    fixture::FixtureProvider,
    theme::Theme,
    ui,
};
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};

fn nested() -> (FixtureProvider, App) {
    let (provider, sources, views) = FixtureProvider::nested_demo();
    let mut app = App::new(sources, views, true);
    app.sync_provider(&provider, 10);
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
    let key = KeyEvent::new(code, modifiers);
    let action = if app.focus == Focus::Layer {
        Action::Raw(RawEvent::Key(key))
    } else {
        app.key_to_action(key)
    };
    app.handle(action, provider);
}

fn plain(app: &mut App, provider: &FixtureProvider, code: KeyCode) {
    key(app, provider, code, KeyModifiers::NONE);
}

// ---------------------------------------------------------------------------
// Details
// ---------------------------------------------------------------------------

#[test]
fn details_shows_a_json_record_as_a_tree_with_containers_collapsed() {
    let (provider, mut app) = nested();
    app.handle(Action::Top, &provider);
    app.handle(Action::ToggleDetails, &provider);
    let text = screen(&draw(&provider, &mut app, 120, 40));
    assert!(text.contains("▸ http: {3 keys}"), "{text}");
    // The record's own bytes, quotes and all; nothing re-serialised.
    assert!(text.contains("level: \"INFO\""), "{text}");
    assert!(
        !text.contains("status: 200"),
        "collapsed container leaked children:\n{text}"
    );
}

#[test]
fn details_expands_in_place_and_the_memory_is_per_view_and_per_path() {
    let (provider, mut app) = nested();
    app.handle(Action::Top, &provider);
    app.handle(Action::ToggleDetails, &provider);
    app.focus = Focus::Details;
    // Cursor to `http` (third top-level row), Enter opens it.
    plain(&mut app, &provider, KeyCode::Down);
    plain(&mut app, &provider, KeyCode::Down);
    plain(&mut app, &provider, KeyCode::Enter);
    let text = screen(&draw(&provider, &mut app, 120, 40));
    assert!(text.contains("▾ http: {3 keys}"), "{text}");
    assert!(text.contains("status: 200"), "{text}");
    assert!(text.contains("▸ tags: [2]"), "{text}");
    assert!(app.view_state().unwrap().expanded_paths.contains("http"));

    // Right on `tags` opens it; Left on a leaf climbs; Left again closes.
    plain(&mut app, &provider, KeyCode::Down);
    plain(&mut app, &provider, KeyCode::Down);
    plain(&mut app, &provider, KeyCode::Down);
    plain(&mut app, &provider, KeyCode::Right);
    let text = screen(&draw(&provider, &mut app, 120, 40));
    assert!(text.contains("[0]: \"api\""), "{text}");
    plain(&mut app, &provider, KeyCode::Down);
    plain(&mut app, &provider, KeyCode::Left);
    assert_eq!(
        app.view_state().unwrap().details_cursor,
        5,
        "climbed to tags"
    );
    plain(&mut app, &provider, KeyCode::Left);
    assert!(
        !app.view_state()
            .unwrap()
            .expanded_paths
            .contains("http.tags")
    );

    // Another record of the same view shows `http` open: memory is by path.
    app.handle(Action::MoveLine(3), &provider);
    let text = screen(&draw(&provider, &mut app, 120, 40));
    assert!(
        text.contains("▾ http: {3 keys}") && text.contains("/v1/items/4"),
        "{text}"
    );

    // The Fields dialog reads the same memory.
    app.focus = Focus::Logs;
    app.handle(Action::Open(Open::Fields), &provider);
    let text = screen(&draw(&provider, &mut app, 120, 40));
    assert!(
        text.contains("▾") && text.contains("status") && text.contains("[2]"),
        "{text}"
    );
}

#[test]
fn a_record_that_is_not_json_keeps_flat_rows_and_the_arrows_scroll() {
    let (provider, mut app) = nested();
    app.handle(Action::End, &provider);
    app.handle(Action::ToggleDetails, &provider);
    app.focus = Focus::Details;
    let text = screen(&draw(&provider, &mut app, 120, 40));
    assert!(text.contains("level: INFO"), "{text}");
    assert!(
        text.contains('\u{fffd}'),
        "the lossy byte is shown as the replacement character:\n{text}"
    );
    assert!(app.details_rows(&provider).is_empty());
    plain(&mut app, &provider, KeyCode::Enter);
    assert!(app.view_state().unwrap().expanded_paths.is_empty());
}

// ---------------------------------------------------------------------------
// Fields
// ---------------------------------------------------------------------------

#[test]
fn fields_lists_the_tree_and_enter_on_a_container_opens_it_while_pin_stays_the_default() {
    let (provider, mut app) = nested();
    app.handle(Action::Top, &provider);
    app.handle(Action::Open(Open::Fields), &provider);
    let row = app.selected_row(&provider).unwrap();
    let rows = field_rows(&row, &Default::default());
    let labels: Vec<&str> = rows.iter().map(|row| row.label.as_str()).collect();
    assert_eq!(labels, ["level", "message", "http", "service"]);
    assert!(
        matches!(rows[2].shape, FieldShape::Container { expanded: false, ref summary } if summary == "{3 keys}")
    );

    // Enter on a scalar row is the default, Pin.
    plain(&mut app, &provider, KeyCode::Enter);
    assert_eq!(
        app.view_state().unwrap().pinned_columns,
        vec!["level".to_owned()]
    );
    // Enter on the container row opens it instead of pinning.
    plain(&mut app, &provider, KeyCode::Down);
    plain(&mut app, &provider, KeyCode::Down);
    plain(&mut app, &provider, KeyCode::Enter);
    assert_eq!(app.view_state().unwrap().pinned_columns.len(), 1);
    assert!(app.view_state().unwrap().expanded_paths.contains("http"));
    let text = screen(&draw(&provider, &mut app, 120, 40));
    assert!(text.contains("status") && text.contains("[2]"), "{text}");
    // A nested scalar pins through its top-level column.
    plain(&mut app, &provider, KeyCode::Down);
    plain(&mut app, &provider, KeyCode::Char(' '));
    assert_eq!(
        app.view_state().unwrap().pinned_columns,
        vec!["level".to_owned(), "http".to_owned()]
    );
}

#[test]
fn the_value_pane_describes_the_selected_path_over_the_bounded_sample() {
    let (provider, mut app) = nested();
    app.handle(Action::Top, &provider);
    app.handle(Action::Open(Open::Fields), &provider);
    let stats = field_stats(&provider, "all", "http.status");
    assert_eq!(stats.sampled, 41);
    assert_eq!(
        stats.present, 40,
        "the plain-text record has no http.status"
    );
    let guess = stats.guess.as_ref().unwrap();
    assert_eq!(guess.kind, ValueType::Integer);
    assert_eq!(guess.confidence_percent(stats.present), 100);
    assert_eq!(guess.sample, "200");
    assert_eq!(guess.sample_row.sequence, 1);
    assert_eq!(stats.distinct, 2);
    assert_eq!(
        stats.top,
        vec![("200".to_owned(), 36), ("503".to_owned(), 4)]
    );
    assert_eq!(stats.range, Some(("200".to_owned(), "503".to_owned())));

    let level = field_stats(&provider, "all", "level");
    assert_eq!(
        level.present, 41,
        "the logfmt record contributes its recognised field"
    );
    assert_eq!(level.guess.as_ref().unwrap().kind, ValueType::String);
    assert_eq!(level.range, None);

    // On screen, for the selected path, at both acceptance sizes.
    for (width, height) in [(80, 24), (54, 16)] {
        let text = screen(&draw(&provider, &mut app, width, height));
        assert!(text.contains("Value · level"), "{width}x{height}\n{text}");
        assert!(
            text.contains("Type") && text.contains("string"),
            "{width}x{height}\n{text}"
        );
        assert!(
            text.contains("2,048"),
            "{width}x{height}: the bound is named\n{text}"
        );
    }
}

#[test]
fn filter_exclude_and_fold_act_on_the_selected_value() {
    let (provider, mut app) = nested();
    app.handle(Action::Top, &provider);
    app.handle(Action::Open(Open::Fields), &provider);
    // Open http, select status.
    plain(&mut app, &provider, KeyCode::Down);
    plain(&mut app, &provider, KeyCode::Down);
    plain(&mut app, &provider, KeyCode::Right);
    plain(&mut app, &provider, KeyCode::Down);
    key(&mut app, &provider, KeyCode::Char('x'), KeyModifiers::ALT);
    let request = app
        .take_query_requests()
        .pop()
        .expect("an Advanced filter was submitted");
    assert_eq!(
        request.constraints.advanced_polars.as_deref(),
        Some(
            r#"pl.col('http').str.json_path_match('$.status').cast(pl.Float64, strict=False) != 200"#
        )
    );
    assert_eq!(
        app.view_state().unwrap().advanced.draft,
        r#"pl.col('http').str.json_path_match('$.status').cast(pl.Float64, strict=False) != 200"#
    );

    // Once that is applied, a top-level string filters typed and is joined
    // to it, so the user sees exactly what is in force.
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: request.view_id,
        generation: request.generation,
        revision: request.revision,
        purpose: QueryPurpose::Advanced,
        result: Ok(()),
    }));
    plain(&mut app, &provider, KeyCode::Up);
    plain(&mut app, &provider, KeyCode::Up);
    plain(&mut app, &provider, KeyCode::Up);
    key(&mut app, &provider, KeyCode::Char('f'), KeyModifiers::ALT);
    let request = app.take_query_requests().pop().expect("a second filter");
    assert_eq!(
        request.constraints.advanced_polars.as_deref(),
        Some(
            r#"(pl.col('http').str.json_path_match('$.status').cast(pl.Float64, strict=False) != 200) & (pl.col('level') == 'INFO')"#
        )
    );

    // Fold by field sets the fold key and turns folding on, like the dialog.
    key(&mut app, &provider, KeyCode::Char('d'), KeyModifiers::ALT);
    let state = app.view_state().unwrap();
    assert!(state.fold_enabled);
    assert_eq!(state.fold_key_column.as_deref(), Some("level"));
    assert_eq!(app.action_notice.as_deref(), Some("folding on level"));

    // §8.9: the action follows the state it acts on. Folding from here and
    // then having to find the Folding dialog to undo it is the trap that rule
    // exists to close, so the same key on the same column stops folding.
    key(&mut app, &provider, KeyCode::Char('d'), KeyModifiers::ALT);
    let state = app.view_state().unwrap();
    assert!(!state.fold_enabled);
    assert_eq!(
        app.action_notice.as_deref(),
        Some("folding off; was on level")
    );

    // And turning it on again from the same place works.
    key(&mut app, &provider, KeyCode::Char('d'), KeyModifiers::ALT);
    assert!(app.view_state().unwrap().fold_enabled);
}

/// Switching the fold from one column to another says which key it left, since
/// every run on screen changes and the status count moving is otherwise the
/// only sign of it.
#[test]
fn folding_from_fields_by_a_second_column_names_the_key_it_replaces() {
    let (provider, mut app) = nested();
    app.handle(Action::Top, &provider);
    app.handle(Action::Open(Open::Fields), &provider);
    // The first top-level column the picker offers.
    key(&mut app, &provider, KeyCode::Char('d'), KeyModifiers::ALT);
    let first = app
        .view_state()
        .unwrap()
        .fold_key_column
        .clone()
        .expect("a fold key");
    assert_eq!(
        app.action_notice.as_deref(),
        Some(&*format!("folding on {first}"))
    );

    // Move to a different top-level column and fold by that instead.
    plain(&mut app, &provider, KeyCode::Down);
    key(&mut app, &provider, KeyCode::Char('d'), KeyModifiers::ALT);
    let state = app.view_state().unwrap();
    let second = state.fold_key_column.clone().expect("a fold key");
    assert!(state.fold_enabled);
    assert_ne!(second, first, "the second column must differ");
    assert_eq!(
        app.action_notice.as_deref(),
        Some(&*format!("folding by {first} → {second}"))
    );
}

#[test]
fn predicates_are_typed_at_the_top_level_and_by_json_path_below_it() {
    use lvu::json_spans::JsonKind;
    // Nested numbers compare as numbers, so 503 is never 5033; nested
    // strings compare whole, so 'slow' is never 'slower'.
    assert_eq!(
        value_predicate("http.status", "503", Some(&JsonKind::Number), false).as_deref(),
        Some(
            r#"pl.col('http').str.json_path_match('$.status').cast(pl.Float64, strict=False) == 503"#
        )
    );
    assert_eq!(
        value_predicate("http.path", r#""/v1""#, Some(&JsonKind::String), true).as_deref(),
        Some(r#"pl.col('http').str.json_path_match('$.path') != '/v1'"#)
    );
    assert_eq!(
        value_predicate("meta.ok", "true", Some(&JsonKind::Boolean), false).as_deref(),
        Some(r#"pl.col('meta').str.json_path_match('$.ok') == 'true'"#)
    );
    // A key the path syntax cannot spell keeps the lexical pair match.
    assert_eq!(
        value_predicate("meta.it's", "1", Some(&JsonKind::Number), false).as_deref(),
        Some(r#"pl.col('meta').str.contains('"it\'s"\\s*:\\s*1')"#)
    );
    assert_eq!(
        value_predicate("status", "200", Some(&JsonKind::Number), false).as_deref(),
        Some(r#"pl.col('status') == 200"#)
    );
    assert_eq!(
        value_predicate("ok", "true", Some(&JsonKind::Boolean), true).as_deref(),
        Some(r#"pl.col('ok') != True"#)
    );
    assert_eq!(
        value_predicate("none", "null", Some(&JsonKind::Null), false).as_deref(),
        Some(r#"pl.col('none').is_null()"#)
    );
    assert_eq!(
        value_predicate("msg", r#""a \"q\" b""#, Some(&JsonKind::String), false).as_deref(),
        Some(r#"pl.col('msg') == 'a "q" b'"#)
    );
    assert_eq!(
        value_predicate("level", "INFO", None, false).as_deref(),
        Some(r#"pl.col('level') == 'INFO'"#)
    );
    assert_eq!(
        value_predicate("http.tags[1]", r#""slow""#, Some(&JsonKind::String), false).as_deref(),
        Some(r#"pl.col('http').str.json_path_match('$.tags[1]') == 'slow'"#)
    );
}

#[test]
fn every_fields_action_is_a_button_with_a_mnemonic_and_a_palette_row() {
    let (provider, mut app) = nested();
    app.handle(Action::Top, &provider);
    app.handle(Action::Open(Open::Fields), &provider);
    let text = screen(&draw(&provider, &mut app, 100, 30));
    for label in [
        "[ Pin ]",
        "[ Filter ]",
        "[ Exclude ]",
        "[ Color ]",
        "[ Fold ]",
        "[ Correlate ]",
    ] {
        assert!(text.contains(label), "{label}\n{text}");
    }
    let entries = app.layers.fields.commands(&app.views);
    let shortcuts: Vec<Option<&str>> = entries.iter().map(|entry| entry.spec.shortcut).collect();
    assert_eq!(
        shortcuts,
        // §8.10: the palette prints the letter the button underlines. Fields
        // takes no text, so the bare letter is always live in it.
        vec![
            Some("Space"),
            Some("f"),
            Some("x"),
            Some("c"),
            Some("d"),
            Some("r")
        ]
    );
    assert!(
        entries
            .iter()
            .any(|entry| entry.spec.id == CommandId::FoldByField)
    );
    // The row is the default: filled, first.
    let controls = app.layers.fields.control_rects();
    assert_eq!(
        controls.first().map(|(_, control)| *control),
        Some(FieldPickerControl::Pin)
    );
}

// ---------------------------------------------------------------------------
// The picker
// ---------------------------------------------------------------------------

#[test]
fn the_editors_completion_offers_nested_paths_by_json_path() {
    let (provider, mut app) = nested();
    app.handle(Action::Open(Open::Advanced), &provider);
    plain(&mut app, &provider, KeyCode::Tab);
    let completion = app
        .layers
        .filter
        .completion()
        .expect("the field picker is open");
    let nested: Vec<&str> = completion
        .items
        .iter()
        .filter(|item| item.label.contains("nested"))
        .map(|item| item.label.as_str())
        .collect();
    assert!(
        nested.iter().any(|label| label.contains("http.status")),
        "{nested:?}"
    );
    assert!(
        nested.iter().any(|label| label.contains("http.tags[0]")),
        "{nested:?}"
    );
    let status = completion
        .items
        .iter()
        .find(|item| item.label.contains("http.status"))
        .unwrap();
    assert_eq!(
        status.insertion,
        r#"pl.col('http').str.json_path_match('$.status')"#
    );
    // Accepting inserts it into the draft: nothing was typed by hand.
    let index = completion
        .items
        .iter()
        .position(|item| item.label.contains("http.status"))
        .unwrap();
    for _ in 0..index {
        plain(&mut app, &provider, KeyCode::Down);
    }
    plain(&mut app, &provider, KeyCode::Enter);
    assert_eq!(
        app.view_state().unwrap().advanced.draft,
        r#"pl.col('http').str.json_path_match('$.status')"#
    );
}
