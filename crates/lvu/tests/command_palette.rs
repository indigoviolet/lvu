use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use lvu::app::Views;
use lvu::command_palette::{
    CommandId, MAX_QUERY_BYTES, Palette, PaletteContext, PaletteOutcome, REQUIRED_COMMANDS,
};
use lvu::component::Component;
use lvu::component::{CommandEntry, CommandSpec, LayerId, Open};
use lvu::components::bookmarks::BookmarksDialog;
use lvu::components::enrichment::EnrichmentDialog;
use lvu::components::enrichment_step::EnrichmentStepLayer;
use lvu::components::external_command::ExternalCommandDialog;
use lvu::components::fields::FieldsDialog;
use lvu::components::recipes::RecipesDialog;
use lvu::components::source::SourceDialog;
use lvu::components::storage::CLEANUP_COMMAND;
use lvu::components::time::TimeDialog;
use lvu::components::view::ViewDialog;
use lvu::{Action, Focus};
use ratatui::{Terminal, backend::TestBackend, layout::Rect};
use std::collections::BTreeSet;

fn buffer_rows(terminal: &Terminal<TestBackend>) -> Vec<String> {
    let buffer = terminal.backend().buffer();
    (0..buffer.area.height)
        .map(|y| {
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect()
        })
        .collect()
}

fn last_cell_column(buffer: &ratatui::buffer::Buffer, row: u16, needle: &str) -> Option<u16> {
    (0..buffer.area.width).rev().find(|x| {
        (*x..buffer.area.width)
            .map(|column| buffer[(column, row)].symbol())
            .collect::<String>()
            .starts_with(needle)
    })
}

