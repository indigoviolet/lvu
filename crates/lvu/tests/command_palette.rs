use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use lvu::command_palette::{
    CommandId, MAX_QUERY_BYTES, Palette, PaletteContext, PaletteOutcome, REQUIRED_COMMANDS,
};
use lvu::{Action, Focus};
use ratatui::{Terminal, backend::TestBackend, layout::Rect};
use std::collections::BTreeSet;

fn press(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

#[test]
fn pasted_search_is_bounded_and_does_not_execute() {
    let mut palette = Palette::new();
    palette.open(PaletteContext::new(Focus::Logs, true));
    palette.handle_paste(&"x".repeat(MAX_QUERY_BYTES * 2));
    assert_eq!(palette.query().len(), MAX_QUERY_BYTES);
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
    let context = palette.context();
    palette.handle_key(key, context)
}

fn open_logs() -> Palette {
    let mut palette = Palette::new();
    palette.open(PaletteContext::new(Focus::Logs, true));
    palette
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
    assert_eq!(palette.results().count(), REQUIRED_COMMANDS.len());
    assert!(palette.commands().iter().all(|command| {
        !command.name.is_empty() && !command.description.is_empty() && !command.category.is_empty()
    }));
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
        PaletteOutcome::Execute(Action::OpenSearch)
    );
    assert!(!palette.is_open());
}

#[test]
fn terminal_command_catalog_actions_are_enabled_in_their_actual_contexts() {
    let mut open = open_logs();
    type_query(&mut open, "terminal command step");
    assert_eq!(
        handle(&mut open, press(KeyCode::Enter)),
        PaletteOutcome::Execute(Action::OpenCommandEnrichment)
    );

    let mut save = Palette::new();
    save.open(PaletteContext::new(Focus::CommandEnrichment, true));
    type_query(&mut save, "save command enrichment");
    assert_eq!(
        handle(&mut save, press(KeyCode::Enter)),
        PaletteOutcome::Execute(Action::SaveCommandEnrichment)
    );
}

#[test]
fn disabled_commands_remain_visible_explain_why_and_do_not_execute() {
    let mut palette = Palette::new();
    palette.open(PaletteContext::new(Focus::Logs, false));
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
fn enter_revalidates_current_context_instead_of_opening_snapshot() {
    let mut palette = Palette::new();
    let mut confirmed = PaletteContext::new(Focus::Storage, true);
    confirmed.storage_confirmation_ready = true;
    palette.open(confirmed);
    type_query(&mut palette, "confirm derived-data cleanup");
    assert!(palette.selected_command().unwrap().is_enabled());

    let no_longer_confirmed = PaletteContext::new(Focus::Storage, true);
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
    source.open(PaletteContext::new(Focus::SourceDialog, true));
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
    palette.open(PaletteContext::new(Focus::AdvancedEditor, true));
    type_query(&mut palette, "draft stays outside palette");
    assert_eq!(
        handle(&mut palette, press(KeyCode::Esc)),
        PaletteOutcome::Closed {
            restore_focus: Focus::AdvancedEditor
        }
    );

    palette.open(PaletteContext::new(Focus::SearchEditor, true));
    assert_eq!(
        handle(
            &mut palette,
            KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL)
        ),
        PaletteOutcome::Closed {
            restore_focus: Focus::SearchEditor
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
fn resize_scroll_mouse_and_tiny_terminal_are_bounded() {
    let mut palette = open_logs();
    palette.resize(Rect::new(0, 0, 30, 6));
    handle(&mut palette, press(KeyCode::PageDown));
    let after_page = palette.selected_command().unwrap().id;
    assert_ne!(after_page, CommandId::AddSource);

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