fn press(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

#[test]
fn pasted_search_is_bounded_and_does_not_execute() {
    let mut palette = Palette::new();
    palette.open(context(Focus::Logs, true));
    palette.handle_paste(&"x".repeat(MAX_QUERY_BYTES * 2));
    assert!(
        palette.query().is_empty(),
        "oversized paste is rejected atomically"
    );
    palette.handle_paste("usable");
    assert_eq!(palette.query(), "usable");
    assert!(palette.is_open());
}

fn type_query(palette: &mut Palette, query: &str) {
    for character in query.chars() {
        assert_eq!(
            handle(palette, press(KeyCode::Char(character))),
            PaletteOutcome::None
        );
    }
}

fn handle(palette: &mut Palette, key: KeyEvent) -> PaletteOutcome {
    let context = palette.context().clone();
    palette.handle_key(key, context)
}

/// Storage's contributed entry (§4.3), as the shell collects it from the
/// component. The component's own availability rule is covered in `ui_state`.
fn storage_cleanup(confirmable: bool) -> Vec<(LayerId, CommandEntry)> {
    vec![(
        LayerId::Storage,
        CommandEntry {
            spec: CommandSpec {
                shortcut: confirmable.then_some("c"),
                ..CLEANUP_COMMAND
            },
            unavailable_reason: (!confirmable)
                .then_some("confirm cleanup in Storage preview first"),
        },
    )]
}

/// Time contributes eight entries, all unavailable until its layer is open.
fn time_commands() -> Vec<(LayerId, CommandEntry)> {
    TimeDialog::default()
        .commands(&Views::default())
        .into_iter()
        .map(|entry| (LayerId::Time, entry))
        .collect()
}

/// Fields' three entries, muted until its layer is open.
fn fields_commands(open: bool) -> Vec<(LayerId, CommandEntry)> {
    let mut fields = FieldsDialog::default();
    if open {
        fields.open_for_test();
    }
    fields
        .commands(&Views::default())
        .into_iter()
        .map(|entry| (LayerId::Fields, entry))
        .collect()
}

/// View contributes its four mode choices, all unavailable until it is open.
fn view_commands() -> Vec<(LayerId, CommandEntry)> {
    ViewDialog::default()
        .commands(&Views::default())
        .into_iter()
        .map(|entry| (LayerId::View, entry))
        .collect()
}

fn enrichment_commands(open: bool) -> Vec<(LayerId, CommandEntry)> {
    let mut enrichment = EnrichmentDialog::default();
    let mut step = EnrichmentStepLayer::default();
    let mut command = ExternalCommandDialog::default();
    if open {
        enrichment.open_for_test();
        step.open_for_test();
        command.open_for_test();
    }
    enrichment
        .commands(&Views::default())
        .into_iter()
        .map(|entry| (LayerId::Enrichment, entry))
        .chain(
            step.commands(&Views::default())
                .into_iter()
                .map(|entry| (LayerId::EnrichmentStep, entry)),
        )
        .chain(
            command
                .commands(&Views::default())
                .into_iter()
                .map(|entry| (LayerId::ExternalCommand, entry)),
        )
        .collect()
}

fn source_commands(open: bool) -> Vec<(LayerId, CommandEntry)> {
    let mut source = SourceDialog::default();
    if open {
        source.open_for_test();
    }
    source
        .commands(&Views::default())
        .into_iter()
        .map(|entry| (LayerId::Source, entry))
        .collect()
}

/// The same as `context`, with the Source layer on the stack.
fn source_context() -> PaletteContext {
    let mut context = PaletteContext::new(Focus::Layer, true);
    context.layer_commands = storage_cleanup(false);
    context.layer_commands.extend(time_commands());
    context.layer_commands.extend(recipe_commands(false));
    context.layer_commands.extend(source_commands(true));
    context
}

fn recipe_commands(open: bool) -> Vec<(LayerId, CommandEntry)> {
    let mut recipes = RecipesDialog::default();
    if open {
        // The layer's own entries lose their "open Recipes first" reason and
        // gain their shortcut column once it is on the stack (§4.3).
        recipes.open_for_test();
    }
    recipes
        .commands(&Views::default())
        .into_iter()
        .map(|entry| (LayerId::Recipes, entry))
        .collect()
}

/// Bookmarks' single entry, muted until its layer is open.
fn bookmark_commands() -> Vec<(LayerId, CommandEntry)> {
    BookmarksDialog::default()
        .commands(&Views::default())
        .into_iter()
        .map(|entry| (LayerId::Bookmarks, entry))
        .collect()
}

/// The palette always receives every layer's entries, exactly as `terminal.rs`
/// assembles them.
fn context(focus: Focus, has_view: bool) -> PaletteContext {
    let mut context = PaletteContext::new(focus, has_view);
    context.layer_commands = storage_cleanup(false);
    context.layer_commands.extend(time_commands());
    context.layer_commands.extend(fields_commands(false));
    context.layer_commands.extend(view_commands());
    context.layer_commands.extend(recipe_commands(false));
    context.layer_commands.extend(source_commands(false));
    context.layer_commands.extend(enrichment_commands(false));
    context.layer_commands.extend(bookmark_commands());
    context
}

/// The same, with the Recipes layer on the stack.
fn recipes_context() -> PaletteContext {
    let mut context = PaletteContext::new(Focus::Layer, true);
    context.layer_commands = storage_cleanup(false);
    context.layer_commands.extend(time_commands());
    context.layer_commands.extend(recipe_commands(true));
    context.layer_commands.extend(source_commands(false));
    context.layer_commands.extend(bookmark_commands());
    context
}

fn open_logs() -> Palette {
    let mut palette = Palette::new();
    palette.open(context(Focus::Logs, true));
    palette
}

#[test]
fn q_is_palette_query_text_and_escape_closes_only_the_palette() {
    let mut palette = open_logs();
    assert_eq!(
        handle(&mut palette, press(KeyCode::Char('q'))),
        PaletteOutcome::None
    );
    assert_eq!(palette.query(), "q");
    assert!(palette.is_open());
    assert!(matches!(
        handle(&mut palette, press(KeyCode::Esc)),
        PaletteOutcome::Closed { .. }
    ));
    assert!(!palette.is_open());
}

#[test]
fn catalog_covers_every_explicit_semantic_operation_once() {
    let palette = open_logs();
    let actual: BTreeSet<_> = palette
        .commands()
        .iter()
        .map(|command| command.id)
        .collect();
    let expected: BTreeSet<_> = REQUIRED_COMMANDS.iter().copied().collect();
    assert_eq!(actual, expected);
    assert_eq!(palette.commands().len(), REQUIRED_COMMANDS.len());
    assert_eq!(
        palette.results().count(),
        palette
            .commands()
            .iter()
            .filter(|command| command.is_enabled())
            .count()
    );
    assert!(palette.commands().iter().all(|command| {
        !command.name.is_empty() && !command.description.is_empty() && !command.category.is_empty()
    }));
}

#[test]
fn blank_query_exposes_only_actionable_commands() {
    let mut palette = Palette::new();
    palette.open(context(Focus::Logs, false));
    assert!(palette.results().all(|command| command.is_enabled()));
    assert!(
        palette
            .results()
            .any(|command| command.id == CommandId::AddSource)
    );
    assert!(
        !palette
            .results()
            .any(|command| command.id == CommandId::AdvancedFilter)
    );
}

#[test]
fn search_ranks_exact_prefix_alias_and_fuzzy_subsequence() {
    let mut exact = open_logs();
    type_query(&mut exact, "enrichment");
    assert_eq!(exact.selected_command().unwrap().id, CommandId::Enrichment);

    let mut prefix = open_logs();
    type_query(&mut prefix, "literal fil");
    assert_eq!(
        prefix.selected_command().unwrap().id,
        CommandId::LiteralFilter
    );

    let mut alias = open_logs();
    type_query(&mut alias, "grep");
    assert_eq!(
        alias.selected_command().unwrap().id,
        CommandId::LiteralFilter
    );

    let mut fuzzy = open_logs();
    type_query(&mut fuzzy, "adflt");
    assert_eq!(
        fuzzy.selected_command().unwrap().id,
        CommandId::AdvancedFilter
    );
}

#[test]
fn tab_completes_selected_name_and_enter_executes_enabled_action() {
    let mut palette = open_logs();
    type_query(&mut palette, "grep");
    assert_eq!(
        handle(&mut palette, press(KeyCode::Tab)),
        PaletteOutcome::None
    );
    assert_eq!(palette.query(), "Literal filter");
    assert_eq!(
        handle(&mut palette, press(KeyCode::Enter)),
        PaletteOutcome::Execute(Action::Open(Open::Search))
    );
    assert!(!palette.is_open());
}

#[test]
fn terminal_command_catalog_actions_are_enabled_in_their_actual_contexts() {
    let mut open = open_logs();
    type_query(&mut open, "terminal command step");
    assert_eq!(
        handle(&mut open, press(KeyCode::Enter)),
        PaletteOutcome::Execute(Action::Open(Open::ExternalCommand {
            stage: None,
            insert_at: usize::MAX
        }))
    );

    // The layer contributes and routes its own verbs now (§4.3).
    let mut save = Palette::new();
    let mut context = PaletteContext::new(Focus::Layer, true);
    context.layer_commands = enrichment_commands(true);
    save.open(context);
    type_query(&mut save, "save external command");
    assert_eq!(
        handle(&mut save, press(KeyCode::Enter)),
        PaletteOutcome::Execute(Action::Command(
            LayerId::ExternalCommand,
            lvu::command_palette::CommandId::CommandEnrichmentSave
        ))
    );
}

#[test]
fn correlation_is_discoverable_only_from_the_fields_context() {
    // Fields declares the entry itself now (§4.3), so it is listed everywhere
    // and enabled only while the layer is open.
    let mut palette = Palette::new();
    palette.open(context(Focus::Logs, true));
    type_query(&mut palette, "correlate across sources");
    let closed = palette.selected_command().unwrap();
    assert_eq!(closed.id, CommandId::CorrelateField);
    assert_eq!(closed.unavailable_reason, Some("open Fields first"));

    let mut open = PaletteContext::new(Focus::Layer, true);
    open.layer_commands = fields_commands(true);
    palette.refresh_context(open);
    let command = palette.selected_command().unwrap();
    assert!(command.is_enabled());
    assert_eq!(
        handle(&mut palette, press(KeyCode::Enter)),
        PaletteOutcome::Execute(Action::Command(LayerId::Fields, CommandId::CorrelateField))
    );
}

#[test]
fn disabled_commands_remain_visible_explain_why_and_do_not_execute() {
    let mut palette = Palette::new();
    palette.open(context(Focus::Logs, false));
    type_query(&mut palette, "advanced filter");
    let command = palette.selected_command().unwrap();
    assert_eq!(command.id, CommandId::AdvancedFilter);
    assert_eq!(command.unavailable_reason, Some("open a source first"));
    assert_eq!(
        handle(&mut palette, press(KeyCode::Enter)),
        PaletteOutcome::None
    );
    assert!(palette.is_open());

    let cleanup = palette
        .commands()
        .iter()
        .find(|command| command.id == CommandId::StorageClear)
        .unwrap();
    assert_eq!(
        cleanup.unavailable_reason,
        Some("confirm cleanup in Storage preview first")
    );
}

#[test]
fn long_and_short_names_cannot_shift_aligned_palette_columns() {
    let mut palette = Palette::new();
    palette.open(recipes_context());
    type_query(&mut palette, "recipe");
    let mut terminal = Terminal::new(TestBackend::new(100, 28)).unwrap();
    terminal
        .draw(|frame| palette.render(frame, frame.area()))
        .unwrap();
    let rows = buffer_rows(&terminal);
    let relevant_rows = [
        "Adapt suggested recipe with agen",
        "Reject selected recipe suggestio",
        "Recipes",
    ]
    .map(|name| {
        rows.iter()
            .position(|row| row.contains(name) && row.contains("Recipes"))
            .unwrap_or_else(|| panic!("missing {name:?}:\n{}", rows.join("\n"))) as u16
    });
    let buffer = terminal.backend().buffer();
    let category_columns = relevant_rows
        .iter()
        .map(|row| last_cell_column(buffer, *row, "Recipes").unwrap())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        category_columns.len(),
        1,
        "category columns shifted:\n{}",
        rows.join("\n")
    );
    let shortcut_columns = relevant_rows
        .iter()
        .filter_map(|row| last_cell_column(buffer, *row, "Alt-"))
        .collect::<BTreeSet<_>>();
    assert_eq!(
        shortcut_columns.len(),
        1,
        "shortcut columns shifted:\n{}",
        rows.join("\n")
    );

    let mut selected_long = Palette::new();
    selected_long.open(recipes_context());
    type_query(&mut selected_long, "Adapt suggested recipe with agent");
    terminal
        .draw(|frame| selected_long.render(frame, frame.area()))
        .unwrap();
    assert!(
        buffer_rows(&terminal)
            .join("\n")
            .contains("Adapt suggested recipe with agent"),
        "full clipped name is not available in details"
    );
}

#[test]
fn narrow_unavailable_details_prioritize_the_complete_reason() {
    let mut palette = Palette::new();
    palette.open(context(Focus::Logs, false));
    type_query(&mut palette, "Send investigation follow-up");
    let mut terminal = Terminal::new(TestBackend::new(40, 12)).unwrap();
    terminal
        .draw(|frame| palette.render(frame, frame.area()))
        .unwrap();
    let rendered = buffer_rows(&terminal).join("\n");
    assert!(
        // §7.4 retired the `Selected:` label; the name itself still leads
        // the detail row, so a clipped list name stays readable in full.
        rendered.contains("Send investigation follow-up"),
        "{rendered}"
    );
    assert!(rendered.contains("Unavailable:"), "{rendered}");
    assert!(
        rendered.contains("open an active investigation"),
        "{rendered}"
    );
    assert!(rendered.contains("enter a follow-up first"), "{rendered}");
    assert_eq!(
        handle(&mut palette, press(KeyCode::Enter)),
        PaletteOutcome::None
    );
    assert!(palette.is_open());
}

#[test]
fn enter_revalidates_current_context_instead_of_opening_snapshot() {
    let mut palette = Palette::new();
    // The layer declares its own entry and its own availability (§4.3); the
    // palette never inspects the component.
    let mut confirmed = context(Focus::Layer, true);
    confirmed.layer_commands = storage_cleanup(true);
    confirmed.layer_commands.extend(time_commands());
    palette.open(confirmed);
    type_query(&mut palette, "confirm derived-data cleanup");
    assert!(palette.selected_command().unwrap().is_enabled());

    let mut no_longer_confirmed = context(Focus::Layer, true);
    no_longer_confirmed.layer_commands = storage_cleanup(false);
    no_longer_confirmed.layer_commands.extend(time_commands());
    assert_eq!(
        palette.handle_key(press(KeyCode::Enter), no_longer_confirmed),
        PaletteOutcome::None
    );
    assert!(palette.is_open());
    assert_eq!(
        palette.selected_command().unwrap().unavailable_reason,
        Some("confirm cleanup in Storage preview first")
    );
}

#[test]
fn shortcuts_are_derived_for_the_current_focus_only() {
    let logs = open_logs();
    let filter = logs
        .commands()
        .iter()
        .find(|c| c.id == CommandId::LiteralFilter)
        .unwrap();
    assert_eq!(filter.shortcut, Some("/"));
    let discovery = logs
        .commands()
        .iter()
        .find(|c| c.id == CommandId::DiscoverSources)
        .unwrap();
    assert_eq!(discovery.shortcut, None);

    let mut source = Palette::new();
    source.open(source_context());
    let discovery = source
        .commands()
        .iter()
        .find(|c| c.id == CommandId::DiscoverSources)
        .unwrap();
    assert_eq!(discovery.shortcut, Some("Ctrl-D"));
}

#[test]
fn escape_and_toggle_restore_exact_underlying_editor_focus() {
    let mut palette = Palette::new();
    palette.open(context(Focus::Layer, true));
    type_query(&mut palette, "draft stays outside palette");
    assert_eq!(
        handle(&mut palette, press(KeyCode::Esc)),
        PaletteOutcome::Closed {
            restore_focus: Focus::Layer
        }
    );

    palette.open(context(Focus::Details, true));
    assert_eq!(
        handle(
            &mut palette,
            KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL)
        ),
        PaletteOutcome::Closed {
            restore_focus: Focus::Details
        }
    );
}

#[test]
fn input_is_utf8_safe_bounded_and_key_release_is_ignored() {
    let mut palette = open_logs();
    for _ in 0..MAX_QUERY_BYTES {
        handle(&mut palette, press(KeyCode::Char('x')));
    }
    handle(&mut palette, press(KeyCode::Char('é')));
    assert_eq!(palette.query().len(), MAX_QUERY_BYTES);
    assert!(palette.query().is_char_boundary(palette.query().len()));

    let mut released = press(KeyCode::Backspace);
    released.kind = KeyEventKind::Release;
    assert_eq!(handle(&mut palette, released), PaletteOutcome::None);
    assert_eq!(palette.query().len(), MAX_QUERY_BYTES);
}

#[test]
fn narrow_wide_query_scrolls_with_the_logical_cursor_without_orphan_marks() {
    let mut palette = open_logs();
    let query = format!("{}e\u{301}Z", "東京".repeat(40));
    assert!(query.len() <= MAX_QUERY_BYTES);
    palette.handle_paste(&query);

    let mut terminal = Terminal::new(TestBackend::new(24, 10)).unwrap();
    terminal
        .draw(|frame| palette.render(frame, frame.area()))
        .unwrap();
    let first_cursor = terminal.backend().cursor_position();
    let first_row = (3..23)
        .map(|x| terminal.backend().buffer()[(x, 1)].symbol())
        .collect::<String>();
    assert!(
        first_row.contains("e\u{301}Z"),
        "visible tail was {first_row:?}"
    );
    assert_ne!(
        terminal.backend().buffer()[(3, 1)].symbol(),
        "\u{301}",
        "the display window began with an orphan combining mark"
    );

    assert_eq!(
        handle(&mut palette, press(KeyCode::Left)),
        PaletteOutcome::None
    );
    terminal
        .draw(|frame| palette.render(frame, frame.area()))
        .unwrap();
    let left_cursor = terminal.backend().cursor_position();
    assert_ne!(
        left_cursor, first_cursor,
        "Left did not move the rendered caret"
    );

    assert_eq!(
        handle(&mut palette, press(KeyCode::Char('X'))),
        PaletteOutcome::None
    );
    assert!(palette.query().ends_with("e\u{301}XZ"));
    terminal
        .draw(|frame| palette.render(frame, frame.area()))
        .unwrap();
    let inserted_row = (3..23)
        .map(|x| terminal.backend().buffer()[(x, 1)].symbol())
        .collect::<String>();
    assert!(
        inserted_row.contains("e\u{301}X"),
        "visible tail was {inserted_row:?}"
    );
    assert_ne!(terminal.backend().buffer()[(3, 1)].symbol(), "\u{301}");
}

#[test]
fn resize_scroll_mouse_and_tiny_terminal_are_bounded() {
    let mut palette = open_logs();
    palette.resize(Rect::new(0, 0, 30, 6));
    handle(&mut palette, press(KeyCode::Down));
    let after_down = palette.selected_command().unwrap().id;
    assert_ne!(after_down, CommandId::AddSource);

    let backend = TestBackend::new(40, 10);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| palette.render(frame, frame.area()))
        .unwrap();
    let before_mouse = palette.selected_command().unwrap().id;
    palette.handle_mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 2,
        row: 2,
        modifiers: KeyModifiers::NONE,
    });
    let after_click = palette.selected_command().unwrap().id;
    assert_ne!(after_click, before_mouse);
    palette.handle_mouse(MouseEvent {
        kind: MouseEventKind::ScrollDown,
        column: 2,
        row: 3,
        modifiers: KeyModifiers::NONE,
    });
    assert_ne!(palette.selected_command().unwrap().id, after_click);

    let tiny = TestBackend::new(2, 2);
    let mut tiny_terminal = Terminal::new(tiny).unwrap();
    tiny_terminal
        .draw(|frame| palette.render(frame, frame.area()))
        .unwrap();
    assert!(palette.is_open());
}

#[test]
fn prohibited_navigation_keys_and_modifiers_do_not_move_palette_selection() {
    let mut palette = open_logs();
    handle(&mut palette, press(KeyCode::Down));
    let selected = palette.selected_command().unwrap().id;
    for code in [
        KeyCode::PageUp,
        KeyCode::PageDown,
        KeyCode::Home,
        KeyCode::End,
    ] {
        for modifiers in [
            KeyModifiers::NONE,
            KeyModifiers::SHIFT,
            KeyModifiers::ALT,
            KeyModifiers::CONTROL,
            KeyModifiers::ALT | KeyModifiers::SHIFT,
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        ] {
            assert_eq!(
                handle(&mut palette, KeyEvent::new(code, modifiers)),
                PaletteOutcome::None
            );
            assert_eq!(
                palette.selected_command().unwrap().id,
                selected,
                "{code:?} {modifiers:?} moved palette selection"
            );
        }
    }
}

#[test]
fn dialog_actions_cannot_bypass_existing_admission_and_confirmation() {
    let palette = open_logs();
    for id in [
        CommandId::DiscoverSources,
        CommandId::AskAiSource,
        CommandId::ViewBlank,
        CommandId::ViewClone,
        CommandId::ViewRename,
        CommandId::RecipeSave,
        CommandId::PinField,
        CommandId::StorageClear,
        CommandId::NewInvestigation,
    ] {
        let command = palette
            .commands()
            .iter()
            .find(|command| command.id == id)
            .unwrap();
        assert!(!command.is_enabled(), "{id:?} bypassed its existing dialog");
    }
}

/// §8.10: a query lists what can run first and what cannot after it, under
/// one heading, each with the reason its control gives; Enter on such a row
/// does nothing and the reason stays on screen.
#[test]
fn unavailable_matches_form_their_own_group_after_the_available_ones() {
    let mut palette = Palette::new();
    palette.open(context(Focus::Logs, false));
    assert!(
        palette.unavailable_results().next().is_none(),
        "blank query lists only what runs"
    );
    // "source" matches Add source (available without a view) and the view
    // and capture operations that need one.
    type_query(&mut palette, "source");
    let results: Vec<_> = palette.results().cloned().collect();
    let first_unavailable = results
        .iter()
        .position(|command| !command.is_enabled())
        .expect("something unavailable matched");
    assert!(
        first_unavailable > 0,
        "an available match comes first: {results:?}"
    );
    assert!(
        results[..first_unavailable]
            .iter()
            .all(|command| command.is_enabled())
            && results[first_unavailable..]
                .iter()
                .all(|command| !command.is_enabled()),
        "available then unavailable, never mixed: {results:?}"
    );
    assert_eq!(
        palette.unavailable_results().count(),
        results.len() - first_unavailable
    );
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal
        .draw(|frame| palette.render(frame, frame.area()))
        .unwrap();
    let rows = buffer_rows(&terminal);
    let heading = rows
        .iter()
        .position(|row| row.contains(lvu::command_palette::UNAVAILABLE_HEADING))
        .expect("the group has a heading");
    let first_available = rows
        .iter()
        .position(|row| row.contains(results[0].name))
        .unwrap();
    assert!(
        first_available < heading,
        "the heading comes after the available rows"
    );
    let unavailable = results[first_unavailable].clone();
    let row = rows[heading + 1..]
        .iter()
        .find(|row| row.contains(unavailable.name))
        .unwrap_or_else(|| {
            panic!(
                "{} under the heading:\n{}",
                unavailable.name,
                rows.join("\n")
            )
        });
    // §9: a long name leaves the reason clipped at the popup edge; the
    // detail row carries it whole once the row is selected.
    let reason = unavailable.unavailable_reason.unwrap();
    assert!(
        row.contains(&reason[..reason.len().min(12)]),
        "the row carries its reason: {row}"
    );
    assert!(
        !row.contains(unavailable.category),
        "no category on a row that cannot run: {row}"
    );

    // Down walks into the group; Enter there is inert and the reason stays.
    for _ in 0..first_unavailable {
        handle(&mut palette, press(KeyCode::Down));
    }
    assert!(!palette.selected_command().unwrap().is_enabled());
    assert_eq!(
        handle(&mut palette, press(KeyCode::Enter)),
        PaletteOutcome::None
    );
    assert!(palette.is_open());
    terminal
        .draw(|frame| palette.render(frame, frame.area()))
        .unwrap();
    let rendered = buffer_rows(&terminal).join("\n");
    assert!(rendered.contains("Unavailable:"), "{rendered}");
    assert!(
        rendered.matches(&reason[..reason.len().min(12)]).count() >= 2,
        "the reason is on the row and in the detail: {rendered}"
    );
}

/// The reason on a palette row is the reason the owning control gives — one
/// predicate, not a copy the palette keeps for itself.
#[test]
fn a_layers_reason_is_the_palettes_reason() {
    let mut palette = Palette::new();
    let mut ctx = context(Focus::Layer, true);
    ctx.layer_commands = storage_cleanup(false);
    palette.open(ctx);
    type_query(&mut palette, "confirm derived-data cleanup");
    let command = palette
        .unavailable_results()
        .next()
        .expect("the guarded row");
    assert_eq!(
        command.unavailable_reason,
        storage_cleanup(false)[0].1.unavailable_reason,
        "the palette shows what the layer declared"
    );
}
