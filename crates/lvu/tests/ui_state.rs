use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    time::{Duration, Instant},
};

use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};
use lvu::components::ask::AskOpen;
use lvu::theme::{Theme, ThemeId};
use lvu::{
    Action, App, AskAiKind, AskAiRequest, AskAiStage, DisplayRow, Focus, InvestigationItem,
    InvestigationRequest, InvestigationStage, PersistentViewState, QueryCompletion,
    QueryConstraints, QueryFailure, QueryPurpose, QueryRequest, RowId, RowPage, RowProvider,
    SettingsContext, SettingsValues, SourceKind, StorageCategory, StorageEntry, StorageSnapshot,
    ViewportRequest,
    app::{
        CommandEnrichmentControl, CommandEnrichmentField, CommandEnrichmentRequest,
        CommandEnrichmentReview, EnrichmentControl, EnrichmentStepControl, MAX_EDITOR_BYTES,
        RecipeDialogControl, RecipeDialogMode, SEARCH_DEBOUNCE, SourceItem, ViewItem,
        key_to_action,
    },
    component::{Component, LayerId, Open, RawEvent},
    components::folding::FoldingControl,
    components::settings::{SettingsControl, SettingsField, SettingsStatus},
    components::source::{SourceControl, SourceDialogMode},
    components::storage::StorageHit,
    components::time::TimeControl,
    fixture::FixtureProvider,
    terminal::{QueryDispatcher, poll_query_completions, submit_query_requests},
    ui,
};
use pretty_assertions::assert_eq;
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer, layout::Position};

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

#[test]
fn storage_dialog_is_fenced_bounded_and_requires_confirmation() {
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::Storage), &provider);
    let request = app.layers.storage.outbox.take().pop().unwrap();
    assert_eq!(app.focus, Focus::Layer);
    let snapshot = StorageSnapshot {
        entries: vec![StorageEntry {
            category: StorageCategory::Derived,
            label: "unused.rows.idx".into(),
            bytes: 12,
            reclaimable: 12,
            status: "unused, recomputable".into(),
        }],
        total_bytes: 12,
        reclaimable_bytes: 12,
        row_cache_bytes: 1,
        row_cache_limit: 10,
        query_index_bytes: 2,
        query_index_limit: 20,
        derived_index_limit_per_source: 30,
        derived_index_limit_total: 300,
        truncated: false,
        errors: vec![format!("scan warning {}", "bounded detail ".repeat(30))],
    };
    assert!(!app.layers.storage.complete(
        request.generation + 1,
        snapshot.clone(),
        "stale".into(),
        true
    ));
    assert!(
        app.layers
            .storage
            .complete(request.generation, snapshot, "complete".into(), true)
    );
    let screen = render(&provider, &mut app, 100, 25);
    assert!(screen.contains("unused.rows.idx"));
    assert!(screen.contains("not a process RSS limit"));
    // §3 replaces the key-reminder footer with a real action row.
    assert!(screen.contains("[ Refresh ]"), "{screen}");
    assert!(screen.contains("[ Preview cleanup ]"), "{screen}");
    assert!(!screen.contains("↑/↓ active pane"), "{screen}");
    let limit = app.layers.storage.scroll_limit();
    assert!(limit > 0);
    // component-model.md §5.1: the diagnostics pane is the component's own
    // geometry, resolved through `hit()`.
    let popup = app
        .hit_regions
        .selection_modal
        .expect("an open layer publishes its surface");
    let status = (0..popup.height)
        .map(|offset| (popup.x + 1, popup.y + offset))
        .find(|point| app.layers.storage.hit(*point) == Some(StorageHit::Diagnostics))
        .expect("storage status hitbox");
    let selected = app.layers.storage.selected();
    app.handle(
        Action::Raw(RawEvent::Mouse(mouse(
            MouseEventKind::ScrollDown,
            status.0,
            status.1,
        ))),
        &provider,
    );
    assert!(
        app.layers.storage.scroll() > 0,
        "wheel scrolls the hovered status pane"
    );
    assert_eq!(app.layers.storage.selected(), selected);
    for _ in 0..limit {
        app.handle(
            Action::Raw(RawEvent::Mouse(mouse(
                MouseEventKind::ScrollDown,
                status.0,
                status.1,
            ))),
            &provider,
        );
    }
    let scrolled = render(&provider, &mut app, 100, 25);
    assert!(scrolled.contains("bounded detail"));
    assert!(scrolled.contains("unused.rows.idx"));
    app.handle(raw_key(KeyCode::Char('c')), &provider);
    assert!(app.layers.storage.outbox.take().is_empty());
    assert!(app.layers.storage.confirm_clear());
    app.handle(raw_key(KeyCode::Char('c')), &provider);
    assert!(matches!(
        app.layers.storage.outbox.take()[0].kind,
        lvu::StorageRequestKind::ClearUnusedDerived
    ));
    app.handle(raw_key(KeyCode::Esc), &provider);
    assert!(matches!(
        app.layers.storage.outbox.take()[0].kind,
        lvu::StorageRequestKind::Cancel
    ));
    assert_eq!(app.focus, Focus::Logs);
}

fn demo() -> (FixtureProvider, App) {
    let (provider, sources, views) = FixtureProvider::demo();
    (provider, App::new(sources, views, true))
}

#[test]
fn dismissal_keys_close_one_app_layer_before_quitting_workspace() {
    let (provider, mut app) = demo();
    for focus in [Focus::Logs, Focus::Selector] {
        app.focus = focus;
        assert_eq!(
            app.key_to_action(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            Action::Quit
        );
        assert_eq!(
            app.key_to_action(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE)),
            Action::Quit
        );
    }

    app.focus = Focus::Logs;
    app.handle(Action::Open(Open::Fields), &provider);
    // A converted layer owns its keymap: `terminal.rs` hands `q` over raw and
    // the shell turns it into `Event::Dismiss`.
    assert_eq!(
        app.key_to_action(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE)),
        Action::CancelEditor
    );
    app.handle(raw_key(KeyCode::Char('q')), &provider);
    assert_eq!(app.focus, Focus::Logs);

    app.handle(Action::Open(Open::Help), &provider);
    // A converted layer owns its keymap: `terminal.rs` hands `q` over raw and
    // the shell turns it into `Event::Dismiss`, so the layer closes without a
    // `Focus::Help` key table.
    assert_eq!(
        app.key_to_action(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE)),
        Action::CancelEditor
    );
    app.handle(raw_key(KeyCode::Char('q')), &provider);
    assert_eq!(app.focus, Focus::Logs);

    app.configure_settings(settings_context());
    app.handle(Action::Open(Open::Settings), &provider);
    settings_activate(
        &mut app,
        &provider,
        SettingsControl::Field(SettingsField::Theme),
    );
    assert!(app.layers.settings.state().unwrap().dropdown.is_some());
    // §5.3: the innermost thing closes first, so `q` shuts the dropdown and
    // leaves the layer open.
    app.handle(raw_char('q'), &provider);
    assert!(!app.layers.settings.state().unwrap().dropdown.is_some());
    assert_eq!(app.focus, Focus::Layer);
    app.handle(raw_ctrl(KeyCode::Char('c')), &provider);
    assert!(app.should_quit);
}

#[test]
fn details_dismissal_returns_to_logs_before_workspace_quit() {
    for code in [KeyCode::Esc, KeyCode::Char('q')] {
        let (provider, mut app) = demo();
        app.show_details = false;
        app.handle(Action::ToggleDetails, &provider);
        assert_eq!(app.focus, Focus::Details);
        let mut terminal = Terminal::new(TestBackend::new(100, 28)).unwrap();
        terminal
            .draw(|frame| ui::render(frame, &mut app, &provider))
            .unwrap();
        assert!(screen(terminal.backend().buffer()).contains("Selected event details"));

        let action = app.key_to_action(KeyEvent::new(code, KeyModifiers::NONE));
        assert_eq!(action, Action::ToggleDetails);
        app.handle(action, &provider);
        assert_eq!(app.focus, Focus::Logs);
        assert!(!app.show_details);
        assert!(!app.should_quit);
        terminal
            .draw(|frame| ui::render(frame, &mut app, &provider))
            .unwrap();
        assert_eq!(
            app.key_to_action(KeyEvent::new(code, KeyModifiers::NONE)),
            Action::Quit
        );
    }
}

#[test]
fn dismissal_preserves_parent_of_completions_dropdowns_and_context() {
    for code in [KeyCode::Esc, KeyCode::Char('q')] {
        let (provider, mut app) = demo();
        render(&provider, &mut app, 100, 28);
        app.handle(Action::Open(Open::Advanced), &provider);
        app.handle(raw_key(KeyCode::Tab), &provider);
        assert!(app.layers.filter.completion().is_some());
        assert!(render(&provider, &mut app, 100, 28).contains("Complete field"));
        app.handle(raw_key(code), &provider);
        assert!(app.layers.filter.completion().is_none());
        assert_eq!(app.focus, Focus::Layer);
        assert!(app.advanced_state().unwrap().draft.is_empty());
        app.handle(raw_key(KeyCode::Esc), &provider);

        app.handle(Action::Open(Open::Time), &provider);
        time_focus(&mut app, &provider, TimeControl::Basis);
        app.handle(raw_key(KeyCode::Enter), &provider);
        assert!(app.layers.time.state().dropdown.is_some());
        render(&provider, &mut app, 100, 28);
        app.handle(raw_key(code), &provider);
        assert!(app.layers.time.state().dropdown.is_none());
        assert_eq!(app.focus, Focus::Layer);
        app.handle(raw_key(KeyCode::Esc), &provider);

        // `o` is a jump now (raw-context-as-jump.md), not a child of Fields;
        // on the raw stream it only re-pushes Fields, and these keys then
        // dismiss Fields itself, one layer, back to the base.
        app.handle(Action::Open(Open::Fields), &provider);
        app.handle(raw_key(KeyCode::Char('o')), &provider);
        assert!(render(&provider, &mut app, 100, 28).contains("Fields · record"));
        let action = app.key_to_action(KeyEvent::new(code, KeyModifiers::NONE));
        app.handle(action, &provider);
        assert_eq!(app.focus, Focus::Logs);
        assert!(!app.should_quit);

        let mut source = App::new(vec![], vec![], false);
        source.handle(raw_char('a'), &provider);

        let request = take_path_completions(&mut source).pop().unwrap();
        assert!(source.apply_path_completion_result(
            request.generation,
            &request.draft,
            None,
            vec!["alpha".into(), "another".into()],
            None
        ));
        assert!(render(&provider, &mut source, 100, 28).contains("Suggestions"));
        source.handle(
            Action::Raw(RawEvent::Key(KeyEvent::new(code, KeyModifiers::NONE))),
            &provider,
        );
        let dialog = source.layers.source.state();
        assert!(dialog.path_completion.candidates.is_empty());
        assert_eq!(dialog.control, SourceControl::Input);
        let expected_draft = if code == KeyCode::Char('q') {
            "aq"
        } else {
            "a"
        };
        assert_eq!(dialog.draft, expected_draft);
        assert_eq!(source.focus, Focus::Layer);
        assert!(!source.apply_path_completion_result(
            request.generation,
            &request.draft,
            None,
            vec!["stale".into()],
            None
        ));
        source.handle(raw_char('q'), &provider);
        assert_eq!(
            source.layers.source.state().draft,
            format!("{expected_draft}q")
        );
    }
}

#[test]
fn q_is_literal_only_for_the_active_editable_target() {
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::Search), &provider);
    // The layer owns its keymap: `terminal.rs` hands both keys over raw and
    // the shell decides which is a dismissal from `Surface::text_focus`.
    assert!(app.layers.filter.surface().text_focus);
    app.handle(raw_key(KeyCode::Char('q')), &provider);
    assert_eq!(app.search_state().unwrap().draft, "q");
    assert!(
        app.layers.filter.is_open(),
        "q is a character, not a dismissal"
    );
    app.handle(raw_key(KeyCode::Esc), &provider);
    assert!(!app.layers.filter.is_open());
    assert_eq!(app.focus, Focus::Logs);
}

fn settings_context() -> SettingsContext {
    SettingsContext {
        saved: SettingsValues {
            provider: "codex/old".into(),
            mode: "full-access".into(),
            thinking: "medium".into(),
            theme: ThemeId::Terminal,
            display_zone: "Z".into(),
            delight_enabled: true,
            reduced_motion: false,
            ascii: false,
            rows_mib: "4".into(),
            membership_mib: "256".into(),
            disk_total_mib: "5120".into(),
            index_per_source_mib: "256".into(),
        },
        effective_provider: "codex/env".into(),
        effective_mode: "full-access".into(),
        effective_thinking: "medium".into(),
        effective_theme: ThemeId::Terminal,
        effective_display_zone: "Z".into(),
        display_zone_source: "default",
        effective_delight_enabled: false,
        effective_reduced_motion: true,
        effective_ascii: false,
        provider_source: "environment LVU_AI_PROVIDER".into(),
        mode_source: "settings.toml".into(),
        thinking_source: "settings.toml".into(),
        delight_source: "environment LVU_NO_DELIGHT".into(),
        reduced_motion_source: "environment LVU_REDUCED_MOTION".into(),
        ascii_source: "settings.toml".into(),
        settings_path: "/config/lvu/settings.toml".into(),
        data_path: "/data/lvu".into(),
        cache_path: "/cache/lvu".into(),
        capture_path: "/data/lvu/captures".into(),
        applied_rows_mib: 4,
        applied_membership_mib: 256,
        applied_disk_total_mib: 5120,
        applied_index_per_source_mib: 256,
    }
}

#[test]
fn settings_preview_save_and_dialog_generation_are_fenced() {
    let (provider, mut app) = demo();
    app.configure_settings(settings_context());
    app.handle(Action::Open(Open::Settings), &provider);
    let first_generation = app.layers.settings.state().unwrap().generation;
    settings_choose_theme(&mut app, &provider, 1);
    assert_eq!(
        app.appearance.theme_id,
        ThemeId::LoveDark,
        "theme previews immediately"
    );
    settings_activate(&mut app, &provider, SettingsControl::Save);
    let request = app.layers.settings.outbox.take().pop().unwrap();
    assert_eq!(request.generation, first_generation);

    app.handle(raw_key(KeyCode::Esc), &provider);
    assert_eq!(
        app.appearance.theme_id,
        ThemeId::Terminal,
        "cancel restores effective theme"
    );
    app.handle(Action::Open(Open::Settings), &provider);
    assert_ne!(
        app.layers.settings.state().unwrap().generation,
        first_generation
    );
    assert!(app.complete_settings_save(first_generation, Ok(settings_context())));
    assert_eq!(
        app.layers.settings.state().unwrap().draft.theme,
        ThemeId::Terminal
    );

    let backend = TestBackend::new(110, 28);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| ui::render(frame, &mut app, &provider))
        .unwrap();
    let cursor = terminal.backend().cursor_position();
    assert_ne!((cursor.x, cursor.y), (0, 0));
    assert_eq!(
        terminal.backend().buffer()[(cursor.x, cursor.y)].bg,
        app.appearance.theme_id.theme().cursor,
        "focused editable settings field has a visible semantic cursor"
    );

    let settings_screen = render(&provider, &mut app, 110, 28);
    assert!(settings_screen.contains("Settings"));

    // One press per field: the display zone joined the Appearance section.
    for _ in 0..11 {
        app.handle(raw_key(KeyCode::Down), &provider);
    }
    let backend = TestBackend::new(54, 12);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| ui::render(frame, &mut app, &provider))
        .unwrap();
    let cursor = terminal.backend().cursor_position();
    let rendered = screen(terminal.backend().buffer());
    assert!(rendered.contains("Per source"), "{rendered}");
    assert!(!rendered.contains("Space toggle"), "{rendered}");
    assert!(cursor.y < 12, "cursor must stay inside the dialog");
}

#[test]
fn older_settings_completion_advances_new_dialog_rollback_without_losing_preview() {
    let (provider, mut app) = demo();
    app.configure_settings(settings_context());

    app.handle(Action::Open(Open::Settings), &provider);
    let generation_a = app.layers.settings.state().unwrap().generation;
    settings_choose_theme(&mut app, &provider, 1);
    settings_activate(&mut app, &provider, SettingsControl::Save);
    assert_eq!(
        app.layers.settings.outbox.take()[0].generation,
        generation_a
    );
    app.handle(raw_key(KeyCode::Esc), &provider);

    app.handle(Action::Open(Open::Settings), &provider);
    let generation_b = app.layers.settings.state().unwrap().generation;
    assert_ne!(generation_b, generation_a);
    settings_choose_theme(&mut app, &provider, 2);
    assert_eq!(app.appearance.theme_id, ThemeId::LoveLight);

    let mut saved_a = settings_context();
    saved_a.saved.theme = ThemeId::LoveDark;
    saved_a.effective_theme = ThemeId::LoveDark;
    assert!(app.complete_settings_save(generation_a, Ok(saved_a)));
    let dialog_b = app.layers.settings.state().unwrap();
    assert_eq!(dialog_b.generation, generation_b);
    assert_eq!(dialog_b.draft.theme, ThemeId::LoveLight);
    assert_eq!(dialog_b.context.effective_theme, ThemeId::LoveDark);
    assert_eq!(
        app.appearance.theme_id,
        ThemeId::LoveLight,
        "new preview remains active"
    );

    app.handle(raw_key(KeyCode::Esc), &provider);
    assert_eq!(
        app.appearance.theme_id,
        ThemeId::LoveDark,
        "closing the newer dialog restores the latest saved baseline"
    );
}

#[test]
fn settings_form_has_bounded_controls_dropdown_status_and_real_overflow() {
    let (provider, mut app) = demo();
    app.configure_settings(settings_context());
    app.handle(Action::Open(Open::Settings), &provider);

    let wide = render(&provider, &mut app, 150, 40);
    assert!(wide.contains("[ Save ]"), "{wide}");
    assert!(wide.contains("Saved"), "{wide}");
    assert!(
        wide.contains("cache limits apply after restart"),
        "the saved state still explains the restart requirement: {wide}"
    );
    assert!(!wide.contains("Saved: Saved"), "no status stutter: {wide}");
    assert!(wide.contains("Effective values and paths"), "{wide}");
    assert!(!wide.contains("Space toggle"), "{wide}");
    assert!(
        wide.contains("[ More ]"),
        "real wrapped details overflow: {wide}"
    );
    let provider_y = settings_control_rect(&app, SettingsControl::Field(SettingsField::Provider))
        .unwrap()
        .y;
    // dialog-system.md §12.14 gives each agent setting its own labelled row; the
    // invariant that replaced "share a row" is that they share a field column
    // and stay adjacent, instead of being flung 30 columns apart at 150 wide.
    let provider_x = settings_control_rect(&app, SettingsControl::Field(SettingsField::Provider))
        .unwrap()
        .x;
    for (offset, field) in [SettingsField::Mode, SettingsField::Thinking]
        .into_iter()
        .enumerate()
    {
        let rect = settings_control_rect(&app, SettingsControl::Field(field)).unwrap();
        assert_eq!(rect.x, provider_x, "agent fields share the field column");
        assert_eq!(
            rect.y,
            provider_y + u16::try_from(offset).unwrap() + 1,
            "agent fields stay adjacent"
        );
    }

    settings_focus(
        &mut app,
        &provider,
        SettingsControl::Field(SettingsField::Theme),
    );
    render(&provider, &mut app, 150, 40);
    assert!(
        app.layers.settings.surface().caret.is_none(),
        "a dropdown field takes no caret"
    );
    // Space is the activate key the layer's own table keeps.
    app.handle(raw_char(' '), &provider);
    assert!(app.layers.settings.state().unwrap().dropdown.is_some());
    let dropdown = render(&provider, &mut app, 80, 24);
    assert!(dropdown.contains("love-dark"), "{dropdown}");
    assert_eq!(
        app.layers.settings.theme_choice_rects().len(),
        ThemeId::ALL.len()
    );
    app.handle(raw_key(KeyCode::Down), &provider);
    app.handle(raw_key(KeyCode::Enter), &provider);
    assert_eq!(app.appearance.theme_id, ThemeId::LoveDark);
    assert_eq!(
        app.layers.settings.state().unwrap().status_kind,
        SettingsStatus::Pending
    );

    let narrow = render(&provider, &mut app, 54, 12);
    assert!(narrow.contains("[ More ]"), "{narrow}");
    assert!(settings_control_rect(&app, SettingsControl::More).is_some());
    settings_focus(&mut app, &provider, SettingsControl::More);
    let resized = render(&provider, &mut app, 150, 40);
    if resized.contains("[ More ]") {
        assert_eq!(
            app.layers.settings.state().unwrap().focus,
            SettingsControl::More,
            "real wrapped overflow keeps its visible focus"
        );
        assert!(settings_control_rect(&app, SettingsControl::More).is_some());
    } else {
        assert_eq!(
            app.layers.settings.state().unwrap().focus,
            SettingsControl::Save,
            "removed overflow must not strand invisible focus"
        );
        assert!(settings_control_rect(&app, SettingsControl::More).is_none());
    }
    settings_focus(
        &mut app,
        &provider,
        SettingsControl::Field(SettingsField::IndexPerSource),
    );
    let narrow = render(&provider, &mut app, 54, 12);
    assert!(narrow.contains("Per source"), "{narrow}");
    assert!(
        app.layers.settings.surface().caret.is_some(),
        "a focused editable field draws a caret"
    );

    settings_focus(&mut app, &provider, SettingsControl::Save);
    render(&provider, &mut app, 80, 24);
    let save = settings_control_rect(&app, SettingsControl::Save).unwrap();
    app.handle(raw_click(save.x, save.y), &provider);
    assert_eq!(
        app.layers.settings.state().unwrap().status_kind,
        SettingsStatus::Pending
    );
    assert!(matches!(app.layers.settings.outbox.take().as_slice(), [_]));
    let generation = app.layers.settings.state().unwrap().generation;
    assert!(app.complete_settings_save(generation, Err("invalid cache limit".into())));
    assert_eq!(
        app.layers.settings.state().unwrap().status_kind,
        SettingsStatus::Error
    );
    let error = render(&provider, &mut app, 80, 24);
    // §7.4: one message row, no `Label: Sentence` stutter, and the failure
    // text is the sentence rather than a pane the user has to scroll to.
    assert!(error.contains("Error"), "{error}");
    assert!(
        error.contains("save failed: invalid cache limit"),
        "{error}"
    );
}

#[test]
fn shared_time_and_settings_surfaces_keep_semantic_contrast() {
    use lvu::dialog_controls::DialogStyles;

    fn assert_text_fg(buffer: &Buffer, needle: &str, expected: ratatui::style::Color) {
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                let matches = needle.chars().enumerate().all(|(offset, character)| {
                    let offset = u16::try_from(offset).unwrap();
                    x.saturating_add(offset) < buffer.area.width
                        && buffer[(x + offset, y)].symbol() == character.to_string()
                });
                if matches {
                    assert_eq!(buffer[(x, y)].fg, expected, "style for {needle:?}");
                    return;
                }
            }
        }
        panic!("missing rendered text {needle:?}");
    }

    for theme in [Theme::LOVE_DARK, Theme::LOVE_LIGHT] {
        let styles = DialogStyles::new(theme);
        let (provider, mut app) = demo();
        app.configure_settings(settings_context());
        app.handle(Action::Open(Open::Settings), &provider);
        // The scrimmed sidebar also draws "●", so anchor on the message row's
        // glyph-plus-state-word pair, which occurs only there.
        let message_glyph = "● Saved";
        settings_focus(
            &mut app,
            &provider,
            SettingsControl::Field(SettingsField::Mode),
        );
        let mut terminal = Terminal::new(TestBackend::new(150, 40)).unwrap();
        terminal
            .draw(|frame| ui::render_with_theme(frame, &mut app, &provider, theme, None))
            .unwrap();
        let provider_rect =
            settings_control_rect(&app, SettingsControl::Field(SettingsField::Provider)).unwrap();
        assert!(
            (provider_rect.x..provider_rect.right())
                .all(|x| terminal.backend().buffer()[(x, provider_rect.y)].bg == theme.input_bg)
        );
        assert_text_fg(
            terminal.backend().buffer(),
            message_glyph,
            styles.applied.fg.unwrap(),
        );

        settings_focus(
            &mut app,
            &provider,
            SettingsControl::Field(SettingsField::Delight),
        );
        terminal
            .draw(|frame| ui::render_with_theme(frame, &mut app, &provider, theme, None))
            .unwrap();
        let delight =
            settings_control_rect(&app, SettingsControl::Field(SettingsField::Delight)).unwrap();
        assert_eq!(
            terminal.backend().buffer()[(delight.x, delight.y)].bg,
            theme.selection_bg
        );

        app.handle(raw_key(KeyCode::Esc), &provider);
        app.handle(Action::Open(Open::Time), &provider);
        time_focus(&mut app, &provider, TimeControl::StartClock);
        terminal
            .draw(|frame| ui::render_with_theme(frame, &mut app, &provider, theme, None))
            .unwrap();
        let start_date = app
            .layers
            .time
            .control_rects()
            .iter()
            .find_map(|(rect, control)| (*control == TimeControl::StartDate).then_some(*rect))
            .unwrap();
        assert!(
            (start_date.x..start_date.right())
                .all(|x| terminal.backend().buffer()[(x, start_date.y)].bg == theme.input_bg)
        );
        assert_text_fg(
            terminal.backend().buffer(),
            "● Applied",
            styles.applied.fg.unwrap(),
        );

        time_focus(&mut app, &provider, TimeControl::Apply);
        terminal
            .draw(|frame| ui::render_with_theme(frame, &mut app, &provider, theme, None))
            .unwrap();
        let apply = app
            .layers
            .time
            .control_rects()
            .iter()
            .find_map(|(rect, control)| (*control == TimeControl::Apply).then_some(*rect))
            .unwrap();
        assert_eq!(
            terminal.backend().buffer()[(apply.x, apply.y)].bg,
            theme.selection_bg
        );
    }
}

#[test]
fn short_dropdowns_reveal_the_active_choice_and_use_selection_colors() {
    use lvu::dialog_controls::DialogStyles;

    let (provider, mut app) = demo();
    let mut context = settings_context();
    let last_theme = *ThemeId::ALL.last().unwrap();
    context.saved.theme = last_theme;
    context.effective_theme = last_theme;
    app.configure_settings(context);
    app.handle(Action::Open(Open::Settings), &provider);
    settings_activate(
        &mut app,
        &provider,
        SettingsControl::Field(SettingsField::Theme),
    );
    let mut terminal = Terminal::new(TestBackend::new(54, 8)).unwrap();
    terminal
        .draw(|frame| ui::render(frame, &mut app, &provider))
        .unwrap();
    let selected = app
        .layers
        .settings
        .theme_choice_rects()
        .iter()
        .find_map(|(rect, index)| (*index == ThemeId::ALL.len() - 1).then_some(*rect))
        .expect("last selected theme has a visible hitbox in the truncated dropdown");
    assert!(
        app.layers.settings.theme_choice_rects().len() < ThemeId::ALL.len(),
        "regression setup must render fewer choices than the complete theme list"
    );
    assert!(screen(terminal.backend().buffer()).contains(last_theme.as_str()));
    app.handle(raw_click(selected.x, selected.y), &provider);
    assert!(!app.layers.settings.state().unwrap().dropdown.is_some());
    assert_eq!(app.layers.settings.state().unwrap().draft.theme, last_theme);

    for theme in [Theme::LOVE_DARK, Theme::LOVE_LIGHT] {
        let styles = DialogStyles::new(theme);
        let (provider, mut app) = demo();
        app.handle(Action::Open(Open::Time), &provider);
        time_focus(&mut app, &provider, TimeControl::StartZoneMenu);
        app.handle(raw_key(KeyCode::Enter), &provider);
        app.handle(raw_key(KeyCode::Down), &provider);
        let highlighted = app.layers.time.state().highlighted;
        let mut terminal = Terminal::new(TestBackend::new(100, 28)).unwrap();
        terminal
            .draw(|frame| ui::render_with_theme(frame, &mut app, &provider, theme, None))
            .unwrap();
        let row = app
            .layers
            .time
            .choice_rects()
            .iter()
            .find_map(|(rect, index)| (*index == highlighted).then_some(*rect))
            .expect("keyboard-highlighted Time choice is visible");
        let cell = &terminal.backend().buffer()[(row.x, row.y)];
        assert_eq!(cell.fg, styles.selection.fg.unwrap());
        assert_eq!(cell.bg, styles.selection.bg.unwrap());
    }
}

#[test]
fn long_unicode_editor_uses_scrolled_input_surface_and_keeps_footer_clear() {
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::Advanced), &provider);
    let draft = "前置き".repeat(40) + " visible-tail";
    app.handle(Action::Raw(RawEvent::Paste(draft)), &provider);
    let backend = TestBackend::new(54, 12);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| ui::render(frame, &mut app, &provider))
        .unwrap();
    let cursor = terminal.backend().cursor_position();
    let buffer = terminal.backend().buffer();
    assert_eq!(
        buffer[(cursor.x, cursor.y)].bg,
        app.appearance.theme_id.theme().cursor
    );
    let rendered = screen(buffer);
    assert!(
        rendered.contains("visible-tail"),
        "the tail nearest the cursor stays visible"
    );
    assert!(!rendered.contains("Enter apply"), "{rendered}");
    assert!(usize::from(cursor.y) < rendered.lines().count());

    app.handle(raw_key(KeyCode::Tab), &provider);
    let backend = TestBackend::new(54, 12);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| ui::render(frame, &mut app, &provider))
        .unwrap();
    assert!(screen(terminal.backend().buffer()).contains("Complete field"));
    assert_ne!(
        terminal.backend().buffer()[terminal.backend().cursor_position()].bg,
        app.appearance.theme_id.theme().cursor,
        "completion overlay owns focus instead of leaving the editor cursor painted above it"
    );
}

#[test]
fn long_source_path_scrolls_inside_padded_body_above_footer() {
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::Source), &provider);
    let path = format!("/tmp/{}/visible.log", "長い path ".repeat(20));
    for character in path.chars() {
        app.handle(raw_char(character), &provider);
    }
    let backend = TestBackend::new(54, 12);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| ui::render(frame, &mut app, &provider))
        .unwrap();
    let rendered = screen(terminal.backend().buffer());
    let cursor = terminal.backend().cursor_position();
    assert!(rendered.contains("visible.log"), "{rendered}");
    assert!(rendered.contains("Manual"), "{rendered}");
    assert!(rendered.contains("Discover"), "{rendered}");
    assert!(rendered.contains("🧠"), "{rendered}");
    assert!(cursor.x > 1, "body keeps a horizontal padding cell");
    assert!(cursor.y < 10, "cursor must stay above the reserved footer");
}

fn render<P: RowProvider>(provider: &P, app: &mut App, width: u16, height: u16) -> String {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("test terminal");
    terminal
        .draw(|frame| ui::render(frame, app, provider))
        .expect("render");
    screen(terminal.backend().buffer())
}

fn take_path_completions(app: &mut App) -> Vec<lvu::PathCompletionRequest> {
    std::thread::sleep(Duration::from_millis(45));
    app.take_path_completion_requests()
}

/// Activate the Ask layer's primary action the way the user does: Tab to the
/// button, then Enter. The layer owns its keymap, so a test drives keys.
fn ask_submit(app: &mut App, provider: &FixtureProvider) {
    use lvu::app::AskControl;
    for _ in 0..8 {
        let focus = app.layers.ask.state().map(|dialog| dialog.focus);
        if matches!(
            focus,
            Some(AskControl::Submit) | Some(AskControl::Apply) | None
        ) {
            break;
        }
        app.handle(raw_key(KeyCode::Tab), provider);
    }
    app.handle(raw_key(KeyCode::Enter), provider);
}

/// The first cell whose hit resolves to `control`, found through the layer's
/// own `hit()` — the test never keeps a rect the component did not publish.
fn layer_body(app: &App) -> (u16, u16) {
    use lvu::component::Component;
    let popup = app.layers.ask.surface().popup;
    for y in popup.y..popup.bottom() {
        for x in popup.x..popup.right() {
            if app.layers.ask.hit((x, y)) == Some(lvu::components::ask::AskHit::Body) {
                return (x, y);
            }
        }
    }
    panic!("no cell hits the body");
}

fn open_investigation(app: &mut App, provider: &FixtureProvider) {
    app.handle(Action::Open(Open::Investigation), provider);
}

/// Tab to the primary and press it. The Question field owns Enter (it inserts
/// a newline), so submitting is reaching the button, exactly as it is for a
/// user.
fn investigation_submit(app: &mut App, provider: &FixtureProvider) {
    use lvu::app::InvestigationControl;
    for _ in 0..8 {
        match app.layers.investigation.state().map(|dialog| dialog.focus) {
            Some(InvestigationControl::Submit) | None => break,
            _ => app.handle(raw_key(KeyCode::Tab), provider),
        }
    }
    app.handle(raw_key(KeyCode::Enter), provider);
}

/// Whether the Ask layer draws `control` anywhere on its surface. The
/// component owns its hit regions, so "the dialog does not offer this button"
/// is a statement about `hit()`, not about a shell-wide table.
fn ask_offers(app: &App, control: lvu::app::AskControl) -> bool {
    use lvu::component::Component;
    let popup = app.layers.ask.surface().popup;
    (popup.y..popup.bottom()).any(|y| {
        (popup.x..popup.right()).any(|x| {
            app.layers.ask.hit((x, y)) == Some(lvu::components::ask::AskHit::Control(control))
        })
    })
}

fn layer_rect(app: &App, control: lvu::app::AskControl) -> (u16, u16) {
    use lvu::component::Component;
    let popup = app.layers.ask.surface().popup;
    for y in popup.y..popup.bottom() {
        for x in popup.x..popup.right() {
            if app.layers.ask.hit((x, y)) == Some(lvu::components::ask::AskHit::Control(control)) {
                return (x, y);
            }
        }
    }
    panic!("no cell hits {control:?}");
}

fn raw_key(code: KeyCode) -> Action {
    Action::Raw(RawEvent::Key(KeyEvent::new(code, KeyModifiers::NONE)))
}

fn raw_ctrl(code: KeyCode) -> Action {
    Action::Raw(RawEvent::Key(KeyEvent::new(code, KeyModifiers::CONTROL)))
}

fn raw_alt(code: KeyCode) -> Action {
    Action::Raw(RawEvent::Key(KeyEvent::new(code, KeyModifiers::ALT)))
}

fn raw_char(character: char) -> Action {
    raw_key(KeyCode::Char(character))
}

/// The Time layer owns its keymap now, so a test reaches a control the way a
/// user does: Tab until it has focus. This is what `Action::TimeFocus` did.
fn time_focus<P: RowProvider>(app: &mut App, provider: &P, control: TimeControl) {
    if !app.layers.time.is_open() {
        return;
    }
    for _ in 0..64 {
        if app.layers.time.state().focus == control {
            return;
        }
        app.handle(raw_key(KeyCode::Tab), provider);
    }
    panic!("{control:?} never took focus");
}

fn time_activate<P: RowProvider>(app: &mut App, provider: &P, control: TimeControl) {
    time_focus(app, provider, control);
    app.handle(raw_key(KeyCode::Enter), provider);
}

/// Open the dropdown on `control`, pick row `index`, commit it.
fn time_choose<P: RowProvider>(app: &mut App, provider: &P, control: TimeControl, index: usize) {
    time_activate(app, provider, control);
    app.layers.time.highlight(index);
    app.handle(raw_key(KeyCode::Enter), provider);
}

/// Settings owns its keymap now, so a test reaches a control the way a user
/// does: arrow down until it has focus. This is what `Action::FocusSettings`
/// did. `More` only exists while the body genuinely overflows, so render first.
/// Recipes owns its keymap now, so a test reaches a control the way a user
/// does: Tab until it has focus. This is what `Action::FocusRecipeControl` did.
fn recipe_focus<P: RowProvider>(app: &mut App, provider: &P, control: RecipeDialogControl) {
    for _ in 0..64 {
        if app.layers.recipes.state().control == control {
            return;
        }
        app.handle(raw_key(KeyCode::Tab), provider);
    }
    panic!("{control:?} never took focus");
}

fn recipe_activate<P: RowProvider>(app: &mut App, provider: &P, control: RecipeDialogControl) {
    recipe_focus(app, provider, control);
    app.handle(raw_key(KeyCode::Enter), provider);
}

/// `Action::SubmitRecipe` reached whichever recipe layer had focus, so the
/// tests that drove it now deliver the `CommandId` the top slot declares.
fn recipe_apply<P: RowProvider>(app: &mut App, provider: &P) {
    let layer = app.layers.top().expect("a recipe layer is open");
    app.handle(
        Action::Command(layer, lvu::command_palette::CommandId::RecipeApply),
        provider,
    );
}

/// The meta of the request the layer actually has outstanding. Tests used to
/// forge one by writing `pending_request_id`; the fence is the component's now.
fn recipe_request<P: RowProvider>(app: &mut App, provider: &P) -> lvu::RecipeRequestMeta {
    let _ = provider;
    // The newest outstanding request: a reopened layer may have older ones
    // still queued behind it, and the fence only matches the latest.
    app.take_recipe_requests()
        .into_iter()
        .rev()
        .find_map(|request| match request {
            lvu::RecipeRequest::List { meta }
            | lvu::RecipeRequest::History { meta, .. }
            | lvu::RecipeRequest::Save { meta, .. }
            | lvu::RecipeRequest::Import { meta, .. }
            | lvu::RecipeRequest::Export { meta, .. } => Some(meta),
            lvu::RecipeRequest::Outcome(_) => None,
        })
        .expect("the layer has a request outstanding")
}

/// Source owns its keymap now, so a test reaches a control by tabbing to it.
fn source_focus<P: RowProvider>(app: &mut App, provider: &P, control: SourceControl) {
    for _ in 0..16 {
        if app.layers.source.state().control == control {
            return;
        }
        app.handle(raw_key(KeyCode::Tab), provider);
    }
    panic!("{control:?} never took focus");
}

fn source_activate<P: RowProvider>(app: &mut App, provider: &P, control: SourceControl) {
    source_focus(app, provider, control);
    app.handle(raw_key(KeyCode::Enter), provider);
}

fn source_mode<P: RowProvider>(app: &mut App, provider: &P, mode: SourceDialogMode) {
    let control = match mode {
        SourceDialogMode::Manual => SourceControl::Manual,
        SourceDialogMode::Discovery => SourceControl::Discovery,
        SourceDialogMode::Ai => SourceControl::Agent,
    };
    if app.layers.source.state().mode != mode {
        source_activate(app, provider, control);
    }
}

/// The three enrichment layers own their keymaps now (§6.3 step 13), so a test
/// reaches a control the way a user does.
fn enrichment_focus<P: RowProvider>(app: &mut App, provider: &P, control: EnrichmentControl) {
    for _ in 0..8 {
        if app
            .views
            .active()
            .is_some_and(|state| state.enrichment_control == control)
        {
            return;
        }
        app.handle(raw_key(KeyCode::Tab), provider);
    }
    panic!("{control:?} never took focus");
}

fn enrichment_add<P: RowProvider>(app: &mut App, provider: &P) {
    app.handle(raw_alt(KeyCode::Char('a')), provider);
}

fn enrichment_edit<P: RowProvider>(app: &mut App, provider: &P) {
    app.handle(raw_alt(KeyCode::Char('e')), provider);
}

fn enrichment_remove<P: RowProvider>(app: &mut App, provider: &P) {
    app.handle(raw_alt(KeyCode::Char('r')), provider);
}

/// The step editor's focus ring, which Tab walks.
fn step_focus<P: RowProvider>(app: &mut App, provider: &P, control: EnrichmentStepControl) {
    for _ in 0..8 {
        if app.layers.enrichment_step.control() == control {
            return;
        }
        app.handle(raw_key(KeyCode::Tab), provider);
    }
    panic!("{control:?} never took focus");
}

/// Save the open step, which is Enter on the expression field.
fn step_submit<P: RowProvider>(app: &mut App, provider: &P) {
    step_focus(app, provider, EnrichmentStepControl::Expression);
    app.handle(raw_key(KeyCode::Enter), provider);
}

/// External command's focus ring: Tab walks the four fields and then the four
/// buttons, in that order.
fn command_focus<P: RowProvider>(
    app: &mut App,
    provider: &P,
    control: CommandEnrichmentControl,
    field: Option<CommandEnrichmentField>,
) {
    for _ in 0..16 {
        if app.layers.external_command.state().is_some_and(|dialog| {
            dialog.selected_control == control
                && field.is_none_or(|field| dialog.selected_field == field)
        }) {
            return;
        }
        app.handle(raw_key(KeyCode::Tab), provider);
    }
    panic!("{control:?}/{field:?} never took focus");
}

fn command_field<P: RowProvider>(app: &mut App, provider: &P, field: CommandEnrichmentField) {
    command_focus(app, provider, CommandEnrichmentControl::Field, Some(field));
}

/// Confirming a reviewed run is Enter while a field has the focus ring, which
/// is what `Action::ConfirmCommandEnrichmentRun` was bound to.
fn command_confirm_run<P: RowProvider>(app: &mut App, provider: &P) {
    command_focus(app, provider, CommandEnrichmentControl::Field, None);
    app.handle(raw_key(KeyCode::Enter), provider);
}

/// A command save is a chain change through the query seam (§12.5): one
/// enrichment query request, accepted here, never a command request.
fn accept_command_save(app: &mut App) -> String {
    assert!(
        app.take_command_enrichment_requests().is_empty(),
        "saving must not run"
    );
    let request = app.take_query_requests().pop().expect("one chain request");
    assert_eq!(request.purpose, QueryPurpose::Enrichment);
    let view_id = request.view_id.clone();
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: request.view_id,
        generation: request.generation,
        revision: request.revision,
        purpose: QueryPurpose::Enrichment,
        result: Ok(()),
    }));
    view_id
}

fn command_state(app: &App) -> &lvu::app::CommandEnrichmentDialogState {
    app.layers
        .external_command
        .state()
        .expect("command enrichment dialog")
}

fn settings_focus<P: RowProvider>(app: &mut App, provider: &P, control: SettingsControl) {
    for _ in 0..64 {
        if app
            .layers
            .settings
            .state()
            .is_some_and(|dialog| dialog.focus == control)
        {
            return;
        }
        app.handle(raw_key(KeyCode::Tab), provider);
    }
    panic!("{control:?} never took focus");
}

fn settings_activate<P: RowProvider>(app: &mut App, provider: &P, control: SettingsControl) {
    settings_focus(app, provider, control);
    app.handle(raw_key(KeyCode::Enter), provider);
}

/// Open the theme dropdown and commit the choice at `index`.
fn settings_choose_theme<P: RowProvider>(app: &mut App, provider: &P, index: usize) {
    settings_activate(app, provider, SettingsControl::Field(SettingsField::Theme));
    for _ in 0..ThemeId::ALL.len() {
        if app
            .layers
            .settings
            .state()
            .is_some_and(|dialog| dialog.choice_selected == index)
        {
            break;
        }
        app.handle(raw_key(KeyCode::Down), provider);
    }
    app.handle(raw_key(KeyCode::Enter), provider);
}

fn settings_control_rect(app: &App, control: SettingsControl) -> Option<ratatui::layout::Rect> {
    app.layers
        .settings
        .control_rects()
        .iter()
        .find_map(|(rect, candidate)| (*candidate == control).then_some(*rect))
}

fn raw_click(column: u16, row: u16) -> Action {
    Action::Raw(RawEvent::Mouse(mouse(
        MouseEventKind::Down(MouseButton::Left),
        column,
        row,
    )))
}

/// The Bookmarks layer owns its keymap: reach a control the way a user does.
fn bookmark_focus<P: RowProvider>(
    app: &mut App,
    provider: &P,
    control: lvu::app::BookmarkDialogControl,
) {
    for _ in 0..32 {
        if app.layers.bookmarks.state().control == control {
            return;
        }
        app.handle(raw_key(KeyCode::Tab), provider);
    }
    panic!("{control:?} never took focus");
}

fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

#[test]
fn initial_layout_fills_viewport_and_layout_changes_keep_newest_visible() {
    let (provider, mut app) = demo();
    let initial = render(&provider, &mut app, 88, 24);
    assert!(initial.contains("fixture request 01 completed"));
    assert!(initial.contains("fixture request 16 completed"));
    assert_eq!(app.view_state().expect("state").viewport_height, 19);

    app.handle(Action::ToggleDetails, &provider);
    let details = render(&provider, &mut app, 88, 24);
    assert!(details.contains("fixture request 16 completed"));
    assert!(details.contains("stable display id: api:16"));
    assert_eq!(
        app.view_state().expect("state").selected,
        Some(RowId::new("api", 16))
    );

    let resized = render(&provider, &mut app, 88, 12);
    assert!(resized.contains("fixture request 16 completed"));
    assert!(resized.contains("stable display id: api:16"));
}

#[test]
fn navigation_keeps_stable_selection_and_per_view_state() {
    let (mut provider, mut app) = demo();
    app.sync_provider(&provider, 4);
    app.handle(Action::ToggleFollow, &provider);
    app.handle(Action::MoveLine(-2), &provider);
    let frozen = app.view_state().expect("state").selected.clone();
    let frozen_top = app.view_state().expect("state").top;

    provider.advance();
    app.sync_provider(&provider, 4);
    assert_eq!(app.view_state().expect("state").selected, frozen);
    assert_eq!(app.view_state().expect("state").top, frozen_top);

    app.handle(Action::NextView, &provider);
    app.sync_provider(&provider, 4);
    app.handle(Action::Top, &provider);
    assert_eq!(
        app.view_state().expect("state").selected,
        Some(RowId::new("worker", 4))
    );
    app.handle(Action::PreviousView, &provider);
    assert_eq!(app.view_state().expect("state").selected, frozen);
}

#[test]
fn drafts_and_async_results_are_independent_generation_fenced_and_bounded() {
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::Search), &provider);
    app.handle(Action::Raw(RawEvent::Paste("request 01".into())), &provider);
    app.handle(raw_key(KeyCode::Esc), &provider);
    app.handle(Action::Open(Open::Search), &provider);
    assert_eq!(app.search_state().expect("search").draft, "request 01");

    app.handle(raw_key(KeyCode::Enter), &provider);
    app.handle(raw_key(KeyCode::Char('2')), &provider);
    app.handle(raw_key(KeyCode::Enter), &provider);
    let requests = app.take_query_requests();
    assert_eq!(requests.len(), 1, "submissions coalesce per view");
    let newest = requests[0].clone();
    assert_eq!(newest.generation, 2);
    assert_eq!(newest.purpose, QueryPurpose::Search);
    assert_eq!(
        newest.constraints.text.expect("text").literal,
        "request 012"
    );
    assert!(newest.constraints.advanced_polars.is_none());

    assert!(!app.apply_query_completion(QueryCompletion {
        view_id: "all".into(),
        generation: 1,
        revision: 1,
        purpose: QueryPurpose::Search,
        result: Ok(()),
    }));
    app.handle(raw_key(KeyCode::Esc), &provider);
    app.handle(Action::NextView, &provider);
    app.handle(Action::Open(Open::Search), &provider);
    app.handle(Action::Raw(RawEvent::Paste("queue".into())), &provider);
    assert_eq!(app.search_state().expect("search").draft, "queue");

    assert!(app.apply_query_completion(QueryCompletion {
        view_id: newest.view_id,
        generation: newest.generation,
        revision: newest.revision,
        purpose: QueryPurpose::Search,
        result: Ok(()),
    }));
    assert_eq!(app.active_view_id(), Some("errors"));
    assert_eq!(app.search_state().expect("search").applied, "");
    app.handle(raw_key(KeyCode::Esc), &provider);
    app.handle(Action::PreviousView, &provider);
    assert_eq!(app.search_state().expect("search").applied, "request 012");

    app.handle(Action::Open(Open::Search), &provider);
    let before_oversized_paste = app.search_state().expect("search").draft.clone();
    app.handle(
        Action::Raw(RawEvent::Paste("x".repeat(MAX_EDITOR_BYTES + 100))),
        &provider,
    );
    assert_eq!(
        app.search_state().expect("search").draft,
        before_oversized_paste
    );
}

#[test]
fn enrichment_editor_emits_composite_request_and_failed_draft_preserves_applied() {
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::Enrichment), &provider);
    let list = render(&provider, &mut app, 100, 28);
    assert!(list.contains("Steps"), "{list}");
    assert!(!list.contains("Expression"), "{list}");
    enrichment_add(&mut app, &provider);
    let step = render(&provider, &mut app, 100, 28);
    assert!(step.contains("Input record"), "{step}");
    assert!(step.contains("No accepted outputs yet"), "{step}");
    app.handle(
        Action::Raw(RawEvent::Paste("status = pl.lit(200)".into())),
        &provider,
    );
    step_submit(&mut app, &provider);
    let request = app.take_query_requests().pop().unwrap();
    assert_eq!(request.purpose, QueryPurpose::Enrichment);
    assert_eq!(request.constraints.enrichment, None);
    assert_eq!(
        request.constraints.enrichments[0].source,
        "status = pl.lit(200)"
    );
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: request.view_id.clone(),
        generation: request.generation,
        revision: request.revision,
        purpose: QueryPurpose::Enrichment,
        result: Ok(()),
    }));
    assert_eq!(
        app.view_state().unwrap().enrichment.applied,
        "status = pl.lit(200)"
    );
    assert_eq!(app.layers.top(), Some(LayerId::Enrichment));

    enrichment_add(&mut app, &provider);
    app.handle(
        Action::Raw(RawEvent::Paste("invalid expression".into())),
        &provider,
    );
    step_submit(&mut app, &provider);
    let invalid = app.take_query_requests().pop().unwrap();
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: invalid.view_id.clone(),
        generation: invalid.generation,
        revision: invalid.revision,
        purpose: QueryPurpose::Enrichment,
        result: Err(QueryFailure {
            purpose: QueryPurpose::Enrichment,
            message: "invalid expression".into(),
        }),
    }));
    assert_eq!(
        app.view_state().unwrap().enrichment.applied,
        "status = pl.lit(200)"
    );
    let rebase = app.take_query_requests().pop().unwrap();
    assert_eq!(rebase.purpose, QueryPurpose::Enrichment);
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: rebase.view_id,
        generation: rebase.generation,
        revision: rebase.revision,
        purpose: QueryPurpose::Enrichment,
        result: Ok(()),
    }));
    assert_eq!(
        app.view_state().unwrap().enrichment.error.as_deref(),
        Some("invalid expression")
    );
}

#[test]
fn enrichment_stages_accumulate_edit_and_remove_transactionally() {
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::Enrichment), &provider);
    enrichment_add(&mut app, &provider);
    app.handle(
        Action::Raw(RawEvent::Paste("status = pl.lit('ready')".into())),
        &provider,
    );
    step_submit(&mut app, &provider);
    let first = app.take_query_requests().pop().unwrap();
    assert_eq!(first.constraints.enrichments.len(), 1);
    let first_id = first.constraints.enrichments[0].id.clone();
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: first.view_id,
        generation: first.generation,
        revision: first.revision,
        purpose: first.purpose,
        result: Ok(()),
    }));
    assert!(app.view_state().unwrap().enrichment.draft.is_empty());
    assert_eq!(app.layers.top(), Some(LayerId::Enrichment));

    enrichment_add(&mut app, &provider);
    app.handle(
        Action::Raw(RawEvent::Paste(
            "upper = pl.col('status').str.to_uppercase()".into(),
        )),
        &provider,
    );
    step_submit(&mut app, &provider);
    let second = app.take_query_requests().pop().unwrap();
    assert_eq!(second.constraints.enrichments.len(), 2);
    assert_eq!(second.constraints.enrichments[0].id, first_id);
    assert!(second.constraints.enrichments[1].source.contains("status"));
    let second_id = second.constraints.enrichments[1].id.clone();
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: second.view_id,
        generation: second.generation,
        revision: second.revision,
        purpose: second.purpose,
        result: Ok(()),
    }));
    assert_eq!(app.view_state().unwrap().enrichment_selected, 1);

    enrichment_edit(&mut app, &provider);
    while !app.view_state().unwrap().enrichment.draft.is_empty() {
        app.handle(raw_key(KeyCode::Backspace), &provider);
    }
    app.handle(
        Action::Raw(RawEvent::Paste("upper = invalid".into())),
        &provider,
    );
    step_submit(&mut app, &provider);
    let invalid_edit = app.take_query_requests().pop().unwrap();
    assert_eq!(invalid_edit.constraints.enrichments[1].id, second_id);
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: invalid_edit.view_id,
        generation: invalid_edit.generation,
        revision: invalid_edit.revision,
        purpose: invalid_edit.purpose,
        result: Err(QueryFailure {
            purpose: QueryPurpose::Enrichment,
            message: "invalid second stage".into(),
        }),
    }));
    assert!(
        app.view_state().unwrap().enrichments[1]
            .source
            .contains("to_uppercase")
    );
    assert_eq!(
        app.view_state().unwrap().enrichment.draft,
        "upper = invalid"
    );
    let reaffirm = app.take_query_requests().pop().unwrap();
    assert_eq!(
        reaffirm.constraints.enrichments,
        app.view_state().unwrap().enrichments
    );
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: reaffirm.view_id,
        generation: reaffirm.generation,
        revision: reaffirm.revision,
        purpose: reaffirm.purpose,
        result: Ok(()),
    }));
    assert_eq!(
        app.view_state().unwrap().enrichment.error.as_deref(),
        Some("invalid second stage")
    );
    // A rejected step keeps its own layer open; leaving it keeps the rejected
    // draft for correction and never touches the accepted chain.
    assert_eq!(app.layers.top(), Some(LayerId::EnrichmentStep));
    app.handle(raw_key(KeyCode::Esc), &provider);
    assert_eq!(app.layers.top(), Some(LayerId::Enrichment));
    assert_eq!(
        app.view_state().unwrap().enrichment.draft,
        "upper = invalid"
    );
    assert_eq!(
        app.view_state().unwrap().enrichment_editing,
        Some(second_id)
    );
    assert_eq!(app.view_state().unwrap().enrichments.len(), 2);

    enrichment_remove(&mut app, &provider);
    let removal = app.take_query_requests().pop().unwrap();
    assert_eq!(removal.constraints.enrichments.len(), 1);
    assert_eq!(removal.constraints.enrichments[0].id, first_id);
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: removal.view_id,
        generation: removal.generation,
        revision: removal.revision,
        purpose: removal.purpose,
        result: Ok(()),
    }));
    assert_eq!(app.view_state().unwrap().enrichments.len(), 1);

    enrichment_add(&mut app, &provider);
    app.handle(
        Action::Raw(RawEvent::Paste(r"/(?P<code>\d+) (?P<message>.*)/".into())),
        &provider,
    );
    step_submit(&mut app, &provider);
    let regex = app.take_query_requests().pop().unwrap();
    assert_eq!(regex.constraints.enrichments.len(), 2);
    assert_eq!(
        regex.constraints.enrichments[1].source,
        r"/(?P<code>\d+) (?P<message>.*)/"
    );
    assert_ne!(regex.constraints.enrichments[1].id, first_id);
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: regex.view_id,
        generation: regex.generation,
        revision: regex.revision,
        purpose: regex.purpose,
        result: Ok(()),
    }));
    let persisted = app
        .persistent_view_state(app.active_view_id().unwrap())
        .unwrap();
    assert_eq!(persisted.applied_enrichments.len(), 2);
    assert_eq!(persisted.enrichment_selected, 1);
    assert_eq!(persisted.enrichment_editing, None);
    let rendered = render(&provider, &mut app, 100, 28);
    assert!(rendered.contains("Steps"), "{rendered}");
    assert!(rendered.contains("(?P<code>"), "{rendered}");
    let first_row = app.layers.enrichment.row_rects()[0];
    app.handle(raw_click(first_row.0.x, first_row.0.y), &provider);
    assert_eq!(app.view_state().unwrap().enrichment_selected, first_row.1);
    enrichment_focus(&mut app, &provider, EnrichmentControl::Steps);
    app.handle(raw_key(KeyCode::Down), &provider);
    enrichment_remove(&mut app, &provider);
    let failed_remove = app.take_query_requests().pop().unwrap();
    assert_eq!(failed_remove.constraints.enrichments.len(), 1);
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: failed_remove.view_id,
        generation: failed_remove.generation,
        revision: failed_remove.revision,
        purpose: failed_remove.purpose,
        result: Err(QueryFailure {
            purpose: QueryPurpose::Enrichment,
            message: "remove validation failed".into(),
        }),
    }));
    assert_eq!(app.view_state().unwrap().enrichments.len(), 2);
    let reaffirm = app.take_query_requests().pop().unwrap();
    assert_eq!(reaffirm.constraints.enrichments.len(), 2);

    app.handle(Action::NextView, &provider);
    assert!(app.view_state().unwrap().enrichments.is_empty());
}

#[test]
fn enrichment_preview_uses_authoritative_details_and_small_layout_reserves_draft() {
    let provider = GrowingProvider {
        rows: RefCell::new(vec![DisplayRow {
            id: RowId::new("source", 1),
            timestamp: "00:00:01".into(),
            captured_at_unix_nanos: Some(1),
            level: "INFO".into(),
            text: "id=42 hello".into(),
            details: vec![
                ("derived.id".into(), "42".into()),
                ("derived.message".into(), "hello".into()),
            ],
            fields: vec![],
        }]),
    };
    let mut app = App::new(
        vec![SourceItem {
            id: "source".into(),
            name: "source".into(),
            health: "ready".into(),
        }],
        vec![ViewItem {
            id: "view".into(),
            source_id: "source".into(),
            name: "view".into(),
        }],
        false,
    );
    app.sync_provider(&provider, 8);
    let stages = (0..8)
        .map(|index| lvu::EnrichmentDefinition {
            id: lvu::EnrichmentStageId(format!("stage-{index}")),
            source: if index == 7 {
                r"/id=(?P<id>\d+) (?P<message>.*)/".into()
            } else {
                format!("field_{index} = pl.lit({index})")
            },
            command: None,
        })
        .collect::<Vec<_>>();
    assert!(app.restore_persistent_view(
        "view",
        PersistentViewState {
            applied_enrichments: stages.clone(),
            enrichment_draft: "next = pl.col('field_6')".into(),
            enrichment_editing: Some(stages[7].id.clone()),
            enrichment_selected: 7,
            ..PersistentViewState::default()
        }
    ));
    let request = app.take_query_requests().pop().unwrap();
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: request.view_id,
        generation: request.generation,
        revision: request.revision,
        purpose: request.purpose,
        result: Ok(()),
    }));
    app.handle(Action::Open(Open::Enrichment), &provider);
    let list = render(&provider, &mut app, 100, 28);
    assert!(list.contains("(?P<id>"), "{list}");
    assert!(list.contains("unsaved draft kept"), "{list}");
    assert!(!list.contains("id  42"), "{list}");

    // Layer two resumes the restored unfinished edit and shows the record it
    // reads next to the output the accepted chain already produced.
    enrichment_edit(&mut app, &provider);
    assert_eq!(
        app.view_state().unwrap().enrichment.draft,
        "next = pl.col('field_6')"
    );
    let normal = render(&provider, &mut app, 100, 28);
    assert!(normal.contains("id  42"), "{normal}");
    assert!(normal.contains("message  hello"), "{normal}");
    assert!(normal.contains("id=42 hello"), "{normal}");

    let backend = TestBackend::new(54, 12);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| ui::render(frame, &mut app, &provider))
        .unwrap();
    let rendered = screen(terminal.backend().buffer());
    let cursor = terminal.backend().cursor_position();
    assert!(rendered.contains("next = pl.col("), "{rendered}");
    assert!(cursor.y < 10, "draft cursor must remain above the footer");
    assert!(!rendered.contains("Native"), "{rendered}");
}

#[test]
fn enrichment_dependency_failure_restores_chain_and_accepted_advanced_filter() {
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::Enrichment), &provider);
    enrichment_add(&mut app, &provider);
    app.handle(
        Action::Raw(RawEvent::Paste("status = pl.lit('ready')".into())),
        &provider,
    );
    step_submit(&mut app, &provider);
    let stage = app.take_query_requests().pop().unwrap();
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: stage.view_id,
        generation: stage.generation,
        revision: stage.revision,
        purpose: stage.purpose,
        result: Ok(()),
    }));
    let accepted = app.view_state().unwrap().enrichments.clone();

    app.handle(raw_key(KeyCode::Esc), &provider);
    app.handle(Action::Open(Open::Advanced), &provider);
    app.handle(
        Action::Raw(RawEvent::Paste("pl.col('status') == 'ready'".into())),
        &provider,
    );
    app.handle(raw_key(KeyCode::Enter), &provider);
    let advanced = app.take_query_requests().pop().unwrap();
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: advanced.view_id,
        generation: advanced.generation,
        revision: advanced.revision,
        purpose: advanced.purpose,
        result: Ok(()),
    }));
    app.handle(raw_key(KeyCode::Esc), &provider);

    app.handle(Action::Open(Open::Enrichment), &provider);
    enrichment_remove(&mut app, &provider);
    let removal = app.take_query_requests().pop().unwrap();
    assert!(removal.constraints.enrichments.is_empty());
    assert_eq!(
        removal.constraints.advanced_polars.as_deref(),
        Some("pl.col('status') == 'ready'")
    );
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: removal.view_id,
        generation: removal.generation,
        revision: removal.revision,
        purpose: removal.purpose,
        result: Err(QueryFailure {
            purpose: QueryPurpose::Enrichment,
            message: "advanced filter still requires derived field status".into(),
        }),
    }));
    assert_eq!(app.view_state().unwrap().enrichments, accepted);
    assert_eq!(
        app.advanced_state().unwrap().applied,
        "pl.col('status') == 'ready'"
    );
    let reaffirm = app.take_query_requests().pop().unwrap();
    assert_eq!(reaffirm.constraints.enrichments, accepted);
    assert_eq!(
        reaffirm.constraints.advanced_polars.as_deref(),
        Some("pl.col('status') == 'ready'")
    );
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: reaffirm.view_id,
        generation: reaffirm.generation,
        revision: reaffirm.revision,
        purpose: reaffirm.purpose,
        result: Ok(()),
    }));
    assert!(app.take_query_requests().is_empty());
    assert_eq!(
        app.view_state().unwrap().enrichment.error.as_deref(),
        Some("advanced filter still requires derived field status")
    );

    enrichment_edit(&mut app, &provider);
    while !app.view_state().unwrap().enrichment.draft.is_empty() {
        app.handle(raw_key(KeyCode::Backspace), &provider);
    }
    app.handle(
        Action::Raw(RawEvent::Paste("renamed = pl.lit('ready')".into())),
        &provider,
    );
    step_submit(&mut app, &provider);
    let rename = app.take_query_requests().pop().unwrap();
    assert_eq!(rename.constraints.enrichments[0].id, accepted[0].id);
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: rename.view_id,
        generation: rename.generation,
        revision: rename.revision,
        purpose: rename.purpose,
        result: Err(QueryFailure {
            purpose: QueryPurpose::Enrichment,
            message: "renamed stage breaks accepted filter".into(),
        }),
    }));
    assert_eq!(app.view_state().unwrap().enrichments, accepted);
    let rename_reaffirm = app.take_query_requests().pop().unwrap();
    assert_eq!(rename_reaffirm.constraints.enrichments, accepted);
}

#[test]
fn restored_constraints_are_pending_until_real_dispatch_completion() {
    let (provider, mut app) = demo();
    let view_id = app.active_view_id().unwrap().to_owned();
    assert!(app.restore_persistent_view(
        &view_id,
        PersistentViewState {
            source_ids: Vec::new(),
            bookmarks: Vec::new(),
            color_rules: Vec::new(),
            selected_at: 0,
            view_name: "All events".into(),
            applied_time_field: None,
            time_field_draft: None,
            time_gap_threshold_seconds: 0,
            exact_field: None,
            union: None,
            applied_search: "request 01".into(),
            search_draft: "unfinished literal".into(),
            search_error: None,
            applied_advanced: "pl.col('raw').str.contains('completed')".into(),
            advanced_draft: "invalid (".into(),
            advanced_error: Some("invalid expression".into()),
            applied_enrichment: String::new(),
            applied_enrichments: vec![],
            enrichment_draft: String::new(),
            enrichment_error: None,
            enrichment_editing: None,
            enrichment_selected: 0,
            command_steps: Default::default(),
            applied_grouping: String::new(),
            grouping_draft: String::new(),
            grouping_error: None,
            applied_capture_time: None,
            applied_capture_time_policy: None,
            applied_time_basis: lvu::TimeBasis::Capture,
            time_start_draft: String::new(),
            time_end_draft: String::new(),
            time_recent_draft: String::new(),
            time_error: None,
            time_draft_touched: false,
            time_window_draft: Default::default(),
            time_basis_draft: Default::default(),
            time_start_date_draft: String::new(),
            time_start_clock_draft: String::new(),
            time_start_zone_draft: String::new(),
            time_end_date_draft: String::new(),
            time_end_clock_draft: String::new(),
            time_end_zone_draft: String::new(),
            time_structured_draft_present: false,
            selected: Some(RowId::new("api", 1)),
            follow: false,
            pinned_columns: vec![],
            color_field: None,
            severity_column: None,
            timestamp_column: None,
            fold_enabled: false,
            fold_minimum_run: 0,
            fold_key_column: None,
            fold_lookback: 0,
            fold_normalisation: lvu::FoldNormalisation::Standard,
            fold_expanded: Vec::new(),
        }
    ));
    assert!(app.search_state().unwrap().applied.is_empty());
    assert!(app.advanced_state().unwrap().applied.is_empty());
    let request = app.take_query_requests().pop().unwrap();
    assert_eq!(
        request.constraints.text.as_ref().unwrap().literal,
        "request 01"
    );
    assert_eq!(
        request.constraints.advanced_polars.as_deref(),
        Some("pl.col('raw').str.contains('completed')")
    );
    app.apply_query_completion(QueryCompletion {
        view_id,
        generation: request.generation,
        revision: request.revision,
        purpose: request.purpose,
        result: Ok(()),
    });
    assert_eq!(app.search_state().unwrap().applied, "request 01");
    assert_eq!(app.search_state().unwrap().draft, "unfinished literal");
    assert_eq!(app.advanced_state().unwrap().draft, "invalid (");
    assert!(!app.view_state().unwrap().follow);
    assert_eq!(
        app.view_state().unwrap().selected,
        Some(RowId::new("api", 1))
    );
    let _ = provider;
}

#[test]
fn enrichment_only_restore_remains_pending_until_recipe_is_accepted() {
    let (_provider, mut app) = demo();
    let view_id = app.active_view_id().unwrap().to_owned();
    let restored = vec![
        lvu::EnrichmentDefinition {
            id: lvu::EnrichmentStageId("extract".into()),
            source: r"/(?P<code>\d+)/".into(),
            command: None,
        },
        lvu::EnrichmentDefinition {
            id: lvu::EnrichmentStageId("label".into()),
            source: "label = pl.col('code').cast(pl.String)".into(),
            command: None,
        },
    ];
    assert!(app.restore_persistent_view(
        &view_id,
        PersistentViewState {
            applied_enrichments: restored.clone(),
            enrichment_draft: "label = pl.col(".into(),
            enrichment_error: Some("unfinished".into()),
            enrichment_editing: Some(lvu::EnrichmentStageId("label".into())),
            enrichment_selected: 1,
            ..PersistentViewState::default()
        }
    ));
    assert!(app.view_has_pending_query(&view_id));
    assert!(
        app.persistent_view_state(&view_id)
            .unwrap()
            .applied_enrichments
            .is_empty(),
        "autosave must not replace the stored recipe while restoration compiles"
    );
    let request = app.take_query_requests().pop().unwrap();
    assert_eq!(request.purpose, QueryPurpose::Enrichment);
    assert_eq!(request.constraints.enrichments, restored);
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: view_id.clone(),
        generation: request.generation,
        revision: request.revision,
        purpose: request.purpose,
        result: Ok(()),
    }));
    assert!(!app.view_has_pending_query(&view_id));
    let persisted = app.persistent_view_state(&view_id).unwrap();
    assert_eq!(persisted.applied_enrichments, restored);
    assert_eq!(persisted.enrichment_draft, "label = pl.col(");
    assert_eq!(
        persisted.enrichment_editing,
        Some(lvu::EnrichmentStageId("label".into()))
    );
    assert_eq!(persisted.enrichment_selected, 1);
    app.handle(Action::Open(Open::Enrichment), &_provider);
    app.handle(raw_key(KeyCode::Esc), &_provider);
    app.handle(Action::Open(Open::Enrichment), &_provider);
    assert_eq!(
        app.view_state().unwrap().enrichment_editing,
        Some(lvu::EnrichmentStageId("label".into()))
    );
    // Reopening the step editor resumes the restored unfinished edit rather
    // than replacing it with the accepted source.
    enrichment_edit(&mut app, &_provider);
    assert_eq!(
        app.view_state().unwrap().enrichment.draft,
        "label = pl.col("
    );
    step_submit(&mut app, &_provider);
    let edit = app.take_query_requests().pop().unwrap();
    assert_eq!(edit.constraints.enrichments.len(), 2);
    assert_eq!(edit.constraints.enrichments[1].id.0, "label");
    assert_eq!(edit.constraints.enrichments[1].source, "label = pl.col(");
}

#[test]
fn delayed_restore_cannot_overwrite_new_user_draft_applied_filter_or_navigation() {
    let (provider, mut app) = demo();
    app.sync_provider(&provider, 4);
    let view_id = app.active_view_id().unwrap().to_owned();
    let load_fence = app.view_interaction_revision(&view_id).unwrap();

    app.handle(Action::Open(Open::Search), &provider);
    app.handle(Action::Raw(RawEvent::Paste("new filter".into())), &provider);
    app.handle(raw_key(KeyCode::Enter), &provider);
    let request = app.take_query_requests().pop().unwrap();
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: view_id.clone(),
        generation: request.generation,
        revision: request.revision,
        purpose: request.purpose,
        result: Ok(()),
    }));
    app.handle(raw_key(KeyCode::Esc), &provider);
    app.handle(Action::ToggleFollow, &provider);
    app.handle(Action::MoveLine(-1), &provider);
    let before = app.persistent_view_state(&view_id).unwrap();

    assert!(!app.restore_persistent_view_if_unmodified(
        &view_id,
        load_fence,
        PersistentViewState {
            applied_search: "old filter".into(),
            search_draft: "old draft".into(),
            follow: true,
            ..PersistentViewState::default()
        }
    ));
    assert_eq!(app.persistent_view_state(&view_id).unwrap(), before);
    assert_eq!(app.search_state().unwrap().applied, "new filter");
}

#[test]
fn advanced_error_preserves_applied_filter_and_active_search_constraint() {
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::Advanced), &provider);
    app.handle(
        Action::Raw(RawEvent::Paste("pl.col('level') == 'ERROR'".into())),
        &provider,
    );
    app.handle(raw_key(KeyCode::Enter), &provider);
    let valid = app.take_query_requests().pop().expect("advanced request");
    assert_eq!(valid.purpose, QueryPurpose::Advanced);
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: valid.view_id,
        generation: valid.generation,
        revision: valid.revision,
        purpose: QueryPurpose::Advanced,
        result: Ok(()),
    }));

    app.handle(raw_key(KeyCode::Esc), &provider);
    app.handle(Action::Open(Open::Search), &provider);
    app.handle(Action::Raw(RawEvent::Paste("request".into())), &provider);
    app.handle(raw_key(KeyCode::Enter), &provider);
    let search = app.take_query_requests().pop().expect("search request");
    assert_eq!(
        search.constraints.advanced_polars.as_deref(),
        Some("pl.col('level') == 'ERROR'")
    );
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: search.view_id,
        generation: search.generation,
        revision: search.revision,
        purpose: QueryPurpose::Search,
        result: Ok(()),
    }));

    app.handle(raw_key(KeyCode::Esc), &provider);
    app.handle(Action::Open(Open::Advanced), &provider);
    app.handle(Action::Raw(RawEvent::Paste(" invalid".into())), &provider);
    app.handle(raw_key(KeyCode::Enter), &provider);
    let invalid = app.take_query_requests().pop().expect("invalid request");
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: invalid.view_id,
        generation: invalid.generation,
        revision: invalid.revision,
        purpose: QueryPurpose::Advanced,
        result: Err(QueryFailure {
            purpose: QueryPurpose::Advanced,
            message: "invalid advanced expression".into(),
        }),
    }));
    assert_eq!(app.search_state().expect("search").applied, "request");
    assert_eq!(
        app.advanced_state().expect("advanced").applied,
        "pl.col('level') == 'ERROR'"
    );
    let restore = app
        .take_query_requests()
        .pop()
        .expect("accepted composite restoration");
    assert_eq!(restore.purpose, QueryPurpose::Search);
    assert_eq!(restore.base_revision, search.revision);
    assert_eq!(
        restore.constraints.text.as_ref().expect("search").literal,
        "request"
    );
    assert_eq!(
        restore.constraints.advanced_polars.as_deref(),
        Some("pl.col('level') == 'ERROR'")
    );
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: restore.view_id,
        generation: restore.generation,
        revision: restore.revision,
        purpose: restore.purpose,
        result: Ok(()),
    }));
    assert_eq!(
        app.advanced_state().expect("advanced").error.as_deref(),
        Some("invalid advanced expression")
    );
}

fn finish_debounced_search(app: &mut App, dispatcher: &mut impl QueryDispatcher) {
    assert!(app.flush_debounced_searches(Instant::now() + SEARCH_DEBOUNCE));
    assert!(submit_query_requests(app, dispatcher));
    assert!(poll_query_completions(app, dispatcher));
}

#[test]
fn fixture_search_filters_arrivals_and_clear_restores_selection() {
    let (mut provider, mut app) = demo();
    let mut dispatcher = provider.query_dispatcher();
    app.sync_provider(&provider, 19);
    app.handle(Action::ToggleFollow, &provider);
    let selected = app.view_state().expect("state").selected.clone();

    app.handle(Action::Open(Open::Search), &provider);
    app.handle(Action::Raw(RawEvent::Paste("LATE".into())), &provider);
    finish_debounced_search(&mut app, &mut dispatcher);
    app.sync_provider(&provider, 19);
    assert_eq!(app.view_state().expect("state").last_total, 0);
    assert_eq!(app.view_state().expect("state").selected, selected);
    assert!(render(&provider, &mut app, 88, 24).contains("No matches"));

    provider.advance();
    app.sync_provider(&provider, 19);
    assert_eq!(app.view_state().expect("state").last_total, 1);
    assert_eq!(app.visible_rows(&provider)[0].id, RowId::new("api", 17));
    assert!(!app.view_state().expect("state").follow);

    for _ in 0..4 {
        app.handle(raw_key(KeyCode::Backspace), &provider);
    }
    finish_debounced_search(&mut app, &mut dispatcher);
    app.sync_provider(&provider, 19);
    assert_eq!(app.view_state().expect("state").last_total, 17);
    assert_eq!(app.view_state().expect("state").selected, selected);
    assert!(app.search_state().expect("search").applied.is_empty());
}

#[derive(Default)]
struct DelayedDispatcher {
    submitted: Vec<QueryRequest>,
    ready: Vec<QueryCompletion>,
}

impl QueryDispatcher for DelayedDispatcher {
    fn submit(&mut self, request: QueryRequest) -> Result<(), String> {
        self.submitted.push(request);
        Ok(())
    }

    fn poll(&mut self) -> Option<QueryCompletion> {
        if self.ready.is_empty() {
            None
        } else {
            Some(self.ready.remove(0))
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PublishedMembership {
    revision: u64,
    constraints: QueryConstraints,
}

#[derive(Default)]
struct MembershipDispatcher {
    submitted: Vec<QueryRequest>,
    ready: Vec<QueryCompletion>,
    latest_revision: HashMap<String, u64>,
    published: HashMap<String, PublishedMembership>,
}

impl MembershipDispatcher {
    fn finish(&mut self, revision: u64, result: Result<(), (QueryPurpose, &str)>) {
        let request = self
            .submitted
            .iter()
            .find(|request| request.revision == revision)
            .expect("submitted revision")
            .clone();
        if result.is_ok() && self.latest_revision.get(&request.view_id) == Some(&request.revision) {
            self.published.insert(
                request.view_id.clone(),
                PublishedMembership {
                    revision: request.revision,
                    constraints: request.constraints.clone(),
                },
            );
        }
        self.ready.push(QueryCompletion {
            view_id: request.view_id,
            generation: request.generation,
            revision: request.revision,
            purpose: request.purpose,
            result: result.map_err(|(purpose, message)| QueryFailure {
                purpose,
                message: message.to_owned(),
            }),
        });
    }
}

impl QueryDispatcher for MembershipDispatcher {
    fn submit(&mut self, request: QueryRequest) -> Result<(), String> {
        self.latest_revision
            .entry(request.view_id.clone())
            .and_modify(|revision| *revision = (*revision).max(request.revision))
            .or_insert(request.revision);
        self.submitted.push(request);
        Ok(())
    }

    fn poll(&mut self) -> Option<QueryCompletion> {
        if self.ready.is_empty() {
            None
        } else {
            Some(self.ready.remove(0))
        }
    }
}

fn submit_overlapping_constraints(
    app: &mut App,
    provider: &FixtureProvider,
    dispatcher: &mut MembershipDispatcher,
) -> (u64, u64) {
    app.handle(Action::Open(Open::Search), provider);
    app.handle(Action::Raw(RawEvent::Paste("request".into())), provider);
    app.handle(raw_key(KeyCode::Enter), provider);
    assert!(submit_query_requests(app, dispatcher));
    let search = dispatcher.submitted.last().expect("search").clone();

    app.handle(raw_key(KeyCode::Esc), provider);
    app.handle(Action::Open(Open::Advanced), provider);
    app.handle(
        Action::Raw(RawEvent::Paste("level == 'INFO'".into())),
        provider,
    );
    app.handle(raw_key(KeyCode::Enter), provider);
    assert!(submit_query_requests(app, dispatcher));
    let advanced = dispatcher.submitted.last().expect("advanced").clone();

    assert_eq!(search.revision, 1);
    assert_eq!(search.base_revision, 0);
    assert_eq!(search.base_constraints, QueryConstraints::default());
    assert_eq!(advanced.revision, 2);
    assert_eq!(advanced.base_revision, 0);
    assert_eq!(advanced.base_constraints, QueryConstraints::default());
    assert_eq!(
        advanced.constraints.text.as_ref().expect("text").literal,
        "request"
    );
    assert_eq!(
        advanced.constraints.advanced_polars.as_deref(),
        Some("level == 'INFO'")
    );
    (search.revision, advanced.revision)
}

fn submit_overlapping_constraints_advanced_first(
    app: &mut App,
    provider: &FixtureProvider,
    dispatcher: &mut MembershipDispatcher,
) -> (u64, u64) {
    app.handle(Action::Open(Open::Advanced), provider);
    app.handle(
        Action::Raw(RawEvent::Paste("level == 'INFO'".into())),
        provider,
    );
    app.handle(raw_key(KeyCode::Enter), provider);
    assert!(submit_query_requests(app, dispatcher));
    let advanced = dispatcher.submitted.last().expect("advanced").clone();

    app.handle(raw_key(KeyCode::Esc), provider);
    app.handle(Action::Open(Open::Search), provider);
    app.handle(Action::Raw(RawEvent::Paste("request".into())), provider);
    app.handle(raw_key(KeyCode::Enter), provider);
    assert!(submit_query_requests(app, dispatcher));
    let search = dispatcher.submitted.last().expect("search").clone();

    assert_eq!(advanced.revision, 1);
    assert_eq!(search.revision, 2);
    assert_eq!(
        search.constraints.text.as_ref().expect("text").literal,
        "request"
    );
    assert_eq!(
        search.constraints.advanced_polars.as_deref(),
        Some("level == 'INFO'")
    );
    (search.revision, advanced.revision)
}

#[test]
fn overlapping_composite_constraints_are_consistent_in_all_orders() {
    for search_submitted_first in [false, true] {
        for advanced_finishes_first in [false, true] {
            let (provider, mut app) = demo();
            let mut dispatcher = MembershipDispatcher::default();
            let (search_revision, advanced_revision) = if search_submitted_first {
                submit_overlapping_constraints(&mut app, &provider, &mut dispatcher)
            } else {
                submit_overlapping_constraints_advanced_first(&mut app, &provider, &mut dispatcher)
            };
            let latest_revision = search_revision.max(advanced_revision);
            let order = if advanced_finishes_first {
                [advanced_revision, search_revision]
            } else {
                [search_revision, advanced_revision]
            };

            dispatcher.finish(order[0], Ok(()));
            poll_query_completions(&mut app, &mut dispatcher);
            if order[0] != latest_revision {
                assert!(!dispatcher.published.contains_key("all"));
                assert!(app.search_state().expect("search").applied.is_empty());
            }
            dispatcher.finish(order[1], Ok(()));
            poll_query_completions(&mut app, &mut dispatcher);

            let membership = dispatcher.published.get("all").expect("membership");
            assert_eq!(membership.revision, latest_revision);
            assert_eq!(app.view_state().expect("state").applied_query_revision, 2);
            assert_eq!(app.search_state().expect("search").applied, "request");
            assert_eq!(
                app.advanced_state().expect("advanced").applied,
                "level == 'INFO'"
            );
        }
    }
}

#[test]
fn rejected_advanced_rebases_latest_search_without_stale_membership() {
    let (provider, mut app) = demo();
    let mut dispatcher = MembershipDispatcher::default();
    let (stale_search, rejected_advanced) =
        submit_overlapping_constraints(&mut app, &provider, &mut dispatcher);
    app.handle(Action::CancelEditor, &provider);
    app.handle(Action::Open(Open::Search), &provider);
    app.handle(Action::Raw(RawEvent::Paste(" unsaved".into())), &provider);

    dispatcher.finish(stale_search, Ok(()));
    assert!(!poll_query_completions(&mut app, &mut dispatcher));
    assert!(!dispatcher.published.contains_key("all"));
    dispatcher.finish(
        rejected_advanced,
        Err((QueryPurpose::Advanced, "invalid advanced")),
    );
    assert!(poll_query_completions(&mut app, &mut dispatcher));
    assert!(!dispatcher.published.contains_key("all"));
    assert_eq!(
        app.advanced_state().expect("advanced").error.as_deref(),
        Some("invalid advanced")
    );

    assert!(submit_query_requests(&mut app, &mut dispatcher));
    let rebased = dispatcher.submitted.last().expect("rebased search").clone();
    assert_eq!(rebased.revision, 3);
    assert_eq!(rebased.base_revision, 0);
    assert_eq!(
        rebased.constraints.text.as_ref().expect("text").literal,
        "request"
    );
    assert_eq!(app.search_state().expect("search").draft, "request unsaved");
    assert!(rebased.constraints.advanced_polars.is_none());

    dispatcher.finish(rebased.revision, Ok(()));
    assert!(poll_query_completions(&mut app, &mut dispatcher));

    let membership = dispatcher.published.get("all").expect("membership");
    assert_eq!(membership.revision, 3);
    assert_eq!(app.search_state().expect("search").applied, "request");
    assert!(app.advanced_state().expect("advanced").applied.is_empty());
    assert_eq!(
        app.advanced_state().expect("advanced").error.as_deref(),
        Some("invalid advanced")
    );
}

#[test]
fn composite_failure_rebases_both_other_constraints_and_keeps_unfinished_drafts() {
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::Enrichment), &provider);
    enrichment_add(&mut app, &provider);
    app.handle(
        Action::Raw(RawEvent::Paste("code = pl.lit(200)".into())),
        &provider,
    );
    step_submit(&mut app, &provider);
    app.handle(raw_key(KeyCode::Esc), &provider);
    app.handle(Action::Open(Open::Advanced), &provider);
    app.handle(
        Action::Raw(RawEvent::Paste("invalid advanced".into())),
        &provider,
    );
    app.handle(raw_key(KeyCode::Enter), &provider);
    app.handle(raw_key(KeyCode::Esc), &provider);
    app.handle(Action::Open(Open::Search), &provider);
    app.handle(Action::Raw(RawEvent::Paste("request".into())), &provider);
    app.handle(raw_key(KeyCode::Enter), &provider);

    let requests = app.take_query_requests();
    let latest = requests
        .iter()
        .max_by_key(|request| request.revision)
        .unwrap()
        .clone();
    assert_eq!(latest.purpose, QueryPurpose::Search);
    assert_eq!(
        latest.constraints.enrichments[0].source,
        "code = pl.lit(200)"
    );
    assert_eq!(
        latest.constraints.advanced_polars.as_deref(),
        Some("invalid advanced")
    );

    app.handle(
        Action::Raw(RawEvent::Paste(" unfinished".into())),
        &provider,
    );
    app.handle(raw_key(KeyCode::Esc), &provider);
    app.handle(Action::Open(Open::Enrichment), &provider);
    enrichment_add(&mut app, &provider);
    app.handle(
        Action::Raw(RawEvent::Paste(" unfinished".into())),
        &provider,
    );
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: latest.view_id,
        generation: latest.generation,
        revision: latest.revision,
        purpose: latest.purpose,
        result: Err(QueryFailure {
            purpose: QueryPurpose::Advanced,
            message: "advanced rejected".into(),
        }),
    }));

    let rebased = app.take_query_requests().pop().unwrap();
    assert_eq!(rebased.revision, latest.revision + 1);
    assert_eq!(rebased.purpose, QueryPurpose::Enrichment);
    assert_eq!(rebased.constraints.advanced_polars, None);
    assert_eq!(
        rebased.constraints.enrichments[0].source,
        "code = pl.lit(200)"
    );
    assert_eq!(
        rebased
            .constraints
            .text
            .as_ref()
            .map(|text| text.literal.as_str()),
        Some("request")
    );
    assert_eq!(app.search_state().unwrap().draft, "request unfinished");
    assert_eq!(
        app.view_state().unwrap().enrichment.draft,
        "code = pl.lit(200) unfinished"
    );

    assert!(app.apply_query_completion(QueryCompletion {
        view_id: rebased.view_id,
        generation: rebased.generation,
        revision: rebased.revision,
        purpose: rebased.purpose,
        result: Ok(()),
    }));
    assert_eq!(app.search_state().unwrap().applied, "request");
    assert_eq!(
        app.view_state().unwrap().enrichment.applied,
        "code = pl.lit(200)"
    );
    assert!(app.advanced_state().unwrap().applied.is_empty());
    assert_eq!(
        app.advanced_state().unwrap().error.as_deref(),
        Some("advanced rejected")
    );

    for stale in requests
        .into_iter()
        .filter(|request| request.revision < latest.revision)
    {
        assert!(!app.apply_query_completion(QueryCompletion {
            view_id: stale.view_id,
            generation: stale.generation,
            revision: stale.revision,
            purpose: stale.purpose,
            result: Ok(()),
        }));
    }
    assert_eq!(
        app.view_state().unwrap().applied_query_revision,
        rebased.revision
    );
}

#[test]
fn fixture_validates_advanced_inside_search_and_rebases_literal_membership() {
    let (provider, mut app) = demo();
    let mut dispatcher = provider.query_dispatcher();

    app.handle(Action::Open(Open::Advanced), &provider);
    app.handle(
        Action::Raw(RawEvent::Paste("invalid advanced".into())),
        &provider,
    );
    app.handle(raw_key(KeyCode::Enter), &provider);
    assert!(submit_query_requests(&mut app, &mut dispatcher));
    app.handle(raw_key(KeyCode::Esc), &provider);
    app.handle(Action::Open(Open::Search), &provider);
    app.handle(Action::Raw(RawEvent::Paste("request 05".into())), &provider);
    app.handle(raw_key(KeyCode::Enter), &provider);
    assert!(submit_query_requests(&mut app, &mut dispatcher));

    assert!(poll_query_completions(&mut app, &mut dispatcher));
    assert!(app.search_state().expect("search").applied.is_empty());
    assert!(app.advanced_state().expect("advanced").applied.is_empty());
    assert!(
        app.advanced_state()
            .expect("advanced")
            .error
            .as_deref()
            .expect("structured failure")
            .contains("not wired")
    );

    assert!(submit_query_requests(&mut app, &mut dispatcher));
    assert!(poll_query_completions(&mut app, &mut dispatcher));
    app.sync_provider(&provider, 19);
    assert_eq!(app.search_state().expect("search").applied, "request 05");
    assert!(app.advanced_state().expect("advanced").applied.is_empty());
    assert_eq!(app.view_state().expect("state").last_total, 1);
    assert_eq!(app.visible_rows(&provider)[0].id, RowId::new("api", 5));
}

#[test]
fn delayed_dispatcher_does_not_block_actions_and_late_results_are_fenced() {
    let (provider, mut app) = demo();
    app.sync_provider(&provider, 5);
    app.handle(Action::Open(Open::Search), &provider);
    app.handle(Action::Raw(RawEvent::Paste("slow".into())), &provider);
    app.handle(raw_key(KeyCode::Enter), &provider);
    let mut dispatcher = DelayedDispatcher::default();
    assert!(submit_query_requests(&mut app, &mut dispatcher));
    assert_eq!(dispatcher.submitted.len(), 1);
    assert!(!poll_query_completions(&mut app, &mut dispatcher));

    app.handle(raw_key(KeyCode::Esc), &provider);
    app.handle(Action::Resize(55, 9), &provider);
    app.handle(Action::MoveLine(-1), &provider);
    assert_eq!(app.shell.size, (55, 9));
    assert!(!app.should_quit);

    app.handle(Action::Open(Open::Search), &provider);
    app.handle(raw_key(KeyCode::Char('2')), &provider);
    app.handle(raw_key(KeyCode::Enter), &provider);
    let stale = dispatcher.submitted[0].clone();
    submit_query_requests(&mut app, &mut dispatcher);
    dispatcher.ready.push(QueryCompletion {
        view_id: stale.view_id,
        generation: stale.generation,
        revision: stale.revision,
        purpose: QueryPurpose::Search,
        result: Err(QueryFailure {
            purpose: QueryPurpose::Search,
            message: "late error".into(),
        }),
    });
    assert!(!poll_query_completions(&mut app, &mut dispatcher));
    assert!(app.search_state().expect("search").error.is_none());
    app.handle(Action::Quit, &provider);
    assert!(app.should_quit);
}

#[test]
fn pending_submission_queue_has_a_hard_limit() {
    use lvu::RecipeRequest;
    let source = SourceItem {
        id: "source".into(),
        name: "Source".into(),
        health: "ok".into(),
    };
    let views = (0..33)
        .map(|index| ViewItem {
            id: format!("view-{index}"),
            source_id: "source".into(),
            name: format!("View {index}"),
        })
        .collect();
    let provider = EmptyProvider;
    let mut app = App::new(vec![source], views, false);
    app.handle(Action::Open(Open::Search), &provider);
    for index in 0..33 {
        app.handle(raw_key(KeyCode::Enter), &provider);
        if index < 32 {
            app.handle(Action::NextView, &provider);
        }
    }
    assert_eq!(app.take_query_requests().len(), 32);
    assert_eq!(
        app.search_state().expect("search").error.as_deref(),
        Some("query submission queue is full; draft was preserved")
    );
    for _ in 0..32 {
        app.handle(raw_key(KeyCode::Enter), &provider);
        app.handle(Action::NextView, &provider);
    }
    app.handle(
        Action::Open(Open::Recipes {
            mode: RecipeDialogMode::Browse,
        }),
        &provider,
    );
    let RecipeRequest::List { meta } = app.take_recipe_requests().pop().unwrap() else {
        panic!("recipe list")
    };
    app.set_recipes(
        meta,
        vec![lvu::RecipeItem {
            id: "full".into(),
            revision: "one".into(),
            saved_at_unix_nanos: None,
            name: "Queued".into(),
            config: lvu::RecipeConfig {
                pinned_columns: vec!["must-not-apply".into()],
                ..lvu::RecipeConfig::default()
            },
            incompatibility: None,
        }],
        None,
    );
    recipe_apply(&mut app, &provider);
    assert_eq!(app.take_query_requests().len(), 32);
    assert!(app.layers.recipes.state().status.contains("queue is full"));
    assert!(app.view_state().unwrap().pinned_columns.is_empty());
}

#[test]
fn empty_startup_is_actionable_and_navigation_safe() {
    let provider = EmptyProvider;
    let mut app = App::new(vec![], vec![], false);
    app.handle(Action::NextView, &provider);
    app.handle(Action::Top, &provider);
    app.handle(Action::ToggleFollow, &provider);
    let output = render(&provider, &mut app, 80, 18);
    assert!(output.contains("add or discover a source"));
    assert!(output.contains("Add source"));
    assert!(output.contains("Kind"), "{output}");
    assert!(output.contains("Path"), "{output}");
    // §5.2.1: the startup dialog reserves its suggestion rows, so in an
    // eighteen-row frame it covers the viewport behind it. The footer still
    // says what to do, and the dialog is the thing that does it; the viewport's
    // own empty-state line is asserted where the dialog leaves it visible.
    let roomy = render(&provider, &mut app, 140, 40);
    assert!(roomy.contains("No view selected"), "{roomy}");
    assert_eq!(app.active_view_id(), None);
}

#[test]
fn named_recipe_dialog_saves_accepted_state_and_applies_through_query_request() {
    use lvu::{RecipeConfig, RecipeItem, RecipeRequest};
    let (mut provider, mut app) = demo();
    let target = app.active_view_id().unwrap().to_owned();
    let old = PersistentViewState {
        applied_search: "old".into(),
        applied_advanced: "pl.col('raw').is_not_null()".into(),
        applied_enrichment: "old_field = pl.lit('ok')".into(),
        pinned_columns: vec!["old_field".into()],
        color_field: Some("old_field".into()),
        ..PersistentViewState::default()
    };
    let fence = app.view_interaction_revision(&target).unwrap();
    assert!(app.restore_persistent_view_if_unmodified(&target, fence, old.clone()));
    let restoration = app.take_query_requests().pop().unwrap();
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: target.clone(),
        generation: restoration.generation,
        revision: restoration.revision,
        purpose: restoration.purpose,
        result: Ok(()),
    }));
    app.handle(
        Action::Open(Open::Recipes {
            mode: RecipeDialogMode::Browse,
        }),
        &provider,
    );
    assert!(matches!(
        app.take_recipe_requests().as_slice(),
        [RecipeRequest::List { .. }]
    ));
    app.handle(raw_alt(KeyCode::Char('s')), &provider);
    app.handle(raw_char('E'), &provider);
    recipe_apply(&mut app, &provider);
    assert!(
        matches!(app.take_recipe_requests().as_slice(), [RecipeRequest::Save { name, view_id, config, .. }] if name == "E" && view_id == &target && config.search == "old" && config.enrichment == "old_field = pl.lit('ok')")
    );
    app.handle(raw_alt(KeyCode::Char('i')), &provider);
    app.handle(raw_key(KeyCode::Backspace), &provider);
    for character in "/tmp/recipe.toml".chars() {
        app.handle(raw_char(character), &provider);
    }
    recipe_apply(&mut app, &provider);
    assert!(matches!(
        app.take_recipe_requests().as_slice(),
        [RecipeRequest::Import { path, .. }] if path == "/tmp/recipe.toml"
    ));

    // Delivering a list against the request the layer actually has
    // outstanding: the import it just issued.
    app.handle(raw_alt(KeyCode::Char('b')), &provider);
    let response = recipe_request(&mut app, &provider);
    app.set_recipes(
        response,
        vec![lvu::RecipeItem {
            id: "r".into(),
            revision: "rev".into(),
            saved_at_unix_nanos: None,
            name: "Errors".into(),
            incompatibility: None,
            config: lvu::RecipeConfig {
                search: "error".into(),
                advanced: "pl.col('status') == 500".into(),
                enrichment: "status = pl.col('missing').strict_cast(pl.Int64)".into(),
                enrichments: vec![],
                pinned_columns: vec!["level".into()],
                color_field: Some("request_id".into()),
                capture_time: Some(lvu::CaptureTimeRange {
                    start_unix_nanos: 10,
                    end_unix_nanos: 20,
                }),
                capture_time_policy: Some(lvu::CaptureTimePolicy::Absolute(
                    lvu::CaptureTimeRange {
                        start_unix_nanos: 10,
                        end_unix_nanos: 20,
                    },
                )),
                time_basis: lvu::TimeBasis::Event,
                grouping: String::new(),
            },
        }],
        None,
    );
    app.handle(raw_alt(KeyCode::Char('b')), &provider);
    recipe_apply(&mut app, &provider);
    let request = app
        .take_query_requests()
        .pop()
        .expect("native query request");
    assert_eq!(request.view_id, target);
    assert_eq!(request.constraints.text.as_ref().unwrap().literal, "error");
    assert_eq!(request.constraints.time_basis, lvu::TimeBasis::Event);
    assert_eq!(
        request.constraints.capture_time.unwrap().start_unix_nanos,
        10
    );
    assert_eq!(app.search_state().unwrap().applied, "old");
    assert_eq!(app.view_state().unwrap().pinned_columns, vec!["old_field"]);
    app.apply_query_completion(QueryCompletion {
        view_id: request.view_id,
        generation: request.generation,
        revision: request.revision,
        purpose: request.purpose,
        result: Err(QueryFailure {
            purpose: QueryPurpose::Enrichment,
            message: "missing column".into(),
        }),
    });
    assert_eq!(app.search_state().unwrap().applied, "old");
    assert_eq!(
        app.advanced_state().unwrap().applied,
        "pl.col('raw').is_not_null()"
    );
    assert_eq!(
        app.view_state().unwrap().enrichment.applied,
        "old_field = pl.lit('ok')"
    );
    assert!(
        app.view_state()
            .unwrap()
            .enrichment
            .draft
            .contains("missing")
    );
    assert_eq!(app.view_state().unwrap().pinned_columns, vec!["old_field"]);
    let rollback = app.take_query_requests().pop().expect("atomic rollback");
    assert_eq!(rollback.constraints.text.unwrap().literal, "old");
    assert_eq!(
        rollback.constraints.advanced_polars.as_deref(),
        Some("pl.col('raw').is_not_null()")
    );
    assert_eq!(
        rollback.constraints.enrichments[0].source,
        "old_field = pl.lit('ok')"
    );
    assert_eq!(rollback.constraints.capture_time, None);
    assert_eq!(rollback.constraints.time_basis, lvu::TimeBasis::Capture);
    assert!(provider.advance());
    app.sync_provider(&provider, 8);
    assert_eq!(app.search_state().unwrap().applied, "old");
    assert_eq!(
        app.view_state().unwrap().enrichment.applied,
        "old_field = pl.lit('ok')"
    );

    app.handle(
        Action::Open(Open::Recipes {
            mode: RecipeDialogMode::Browse,
        }),
        &provider,
    );
    let response = recipe_request(&mut app, &provider);
    app.set_recipes(
        response,
        vec![RecipeItem {
            id: "unsupported".into(),
            revision: "rev2".into(),
            saved_at_unix_nanos: None,
            name: "Command recipe".into(),
            config: RecipeConfig::default(),
            incompatibility: Some("command enrichment recipes are not supported".into()),
        }],
        None,
    );
    recipe_apply(&mut app, &provider);
    assert!(app.layers.recipes.state().status.contains("not supported"));
    assert!(app.take_query_requests().is_empty());
}

#[test]
fn recipe_results_are_fenced_from_reopened_or_edited_dialogs() {
    use lvu::{RecipeDialogMode, RecipeRequest, RecipeRequestMeta};
    let (provider, mut app) = demo();
    app.handle(
        Action::Open(Open::Recipes {
            mode: RecipeDialogMode::Browse,
        }),
        &provider,
    );
    let RecipeRequest::List { meta: stale } = app.take_recipe_requests().pop().unwrap() else {
        panic!("list request")
    };
    app.handle(Action::CancelEditor, &provider);
    app.handle(
        Action::Open(Open::Recipes {
            mode: RecipeDialogMode::Browse,
        }),
        &provider,
    );
    let RecipeRequest::List { meta: current } = app.take_recipe_requests().pop().unwrap() else {
        panic!("second list request")
    };
    assert_ne!(stale.dialog_id, current.dialog_id);
    app.set_recipes(stale, Vec::new(), Some("stale".into()));
    assert!(app.layers.recipes.state().loading);

    app.handle(raw_alt(KeyCode::Char('s')), &provider);
    app.handle(raw_char('N'), &provider);
    app.set_recipes(current, Vec::new(), None);
    let dialog = app.layers.recipes.state();
    assert_eq!(dialog.mode, RecipeDialogMode::Save);
    assert_eq!(dialog.name, "N");
    assert!(
        !dialog.loading,
        "switching to save retires the old list request"
    );
    assert!(dialog.pending_request_id.is_none());

    app.handle(Action::CancelEditor, &provider);
    app.recipe_failed(
        RecipeRequestMeta {
            request_id: current.request_id,
            ..current
        },
        "durable write failed".into(),
    );
    assert!(
        app.source_notice
            .as_deref()
            .unwrap()
            .contains("durable write failed")
    );
}

#[test]
fn similar_recipe_can_be_rejected_or_opened_for_typed_adaptation() {
    use lvu::{RecipeConfig, RecipeItem, RecipeRequest};
    let (provider, mut app) = demo();
    app.handle(
        Action::Open(Open::Recipes {
            mode: RecipeDialogMode::Browse,
        }),
        &provider,
    );
    let RecipeRequest::List { meta } = app.take_recipe_requests().pop().unwrap() else {
        panic!("list request")
    };
    let item = RecipeItem {
        id: "00000000-0000-0000-0000-000000000041".into(),
        revision: "00000000-0000-0000-0000-000000000042".into(),
        saved_at_unix_nanos: None,
        name: "Errors".into(),
        config: RecipeConfig {
            advanced: "pl.col('level') == 'ERROR'".into(),
            ..Default::default()
        },
        incompatibility: None,
    };
    let suggestion = lvu::app::RecipeSuggestion {
        recipe_id: item.id.clone(),
        evidence: vec!["same project".into(), "2 sampled field names".into()],
        missing_fields: vec!["service".into()],
    };
    app.set_recipes_with_suggestions(meta, vec![item.clone()], vec![suggestion.clone()], None);
    app.handle(raw_char('x'), &provider);
    let RecipeRequest::Outcome(outcome) = app.take_recipe_requests().pop().unwrap() else {
        panic!("outcome request")
    };
    assert!(!outcome.accepted);
    assert!(app.layers.recipes.state().suggestions.is_empty());

    // The fence belongs to the layer now, so the test asks for a fresh list the
    // way the user does rather than forging a request id.
    app.handle(raw_alt(KeyCode::Char('g')), &provider);
    let meta = recipe_request(&mut app, &provider);
    app.set_recipes_with_suggestions(meta, vec![item], vec![suggestion], None);
    app.handle(raw_alt(KeyCode::Char('a')), &provider);
    let dialog = app.layers.ask.state().expect("adaptation dialog");
    assert_eq!(dialog.kind, AskAiKind::Recipe);
    assert!(dialog.prompt.contains("same project"));
    assert_eq!(
        dialog.recipe.as_ref().unwrap().advanced,
        "pl.col('level') == 'ERROR'"
    );
}

#[test]
fn suggested_recipe_records_acceptance_only_after_atomic_query_success() {
    use lvu::{QueryCompletion, RecipeConfig, RecipeItem, RecipeRequest};
    let (provider, mut app) = demo();
    app.handle(
        Action::Open(Open::Recipes {
            mode: RecipeDialogMode::Browse,
        }),
        &provider,
    );
    let RecipeRequest::List { meta } = app.take_recipe_requests().pop().unwrap() else {
        panic!()
    };
    let id = "00000000-0000-0000-0000-000000000051".to_owned();
    let revision = "00000000-0000-0000-0000-000000000052".to_owned();
    app.set_recipes_with_suggestions(
        meta,
        vec![RecipeItem {
            id: id.clone(),
            revision: revision.clone(),
            saved_at_unix_nanos: None,
            name: "Errors".into(),
            config: RecipeConfig {
                search: "error".into(),
                ..Default::default()
            },
            incompatibility: None,
        }],
        vec![lvu::app::RecipeSuggestion {
            recipe_id: id,
            evidence: vec!["same command".into()],
            missing_fields: vec![],
        }],
        None,
    );
    recipe_apply(&mut app, &provider);
    assert!(app.take_recipe_requests().is_empty());
    let request = app.take_query_requests().pop().unwrap();
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: request.view_id,
        generation: request.generation,
        revision: request.revision,
        purpose: request.purpose,
        result: Ok(()),
    }));
    let RecipeRequest::Outcome(outcome) = app.take_recipe_requests().pop().unwrap() else {
        panic!()
    };
    assert!(outcome.accepted);
    assert_eq!(outcome.revision, revision);
}

#[test]
fn failed_or_stale_suggested_recipe_never_records_acceptance() {
    use lvu::{
        QueryCompletion, QueryFailure, QueryPurpose, RecipeConfig, RecipeItem, RecipeRequest,
    };
    let (provider, mut app) = demo();
    let install = |app: &mut App| {
        app.handle(
            Action::Open(Open::Recipes {
                mode: RecipeDialogMode::Browse,
            }),
            &provider,
        );
        let RecipeRequest::List { meta } = app.take_recipe_requests().pop().unwrap() else {
            panic!()
        };
        let id = "00000000-0000-0000-0000-000000000061".to_owned();
        app.set_recipes_with_suggestions(
            meta,
            vec![RecipeItem {
                id: id.clone(),
                revision: "00000000-0000-0000-0000-000000000062".into(),
                saved_at_unix_nanos: None,
                name: "Suggested".into(),
                config: RecipeConfig {
                    advanced: "pl.col('missing')".into(),
                    ..Default::default()
                },
                incompatibility: None,
            }],
            vec![lvu::app::RecipeSuggestion {
                recipe_id: id,
                evidence: vec!["same project".into()],
                missing_fields: vec![],
            }],
            None,
        );
        recipe_apply(app, &provider);
        app.take_query_requests().pop().unwrap()
    };
    let failed = install(&mut app);
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: failed.view_id,
        generation: failed.generation,
        revision: failed.revision,
        purpose: failed.purpose,
        result: Err(QueryFailure {
            purpose: QueryPurpose::Advanced,
            message: "missing field".into()
        }),
    }));
    assert!(app.take_recipe_requests().is_empty());

    let stale = install(&mut app);
    app.handle(Action::Open(Open::Search), &provider);
    app.handle(raw_key(KeyCode::Char('x')), &provider);
    app.handle(raw_key(KeyCode::Enter), &provider);
    assert!(!app.apply_query_completion(QueryCompletion {
        view_id: stale.view_id,
        generation: stale.generation,
        revision: stale.revision,
        purpose: stale.purpose,
        result: Ok(()),
    }));
    assert!(app.take_recipe_requests().is_empty());
}

#[test]
fn recipe_success_does_not_overwrite_newer_user_presentation_edits() {
    use lvu::{RecipeConfig, RecipeItem, RecipeRequest};
    let (provider, mut app) = demo();
    app.sync_provider(&provider, 8);
    app.handle(
        Action::Open(Open::Recipes {
            mode: RecipeDialogMode::Browse,
        }),
        &provider,
    );
    let RecipeRequest::List { meta } = app.take_recipe_requests().pop().unwrap() else {
        panic!("list request")
    };
    app.set_recipes(
        meta,
        vec![RecipeItem {
            id: "presentation".into(),
            revision: "one".into(),
            saved_at_unix_nanos: None,
            name: "Presentation".into(),
            config: RecipeConfig {
                pinned_columns: vec!["recipe_field".into()],
                color_field: Some("recipe_field".into()),
                ..lvu::RecipeConfig::default()
            },
            incompatibility: None,
        }],
        None,
    );
    recipe_apply(&mut app, &provider);
    let request = app.take_query_requests().pop().unwrap();
    app.handle(Action::Open(Open::Fields), &provider);
    app.handle(raw_key(KeyCode::Char(' ')), &provider);
    let user_pins = app.view_state().unwrap().pinned_columns.clone();
    assert!(!user_pins.is_empty());
    app.apply_query_completion(QueryCompletion {
        view_id: request.view_id,
        generation: request.generation,
        revision: request.revision,
        purpose: request.purpose,
        result: Ok(()),
    });
    assert_eq!(app.view_state().unwrap().pinned_columns, user_pins);
    assert_ne!(
        app.view_state().unwrap().color_field.as_deref(),
        Some("recipe_field")
    );
}

#[test]
fn capture_time_dialog_validates_half_open_utc_and_uses_selected_capture_time() {
    let (provider, mut app) = demo();
    app.sync_provider(&provider, 8);
    app.handle(Action::Open(Open::Time), &provider);
    app.handle(
        Action::Raw(RawEvent::Paste("1970-01-01T12:00:01Z".into())),
        &provider,
    );
    app.layers.time.switch_field();
    app.handle(
        Action::Raw(RawEvent::Paste("1970-01-01T12:00:03Z".into())),
        &provider,
    );
    time_activate(&mut app, &provider, TimeControl::Apply);
    let request = app.take_query_requests().pop().unwrap();
    assert_eq!(
        request.constraints.capture_time,
        Some(lvu::CaptureTimeRange {
            start_unix_nanos: lvu::fixture::fixture_capture_nanos(1),
            end_unix_nanos: lvu::fixture::fixture_capture_nanos(3),
        })
    );
    let mut dispatcher = provider.query_dispatcher();
    dispatcher.submit(request.clone()).unwrap();
    app.apply_query_completion(dispatcher.poll().unwrap());
    assert_eq!(
        provider
            .page(&request.view_id, ViewportRequest { start: 0, len: 8 })
            .total,
        2
    );
    app.handle(Action::Open(Open::Time), &provider);
    for _ in 0..32 {
        app.handle(raw_key(KeyCode::Backspace), &provider);
    }
    app.handle(
        Action::Raw(RawEvent::Paste("2026-02-30T00:00:00Z".into())),
        &provider,
    );
    time_activate(&mut app, &provider, TimeControl::Apply);
    assert!(app.layers.time.is_open());
    assert!(app.view_state().unwrap().time_error.is_some());
    assert_eq!(
        app.view_state()
            .unwrap()
            .applied_capture_time
            .unwrap()
            .start_unix_nanos,
        lvu::fixture::fixture_capture_nanos(1)
    );
    app.handle(raw_alt(KeyCode::Char('a')), &provider);
    time_activate(&mut app, &provider, TimeControl::Apply);
    assert!(
        app.take_query_requests()
            .pop()
            .unwrap()
            .constraints
            .capture_time
            .is_some()
    );
}

#[test]
fn unapplied_recent_choice_never_refreshes_or_submits() {
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::Time), &provider);
    time_focus(&mut app, &provider, TimeControl::Window);
    app.handle(raw_key(KeyCode::Enter), &provider);
    for _ in 0..2 {
        app.handle(raw_key(KeyCode::Down), &provider);
    }
    app.handle(raw_key(KeyCode::Enter), &provider);
    assert!(app.take_query_requests().is_empty());
    assert!(!app.refresh_rolling_capture_times(900_000_000_000, Instant::now()));
    assert!(app.take_query_requests().is_empty());
    assert!(
        app.persistent_view_state(app.active_view_id().unwrap())
            .unwrap()
            .applied_capture_time_policy
            .is_none()
    );
}

#[test]
fn time_segment_paste_preserves_other_segments_and_rejects_overflow_whole() {
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::Time), &provider);
    app.handle(
        Action::Raw(RawEvent::Paste(
            "2026-09-06T12:34:56.123456789+05:45".into(),
        )),
        &provider,
    );
    time_focus(&mut app, &provider, TimeControl::StartClock);
    for _ in 0..32 {
        app.handle(raw_key(KeyCode::Backspace), &provider);
    }
    app.handle(
        Action::Raw(RawEvent::Paste("01:02:03.987654321".into())),
        &provider,
    );
    let dialog = app.layers.time.state();
    assert_eq!(dialog.start_date, "2026-09-06");
    assert_eq!(dialog.start_clock, "01:02:03.987654321");
    assert_eq!(dialog.start_zone, "+05:45");
    let accepted_draft = app.view_state().unwrap().time_start_draft.clone();
    app.handle(Action::Raw(RawEvent::Paste("9".repeat(65))), &provider);
    assert_eq!(app.view_state().unwrap().time_start_draft, accepted_draft);
    assert!(app.view_state().unwrap().time_error.is_some());
    assert!(app.take_query_requests().is_empty());
}

#[test]
fn untouched_time_reopen_refreshes_visible_segments_with_opening_selection() {
    let (mut provider, mut app) = demo();
    app.sync_provider(&provider, 4);
    app.handle(Action::Open(Open::Time), &provider);
    let first = app.view_state().unwrap().time_start_draft.clone();
    app.handle(Action::CancelEditor, &provider);
    assert!(provider.advance());
    app.sync_provider(&provider, 4);
    app.handle(Action::Open(Open::Time), &provider);
    let current = &app.view_state().unwrap().time_start_draft;
    assert_ne!(current, &first);
    let (date, clock, zone) = lvu::app::split_time_draft(current);
    let dialog = app.layers.time.state();
    assert_eq!(
        (&dialog.start_date, &dialog.start_clock, &dialog.start_zone),
        (&date, &clock, &zone)
    );
    assert!(app.take_query_requests().is_empty());
}

#[test]
fn basis_and_window_only_drafts_keep_seeded_segments_on_reopen() {
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::Time), &provider);
    let seeded = app.view_state().unwrap().time_start_draft.clone();
    app.handle(raw_alt(KeyCode::Char('e')), &provider);
    app.handle(Action::CancelEditor, &provider);
    app.handle(Action::Open(Open::Time), &provider);
    assert_eq!(app.view_state().unwrap().time_start_draft, seeded);
    assert!(!app.layers.time.state().start_date.is_empty());
    time_focus(&mut app, &provider, TimeControl::Window);
    app.handle(raw_key(KeyCode::Enter), &provider);
    for _ in 0..2 {
        app.handle(raw_key(KeyCode::Down), &provider);
    }
    app.handle(raw_key(KeyCode::Enter), &provider);
    app.handle(Action::CancelEditor, &provider);
    app.handle(Action::Open(Open::Time), &provider);
    assert_eq!(app.view_state().unwrap().time_start_draft, seeded);
    assert_eq!(
        app.layers.time.state().window,
        lvu::app::TimeWindowChoice::Recent(300)
    );
}

#[test]
fn legacy_combined_and_explicit_empty_structured_drafts_restore_losslessly() {
    let (provider, mut app) = demo();
    let view = app.active_view_id().unwrap().to_owned();
    let legacy = PersistentViewState {
        time_start_draft: "2026-09-05T12:30:45.123456789+05:30".into(),
        time_end_draft: "2026-09-05T12:31:45.987654321+05:30".into(),
        time_draft_touched: true,
        time_window_draft: lvu::app::TimeWindowChoice::Recent(777),
        ..PersistentViewState::default()
    };
    assert!(app.restore_persistent_view(&view, legacy));
    app.handle(Action::Open(Open::Time), &provider);
    let dialog = app.layers.time.state();
    assert_eq!(dialog.start_clock, "12:30:45.123456789");
    assert_eq!(dialog.start_zone, "+05:30");
    assert!(
        dialog
            .window_choices
            .contains(&lvu::app::TimeWindowChoice::Recent(777))
    );

    let mut empty = app.persistent_view_state(&view).unwrap();
    empty.time_start_draft = "T".into();
    empty.time_end_draft = "T".into();
    empty.time_start_date_draft.clear();
    empty.time_start_clock_draft.clear();
    empty.time_start_zone_draft.clear();
    empty.time_end_date_draft.clear();
    empty.time_end_clock_draft.clear();
    empty.time_end_zone_draft.clear();
    empty.time_structured_draft_present = true;
    assert!(app.restore_persistent_view(&view, empty));
    app.handle(Action::Open(Open::Time), &provider);
    let dialog = app.layers.time.state();
    assert!(
        dialog.start_date.is_empty()
            && dialog.start_clock.is_empty()
            && dialog.start_zone.is_empty()
    );
    assert!(
        dialog.end_date.is_empty() && dialog.end_clock.is_empty() && dialog.end_zone.is_empty()
    );
}

#[test]
fn narrow_window_dropdown_keeps_last_choice_clickable() {
    let (provider, mut app) = demo();
    app.sync_provider(&provider, 8);
    app.handle(Action::Open(Open::Time), &provider);
    time_focus(&mut app, &provider, TimeControl::Window);
    app.handle(raw_key(KeyCode::Enter), &provider);
    let last = app.layers.time.state().window_choices.len() - 1;
    for _ in 0..last {
        app.handle(raw_key(KeyCode::Down), &provider);
    }
    let rendered = render(&provider, &mut app, 46, 12);
    assert!(rendered.contains("around selected"), "{rendered}");
    let area = app
        .layers
        .time
        .choice_rects()
        .iter()
        .find(|(_, index)| *index == last)
        .unwrap()
        .0;
    app.handle(
        Action::Raw(RawEvent::Mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            area.x,
            area.y,
        ))),
        &provider,
    );
    assert!(matches!(
        app.layers.time.state().window,
        lvu::app::TimeWindowChoice::AroundSelected(_)
    ));
}

#[test]
fn rolling_capture_time_expires_idle_rows_without_changing_definition_revision() {
    let (mut provider, mut app) = demo();
    let view_id = app.active_view_id().unwrap().to_owned();
    let mut dispatcher = provider.query_dispatcher();
    let elapsed = Instant::now();
    // The fixture captures its rows at noon on the epoch day, so the clock this
    // test drives has to be on the same day for a rolling window to cover them.
    let noon = lvu::fixture::fixture_capture_nanos(0);
    assert!(!app.refresh_rolling_capture_times(noon + 20_000_000_000, elapsed));
    app.handle(Action::Open(Open::Search), &provider);
    app.handle(Action::Raw(RawEvent::Paste("fixture".into())), &provider);
    app.handle(raw_key(KeyCode::Enter), &provider);
    let search = app.take_query_requests().pop().unwrap();
    dispatcher.submit(search).unwrap();
    assert!(app.apply_query_completion(dispatcher.poll().unwrap()));

    app.handle(Action::Open(Open::Time), &provider);
    app.handle(raw_alt(KeyCode::Char('5')), &provider);
    let recent = app.take_query_requests().pop().unwrap();
    assert_eq!(recent.constraints.text.as_ref().unwrap().literal, "fixture");
    assert_eq!(
        recent.constraints.capture_time,
        Some(lvu::CaptureTimeRange {
            start_unix_nanos: noon - 280_000_000_000,
            end_unix_nanos: noon + 20_000_000_000,
        })
    );
    dispatcher.submit(recent).unwrap();
    assert!(app.apply_query_completion(dispatcher.poll().unwrap()));
    assert_eq!(
        app.view_state().unwrap().applied_capture_time_policy,
        Some(lvu::CaptureTimePolicy::Recent { seconds: 300 })
    );
    let definition_revision = app.view_definition_revision(&view_id).unwrap();

    assert!(provider.advance());
    let arrivals = provider.page(&view_id, ViewportRequest { start: 0, len: 32 });
    assert_eq!(
        arrivals.total, 16,
        "matching arrivals continue through the independent literal constraint"
    );
    assert!(
        app.refresh_rolling_capture_times(noon + 320_000_000_000, elapsed + Duration::from_secs(1))
    );
    let refresh = app.take_query_requests().pop().unwrap();
    assert_eq!(
        refresh.constraints.text.as_ref().unwrap().literal,
        "fixture"
    );
    assert_eq!(
        refresh.constraints.capture_time,
        Some(lvu::CaptureTimeRange {
            start_unix_nanos: noon + 20_000_000_000,
            end_unix_nanos: noon + 320_000_000_000,
        })
    );
    dispatcher.submit(refresh).unwrap();
    assert!(app.apply_query_completion(dispatcher.poll().unwrap()));
    assert_eq!(
        provider
            .page(&view_id, ViewportRequest { start: 0, len: 32 })
            .total,
        0,
        "rows expire even without a source arrival"
    );
    assert_eq!(
        app.view_definition_revision(&view_id),
        Some(definition_revision)
    );
    assert!(!app.refresh_rolling_capture_times(
        noon + 320_500_000_000,
        elapsed + Duration::from_millis(1500)
    ));
    app.handle(Action::NextView, &provider);
    assert_eq!(app.view_state().unwrap().applied_capture_time_policy, None);
    app.handle(Action::PreviousView, &provider);
    assert_eq!(
        app.view_state().unwrap().applied_capture_time_policy,
        Some(lvu::CaptureTimePolicy::Recent { seconds: 300 })
    );
}

#[test]
fn rolling_recipe_resolves_at_apply_and_persists_policy_not_old_bounds() {
    let (provider, mut app) = demo();
    app.refresh_rolling_capture_times(1_000_000_000_000, Instant::now());
    app.handle(
        Action::Open(Open::Recipes {
            mode: RecipeDialogMode::Browse,
        }),
        &provider,
    );
    let lvu::RecipeRequest::List { meta } = app.take_recipe_requests().pop().unwrap() else {
        panic!("recipe list")
    };
    app.set_recipes(
        meta,
        vec![lvu::RecipeItem {
            id: "recent".into(),
            revision: "one".into(),
            saved_at_unix_nanos: None,
            name: "Recent errors".into(),
            incompatibility: None,
            config: lvu::RecipeConfig {
                search: "fixture".into(),
                capture_time_policy: Some(lvu::CaptureTimePolicy::Recent { seconds: 300 }),
                ..lvu::RecipeConfig::default()
            },
        }],
        None,
    );
    recipe_apply(&mut app, &provider);
    let request = app.take_query_requests().pop().unwrap();
    assert_eq!(
        request.constraints.capture_time,
        Some(lvu::CaptureTimeRange {
            start_unix_nanos: 700_000_000_000,
            end_unix_nanos: 1_000_000_000_000,
        })
    );
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: request.view_id.clone(),
        generation: request.generation,
        revision: request.revision,
        purpose: request.purpose,
        result: Ok(()),
    }));
    let saved = app.persistent_view_state(&request.view_id).unwrap();
    assert_eq!(
        saved.applied_capture_time_policy,
        Some(lvu::CaptureTimePolicy::Recent { seconds: 300 })
    );
    assert_eq!(saved.time_recent_draft, "5m");
}

#[test]
fn rolling_ticks_wait_for_recipe_transactions_and_handle_backward_wall_clock() {
    let (provider, mut app) = demo();
    let elapsed = Instant::now();
    app.refresh_rolling_capture_times(1_000_000_000_000, elapsed);
    app.handle(
        Action::Open(Open::Recipes {
            mode: RecipeDialogMode::Browse,
        }),
        &provider,
    );
    let lvu::RecipeRequest::List { meta } = app.take_recipe_requests().pop().unwrap() else {
        panic!("recipe list")
    };
    app.set_recipes(
        meta,
        vec![lvu::RecipeItem {
            id: "recent".into(),
            revision: "one".into(),
            saved_at_unix_nanos: None,
            name: "Recent".into(),
            incompatibility: None,
            config: lvu::RecipeConfig {
                pinned_columns: vec!["service".into()],
                capture_time_policy: Some(lvu::CaptureTimePolicy::Recent { seconds: 300 }),
                ..lvu::RecipeConfig::default()
            },
        }],
        None,
    );
    recipe_apply(&mut app, &provider);
    let recipe = app.take_query_requests().pop().unwrap();
    for second in 1..=3 {
        assert!(!app.refresh_rolling_capture_times(
            (1_000 + second) * 1_000_000_000,
            elapsed + Duration::from_secs(second as u64),
        ));
        assert!(app.take_query_requests().is_empty());
    }
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: recipe.view_id.clone(),
        generation: recipe.generation,
        revision: recipe.revision,
        purpose: recipe.purpose,
        result: Ok(()),
    }));
    assert_eq!(app.view_state().unwrap().pinned_columns, ["service"]);
    assert_eq!(
        app.view_state().unwrap().applied_capture_time_policy,
        Some(lvu::CaptureTimePolicy::Recent { seconds: 300 })
    );

    assert!(app.refresh_rolling_capture_times(
        1_003_000_000_000,
        elapsed + Duration::from_millis(3100),
    ));
    let catch_up = app.take_query_requests().pop().unwrap();
    assert_eq!(
        catch_up.constraints.capture_time.unwrap().end_unix_nanos,
        1_003_000_000_000
    );
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: catch_up.view_id,
        generation: catch_up.generation,
        revision: catch_up.revision,
        purpose: catch_up.purpose,
        result: Ok(()),
    }));

    assert!(
        app.refresh_rolling_capture_times(900_000_000_000, elapsed + Duration::from_millis(3200),)
    );
    let backward = app.take_query_requests().pop().unwrap();
    assert_eq!(
        backward.constraints.capture_time.unwrap().end_unix_nanos,
        900_000_000_000
    );
}

#[test]
fn rolling_ticks_coalesce_behind_a_slow_inflight_scan_then_publish_latest_clock() {
    let (provider, mut app) = demo();
    let elapsed = Instant::now();
    app.refresh_rolling_capture_times(20_000_000_000, elapsed);
    app.handle(Action::Open(Open::Time), &provider);
    app.handle(raw_alt(KeyCode::Char('5')), &provider);
    let initial = app.take_query_requests().pop().unwrap();
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: initial.view_id,
        generation: initial.generation,
        revision: initial.revision,
        purpose: initial.purpose,
        result: Ok(()),
    }));

    assert!(app.refresh_rolling_capture_times(21_000_000_000, elapsed + Duration::from_secs(1),));
    let slow_scan = app.take_query_requests().pop().unwrap();
    for second in 2..=5 {
        assert!(!app.refresh_rolling_capture_times(
            (20 + second) * 1_000_000_000,
            elapsed + Duration::from_secs(second as u64),
        ));
        assert!(app.take_query_requests().is_empty());
    }
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: slow_scan.view_id,
        generation: slow_scan.generation,
        revision: slow_scan.revision,
        purpose: slow_scan.purpose,
        result: Ok(()),
    }));
    assert!(
        app.refresh_rolling_capture_times(25_000_000_000, elapsed + Duration::from_millis(5100),)
    );
    let catch_up = app.take_query_requests().pop().unwrap();
    assert_eq!(
        catch_up.constraints.capture_time.unwrap().end_unix_nanos,
        25_000_000_000
    );
}

#[test]
fn failed_rolling_recipe_is_atomic_despite_intervening_clock_ticks() {
    let (provider, mut app) = demo();
    let elapsed = Instant::now();
    app.refresh_rolling_capture_times(1_000_000_000_000, elapsed);
    app.handle(
        Action::Open(Open::Recipes {
            mode: RecipeDialogMode::Browse,
        }),
        &provider,
    );
    let lvu::RecipeRequest::List { meta } = app.take_recipe_requests().pop().unwrap() else {
        panic!("recipe list")
    };
    app.set_recipes(
        meta,
        vec![lvu::RecipeItem {
            id: "invalid".into(),
            revision: "one".into(),
            saved_at_unix_nanos: None,
            name: "Invalid recent".into(),
            incompatibility: None,
            config: lvu::RecipeConfig {
                search: "new search".into(),
                advanced: "invalid advanced".into(),
                pinned_columns: vec!["level".into()],
                capture_time_policy: Some(lvu::CaptureTimePolicy::Recent { seconds: 60 }),
                ..lvu::RecipeConfig::default()
            },
        }],
        None,
    );
    recipe_apply(&mut app, &provider);
    let recipe = app.take_query_requests().pop().unwrap();
    for second in 1..=3 {
        assert!(!app.refresh_rolling_capture_times(
            (1_000 + second) * 1_000_000_000,
            elapsed + Duration::from_secs(second as u64),
        ));
    }
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: recipe.view_id,
        generation: recipe.generation,
        revision: recipe.revision,
        purpose: recipe.purpose,
        result: Err(QueryFailure {
            purpose: QueryPurpose::Advanced,
            message: "invalid advanced".into(),
        }),
    }));
    assert!(app.view_state().unwrap().pinned_columns.is_empty());
    assert_eq!(app.view_state().unwrap().applied_capture_time_policy, None);
    assert!(app.search_state().unwrap().applied.is_empty());
    assert_eq!(app.advanced_state().unwrap().draft, "invalid advanced");
    let reaffirm = app.take_query_requests().pop().unwrap();
    assert!(reaffirm.constraints.capture_time.is_none());
    assert!(reaffirm.constraints.text.is_none());
    assert!(reaffirm.constraints.advanced_polars.is_none());
}

#[test]
fn capture_time_rejects_malformed_unicode_and_preserves_last_good_window() {
    let (provider, mut app) = demo();
    let good = lvu::CaptureTimeRange {
        start_unix_nanos: lvu::fixture::fixture_capture_nanos(1),
        end_unix_nanos: lvu::fixture::fixture_capture_nanos(3),
    };
    app.handle(Action::Open(Open::Time), &provider);
    app.handle(
        Action::Raw(RawEvent::Paste("1970-01-01T12:00:01Z".into())),
        &provider,
    );
    app.layers.time.switch_field();
    app.handle(
        Action::Raw(RawEvent::Paste("1970-01-01T12:00:03Z".into())),
        &provider,
    );
    time_activate(&mut app, &provider, TimeControl::Apply);
    let accepted = app.take_query_requests().pop().unwrap();
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: accepted.view_id,
        generation: accepted.generation,
        revision: accepted.revision,
        purpose: accepted.purpose,
        result: Ok(()),
    }));

    for invalid in [
        "000🙂000000000000Z",
        "2026-01-01T+1:00:00Z",
        "2026-01-01T00:00:00.Z",
        "2026-01-01T00:00:-1Z",
        "0000-01-01T00:00:00Z",
        "2026-13-01T00:00:00Z",
        "9999-12-31T23:59:59Z",
    ] {
        app.handle(Action::Open(Open::Time), &provider);
        time_focus(&mut app, &provider, TimeControl::StartDate);
        for _ in 0..64 {
            app.handle(raw_key(KeyCode::Backspace), &provider);
        }
        app.handle(Action::Raw(RawEvent::Paste(invalid.into())), &provider);
        time_activate(&mut app, &provider, TimeControl::Apply);
        assert!(app.view_state().unwrap().time_error.is_some(), "{invalid}");
        assert_eq!(app.view_state().unwrap().applied_capture_time, Some(good));
        assert!(app.take_query_requests().is_empty());
    }
}

#[test]
fn time_drafts_fence_restore_and_inflight_ai_and_around_uses_opening_selection() {
    let (mut provider, mut app) = demo();
    app.sync_provider(&provider, 4);
    let view_id = app.active_view_id().unwrap().to_owned();
    let restore_fence = app.view_interaction_revision(&view_id).unwrap();
    app.handle(Action::Open(Open::Ask(AskOpen::Generic)), &provider);
    app.handle(
        Action::Raw(RawEvent::Paste("suggest a filter".into())),
        &provider,
    );
    ask_submit(&mut app, &provider);
    let AskAiRequest::Start {
        generation,
        definition_revision,
        ..
    } = app.take_ask_ai_requests().pop().unwrap()
    else {
        panic!("AI start")
    };

    app.handle(Action::Open(Open::Time), &provider);
    time_focus(&mut app, &provider, TimeControl::StartDate);
    let anchored = app.view_state().unwrap().selected.clone().unwrap();
    let anchored_time = provider
        .row_by_id(&view_id, &anchored)
        .unwrap()
        .captured_at_unix_nanos
        .unwrap();
    app.handle(raw_char('2'), &provider);
    assert!(!app.restore_persistent_view_if_unmodified(
        &view_id,
        restore_fence,
        PersistentViewState {
            time_start_draft: "stale restore".into(),
            ..PersistentViewState::default()
        },
    ));
    assert!(!app.finish_ask_ai(
        generation,
        &view_id,
        definition_revision,
        Ok(("pl.lit(True)".into(), "stale".into())),
    ));

    assert!(provider.advance());
    app.sync_provider(&provider, 4);
    assert_ne!(app.view_state().unwrap().selected.as_ref(), Some(&anchored));
    app.handle(raw_alt(KeyCode::Char('a')), &provider);
    time_activate(&mut app, &provider, TimeControl::Apply);
    let request = app.take_query_requests().pop().unwrap();
    let window = request.constraints.capture_time.unwrap();
    assert_eq!(window.start_unix_nanos, anchored_time - 30_000_000_000);
    assert_eq!(window.end_unix_nanos, anchored_time + 30_000_000_000);
}

#[test]
fn event_time_basis_is_explicit_transactional_and_persistent() {
    let (provider, mut app) = demo();
    let view_id = app.active_view_id().unwrap().to_owned();
    app.handle(Action::Open(Open::Time), &provider);
    app.handle(raw_alt(KeyCode::Char('e')), &provider);
    app.handle(
        Action::Raw(RawEvent::Paste("2026-09-05T12:30:45Z".into())),
        &provider,
    );
    app.layers.time.switch_field();
    app.handle(
        Action::Raw(RawEvent::Paste("2026-09-05T12:30:46Z".into())),
        &provider,
    );
    time_activate(&mut app, &provider, TimeControl::Apply);
    let request = app.take_query_requests().pop().unwrap();
    assert_eq!(request.constraints.time_basis, lvu::TimeBasis::Event);
    assert_eq!(
        app.view_state().unwrap().applied_time_basis,
        lvu::TimeBasis::Capture,
        "basis changes only after native membership publishes"
    );
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: request.view_id,
        generation: request.generation,
        revision: request.revision,
        purpose: request.purpose,
        result: Ok(()),
    }));
    assert_eq!(
        app.view_state().unwrap().applied_time_basis,
        lvu::TimeBasis::Event
    );
    assert_eq!(
        app.persistent_view_state(&view_id)
            .unwrap()
            .applied_time_basis,
        lvu::TimeBasis::Event
    );

    app.sync_provider(&provider, 8);
    let selected = app.view_state().unwrap().selected.clone().unwrap();
    let event_center = provider
        .row_by_id(&view_id, &selected)
        .unwrap()
        .details
        .into_iter()
        .find(|(key, _)| key == "event_time_utc_nanos")
        .unwrap()
        .1
        .parse::<i64>()
        .unwrap();
    app.handle(Action::Open(Open::Time), &provider);
    app.handle(raw_alt(KeyCode::Char('a')), &provider);
    time_activate(&mut app, &provider, TimeControl::Apply);
    let around = app.take_query_requests().pop().unwrap();
    let window = around.constraints.capture_time.unwrap();
    assert_eq!(around.constraints.time_basis, lvu::TimeBasis::Event);
    assert_eq!(window.start_unix_nanos, event_center - 30_000_000_000);
    assert_eq!(window.end_unix_nanos, event_center + 30_000_000_000);
}

#[test]
fn pending_advanced_and_time_are_one_composite_in_both_submission_orders() {
    for time_first in [false, true] {
        let (provider, mut app) = demo();
        let submit_advanced = |app: &mut App| {
            app.handle(Action::Open(Open::Advanced), &provider);
            app.handle(
                Action::Raw(RawEvent::Paste("pl.lit(True)".into())),
                &provider,
            );
            app.handle(raw_key(KeyCode::Enter), &provider);
        };
        let submit_time = |app: &mut App| {
            app.handle(Action::Open(Open::Time), &provider);
            app.handle(
                Action::Raw(RawEvent::Paste("1970-01-01T12:00:01Z".into())),
                &provider,
            );
            app.layers.time.switch_field();
            app.handle(
                Action::Raw(RawEvent::Paste("1970-01-01T12:00:03Z".into())),
                &provider,
            );
            time_activate(app, &provider, TimeControl::Apply);
        };
        if time_first {
            submit_time(&mut app);
            submit_advanced(&mut app);
        } else {
            submit_advanced(&mut app);
            submit_time(&mut app);
        }
        let request = app.take_query_requests().pop().unwrap();
        assert_eq!(
            request.constraints.advanced_polars.as_deref(),
            Some("pl.lit(True)")
        );
        assert!(request.constraints.capture_time.is_some());
        assert!(app.apply_query_completion(QueryCompletion {
            view_id: request.view_id,
            generation: request.generation,
            revision: request.revision,
            purpose: request.purpose,
            result: Ok(()),
        }));
        assert_eq!(app.advanced_state().unwrap().applied, "pl.lit(True)");
        assert!(app.view_state().unwrap().applied_capture_time.is_some());
    }
}

#[test]
fn stale_time_or_advanced_completion_cannot_publish_an_older_composite() {
    for (time_first, latest_first) in [(true, false), (true, true), (false, false), (false, true)] {
        let (provider, mut app) = demo();
        let submit_advanced = |app: &mut App| {
            app.handle(Action::Open(Open::Advanced), &provider);
            app.handle(
                Action::Raw(RawEvent::Paste("pl.lit(True)".into())),
                &provider,
            );
            app.handle(raw_key(KeyCode::Enter), &provider);
        };
        let submit_time = |app: &mut App| {
            app.handle(Action::Open(Open::Time), &provider);
            app.handle(
                Action::Raw(RawEvent::Paste("1970-01-01T12:00:01Z".into())),
                &provider,
            );
            app.layers.time.switch_field();
            app.handle(
                Action::Raw(RawEvent::Paste("1970-01-01T12:00:03Z".into())),
                &provider,
            );
            time_activate(app, &provider, TimeControl::Apply);
        };
        if time_first {
            submit_time(&mut app);
        } else {
            submit_advanced(&mut app);
        }
        let older = app.take_query_requests().pop().unwrap();
        if time_first {
            submit_advanced(&mut app);
        } else {
            submit_time(&mut app);
        }
        let latest = app.take_query_requests().pop().unwrap();
        let completion = |request: &lvu::QueryRequest| QueryCompletion {
            view_id: request.view_id.clone(),
            generation: request.generation,
            revision: request.revision,
            purpose: request.purpose,
            result: Ok(()),
        };
        if latest_first {
            assert!(app.apply_query_completion(completion(&latest)));
            assert!(!app.apply_query_completion(completion(&older)));
        } else {
            assert!(!app.apply_query_completion(completion(&older)));
            assert!(app.apply_query_completion(completion(&latest)));
        }
        assert_eq!(app.advanced_state().unwrap().applied, "pl.lit(True)");
        assert!(app.view_state().unwrap().applied_capture_time.is_some());
    }
}

#[test]
fn rejected_pending_advanced_rebases_the_valid_pending_time() {
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::Time), &provider);
    app.handle(
        Action::Raw(RawEvent::Paste("1970-01-01T12:00:01Z".into())),
        &provider,
    );
    app.layers.time.switch_field();
    app.handle(
        Action::Raw(RawEvent::Paste("1970-01-01T12:00:03Z".into())),
        &provider,
    );
    time_activate(&mut app, &provider, TimeControl::Apply);
    app.handle(Action::Open(Open::Advanced), &provider);
    app.handle(
        Action::Raw(RawEvent::Paste("invalid advanced".into())),
        &provider,
    );
    app.handle(raw_key(KeyCode::Enter), &provider);
    let failed = app.take_query_requests().pop().unwrap();
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: failed.view_id,
        generation: failed.generation,
        revision: failed.revision,
        purpose: failed.purpose,
        result: Err(lvu::QueryFailure {
            purpose: QueryPurpose::Advanced,
            message: "invalid advanced".into(),
        }),
    }));
    assert_eq!(app.advanced_state().unwrap().draft, "invalid advanced");
    assert!(app.advanced_state().unwrap().error.is_some());
    let rebased = app.take_query_requests().pop().expect("time rebase");
    assert!(rebased.constraints.advanced_polars.is_none());
    assert!(rebased.constraints.capture_time.is_some());
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: rebased.view_id,
        generation: rebased.generation,
        revision: rebased.revision,
        purpose: rebased.purpose,
        result: Ok(()),
    }));
    assert!(app.view_state().unwrap().applied_capture_time.is_some());
    assert!(app.advanced_state().unwrap().applied.is_empty());
    assert_eq!(app.advanced_state().unwrap().draft, "invalid advanced");
}

#[test]
fn named_view_dialog_emits_blank_clone_and_rename_requests() {
    let (provider, mut app) = demo();
    let selected = app.active_view_id().unwrap().to_owned();
    app.handle(Action::Open(Open::View), &provider);
    assert_eq!(app.focus, Focus::Layer);
    // dialog-system.md §11 retires ALL-CAPS mode banners: the active mode is
    // shown by the selected header segment instead (§8.6); Apply is the only
    // button.
    let opened = render(&provider, &mut app, 90, 24);
    assert!(opened.contains("Clone"), "{opened}");
    assert!(!opened.contains("[ Clone ]"), "{opened}");
    assert!(opened.contains("[ Apply ]"), "{opened}");
    assert_eq!(app.layers.view.mode(), lvu::ViewDialogMode::Clone);
    app.handle(raw_alt(KeyCode::Char('b')), &provider);
    for _ in 0.."New view".len() {
        app.handle(raw_key(KeyCode::Backspace), &provider);
    }
    for character in "Errors".chars() {
        app.handle(raw_key(KeyCode::Char(character)), &provider);
    }
    app.handle(raw_key(KeyCode::Enter), &provider);
    let blank = app.layers.view.outbox.take().pop().unwrap();
    assert_eq!(blank.mode, lvu::ViewDialogMode::Blank);
    assert_eq!(blank.view_id, selected);
    assert_eq!(blank.name, "Errors");

    app.handle(raw_alt(KeyCode::Char('r')), &provider);
    app.handle(raw_key(KeyCode::Enter), &provider);
    assert_eq!(
        app.layers.view.outbox.take().pop().unwrap().mode,
        lvu::ViewDialogMode::Rename
    );
}

#[test]
fn stale_restore_is_fenced_per_named_view() {
    let provider = EmptyProvider;
    let source = SourceItem {
        id: "source".into(),
        name: "Source".into(),
        health: "raw".into(),
    };
    let views = vec![
        ViewItem {
            id: "first".into(),
            source_id: "source".into(),
            name: "First".into(),
        },
        ViewItem {
            id: "second".into(),
            source_id: "source".into(),
            name: "Second".into(),
        },
    ];
    let mut app = App::new(vec![source], views, false);
    let first_fence = app.view_interaction_revision("first").unwrap();
    let second_fence = app.view_interaction_revision("second").unwrap();
    app.select_view("second");
    app.handle(Action::Open(Open::Search), &provider);
    app.handle(Action::Raw(RawEvent::Paste("new draft".into())), &provider);
    assert!(app.restore_persistent_view_if_unmodified(
        "first",
        first_fence,
        PersistentViewState {
            view_name: "Restored first".into(),
            search_draft: "first draft".into(),
            ..PersistentViewState::default()
        },
    ));
    assert!(!app.restore_persistent_view_if_unmodified(
        "second",
        second_fence,
        PersistentViewState {
            view_name: "Stale second".into(),
            search_draft: "stale".into(),
            ..PersistentViewState::default()
        },
    ));
    assert_eq!(app.search_state().unwrap().draft, "new draft");
    assert_eq!(app.views()[0].name, "Restored first");
    assert_eq!(app.views()[1].name, "Second");
}

#[test]
fn user_rename_fences_whole_restore_and_rejects_sibling_name() {
    let provider = EmptyProvider;
    let source = SourceItem {
        id: "source".into(),
        name: "Source".into(),
        health: "raw".into(),
    };
    let views = vec![
        ViewItem {
            id: "first".into(),
            source_id: "source".into(),
            name: "First".into(),
        },
        ViewItem {
            id: "second".into(),
            source_id: "source".into(),
            name: "Second".into(),
        },
    ];
    let mut app = App::new(vec![source], views, false);
    let fence = app.view_interaction_revision("first").unwrap();
    assert!(app.rename_view("first", "User name".into()));
    app.select_view("first");
    app.handle(Action::Open(Open::Search), &provider);
    app.handle(
        Action::Raw(RawEvent::Paste("user filter".into())),
        &provider,
    );
    app.handle(raw_key(KeyCode::Enter), &provider);
    let request = app.take_query_requests().pop().expect("search request");
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: "first".into(),
        generation: request.generation,
        revision: request.revision,
        purpose: request.purpose,
        result: Ok(()),
    }));

    assert!(!app.restore_persistent_view_if_unmodified(
        "first",
        fence,
        PersistentViewState {
            view_name: "Saved name".into(),
            search_draft: "saved filter".into(),
            ..PersistentViewState::default()
        },
    ));
    assert_eq!(app.views()[0].name, "User name");
    assert_eq!(app.search_state().unwrap().draft, "user filter");
    assert_eq!(app.search_state().unwrap().applied, "user filter");
    assert!(!app.rename_view("first", "Second".into()));
    assert_eq!(app.views()[0].name, "User name");
}

#[test]
fn ask_ai_proposal_is_fenced_and_applies_through_native_editor_request() {
    let (mut provider, mut app) = demo();
    let view_id = app.active_view_id().unwrap().to_owned();
    app.handle(Action::Open(Open::Ask(AskOpen::Generic)), &provider);
    let dialog = render(&provider, &mut app, 120, 28);
    assert!(dialog.contains("Ask 🧠"));
    assert!(dialog.contains("codex/gpt-5.6-sol"));
    app.handle(
        Action::Raw(RawEvent::Paste("only errors".into())),
        &provider,
    );
    ask_submit(&mut app, &provider);
    let request = app.take_ask_ai_requests().pop().expect("AI start");
    let AskAiRequest::Start {
        generation,
        definition_revision,
        kind,
        ..
    } = request
    else {
        panic!("start request")
    };
    assert_eq!(kind, AskAiKind::Filter);
    assert!(app.update_ask_ai_progress(
        generation,
        AskAiStage::Proposing,
        "working".into(),
        Some("session-fixture".into()),
        Some("/tmp/snapshot-fixture".into()),
    ));
    assert!(
        provider.advance(),
        "ordinary arrivals continue during the session"
    );
    app.sync_provider(&provider, 8);
    app.handle(Action::MoveLine(-1), &provider);
    assert!(app.finish_ask_ai(
        generation,
        &view_id,
        definition_revision,
        Ok((
            "pl.col('level') == 'ERROR'".into(),
            "keeps error records".into(),
        )),
    ));
    ask_submit(&mut app, &provider);
    let query = app
        .take_query_requests()
        .pop()
        .expect("native query request");
    assert_eq!(query.purpose, QueryPurpose::Advanced);
    assert_eq!(
        query.constraints.advanced_polars.as_deref(),
        Some("pl.col('level') == 'ERROR'")
    );
    // The proposal is applied by opening Advanced on the draft it wrote, so
    // Ask is off the stack and the editor is what the user is left looking at.
    assert_eq!(app.focus, Focus::Layer);
    assert_eq!(app.layers.stack_ids(), vec![LayerId::Filter]);
    assert_eq!(app.layers.filter.purpose(), QueryPurpose::Advanced);
}

#[test]
fn ask_form_has_bounded_controls_multiline_cursor_dropdown_and_real_overflow() {
    use lvu::app::AskControl;
    let (provider, mut app) = demo();
    let view_id = app.active_view_id().unwrap().to_owned();
    app.handle(Action::Open(Open::Ask(AskOpen::Generic)), &provider);
    assert_eq!(app.layers.ask.state().unwrap().focus, AskControl::Prompt);
    // Text focus is the layer's, derived from state rather than from the last
    // frame, so it is already right before anything has been painted.
    assert!(app.layers.ask.text_focus());
    render(&provider, &mut app, 120, 28);
    assert!(app.layers.ask.text_focus());
    app.handle(raw_key(KeyCode::BackTab), &provider);
    app.handle(raw_key(KeyCode::Enter), &provider);
    render(&provider, &mut app, 120, 28);
    assert!(
        !app.layers.ask.text_focus(),
        "open dropdown suppresses the prompt caret"
    );
    app.handle(raw_key(KeyCode::Esc), &provider);
    // Escape closed the list only; Tab returns to the Request field, and the
    // caret is back.
    app.handle(raw_key(KeyCode::Tab), &provider);
    render(&provider, &mut app, 120, 28);
    assert!(app.layers.ask.text_focus());
    for code in "first 界"
        .chars()
        .map(KeyCode::Char)
        .chain([KeyCode::Enter])
        .chain(
            "second e\u{301}rrors REQUEST-TAIL"
                .chars()
                .map(KeyCode::Char),
        )
    {
        // The layer owns its keymap; the shell hands it the key untranslated.
        app.handle(raw_key(code), &provider);
    }
    assert_eq!(
        app.layers.ask.state().unwrap().prompt,
        "first 界\nsecond e\u{301}rrors REQUEST-TAIL"
    );
    let backend = TestBackend::new(72, 20);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| ui::render(frame, &mut app, &provider))
        .unwrap();
    let rendered = screen(terminal.backend().buffer());
    // §12.17: a Kind field with a chevron, the Request field, one message row
    // and the actions last — no `[ Kind: … ]` button above the fields.
    assert!(rendered.contains("Kind"), "{rendered}");
    assert!(rendered.contains("Filter"), "{rendered}");
    assert!(rendered.contains("[ Submit ]"), "{rendered}");
    assert!(rendered.contains("Ready"), "{rendered}");
    let request_row = rendered
        .lines()
        .position(|line| line.contains("Request"))
        .expect("a Request row");
    let submit_row = rendered
        .lines()
        .position(|line| line.contains("[ Submit ]"))
        .expect("an action row");
    assert!(
        submit_row > request_row,
        "the action row belongs after the fields it acts on: {rendered}"
    );
    // Geometry is the component's now (§5.1): the caret it drew resolves back
    // to the Request field through its own `hit`.
    let caret = terminal.backend().cursor_position();
    assert_eq!(
        app.layers.ask.surface().caret,
        Some((caret.x, caret.y)),
        "the layer reports the caret it drew"
    );
    assert_eq!(
        app.layers.ask.hit((caret.x, caret.y)),
        Some(lvu::components::ask::AskHit::Control(AskControl::Prompt)),
    );
    assert_eq!(
        terminal.backend().buffer()[caret].bg,
        app.appearance.theme_id.theme().cursor
    );

    app.handle(raw_key(KeyCode::BackTab), &provider);
    assert_eq!(app.layers.ask.state().unwrap().focus, AskControl::Kind);
    render(&provider, &mut app, 72, 20);
    let kind = layer_rect(&app, AskControl::Kind);
    app.handle(
        Action::Raw(RawEvent::Mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            kind.0,
            kind.1,
        ))),
        &provider,
    );
    let dropdown = render(&provider, &mut app, 72, 20);
    assert!(dropdown.contains("Filter"), "{dropdown}");
    assert!(dropdown.contains("Enrichment"), "{dropdown}");
    // A click on the dialog behind the open list is absorbed by the list: the
    // dropdown is the innermost surface and nothing else acts.
    let submit = layer_rect(&app, AskControl::Submit);
    app.handle(
        Action::Raw(RawEvent::Mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            submit.0,
            submit.1,
        ))),
        &provider,
    );
    assert!(app.layers.ask.state().unwrap().kind_dropdown);
    assert!(app.take_ask_ai_requests().is_empty());
    app.handle(raw_key(KeyCode::Down), &provider);
    app.handle(raw_key(KeyCode::Esc), &provider);
    assert_eq!(app.focus, Focus::Layer);
    assert_eq!(app.layers.ask.state().unwrap().kind, AskAiKind::Filter);
    assert!(!app.layers.ask.state().unwrap().kind_dropdown);

    app.handle(raw_key(KeyCode::Enter), &provider);
    app.handle(raw_key(KeyCode::Down), &provider);
    app.handle(raw_key(KeyCode::Enter), &provider);
    assert_eq!(app.layers.ask.state().unwrap().kind, AskAiKind::Enrichment);
    app.handle(raw_key(KeyCode::Tab), &provider);
    app.handle(raw_key(KeyCode::Tab), &provider);
    assert_eq!(app.layers.ask.state().unwrap().focus, AskControl::Submit);
    app.handle(raw_key(KeyCode::Enter), &provider);
    let AskAiRequest::Start {
        generation,
        definition_revision,
        ..
    } = app.take_ask_ai_requests().pop().unwrap()
    else {
        panic!("Ask start")
    };
    assert!(app.finish_ask_ai(
        generation,
        &view_id,
        definition_revision,
        Ok((
            "field = pl.col('message')".into(),
            "static explanation ".repeat(80),
        )),
    ));
    assert_eq!(app.layers.ask.state().unwrap().focus, AskControl::Apply);
    let narrow = render(&provider, &mut app, 54, 14);
    assert!(narrow.contains("Proposal"), "{narrow}");
    assert!(narrow.contains("[ Apply ]"), "{narrow}");
    // §11.10: overflow is a scrollbar on the body, never a `[ More ]` button.
    assert!(!narrow.contains("[ More ]"), "{narrow}");
    assert!(app.layers.ask.state().unwrap().review_scroll_limit > 0);
    // The body's wheel hitbox is the component's; a point outside the layer is
    // dropped by the shell before the component sees it (§5.2).
    let details = layer_body(&app);
    app.handle(
        Action::Raw(RawEvent::Mouse(mouse(
            MouseEventKind::ScrollDown,
            app.layers.ask.surface().popup.x.saturating_sub(1),
            details.1,
        ))),
        &provider,
    );
    assert_eq!(app.layers.ask.state().unwrap().review_scroll, 0);
    app.handle(
        Action::Raw(RawEvent::Mouse(mouse(
            MouseEventKind::ScrollDown,
            details.0,
            details.1,
        ))),
        &provider,
    );
    assert!(app.layers.ask.state().unwrap().review_scroll > 0);
    let mut request_tail_seen = false;
    for _ in 0..app.layers.ask.state().unwrap().review_scroll_limit {
        let screen = render(&provider, &mut app, 54, 14);
        request_tail_seen |= screen.contains("REQUEST-TAIL");
        app.handle(raw_key(KeyCode::Down), &provider);
    }
    assert!(
        request_tail_seen,
        "full submitted request must be inspectable"
    );
    app.handle(raw_key(KeyCode::Tab), &provider);
    // A short explanation is what stops the panes overflowing; the dialog only
    // learns it the way it ever does, from a completion.
    let (generation, view, revision) = {
        let dialog = app.layers.ask.state().unwrap();
        (
            dialog.generation,
            dialog.view_id.clone(),
            dialog.definition_revision,
        )
    };
    assert!(app.finish_ask_ai(
        generation,
        &view,
        revision,
        Ok(("field = pl.col('message')".into(), "short".into())),
    ));
    let backend = TestBackend::new(160, 40);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| ui::render(frame, &mut app, &provider))
        .unwrap();
    let wide = screen(terminal.backend().buffer());
    assert!(!wide.contains("[ More ]"), "{wide}");
    assert_eq!(app.layers.ask.state().unwrap().focus, AskControl::Apply);
    assert!(!ask_offers(&app, AskControl::More));
    let (apply_y, apply_x) = wide
        .lines()
        .enumerate()
        .find_map(|(y, line)| line.find("[ Apply ]").map(|x| (y, x)))
        .unwrap();
    assert_eq!(
        terminal.backend().buffer()[(apply_x as u16, apply_y as u16)].bg,
        app.appearance.theme_id.theme().selection_bg,
        "normalized Apply focus is painted in the same frame"
    );

    let (_, mut error_app) = demo();
    error_app.handle(Action::Open(Open::Ask(AskOpen::Generic)), &provider);
    error_app.handle(raw_key(KeyCode::Tab), &provider);
    error_app.handle(raw_key(KeyCode::Enter), &provider);
    let backend = TestBackend::new(72, 20);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| ui::render(frame, &mut error_app, &provider))
        .unwrap();
    let error_screen = screen(terminal.backend().buffer());
    let (error_y, error_x) = error_screen
        .lines()
        .enumerate()
        .find_map(|(y, line)| line.find("Error").map(|x| (y, x)))
        .expect("explicit Error state");
    assert_eq!(
        terminal.backend().buffer()[(error_x as u16, error_y as u16)].fg,
        error_app.appearance.theme_id.theme().severity.error
    );
}

#[test]
fn unsubmitted_editor_draft_invalidates_an_inflight_ai_proposal() {
    let (provider, mut app) = demo();
    let view_id = app.active_view_id().unwrap().to_owned();
    app.handle(Action::Open(Open::Ask(AskOpen::Generic)), &provider);
    app.handle(
        Action::Raw(RawEvent::Paste("suggest a filter".into())),
        &provider,
    );
    ask_submit(&mut app, &provider);
    let AskAiRequest::Start {
        generation,
        definition_revision,
        ..
    } = app.take_ask_ai_requests().pop().expect("AI start")
    else {
        panic!("start request")
    };

    // A newer, unfinished draft is part of the user's view definition even
    // though it has not advanced the native query adapter revision yet.
    app.handle(Action::Open(Open::Advanced), &provider);
    app.handle(
        Action::Raw(RawEvent::Paste("pl.col('message').is_not_null()".into())),
        &provider,
    );
    assert!(!app.finish_ask_ai(
        generation,
        &view_id,
        definition_revision,
        Ok(("pl.lit(True)".into(), "stale proposal".into())),
    ));
    assert_eq!(
        app.advanced_state().expect("advanced editor").draft,
        "pl.col('message').is_not_null()"
    );
}

#[test]
fn investigation_starts_follows_up_and_explicitly_resumes_saved_session() {
    let (provider, mut app) = demo();
    open_investigation(&mut app, &provider);
    assert_eq!(app.focus, Focus::Layer);
    assert_eq!(app.layers.stack_ids(), vec![LayerId::Investigation]);
    assert!(render(&provider, &mut app, 120, 30).contains("Investigation"));
    app.handle(
        Action::Raw(RawEvent::Paste("explain the failures".into())),
        &provider,
    );
    investigation_submit(&mut app, &provider);
    let InvestigationRequest::Start {
        generation,
        view_id,
        definition_revision,
        question,
        ..
    } = app.take_investigation_requests().pop().expect("start")
    else {
        panic!("start request")
    };
    assert_eq!(question, "explain the failures");
    let item = InvestigationItem {
        id: "investigation-1".into(),
        view_id,
        session_id: "session-1".into(),
        snapshot_dir: "/tmp/investigation-1".into(),
        manifest_path: "/tmp/investigation-1/manifest.json".into(),
        question,
    };
    assert!(app.investigation_ready(generation, item.clone()));
    assert!(app.push_investigation_event(
        "session-1",
        "Agent: two failures share request_id".into(),
        Ok(()),
    ));
    for index in 0..80 {
        assert!(app.push_investigation_event(
            "session-1",
            format!("event {index} {}", "x".repeat(20_000)),
            Ok(()),
        ));
    }
    let dialog = app.layers.investigation.state().unwrap();
    assert_eq!(dialog.messages.len(), 64);
    assert!(
        dialog
            .messages
            .iter()
            .all(|message| message.len() <= 16_387)
    );
    assert_eq!(dialog.stage, InvestigationStage::Conversation);
    app.handle(
        Action::Raw(RawEvent::Paste("show the first one".into())),
        &provider,
    );
    investigation_submit(&mut app, &provider);
    assert!(matches!(
        app.take_investigation_requests().as_slice(),
        [InvestigationRequest::Send { session_id, prompt, .. }]
            if session_id == "session-1" && prompt == "show the first one"
    ));

    // The transcript has focus, so `q` is a dismissal rather than a character,
    // and a turn in flight is cancelled on the way out.
    app.handle(raw_key(KeyCode::Char('q')), &provider);
    assert!(app.layers.stack_ids().is_empty());
    assert!(matches!(
        app.take_investigation_requests().as_slice(),
        [InvestigationRequest::Cancel { generation: value }] if *value == generation
    ));
    app.set_investigations(vec![item]);
    open_investigation(&mut app, &provider);
    // Reopening with saved investigations lands on the list, which is how the
    // user picks one.
    assert!(app.layers.investigation.state().unwrap().saved_mode);
    investigation_submit(&mut app, &provider);
    assert!(matches!(
        app.take_investigation_requests().as_slice(),
        [InvestigationRequest::Resume { item, .. }] if item.session_id == "session-1"
    ));
    // Once one is picked, the dialog is a conversation again: the transcript
    // is where a resumed session's replies appear, and leaving the list up
    // left them with nowhere on screen to go.
    assert!(!app.layers.investigation.state().unwrap().saved_mode);
    assert!(
        render(&provider, &mut app, 120, 30).contains("Transcript"),
        "the resumed conversation is what the user is looking at"
    );
    assert_eq!(
        app.view_definition_revision(app.active_view_id().unwrap()),
        Some(definition_revision)
    );
}

#[test]
fn delayed_investigation_load_merges_with_session_created_in_memory() {
    let (provider, mut app) = demo();
    open_investigation(&mut app, &provider);
    app.handle(
        Action::Raw(RawEvent::Paste("new question".into())),
        &provider,
    );
    investigation_submit(&mut app, &provider);
    let InvestigationRequest::Start {
        generation,
        view_id,
        question,
        ..
    } = app.take_investigation_requests().pop().unwrap()
    else {
        panic!("start request")
    };
    let current = InvestigationItem {
        id: "current".into(),
        view_id: view_id.clone(),
        session_id: "current-session".into(),
        snapshot_dir: "/tmp/current".into(),
        manifest_path: "/tmp/current/manifest.json".into(),
        question,
    };
    assert!(app.investigation_ready(generation, current.clone()));
    let loaded = InvestigationItem {
        id: "loaded".into(),
        view_id,
        session_id: "loaded-session".into(),
        snapshot_dir: "/tmp/loaded".into(),
        manifest_path: "/tmp/loaded/manifest.json".into(),
        question: "older question".into(),
    };

    app.set_investigations(vec![loaded]);
    app.handle(Action::CancelEditor, &provider);
    open_investigation(&mut app, &provider);
    let screen = render(&provider, &mut app, 120, 30);
    assert!(screen.contains("new question"));
    assert!(screen.contains("older question"));
}

#[test]
fn cancelled_or_definition_stale_ai_cannot_overwrite_later_edits() {
    let (provider, mut app) = demo();
    let view_id = app.active_view_id().unwrap().to_owned();
    app.handle(Action::Open(Open::Ask(AskOpen::Generic)), &provider);
    app.handle(
        Action::Raw(RawEvent::Paste("derive status".into())),
        &provider,
    );
    app.handle(raw_alt(KeyCode::Char('e')), &provider);
    ask_submit(&mut app, &provider);
    let AskAiRequest::Start {
        generation,
        definition_revision,
        ..
    } = app.take_ask_ai_requests().pop().unwrap()
    else {
        panic!("start request")
    };
    // A layer owns its keymap, so dismissal arrives as a raw key; `q` is a
    // dismissal because the waiting dialog has no text field focused.
    render(&provider, &mut app, 120, 28);
    app.handle(raw_key(KeyCode::Char('q')), &provider);
    assert!(matches!(
        app.take_ask_ai_requests().as_slice(),
        [AskAiRequest::Cancel { generation: value }] if *value == generation
    ));
    app.handle(Action::Open(Open::Enrichment), &provider);
    enrichment_add(&mut app, &provider);
    app.handle(
        Action::Raw(RawEvent::Paste("status = pl.lit('user')".into())),
        &provider,
    );
    assert!(!app.finish_ask_ai(
        generation,
        &view_id,
        definition_revision,
        Ok(("status = pl.lit('agent')".into(), "stale".into())),
    ));
    assert_eq!(
        app.view_state().unwrap().enrichment.draft,
        "status = pl.lit('user')"
    );
}

#[test]
fn ai_proposal_cannot_cross_views_or_a_new_definition_revision() {
    let (provider, mut app) = demo();
    let original = app.active_view_id().unwrap().to_owned();
    app.handle(Action::Open(Open::Ask(AskOpen::Generic)), &provider);
    app.handle(Action::Raw(RawEvent::Paste("errors".into())), &provider);
    ask_submit(&mut app, &provider);
    let AskAiRequest::Start {
        generation,
        definition_revision,
        ..
    } = app.take_ask_ai_requests().pop().unwrap()
    else {
        panic!("start request")
    };
    app.handle(Action::NextView, &provider);
    assert!(app.finish_ask_ai(
        generation,
        &original,
        definition_revision,
        Ok(("pl.lit(True)".into(), "proposal".into())),
    ));
    ask_submit(&mut app, &provider);
    assert!(app.take_query_requests().is_empty());
    assert!(
        app.layers
            .ask
            .state()
            .as_ref()
            .is_some_and(|dialog| dialog.stage == AskAiStage::Error)
    );

    app.handle(Action::CancelEditor, &provider);
    app.handle(Action::PreviousView, &provider);
    app.handle(Action::Open(Open::Ask(AskOpen::Generic)), &provider);
    app.handle(Action::Raw(RawEvent::Paste("fresh".into())), &provider);
    ask_submit(&mut app, &provider);
    let AskAiRequest::Start {
        generation,
        definition_revision,
        ..
    } = app.take_ask_ai_requests().pop().unwrap()
    else {
        panic!("start request")
    };
    assert!(app.restore_persistent_view(
        &original,
        PersistentViewState {
            applied_search: "new definition".into(),
            ..PersistentViewState::default()
        }
    ));
    assert!(!app.finish_ask_ai(
        generation,
        &original,
        definition_revision,
        Ok(("pl.lit(True)".into(), "stale".into())),
    ));
    assert!(app.take_query_requests().iter().all(|request| {
        request
            .constraints
            .text
            .as_ref()
            .map(|text| text.literal.as_str())
            == Some("new definition")
    }));
}

#[test]
fn empty_start_source_dialog_preserves_input_and_emits_typed_requests() {
    let provider = EmptyProvider;
    let mut app = App::new(vec![], vec![], false);
    assert_eq!(app.focus, Focus::Layer);
    app.handle(
        Action::Raw(RawEvent::Paste("./events.log".into())),
        &provider,
    );
    app.handle(raw_alt(KeyCode::Char('c')), &provider);
    assert_eq!(app.layers.source.state().kind, SourceKind::Command);
    assert_eq!(app.layers.source.state().draft, "./events.log");
    app.handle(raw_key(KeyCode::Enter), &provider);
    let request = app.take_source_requests().pop().expect("request");
    assert_eq!(request.kind, SourceKind::Command);
    assert_eq!(request.text, "./events.log");
}

#[test]
fn empty_start_source_ai_requires_review_and_fences_stale_results() {
    use lvu::{SourceAiPreview, SourceAiPreviewItem, SourceAiRequest, SourceAiStage};

    let provider = EmptyProvider;
    let mut app = App::new(Vec::new(), Vec::new(), false);
    app.handle(
        Action::Command(
            LayerId::Source,
            lvu::command_palette::CommandId::AskAiSource,
        ),
        &provider,
    );
    app.handle(
        Action::Raw(RawEvent::Paste("follow backend docker logs".into())),
        &provider,
    );
    app.handle(raw_key(KeyCode::Enter), &provider);
    let SourceAiRequest::Start {
        generation,
        instruction,
        ..
    } = app.take_source_ai_requests().pop().unwrap()
    else {
        panic!("source AI start")
    };
    assert_eq!(instruction, "follow backend docker logs");
    assert_eq!(app.layers.source.state().ai.stage, SourceAiStage::Preparing);
    assert!(!app.finish_source_ai(generation - 1, Err("stale".into())));
    assert!(app.finish_source_ai(
        generation,
        Ok(SourceAiPreview {
            sources: vec![SourceAiPreviewItem {
                name: "backend".into(),
                kind: "command".into(),
                launch: r#"{"executable":"docker","args":["logs","-f","backend"]}"#.into(),
                effective_path_or_cwd: "/project".into(),
                restart: "never".into(),
                environment: (0..16).map(|index| format!("KEY{index}=value")).collect(),
            }],
            explanation: "matched Compose service".into(),
        })
    ));
    let screen = render(&provider, &mut app, 120, 30);
    assert!(screen.contains("preview never executes"));
    assert!(screen.contains("docker"));
    for _ in 0..20 {
        app.handle(raw_key(KeyCode::Down), &provider);
    }
    assert!(render(&provider, &mut app, 120, 30).contains("KEY15=value"));
    assert!(app.take_source_requests().is_empty());
    app.handle(raw_key(KeyCode::Enter), &provider);
    assert!(matches!(
        app.take_source_ai_requests().as_slice(),
        [SourceAiRequest::Apply { generation: value }] if *value == generation
    ));
}

#[test]
fn closing_pending_source_ai_emits_only_its_generation_and_manual_mode_stays_usable() {
    use lvu::SourceAiRequest;

    let provider = EmptyProvider;
    let mut app = App::new(Vec::new(), Vec::new(), false);
    app.handle(
        Action::Raw(RawEvent::Paste("manual path.log".into())),
        &provider,
    );
    app.handle(
        Action::Command(
            LayerId::Source,
            lvu::command_palette::CommandId::AskAiSource,
        ),
        &provider,
    );
    app.handle(
        Action::Raw(RawEvent::Paste("slow source suggestion".into())),
        &provider,
    );
    app.handle(raw_key(KeyCode::Enter), &provider);
    let SourceAiRequest::Start { generation, .. } = app.take_source_ai_requests().pop().unwrap()
    else {
        panic!("start")
    };
    app.handle(
        Action::Command(
            LayerId::Source,
            lvu::command_palette::CommandId::AskAiSource,
        ),
        &provider,
    );
    assert_eq!(app.layers.source.state().draft, "manual path.log");
    app.handle(raw_key(KeyCode::Esc), &provider);
    assert!(matches!(
        app.take_source_ai_requests().as_slice(),
        [SourceAiRequest::Cancel { generation: value }] if *value == generation
    ));
    assert!(!app.finish_source_ai(generation, Err("late completion must be ignored".into())));
}

#[test]
fn file_path_completion_is_generation_fenced_and_modes_have_explicit_keys() {
    let provider = EmptyProvider;
    let mut app = App::new(vec![], vec![], false);
    app.handle(Action::Raw(RawEvent::Paste("logs/app".into())), &provider);

    let first = take_path_completions(&mut app)
        .pop()
        .expect("completion request");
    assert_eq!(first.draft, "logs/app");

    app.handle(raw_char('x'), &provider);
    assert_ne!(
        app.active_path_completion_generation(),
        Some(first.generation)
    );
    assert!(!app.apply_path_completion_result(
        first.generation,
        &first.draft,
        Some("logs/application.log".into()),
        vec!["logs/application.log".into()],
        None,
    ));
    assert_eq!(app.layers.source.state().draft, "logs/appx");

    // The layer owns Alt-C/Alt-F and Tab now, so they are asserted by what
    // they do rather than by the `Action` the retired base table produced.
    app.handle(raw_alt(KeyCode::Char('c')), &provider);
    assert_eq!(app.layers.source.state().kind, SourceKind::Command);
    assert!(take_path_completions(&mut app).is_empty());
    let before = app.layers.source.state().control;
    app.handle(raw_key(KeyCode::Tab), &provider);
    assert_ne!(app.layers.source.state().control, before);
    source_focus(&mut app, &provider, SourceControl::Input);
    app.handle(raw_alt(KeyCode::Char('f')), &provider);
    assert_eq!(app.layers.source.state().kind, SourceKind::File);
    // The draft survives the kind switch; typing schedules a fresh scan.
    app.handle(raw_char('x'), &provider);
    let current = take_path_completions(&mut app)
        .pop()
        .expect("current request");
    assert!(app.apply_path_completion_result(
        current.generation,
        &current.draft,
        None,
        vec!["logs/appx one".into(), "logs/appx ünicode".into()],
        None,
    ));
    let output = render(&provider, &mut app, 90, 22);
    assert!(output.contains("Suggestions"));
    assert!(output.contains("logs/appx ünicode"));
    app.handle(raw_key(KeyCode::Down), &provider);
    // Enter accepts the highlighted candidate; a plain file also launches, and
    // the draft it launches is the one the user selected.
    app.handle(raw_key(KeyCode::Enter), &provider);
    assert_eq!(app.layers.source.state().draft, "logs/appx ünicode");
    assert_eq!(
        app.take_source_requests().pop().expect("launch").text,
        "logs/appx ünicode"
    );

    let mut reopened = App::new(vec![], vec![], false);
    reopened.handle(Action::Raw(RawEvent::Paste("same/path".into())), &provider);

    let old_dialog = take_path_completions(&mut reopened)
        .pop()
        .expect("old dialog request");
    // The in-flight completion is the innermost layer; close it before Source.
    reopened.handle(raw_key(KeyCode::Esc), &provider);
    assert!(reopened.layers.source.is_open());
    reopened.handle(raw_key(KeyCode::Esc), &provider);
    assert!(!reopened.layers.source.is_open());
    reopened.handle(Action::Open(Open::Source), &provider);
    reopened.handle(Action::Raw(RawEvent::Paste("same/path".into())), &provider);

    let new_dialog = take_path_completions(&mut reopened)
        .pop()
        .expect("new dialog request");
    assert!(new_dialog.generation > old_dialog.generation);
    assert!(!reopened.apply_path_completion_result(
        old_dialog.generation,
        &old_dialog.draft,
        Some("same/path.log".into()),
        vec!["same/path.log".into()],
        None,
    ));
    assert_eq!(reopened.layers.source.state().draft, "same/path");

    assert!(reopened.apply_path_completion_result(
        new_dialog.generation,
        &new_dialog.draft,
        Some("same/path/".into()),
        vec!["same/path/".into()],
        None,
    ));
    assert_eq!(
        reopened.layers.source.state().path_completion.candidates,
        vec!["same/path/"]
    );
    reopened.handle(raw_key(KeyCode::Enter), &provider);
    assert_eq!(
        take_path_completions(&mut reopened)
            .pop()
            .expect("directory contents request")
            .draft,
        "same/path/"
    );
}

#[test]
fn typing_after_path_completion_appends_after_the_replacement() {
    for selected in [false, true] {
        let provider = EmptyProvider;
        let mut app = App::new(vec![], vec![], false);
        app.handle(Action::Raw(RawEvent::Paste("nested sp".into())), &provider);

        let request = take_path_completions(&mut app).pop().unwrap();
        let completed = "nested space/".to_owned();
        assert!(app.apply_path_completion_result(
            request.generation,
            &request.draft,
            if selected {
                None
            } else {
                Some(completed.clone())
            },
            vec![completed.clone()],
            None,
        ));
        if selected {
            app.handle(raw_key(KeyCode::Down), &provider);
        }
        app.handle(raw_key(KeyCode::Enter), &provider);
        app.handle(Action::Raw(RawEvent::Paste("über.log".into())), &provider);
        assert_eq!(app.layers.source.state().draft, "nested space/über.log");
    }
}

#[test]
fn automatic_completion_has_no_extra_action_and_command_edits_do_not_request_paths() {
    let provider = EmptyProvider;
    let mut app = App::new(vec![], vec![], false);
    app.handle(Action::Raw(RawEvent::Paste("nested sp".into())), &provider);
    let request = take_path_completions(&mut app).pop().unwrap();
    render(&provider, &mut app, 90, 22);
    assert!(app.layers.source.surface().text_focus);
    app.apply_path_completion_result(
        request.generation,
        &request.draft,
        None,
        vec!["nested space/".into()],
        None,
    );
    assert!(!render(&provider, &mut app, 34, 18).contains("Complete path"));
    app.handle(raw_key(KeyCode::Enter), &provider);
    app.handle(Action::Raw(RawEvent::Paste("über.log".into())), &provider);
    assert_eq!(app.layers.source.state().draft, "nested space/über.log");

    app.handle(raw_alt(KeyCode::Char('c')), &provider);
    take_path_completions(&mut app);
    app.handle(Action::Raw(RawEvent::Paste(" --follow".into())), &provider);
    assert!(take_path_completions(&mut app).is_empty());
}

#[test]
fn narrow_source_controls_keep_each_workflow_action_visible_and_live() {
    let provider = EmptyProvider;
    let mut app = App::new(vec![], vec![], false);
    let cases = [
        (SourceDialogMode::Manual, SourceControl::Manual, "Manual"),
        (
            SourceDialogMode::Manual,
            SourceControl::Discovery,
            "Discover",
        ),
        (SourceDialogMode::Manual, SourceControl::Agent, "🧠"),
        (SourceDialogMode::Manual, SourceControl::File, "File"),
        (SourceDialogMode::Manual, SourceControl::Command, "Command"),
        (
            SourceDialogMode::Discovery,
            SourceControl::Refresh,
            "Rescan",
        ),
    ];
    for (mode, control, label) in cases {
        source_mode(&mut app, &provider, mode);
        source_focus(&mut app, &provider, control);
        let output = render(&provider, &mut app, 34, 18);
        assert!(output.contains(label), "missing focused {label}: {output}");
        assert!(
            app.layers
                .source
                .control_rects()
                .iter()
                .any(|(_, visible)| *visible == control),
            "focused {label} has no hitbox"
        );
    }

    source_mode(&mut app, &provider, SourceDialogMode::Manual);
    source_activate(&mut app, &provider, SourceControl::Command);
    assert_eq!(app.layers.source.state().kind, SourceKind::Command);
    source_activate(&mut app, &provider, SourceControl::Discovery);
    assert!(matches!(
        app.take_discovery_requests().as_slice(),
        [lvu::DiscoveryUiRequest::Scan { .. }]
    ));
}

#[test]
fn async_source_results_preserve_newer_dialog_input_and_reopen_dismissed_errors() {
    let provider = EmptyProvider;
    let mut app = App::new(vec![], vec![], false);
    app.handle(Action::Raw(RawEvent::Paste("first.log".into())), &provider);
    app.handle(raw_key(KeyCode::Enter), &provider);
    let first = app.take_source_requests().pop().expect("first request");
    app.handle(Action::Raw(RawEvent::Paste(".newer".into())), &provider);

    app.source_request_succeeded(&first, "unrelated-view");
    assert_eq!(app.focus, Focus::Layer);
    assert_eq!(app.layers.source.state().draft, "first.log.newer");
    app.source_request_failed(first.clone(), "old failure".into());
    assert!(app.layers.source.state().error.is_none());
    assert_eq!(
        app.source_notice.as_deref(),
        Some("source error: old failure")
    );

    app.handle(raw_key(KeyCode::Esc), &provider);
    assert!(
        app.layers.source.is_open(),
        "automatic suggestions close first"
    );
    app.handle(raw_key(KeyCode::Esc), &provider);
    assert!(!app.layers.source.is_open());
    app.source_request_failed(first, "visible failure".into());
    assert_eq!(app.focus, Focus::Layer);
    assert_eq!(app.layers.source.state().draft, "first.log");
    assert_eq!(
        app.layers.source.state().error.as_deref(),
        Some("visible failure")
    );
}

#[test]
fn discovery_diagnostics_keep_readable_text_when_focus_changes() {
    let provider = EmptyProvider;
    for theme in [Theme::LOVE_DARK, Theme::LOVE_LIGHT] {
        for focused in [false, true] {
            let mut app = App::new(vec![], vec![], false);
            app.handle(raw_ctrl(KeyCode::Char('d')), &provider);
            let mut terminal = Terminal::new(TestBackend::new(100, 28)).unwrap();
            terminal
                .draw(|frame| ui::render_with_theme(frame, &mut app, &provider, theme, None))
                .unwrap();
            let area = app
                .layers
                .source
                .scroll_rect()
                .expect("diagnostics surface");
            if focused {
                // Clicking the pane is what hands it focus.
                app.handle(raw_click(area.x, area.y), &provider);
            }
            terminal
                .draw(|frame| ui::render_with_theme(frame, &mut app, &provider, theme, None))
                .unwrap();
            let buffer = terminal.backend().buffer();
            let styles = lvu::dialog_controls::DialogStyles::new(theme);
            // §8.7 removed the box around the pane, so focus is signalled on the
            // heading; the body text keeps its readable role either way.
            assert_eq!(buffer[(area.x + 2, area.y + 1)].fg, theme.base_fg);
            assert_eq!(
                buffer[(area.x, area.y)].fg,
                if focused {
                    styles.shortcut.fg.expect("accent role")
                } else {
                    styles.label.fg.expect("label role")
                }
            );
        }
    }
}

#[test]
fn discovery_dialog_filters_selects_and_fences_cancelled_scans() {
    use lvu::{DiscoveryItem, DiscoveryUiRequest};

    let provider = EmptyProvider;
    let mut app = App::new(vec![], vec![], false);
    app.handle(raw_ctrl(KeyCode::Char('d')), &provider);
    let first = app.take_discovery_requests();
    assert_eq!(first, vec![DiscoveryUiRequest::Scan { generation: 1 }]);
    assert_eq!(app.layers.source.state().mode, SourceDialogMode::Discovery);

    app.handle(raw_ctrl(KeyCode::Char('r')), &provider);
    assert_eq!(
        app.take_discovery_requests(),
        vec![
            DiscoveryUiRequest::Cancel { generation: 1 },
            DiscoveryUiRequest::Scan { generation: 2 }
        ]
    );
    assert!(!app.apply_discovery_result(
        1,
        vec![DiscoveryItem {
            key: "stale".into(),
            label: "stale.log".into(),
            detail: "old scan".into(),
            status: "available".into(),
        }],
        "stale complete".into(),
    ));
    assert!(app.apply_discovery_result(
        2,
        vec![
            DiscoveryItem {
                key: "docker".into(),
                label: "api service".into(),
                detail: "compose service api".into(),
                status: "Docker High Available".into(),
            },
            DiscoveryItem {
                key: "file".into(),
                label: "events.log".into(),
                detail: "/tmp/events.log — writable tee target".into(),
                status: "Procfs High Available".into(),
            },
        ],
        "2 candidates, complete".into(),
    ));
    let discovered = render(&provider, &mut app, 100, 24);
    assert!(discovered.contains("api service"), "{discovered}");
    assert!(discovered.contains("Docker High Available"), "{discovered}");
    assert!(discovered.contains("compose service api"));
    assert!(discovered.contains("2 candidates, complete"));
    assert!(discovered.contains("Manual"));
    assert!(discovered.contains("Discover"));
    assert!(!discovered.contains("wheel select"));
    app.handle(raw_char('t'), &provider);
    app.handle(raw_char('e'), &provider);
    app.handle(raw_char('e'), &provider);
    app.handle(raw_key(KeyCode::Enter), &provider);
    assert_eq!(
        app.take_discovery_requests(),
        vec![DiscoveryUiRequest::Select {
            generation: 2,
            key: "file".into(),
        }]
    );
}

#[test]
fn search_uses_semantic_input_status_and_action_only_footer() {
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::Search), &provider);
    let mut terminal = Terminal::new(TestBackend::new(88, 20)).unwrap();
    terminal
        .draw(|frame| {
            ui::render_with_theme(
                frame,
                &mut app,
                &provider,
                lvu::theme::Theme::LOVE_LIGHT,
                None,
            )
        })
        .unwrap();
    let buffer = terminal.backend().buffer();
    let rendered = screen(buffer);
    assert_eq!(rendered.matches("Search").count(), 1, "{rendered}");
    assert!(
        rendered.find("every record is shown").unwrap() < rendered.find("Examples:").unwrap(),
        "state stays above the help sentence"
    );
    let help_row = rendered
        .lines()
        .position(|line| line.contains("Examples:"))
        .unwrap() as u16;
    // The help sentence's own first cell, not the first `E` on the terminal
    // row: the sidebar's `Errors only` can share the row behind the dialog.
    let help_line = rendered.lines().nth(usize::from(help_row)).unwrap();
    let help_column = help_line[..help_line.find("Examples:").unwrap()]
        .chars()
        .count() as u16;
    assert_eq!(buffer[(help_column, help_row)].symbol(), "E");
    assert_eq!(
        buffer[(help_column, help_row)].fg,
        lvu::theme::Theme::LOVE_LIGHT.base_fg
    );
    assert!(
        rendered.contains("No filter every record is shown"),
        "{rendered}"
    );
    assert!(!rendered.contains("Enter apply now"), "{rendered}");
    assert!(!rendered.contains("300ms"), "{rendered}");
    assert!(!rendered.contains("applied:"), "{rendered}");
    let cursor = terminal.backend().cursor_position();
    assert_eq!(buffer[cursor].bg, lvu::theme::Theme::LOVE_LIGHT.cursor);
    assert_eq!(
        buffer[(cursor.x.saturating_add(1), cursor.y)].bg,
        lvu::theme::Theme::LOVE_LIGHT.input_bg
    );
}

#[test]
fn narrow_dialog_footers_keep_every_context_action_discoverable() {
    let (provider, mut app) = demo();

    app.handle(Action::Open(Open::Time), &provider);
    let time = render(&provider, &mut app, 54, 14);
    assert!(time.contains("Time basis"), "{time}");
    assert!(time.contains("Capture"), "{time}");
    assert!(time.contains("Window"), "{time}");
    assert!(time.contains("All time"), "{time}");
    // §9 retires the scroll pseudo-buttons; height follows content and a
    // scrollbar marks any real overflow.
    assert!(!time.contains("Scroll down"), "{time}");
    assert!(time.contains("[ Apply ]"), "{time}");
    assert!(!time.contains("Enter"), "{time}");
    assert!(!time.contains("Tab"), "{time}");
    assert!(!time.contains("Esc"), "{time}");
    // The chord is the component's now, so its effect is what is asserted.
    app.handle(raw_alt(KeyCode::Char('p')), &provider);
    assert_eq!(app.layers.time.state().basis, lvu::TimeBasis::Capture);
    app.handle(Action::CancelEditor, &provider);

    app.handle(
        Action::Open(Open::Recipes {
            mode: RecipeDialogMode::Browse,
        }),
        &provider,
    );
    // §12.9 retired the mode bar: Save, Update and History are actions and the
    // rest are one menu. Every mode must still be actionable at 54 columns.
    let recipes = render(&provider, &mut app, 54, 20);
    for label in ["Saved recipes", "Save", "Update", "History", "More"] {
        assert!(recipes.contains(label), "missing {label}: {recipes}");
    }
    for control in [
        RecipeDialogControl::Save,
        RecipeDialogControl::Update,
        RecipeDialogControl::History,
        RecipeDialogControl::More,
    ] {
        assert!(
            app.layers
                .recipes
                .control_rects()
                .iter()
                .any(|(area, drawn)| !area.is_empty() && *drawn == control),
            "missing actionable {control:?}: {recipes}"
        );
    }
    recipe_activate(&mut app, &provider, RecipeDialogControl::More);
    let menu = render(&provider, &mut app, 54, 20);
    for label in ["Import", "Export", "Refresh"] {
        assert!(menu.contains(label), "missing {label}: {menu}");
    }
    assert_eq!(
        app.layers.recipes.menu_rects().len(),
        3,
        "every menu entry is clickable: {menu}"
    );
    app.handle(raw_key(KeyCode::Esc), &provider);
    // The layer owns its keymap, so Tab is asserted by what it does rather than
    // by the `Action` the retired base table produced for it (§3).
    let before = app.layers.recipes.state().control;
    app.handle(raw_key(KeyCode::Tab), &provider);
    assert_ne!(app.layers.recipes.state().control, before);
    recipe_activate(&mut app, &provider, RecipeDialogControl::Save);
    assert_eq!(app.layers.recipes.state().mode, RecipeDialogMode::Save);
    app.handle(raw_key(KeyCode::Esc), &provider);

    app.handle(Action::Open(Open::Ask(AskOpen::Generic)), &provider);
    let ask = render(&provider, &mut app, 54, 17);
    for label in ["Kind", "Filter", "[ Submit ]", "Request", "Ready"] {
        assert!(ask.contains(label), "missing {label}: {ask}");
    }
    for reminder in ["Alt-F filter", "Alt-E enrichment", "↑/↓"] {
        assert!(!ask.contains(reminder), "obsolete {reminder}: {ask}");
    }
    // §7.14: the base key table must not leak into a layer's shortcut column —
    // Alt-E is the Ask layer's, and `key_to_action` no longer knows it.
    assert_eq!(
        key_to_action(
            KeyEvent::new(KeyCode::Char('e'), KeyModifiers::ALT),
            Focus::Layer
        ),
        Action::None
    );
    // It still reaches the layer, and still chooses the enrichment kind.
    app.handle(raw_alt(KeyCode::Char('e')), &provider);
    assert_eq!(
        app.layers.ask.state().unwrap().kind,
        lvu::AskAiKind::Enrichment
    );
}

#[test]
fn search_error_keeps_last_accepted_filter_and_scrolls_diagnostics() {
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::Search), &provider);
    app.handle(
        Action::Raw(RawEvent::Paste("accepted needle".into())),
        &provider,
    );
    app.handle(raw_key(KeyCode::Enter), &provider);
    let request = app.take_query_requests().pop().unwrap();
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: request.view_id,
        generation: request.generation,
        revision: request.revision,
        purpose: request.purpose,
        result: Ok(()),
    }));
    for _ in 0.."accepted needle".chars().count() {
        app.handle(raw_key(KeyCode::Backspace), &provider);
    }
    app.handle(
        Action::Raw(RawEvent::Paste("broken draft".into())),
        &provider,
    );
    app.handle(raw_key(KeyCode::Enter), &provider);
    let request = app.take_query_requests().pop().unwrap();
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: request.view_id,
        generation: request.generation,
        revision: request.revision,
        purpose: request.purpose,
        result: Err(QueryFailure {
            purpose: request.purpose,
            message: format!("invalid expression: {}", "details ".repeat(40)),
        }),
    }));
    let top = render(&provider, &mut app, 54, 12);
    assert!(top.contains("Error"), "{top}");
    assert!(top.contains("Diagnostics"), "{top}");
    assert!(app.layers.filter.scroll_limit() > 0);
    assert!(
        app.layers.filter.diagnostics_rect().is_some(),
        "a scrollable diagnostic needs a wheel target"
    );
    // Tab reaches the tab control first, then hands the arrows to the pane,
    // exactly as it did when the flag was `App::dialog_scroll_focused`.
    app.handle(raw_key(KeyCode::Tab), &provider);
    assert!(app.layers.filter.tabs_focused());
    app.handle(raw_key(KeyCode::Tab), &provider);
    for _ in 0..64 {
        app.handle(raw_key(KeyCode::Down), &provider);
    }
    let bottom = render(&provider, &mut app, 54, 12);
    assert!(bottom.contains("last accepted"), "{bottom}");
    assert!(bottom.contains("accepted needle"), "{bottom}");
    assert!(!bottom.contains("Enter apply"), "{bottom}");
}

#[test]
fn discovery_fixed_rows_keep_last_candidate_visible_highlighted_and_clickable() {
    use lvu::{DiscoveryItem, DiscoveryUiRequest};

    let provider = EmptyProvider;
    let mut app = App::new(vec![], vec![], false);
    app.handle(raw_ctrl(KeyCode::Char('d')), &provider);
    let _ = app.take_discovery_requests();
    let items = (0..40)
        .map(|index| DiscoveryItem {
            key: format!("key-{index}"),
            label: format!(
                "candidate {index:02} 東京 with spaces and an intentionally very long name"
            ),
            detail: format!("/tmp/very long directory/候補 {index:02}/events.log — evidence"),
            status: "available with long provider evidence".into(),
        })
        .collect();
    assert!(app.apply_discovery_result(
        1,
        items,
        format!(
            "40 candidates; {}",
            "provider limit details 東京 ".repeat(20)
        )
    ));
    for _ in 0..39 {
        app.handle(raw_key(KeyCode::Down), &provider);
    }

    let backend = TestBackend::new(72, 16);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| ui::render(frame, &mut app, &provider))
        .unwrap();
    let selected_region = app
        .layers
        .source
        .discovery_rects()
        .iter()
        .find(|(_, index)| *index == 39)
        .copied()
        .expect("last selected candidate has a visible one-line hitbox");
    assert_eq!(selected_region.0.height, 1);
    assert_eq!(
        terminal.backend().buffer()[(selected_region.0.x, selected_region.0.y)].bg,
        app.appearance.theme_id.theme().selection_bg
    );
    assert!(screen(terminal.backend().buffer()).contains("candidate 39"));
    assert!(app.layers.source.state().discovery.status_scroll_limit > 0);
    // The status pane takes the arrows once it is clicked, as it always did.
    let pane = app
        .layers
        .source
        .scroll_rect()
        .expect("diagnostics surface");
    app.handle(raw_click(pane.x, pane.y), &provider);
    for _ in 0..64 {
        app.handle(raw_key(KeyCode::Down), &provider);
    }
    terminal
        .draw(|frame| ui::render(frame, &mut app, &provider))
        .unwrap();
    assert!(screen(terminal.backend().buffer()).contains("candidate 39"));

    let first_visible = app.layers.source.discovery_rects()[0];
    app.handle(raw_click(first_visible.0.x, first_visible.0.y), &provider);
    assert_eq!(
        app.layers.source.state().discovery.selected,
        first_visible.1
    );
    app.handle(raw_key(KeyCode::Enter), &provider);
    assert_eq!(
        app.take_discovery_requests(),
        vec![DiscoveryUiRequest::Select {
            generation: 1,
            key: format!("key-{}", first_visible.1),
        }]
    );
}

struct EmptyProvider;

impl RowProvider for EmptyProvider {
    fn page(&self, _: &str, _: ViewportRequest) -> RowPage {
        RowPage {
            total: 0,
            rows: vec![],
        }
    }
    fn row_by_id(&self, _: &str, _: &RowId) -> Option<DisplayRow> {
        None
    }
    fn index_of_id(&self, _: &str, _: &RowId) -> Option<usize> {
        None
    }
    fn revision(&self, _: &str) -> u64 {
        0
    }
}

#[test]
fn advanced_and_enrichment_completion_escape_python_and_never_auto_submit() {
    let row = DisplayRow {
        id: RowId::new("source", 1),
        timestamp: "now".into(),
        captured_at_unix_nanos: Some(1),
        level: "INFO".into(),
        text: "raw".into(),
        details: vec![],
        fields: vec![
            ("say 'hi' 東京".into(), "a\\b'c\n東京".into()),
            ("space field".into(), "200".into()),
        ],
    };
    let provider = GrowingProvider {
        rows: RefCell::new(vec![row]),
    };
    let mut app = App::new(
        vec![SourceItem {
            id: "source".into(),
            name: "source".into(),
            health: "ok".into(),
        }],
        vec![ViewItem {
            id: "view".into(),
            source_id: "source".into(),
            name: "view".into(),
        }],
        false,
    );
    render(&provider, &mut app, 100, 24);
    app.handle(Action::Open(Open::Advanced), &provider);
    app.handle(raw_key(KeyCode::Tab), &provider);
    let field = app
        .layers
        .filter
        .completion()
        .unwrap()
        .items
        .iter()
        .position(|item| item.insertion == "pl.col('say \\'hi\\' 東京')")
        .unwrap();
    assert!(
        app.layers
            .filter
            .completion()
            .unwrap()
            .items
            .iter()
            .any(|item| item.insertion == "pl.col('raw')"),
        "the authoritative raw column remains available for unstructured logs"
    );
    for _ in 0..field {
        app.handle(raw_key(KeyCode::Down), &provider);
    }
    app.handle(raw_key(KeyCode::Enter), &provider);
    assert_eq!(
        app.advanced_state().unwrap().draft,
        "pl.col('say \\'hi\\' 東京')"
    );
    assert!(app.take_query_requests().is_empty());

    app.handle(raw_key(KeyCode::Tab), &provider);
    app.handle(raw_key(KeyCode::Tab), &provider);
    let value = app
        .layers
        .filter
        .completion()
        .unwrap()
        .items
        .iter()
        .position(|item| item.insertion.contains("a\\\\b"))
        .unwrap();
    for _ in 0..value {
        app.handle(raw_key(KeyCode::Down), &provider);
    }
    app.handle(raw_key(KeyCode::Enter), &provider);
    assert!(
        app.advanced_state()
            .unwrap()
            .draft
            .ends_with("'a\\\\b\\'c\\n東京'")
    );
    assert!(app.take_query_requests().is_empty());

    app.handle(raw_key(KeyCode::Esc), &provider);
    app.handle(Action::Open(Open::Enrichment), &provider);
    enrichment_add(&mut app, &provider);
    app.handle(Action::Raw(RawEvent::Paste("copied = ".into())), &provider);
    app.handle(raw_ctrl(KeyCode::Char(' ')), &provider);
    render(&provider, &mut app, 100, 24);
    let space_field = app
        .layers
        .enrichment_step
        .completion()
        .unwrap()
        .items
        .iter()
        .position(|item| item.insertion == "pl.col('space field')")
        .unwrap();
    let row = app
        .layers
        .enrichment_step
        .completion_rects()
        .iter()
        .find(|(_, index)| *index == space_field)
        .unwrap()
        .0;
    app.handle(raw_click(row.x, row.y), &provider);
    step_submit(&mut app, &provider);
    assert_eq!(
        app.view_state().unwrap().enrichment.draft,
        "copied = pl.col('space field')"
    );
    assert!(app.take_query_requests().is_empty());
}

#[test]
fn completion_is_empty_safe_and_fenced_by_edits_views_and_lifetimes() {
    let provider = EmptyProvider;
    let mut app = App::new(
        vec![SourceItem {
            id: "source".into(),
            name: "source".into(),
            health: "ok".into(),
        }],
        vec![
            ViewItem {
                id: "one".into(),
                source_id: "source".into(),
                name: "one".into(),
            },
            ViewItem {
                id: "two".into(),
                source_id: "source".into(),
                name: "two".into(),
            },
        ],
        false,
    );
    app.handle(Action::Open(Open::Advanced), &provider);
    app.handle(raw_key(KeyCode::Tab), &provider);
    assert_eq!(
        app.layers.filter.completion().unwrap().items[0].insertion,
        "pl.col('raw')"
    );
    let generation = app.layers.filter.completion().unwrap().generation;
    app.handle(raw_key(KeyCode::Esc), &provider);
    assert!(app.layers.filter.completion().is_none());
    app.handle(raw_key(KeyCode::Tab), &provider);
    assert!(app.layers.filter.completion().unwrap().generation > generation);
    app.handle(raw_key(KeyCode::Char('x')), &provider);
    assert!(app.layers.filter.completion().is_none());
    app.handle(raw_key(KeyCode::Tab), &provider);
    app.set_selected_view(1);
    app.handle(raw_key(KeyCode::Enter), &provider);
    assert!(app.view_state().unwrap().advanced.draft.is_empty());
    assert!(app.take_query_requests().is_empty());
}

struct GrowingProvider {
    rows: RefCell<Vec<DisplayRow>>,
}

impl RowProvider for GrowingProvider {
    fn page(&self, _: &str, request: ViewportRequest) -> RowPage {
        let rows = self.rows.borrow();
        RowPage {
            total: rows.len(),
            rows: rows
                .iter()
                .skip(request.start)
                .take(request.len)
                .cloned()
                .collect(),
        }
    }
    fn row_by_id(&self, _: &str, id: &RowId) -> Option<DisplayRow> {
        self.rows.borrow().iter().find(|row| &row.id == id).cloned()
    }
    fn index_of_id(&self, _: &str, id: &RowId) -> Option<usize> {
        self.rows.borrow().iter().position(|row| &row.id == id)
    }
    fn revision(&self, _: &str) -> u64 {
        self.rows.borrow().len() as u64
    }
}

#[test]
fn focus_and_hit_regions_route_sidebar_log_and_modal_mouse() {
    let (provider, mut app) = demo();
    render(&provider, &mut app, 88, 24);
    app.handle(Action::CycleFocus, &provider);
    assert_eq!(app.focus, Focus::Selector);
    app.handle(Action::SelectSidebar(1), &provider);
    assert_eq!(app.active_view_id(), Some("errors"));

    let all_region = app.hit_regions.sidebar_views[0].0;
    app.handle(
        Action::Mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            all_region.x,
            all_region.y,
        )),
        &provider,
    );
    assert_eq!(app.active_view_id(), Some("all"));
    render(&provider, &mut app, 88, 24);
    let rows = app.hit_regions.log_rows.expect("log rows");
    app.handle(
        Action::Mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            rows.x,
            rows.y,
        )),
        &provider,
    );
    assert_eq!(
        app.view_state().expect("state").selected,
        Some(RowId::new("api", 1))
    );

    let selected = app.view_state().expect("state").selected.clone();
    app.handle(
        Action::Mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            rows.x,
            rows.y - 1,
        )),
        &provider,
    );
    assert_eq!(
        app.view_state().expect("state").selected,
        selected,
        "table header is not a row"
    );
    app.handle(Action::Open(Open::Help), &provider);
    render(&provider, &mut app, 88, 24);
    app.handle(
        Action::Raw(RawEvent::Mouse(mouse(
            MouseEventKind::ScrollDown,
            rows.x,
            rows.y,
        ))),
        &provider,
    );
    assert_eq!(
        app.view_state().expect("state").selected,
        selected,
        "modal captures mouse"
    );
}

#[test]
fn grouping_editor_is_per_view_transactional_and_groups_expand_by_key_and_mouse() {
    let (fixture, mut app) = demo();
    app.handle(Action::Open(Open::Grouping), &fixture);
    assert_eq!(app.focus, Focus::Layer);
    // The normal control opens on Run with a blank column: Run is the
    // primary configured path and legacy Auto is never the default.
    assert_eq!(
        app.view_state().unwrap().grouping.draft,
        lvu::grouping::run_rule("")
    );
    app.handle(raw_key(KeyCode::Enter), &fixture);
    let request = app.take_query_requests().pop().unwrap();
    assert_eq!(request.purpose, QueryPurpose::Grouping);
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: request.view_id,
        generation: request.generation,
        revision: request.revision,
        purpose: request.purpose,
        result: Ok(()),
    }));
    assert_eq!(
        app.view_state().unwrap().grouping.applied,
        lvu::grouping::run_rule("")
    );
    app.handle(Action::NextView, &fixture);
    assert!(app.view_state().unwrap().grouping.applied.is_empty());
    app.handle(Action::PreviousView, &fixture);

    let grouped = DisplayRow {
        id: RowId::new("api", 1),
        timestamp: "12:00:01".into(),
        captured_at_unix_nanos: Some(1),
        level: "ERROR".into(),
        text: "Error: boom  [2 physical lines]".into(),
        details: vec![
            ("group_line_count".into(), "2".into()),
            ("group_line_1".into(), "api:1: Error: boom".into()),
            ("group_line_2".into(), "api:2:   at worker.rs:42".into()),
        ],
        fields: vec![],
    };
    let provider = GrowingProvider {
        rows: RefCell::new(vec![grouped]),
    };
    let mut grouped_app = App::new(
        vec![SourceItem {
            id: "api".into(),
            name: "api".into(),
            health: "ok".into(),
        }],
        vec![ViewItem {
            id: "all".into(),
            source_id: "api".into(),
            name: "all".into(),
        }],
        false,
    );
    let collapsed = render(&provider, &mut grouped_app, 100, 18);
    assert!(collapsed.contains("[2 physical lines]"));
    grouped_app.handle(Action::ToggleExpandedGroup, &provider);
    let expanded = render(&provider, &mut grouped_app, 100, 18);
    assert!(expanded.contains("at worker.rs:42"));
    assert!(expanded.find("api:1: Error: boom") < expanded.find("api:2:   at worker.rs:42"));
    assert_eq!(
        expanded
            .lines()
            .filter(|line| line.contains("api:"))
            .count(),
        2,
        "count metadata must not become a synthetic expanded row\n{expanded}"
    );
    let region = grouped_app.hit_regions.log_row_indices[0].0;
    grouped_app.handle(
        Action::Mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            region.x,
            region.y,
        )),
        &provider,
    );
    assert!(grouped_app.view_state().unwrap().expanded_groups.is_empty());
}

#[test]
fn expanded_groups_state_shown_total_when_the_page_is_capped() {
    let truncated = DisplayRow {
        id: RowId::new("api", 1),
        timestamp: "12:00:01".into(),
        captured_at_unix_nanos: Some(1),
        level: "ERROR".into(),
        text: "ERROR head  [101 physical lines, first 2 shown]".into(),
        details: vec![
            ("group_line_count".into(), "101".into()),
            ("group_record_count".into(), "101".into()),
            ("group_line_1".into(), "api:1: ERROR head".into()),
            ("group_line_2".into(), "api:2: payload".into()),
            (
                "group_truncated".into(),
                "showing first 2 of 101 records; every member stays in the source view".into(),
            ),
        ],
        fields: vec![],
    };
    let provider = GrowingProvider {
        rows: RefCell::new(vec![truncated]),
    };
    let mut app = App::new(
        vec![SourceItem {
            id: "api".into(),
            name: "api".into(),
            health: "ok".into(),
        }],
        vec![ViewItem {
            id: "all".into(),
            source_id: "api".into(),
            name: "all".into(),
        }],
        false,
    );
    let collapsed = render(&provider, &mut app, 100, 18);
    assert!(collapsed.contains("[101 physical lines, first 2 shown]"));
    app.handle(Action::ToggleExpandedGroup, &provider);
    let expanded = render(&provider, &mut app, 100, 18);
    assert!(expanded.contains("api:1: ERROR head"));
    assert!(expanded.contains("api:2: payload"));
    // The expansion must not imply all lines are displayed: the truncation
    // notice travels with the member lines.
    assert!(
        expanded.contains("showing first 2 of 101 records"),
        "{expanded}"
    );
    assert_eq!(
        expanded
            .lines()
            .filter(|line| line.contains("api:"))
            .count(),
        2,
        "count metadata must not become a synthetic expanded row\n{expanded}"
    );
}

#[test]
fn collapse_all_collapses_expanded_configured_groups_without_enabling() {
    use lvu::command_palette::{CommandId, Palette, PaletteContext};

    let grouped = DisplayRow {
        id: RowId::new("api", 1),
        timestamp: "12:00:01".into(),
        captured_at_unix_nanos: Some(1),
        level: "ERROR".into(),
        text: "Error: boom  [2 physical lines]".into(),
        details: vec![
            ("group_line_count".into(), "2".into()),
            ("group_line_1".into(), "Error: boom [api:1]".into()),
            ("group_line_2".into(), "at worker.rs:42 [api:2]".into()),
        ],
        fields: vec![],
    };
    let provider = GrowingProvider {
        rows: RefCell::new(vec![grouped]),
    };
    let mut app = App::new(
        vec![SourceItem {
            id: "api".into(),
            name: "api".into(),
            health: "ok".into(),
        }],
        vec![ViewItem {
            id: "all".into(),
            source_id: "api".into(),
            name: "all".into(),
        }],
        false,
    );
    let collapsed = render(&provider, &mut app, 100, 18);
    assert!(collapsed.contains("[2 physical lines]"), "{collapsed}");
    app.handle(Action::ToggleExpandedGroup, &provider);
    assert!(!app.view_state().unwrap().expanded_groups.is_empty());
    let expanded = render(&provider, &mut app, 100, 18);
    assert!(expanded.contains("at worker.rs:42"), "{expanded}");

    // The palette row reaches the collapse without enabling anything, and it
    // clears legacy fold expansions alongside group ones.
    let mut palette = Palette::new();
    palette.open(PaletteContext::new(Focus::Logs, true));
    for character in "collapse expanded".chars() {
        let context = palette.context();
        palette.handle_key(
            KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE),
            context.clone(),
        );
    }
    assert_eq!(
        palette.selected_command().map(|command| command.id),
        Some(CommandId::CollapseAllFolds)
    );
    app.handle(Action::CollapseAllFolds, &provider);
    assert!(app.view_state().unwrap().expanded_groups.is_empty());
    assert!(app.view_state().unwrap().fold_expanded.is_empty());
    assert!(!app.view_state().unwrap().fold_enabled);
    let collapsed = render(&provider, &mut app, 100, 18);
    assert!(!collapsed.contains("at worker.rs:42"), "{collapsed}");
    assert!(collapsed.contains("[2 physical lines]"), "{collapsed}");
}

#[test]
fn invalid_grouping_recipe_rolls_back_every_constraint_and_keeps_failed_draft() {
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::Grouping), &provider);
    app.handle(raw_key(KeyCode::Enter), &provider);
    let accepted = app.take_query_requests().pop().unwrap();
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: accepted.view_id,
        generation: accepted.generation,
        revision: accepted.revision,
        purpose: accepted.purpose,
        result: Ok(()),
    }));
    let prior = app.view_state().unwrap().grouping.applied.clone();

    app.handle(
        Action::Open(Open::Recipes {
            mode: RecipeDialogMode::Browse,
        }),
        &provider,
    );
    let lvu::RecipeRequest::List { meta } = app.take_recipe_requests().pop().unwrap() else {
        panic!("recipe list")
    };
    app.set_recipes(
        meta,
        vec![lvu::RecipeItem {
            id: "invalid-group".into(),
            revision: "one".into(),
            saved_at_unix_nanos: None,
            name: "Invalid grouping".into(),
            incompatibility: None,
            config: lvu::RecipeConfig {
                search: "new search".into(),
                grouping: ".*not-anchored".into(),
                pinned_columns: vec!["service".into()],
                ..lvu::RecipeConfig::default()
            },
        }],
        None,
    );
    recipe_apply(&mut app, &provider);
    let recipe = app.take_query_requests().pop().unwrap();
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: recipe.view_id,
        generation: recipe.generation,
        revision: recipe.revision,
        purpose: recipe.purpose,
        result: Err(QueryFailure {
            purpose: QueryPurpose::Grouping,
            message: "rule must begin with ^".into(),
        }),
    }));
    let state = app.view_state().unwrap();
    assert_eq!(state.grouping.applied, prior);
    assert_eq!(state.grouping.draft, ".*not-anchored");
    assert_eq!(
        state.grouping.error.as_deref(),
        Some("rule must begin with ^")
    );
    assert!(state.search.applied.is_empty());
    assert!(state.pinned_columns.is_empty());
    let reaffirm = app.take_query_requests().pop().unwrap();
    assert_eq!(
        reaffirm.constraints.grouping.as_deref(),
        Some(prior.as_str())
    );
    assert!(reaffirm.constraints.text.is_none());
}

#[test]
fn small_dimensions_unicode_and_help_render() {
    let (provider, mut app) = demo();
    assert!(render(&provider, &mut app, 18, 4).contains("terminal too small"));
    render(&provider, &mut app, 70, 16);
    app.handle(Action::Top, &provider);
    app.handle(Action::MoveLine(2), &provider);
    let unicode = render(&provider, &mut app, 70, 16);
    assert!(unicode.contains("Unicode 東"), "{unicode}");
    assert!(unicode.contains("café e\u{301}"), "{unicode}");
    assert_eq!(ui::clipped_width("a東京b", 5), "a東京");
    app.handle(Action::Open(Open::Help), &provider);
    let help = render(&provider, &mut app, 70, 16);
    assert!(help.contains("EVERYWHERE"), "{help}");
    assert_eq!(app.focus, Focus::Layer);
    assert!(app.layers.help.scroll_limit() > 0);
}

#[test]
fn help_is_grouped_styled_scrollable_and_does_not_move_background() {
    let (provider, mut app) = demo();
    app.handle(Action::Top, &provider);
    app.handle(Action::MoveLine(2), &provider);
    render(&provider, &mut app, 72, 16);
    let selected = app.view_state().unwrap().selected.clone();
    app.handle(Action::Open(Open::Help), &provider);

    let mut terminal = Terminal::new(TestBackend::new(72, 16)).unwrap();
    terminal
        .draw(|frame| {
            ui::render_with_theme(
                frame,
                &mut app,
                &provider,
                lvu::theme::Theme::LOVE_LIGHT,
                None,
            )
        })
        .unwrap();
    let buffer = terminal.backend().buffer();
    let rendered = screen(buffer);
    assert!(rendered.contains("EVERYWHERE"), "{rendered}");
    let header = (0..buffer.area.height)
        .flat_map(|y| (0..buffer.area.width).map(move |x| (x, y)))
        .find(|&(x, y)| {
            buffer[(x, y)].symbol() == "E" && {
                let row = (x..buffer.area.width)
                    .map(|column| buffer[(column, y)].symbol())
                    .collect::<String>();
                row.starts_with("EVERYWHERE")
            }
        })
        .expect("styled section header");
    assert_eq!(
        buffer[header].fg,
        lvu::dialog_controls::DialogStyles::new(lvu::theme::Theme::LOVE_LIGHT)
            .label
            .fg
            .unwrap()
    );
    assert!(
        buffer[header]
            .modifier
            .contains(ratatui::style::Modifier::BOLD)
    );

    for _ in 0..200 {
        app.handle(raw_key(KeyCode::Down), &provider);
    }
    let bottom = render(&provider, &mut app, 72, 16);
    assert!(bottom.contains("SOURCES"), "{bottom}");
    // §8.10: the source keys are bare now. Anchored on the row's text, because
    // a bare `R` would match almost anywhere on this screen.
    assert!(bottom.contains("Restart the selected source"), "{bottom}");
    let complete = render(&provider, &mut app, 160, 70);
    for removed in [
        "MOUSE & SELECTION",
        "j/k · ↑/↓",
        "explicit review and apply",
    ] {
        assert!(!complete.contains(removed), "{complete}");
    }
    assert_eq!(app.view_state().unwrap().selected, selected);
    app.handle(raw_key(KeyCode::Char('?')), &provider);
    assert_eq!(app.focus, Focus::Logs);
    assert!(!app.layers.help.is_open());
}

#[test]
fn field_picker_pins_colors_and_preserves_per_view_presentation() {
    let (provider, mut app) = demo();
    app.sync_provider(&provider, 10);
    app.handle(Action::Open(Open::Fields), &provider);
    assert_eq!(app.focus, Focus::Layer);
    let picker = render(&provider, &mut app, 88, 24);
    assert!(picker.contains("Fields · record"), "{picker}");
    assert!(picker.contains("service"));
    app.handle(raw_key(KeyCode::Down), &provider);
    let first_field = app.layers.fields.row_rects()[0].0;
    app.handle(
        Action::Raw(RawEvent::Mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            first_field.x,
            first_field.y,
        ))),
        &provider,
    );
    assert_eq!(app.view_state().unwrap().field_picker_selected, 0);
    app.handle(raw_key(KeyCode::Char(' ')), &provider);
    app.handle(raw_key(KeyCode::Char('c')), &provider);
    app.handle(raw_key(KeyCode::Esc), &provider);
    let pinned = render(&provider, &mut app, 100, 24);
    assert!(pinned.contains("service"));
    assert_eq!(app.view_state().unwrap().pinned_columns, ["service"]);
    assert_eq!(
        app.view_state().unwrap().color_field.as_deref(),
        Some("service")
    );

    app.handle(Action::NextView, &provider);
    assert!(app.view_state().unwrap().pinned_columns.is_empty());
    app.handle(Action::PreviousView, &provider);
    assert_eq!(app.view_state().unwrap().pinned_columns, ["service"]);
}

#[test]
fn field_picker_distinguishes_no_selection_loading_and_empty_fields() {
    let empty = DisplayRow {
        id: RowId::new("source", 7),
        timestamp: "00:00:07".into(),
        captured_at_unix_nanos: Some(7_000_000_000),
        level: "INFO".into(),
        text: "plain unstructured event".into(),
        details: vec![("raw".into(), "plain unstructured event".into())],
        fields: vec![],
    };
    let provider = GrowingProvider {
        rows: RefCell::new(vec![empty.clone()]),
    };
    let make_app = || {
        App::new(
            vec![SourceItem {
                id: "source".into(),
                name: "source".into(),
                health: "ready".into(),
            }],
            vec![ViewItem {
                id: "view".into(),
                source_id: "source".into(),
                name: "view".into(),
            }],
            false,
        )
    };

    let mut empty_app = App::new(vec![], vec![], false);
    empty_app.handle(Action::Open(Open::Fields), &provider);
    let empty_app_screen = render(&provider, &mut empty_app, 72, 16);
    assert_eq!(empty_app.focus, Focus::Layer);
    // §12.11 states the empty case as a body row rather than a sentence.
    assert!(
        empty_app_screen.contains("No record selected"),
        "{empty_app_screen}"
    );

    let mut no_selection = make_app();
    no_selection.handle(Action::Open(Open::Fields), &provider);
    let no_selection_screen = render(&provider, &mut no_selection, 72, 16);
    assert_eq!(no_selection.focus, Focus::Layer);
    assert!(
        no_selection_screen.contains("No record selected"),
        "{no_selection_screen}"
    );
    assert!(
        !no_selection_screen.contains("[ Pin ]"),
        "{no_selection_screen}"
    );
    assert!(
        !no_selection_screen.contains("[ Raw context ]"),
        "{no_selection_screen}"
    );
    assert!(no_selection.layers.fields.row_rects().is_empty());

    let mut app = make_app();
    app.sync_provider(&provider, 8);
    let selected = app.view_state().unwrap().selected.clone().unwrap();
    provider.rows.borrow_mut().clear();
    app.handle(Action::Open(Open::Fields), &provider);
    assert_eq!(
        app.view_state().unwrap().field_picker_row.as_ref(),
        Some(&selected)
    );
    let loading = render(&provider, &mut app, 72, 16);
    // §7.4 states the wait in the message row, and §11 replaces the remembered
    // `o` with the action it stood for.
    assert!(loading.contains("has not arrived yet"), "{loading}");
    assert!(loading.contains("[ Inspect context ]"), "{loading}");
    assert!(!loading.contains("[ Pin ]"), "{loading}");
    assert!(app.layers.fields.row_rects().is_empty());

    for width in [54, 96] {
        let mut terminal = Terminal::new(TestBackend::new(width, 16)).unwrap();
        terminal
            .draw(|frame| {
                ui::render_with_theme(frame, &mut app, &provider, Theme::LOVE_LIGHT, None)
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let status_cell = (0..buffer.area.height)
            .find_map(|y| {
                let line = (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>();
                line.find("has not").map(|x| (x as u16, y))
            })
            .expect("availability status remains visible");
        // §7.4 moved the sentence into the shared message row, so it carries
        // that row's role rather than the raw base foreground.
        assert_eq!(
            Some(buffer[status_cell].fg),
            lvu::dialog_controls::DialogStyles::new(Theme::LOVE_LIGHT)
                .description
                .fg
        );
    }

    provider.rows.borrow_mut().extend([
        empty,
        DisplayRow {
            id: RowId::new("source", 8),
            timestamp: "00:00:08".into(),
            captured_at_unix_nanos: Some(8_000_000_000),
            level: "INFO".into(),
            text: "later structured event".into(),
            details: vec![],
            fields: vec![("later".into(), "value".into())],
        },
    ]);
    let empty_screen = render(&provider, &mut app, 72, 16);
    assert!(
        empty_screen.contains("No fields for this record"),
        "{empty_screen}"
    );
    assert_eq!(
        app.view_state().unwrap().field_picker_row.as_ref(),
        Some(&selected)
    );
    assert!(!empty_screen.contains("[ Pin ]"), "{empty_screen}");
    assert!(!empty_screen.contains("[ Color ]"), "{empty_screen}");
    assert!(!empty_screen.contains("r correlate"), "{empty_screen}");
    assert!(app.layers.fields.row_rects().is_empty());

    // `o` jumps to the record's All events view (raw-context-as-jump.md);
    // this view is that view, so Fields is re-pushed with the reason.
    let view = app.active_view_id().unwrap().to_owned();
    app.set_view_role(&view, lvu::ViewRole::Canonical);
    app.handle(raw_key(KeyCode::Char('o')), &provider);
    assert_eq!(app.focus, Focus::Layer);
    assert!(app.layers.fields.is_open());
    assert_eq!(
        app.action_notice.as_deref(),
        Some("this is the raw stream · o returns nowhere")
    );
}

#[test]
fn field_picker_scrolls_clipped_rows_and_stays_on_opened_event() {
    let many = DisplayRow {
        id: RowId::new("source", 1),
        timestamp: "00:00:01".into(),
        captured_at_unix_nanos: Some(1_000_000_000),
        level: "INFO".into(),
        text: "original structured row".into(),
        details: vec![],
        fields: (0..20)
            .map(|index| (format!("field_{index:02}"), "x".repeat(512)))
            .collect(),
    };
    let provider = GrowingProvider {
        rows: RefCell::new(vec![many]),
    };
    let mut app = App::new(
        vec![SourceItem {
            id: "source".into(),
            name: "source".into(),
            health: "ready".into(),
        }],
        vec![
            ViewItem {
                id: "view".into(),
                source_id: "source".into(),
                name: "view".into(),
            },
            ViewItem {
                id: "other".into(),
                source_id: "source".into(),
                name: "other view".into(),
            },
        ],
        false,
    );
    app.sync_provider(&provider, 8);
    app.handle(Action::Open(Open::Fields), &provider);
    for _ in 0..15 {
        app.handle(raw_key(KeyCode::Down), &provider);
    }
    let picker = render(&provider, &mut app, 56, 12);
    assert!(picker.contains("field_15"));
    assert!(!picker.contains(&"x".repeat(80)));
    assert!(app.layers.fields.row_rects().len() < 12);
    assert!(
        app.layers
            .fields
            .row_rects()
            .iter()
            .all(|(area, _)| area.y < 10 && area.x > 0),
        "picker hitboxes stay in the padded body above the footer"
    );
    let last_visible = *app.layers.fields.row_rects().last().unwrap();
    app.handle(
        Action::Raw(RawEvent::Mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            last_visible.0.x,
            last_visible.0.y,
        ))),
        &provider,
    );
    assert_eq!(
        app.view_state().unwrap().field_picker_selected,
        last_visible.1
    );

    provider.rows.borrow_mut().push(DisplayRow {
        id: RowId::new("source", 2),
        timestamp: "00:00:02".into(),
        captured_at_unix_nanos: Some(2_000_000_000),
        level: "WARN".into(),
        text: "late arrival".into(),
        details: vec![],
        fields: vec![("only".into(), "short".into())],
    });
    app.sync_provider(&provider, 8);
    assert_eq!(
        lvu::components::fields::anchored_row(&app.views, &provider)
            .unwrap()
            .id
            .sequence,
        1
    );
    let first_visible = app.layers.fields.row_rects()[0];
    app.handle(
        Action::Raw(RawEvent::Mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            first_visible.0.x,
            first_visible.0.y,
        ))),
        &provider,
    );
    assert_eq!(
        app.view_state().unwrap().field_picker_selected,
        first_visible.1
    );
    let selected_field = lvu::components::fields::anchored_row(&app.views, &provider)
        .unwrap()
        .fields[first_visible.1]
        .0
        .clone();

    app.handle(raw_key(KeyCode::Char('r')), &provider);
    assert_eq!(app.layers.top(), Some(LayerId::Union));
    assert!(app.take_correlation_requests().is_empty());
    app.handle(raw_key(KeyCode::Down), &provider);
    app.handle(raw_key(KeyCode::Char(' ')), &provider);
    app.handle(raw_key(KeyCode::Enter), &provider);
    let requests = app.layers.union.take_requests();
    let [lvu::UnionDialogRequest::Create { shared_key, .. }] = requests.as_slice() else {
        panic!("unexpected union requests: {requests:?}");
    };
    let shared_key = shared_key.as_ref().expect("shared-key origin");
    assert_eq!(shared_key.row_id.sequence, 1);
    assert_eq!(shared_key.field, selected_field);
    app.handle(raw_key(KeyCode::Esc), &provider);

    app.handle(Action::MoveLine(1), &provider);
    app.handle(Action::Open(Open::Fields), &provider);
    assert_eq!(app.view_state().unwrap().field_picker_selected, 0);
    assert_eq!(
        lvu::components::fields::anchored_row(&app.views, &provider)
            .unwrap()
            .id
            .sequence,
        2
    );
}

#[test]
fn release_keys_are_ignored_and_selector_arrows_do_not_move_logs() {
    let released = KeyEvent {
        code: KeyCode::Down,
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Release,
        state: KeyEventState::NONE,
    };
    assert_eq!(key_to_action(released, Focus::Logs), Action::None);
    assert_eq!(
        key_to_action(
            KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
            Focus::Selector
        ),
        Action::SelectSidebar(1)
    );
}

struct CountingProvider {
    rows: HashMap<String, Vec<DisplayRow>>,
    requests: RefCell<Vec<ViewportRequest>>,
}

#[derive(Clone, Copy)]
enum PageDelivery {
    Tail,
    RetainedOlder,
    Partial,
    Empty,
    Zero,
}

struct ProvenanceProvider {
    rows: Vec<DisplayRow>,
    delivery: Cell<PageDelivery>,
    revision: Cell<u64>,
}

impl RowProvider for ProvenanceProvider {
    fn page(&self, _: &str, request: ViewportRequest) -> RowPage {
        if matches!(self.delivery.get(), PageDelivery::Zero) {
            return RowPage {
                total: 0,
                rows: vec![],
            };
        }
        if request.len == 0 {
            return RowPage {
                total: self.rows.len(),
                rows: vec![],
            };
        }
        let (start, len) = match self.delivery.get() {
            PageDelivery::Tail => (request.start, request.len),
            PageDelivery::RetainedOlder if request.len >= 4 => (2, request.len),
            PageDelivery::RetainedOlder => (request.start, request.len),
            PageDelivery::Partial => (request.start, 1),
            PageDelivery::Empty => {
                return RowPage {
                    total: self.rows.len(),
                    rows: vec![],
                };
            }
            PageDelivery::Zero => unreachable!(),
        };
        let end = start.saturating_add(len).min(self.rows.len());
        RowPage {
            total: self.rows.len(),
            rows: self.rows[start.min(end)..end].to_vec(),
        }
    }

    fn row_by_id(&self, _: &str, id: &RowId) -> Option<DisplayRow> {
        self.rows.iter().find(|row| &row.id == id).cloned()
    }

    fn index_of_id(&self, _: &str, id: &RowId) -> Option<usize> {
        self.rows.iter().position(|row| &row.id == id)
    }

    fn revision(&self, _: &str) -> u64 {
        self.revision.get()
    }
}

#[test]
fn viewport_range_and_tail_selection_follow_served_row_provenance() {
    let rows = (0..12)
        .map(|sequence| DisplayRow {
            id: RowId::new("source", sequence),
            timestamp: String::new(),
            captured_at_unix_nanos: None,
            level: String::new(),
            text: format!("row {sequence}"),
            details: vec![],
            fields: vec![],
        })
        .collect();
    let provider = ProvenanceProvider {
        rows,
        delivery: Cell::new(PageDelivery::Tail),
        revision: Cell::new(1),
    };
    let mut app = App::new(
        vec![SourceItem {
            id: "source".into(),
            name: "source".into(),
            health: "ok".into(),
        }],
        vec![ViewItem {
            id: "view".into(),
            source_id: "source".into(),
            name: "All".into(),
        }],
        true,
    );

    app.sync_provider(&provider, 4);
    assert_eq!(
        app.view_state()
            .unwrap()
            .selected
            .as_ref()
            .unwrap()
            .sequence,
        11
    );

    provider.delivery.set(PageDelivery::RetainedOlder);
    provider.revision.set(2);
    app.sync_provider(&provider, 4);
    let state = app.view_state().unwrap();
    assert_eq!((state.top, state.rows_drawn), (2, 4));
    assert_eq!(state.selected.as_ref().unwrap().sequence, 11);

    provider.delivery.set(PageDelivery::Partial);
    provider.revision.set(3);
    app.sync_provider(&provider, 4);
    let state = app.view_state().unwrap();
    assert_eq!((state.top, state.rows_drawn), (8, 1));
    assert_eq!(state.selected.as_ref().unwrap().sequence, 11);

    // HISTORY navigation owns `top`: a retained old page may be drawn while
    // the requested window is loading, but must not abandon that destination.
    app.handle(Action::ToggleFollow, &provider);
    {
        let state = app.views.active_mut().unwrap();
        state.top = 6;
        state.selected = Some(RowId::new("source", 6));
    }
    provider.delivery.set(PageDelivery::RetainedOlder);
    provider.revision.set(4);
    app.sync_provider(&provider, 4);
    let state = app.view_state().unwrap();
    assert_eq!((state.top, state.served_top, state.rows_drawn), (6, 2, 4));
    assert_eq!(state.selected.as_ref().unwrap().sequence, 6);
    provider.delivery.set(PageDelivery::Tail);
    // No membership revision is required: the differing served/requested
    // anchors themselves keep the requested destination pending.
    app.sync_provider(&provider, 4);
    let state = app.view_state().unwrap();
    assert_eq!((state.top, state.served_top, state.rows_drawn), (6, 6, 4));
    assert_eq!(state.selected.as_ref().unwrap().sequence, 6);

    provider.delivery.set(PageDelivery::Empty);
    provider.revision.set(5);
    app.sync_provider(&provider, 4);
    assert_eq!(app.view_state().unwrap().rows_drawn, 0);
    assert!(render(&provider, &mut app, 160, 20).contains("0-0/12"));

    provider.delivery.set(PageDelivery::Zero);
    provider.revision.set(6);
    app.sync_provider(&provider, 4);
    let state = app.view_state().unwrap();
    assert_eq!((state.top, state.rows_drawn), (0, 0));
    assert_eq!(state.selected.as_ref().unwrap().sequence, 6);
}

impl RowProvider for CountingProvider {
    fn page(&self, view_id: &str, request: ViewportRequest) -> RowPage {
        self.requests.borrow_mut().push(request);
        let rows = &self.rows[view_id];
        let start = request.start.min(rows.len());
        let end = start.saturating_add(request.len).min(rows.len());
        RowPage {
            total: rows.len(),
            rows: rows[start..end].to_vec(),
        }
    }
    fn row_by_id(&self, view_id: &str, id: &RowId) -> Option<DisplayRow> {
        self.rows[view_id].iter().find(|row| &row.id == id).cloned()
    }
    fn index_of_id(&self, view_id: &str, id: &RowId) -> Option<usize> {
        self.rows[view_id].iter().position(|row| &row.id == id)
    }
    fn revision(&self, _: &str) -> u64 {
        1
    }
}

#[test]
fn renderer_requests_only_viewport_rows() {
    let rows = (0..10_000)
        .map(|sequence| DisplayRow {
            id: RowId::new("large", sequence),
            timestamp: "00:00:00".into(),
            captured_at_unix_nanos: Some(sequence as i64),
            level: "INFO".into(),
            text: format!("row {sequence}"),
            details: vec![],
            fields: vec![],
        })
        .collect();
    let provider = CountingProvider {
        rows: HashMap::from([("large-view".into(), rows)]),
        requests: RefCell::new(Vec::new()),
    };
    let mut app = App::new(
        vec![SourceItem {
            id: "large".into(),
            name: "Large".into(),
            health: "ok".into(),
        }],
        vec![ViewItem {
            id: "large-view".into(),
            source_id: "large".into(),
            name: "All".into(),
        }],
        false,
    );
    render(&provider, &mut app, 80, 20);
    let height = app.view_state().expect("state").viewport_height;
    assert!(
        provider
            .requests
            .borrow()
            .iter()
            .all(|request| request.len <= height.max(1))
    );
    assert!(
        provider
            .requests
            .borrow()
            .iter()
            .any(|request| request.len == height)
    );
}

#[test]
fn horizontal_navigation_keeps_selection_and_independent_view_positions() {
    let (provider, mut app) = demo();
    let selected = app.view_state().unwrap().selected.clone();
    app.handle(Action::MoveHorizontal(24), &provider);
    assert_eq!(app.view_state().unwrap().selected, selected);
    let mut terminal = Terminal::new(TestBackend::new(88, 24)).unwrap();
    terminal
        .draw(|frame| ui::render(frame, &mut app, &provider))
        .unwrap();
    assert!(screen(terminal.backend().buffer()).contains("x=24"));
    app.handle(Action::NextView, &provider);
    assert_eq!(app.view_state().unwrap().horizontal_offset, 0);
    app.handle(Action::PreviousView, &provider);
    assert_eq!(app.view_state().unwrap().horizontal_offset, 24);
    app.handle(Action::ResetHorizontal, &provider);
    assert_eq!(app.view_state().unwrap().horizontal_offset, 0);
}

#[test]
fn details_scroll_reaches_late_command_fields_without_moving_log_selection() {
    let rows = vec![
        DisplayRow {
            id: RowId::new("source", 1),
            timestamp: "now".into(),
            captured_at_unix_nanos: Some(1),
            level: "INFO".into(),
            text: "short first".into(),
            fields: vec![],
            details: vec![],
        },
        DisplayRow {
            id: RowId::new("source", 2),
            timestamp: "now".into(),
            captured_at_unix_nanos: Some(2),
            level: "INFO".into(),
            text: format!("{} {}", "long raw 東京 e\u{301}".repeat(40), "tail"),
            fields: (0..12)
                .map(|index| (format!("native_{index}"), format!("value_{index}")))
                .collect(),
            details: vec![
                ("command.late_result".into(), "typed 東京 result".into()),
                (
                    "command.status".into(),
                    "Pending · explicit run required".into(),
                ),
            ],
        },
    ];
    let provider = GrowingProvider {
        rows: RefCell::new(rows),
    };
    let mut app = App::new(
        vec![SourceItem {
            id: "source".into(),
            name: "source".into(),
            health: "ok".into(),
        }],
        vec![ViewItem {
            id: "view".into(),
            source_id: "source".into(),
            name: "view".into(),
        }],
        false,
    );
    app.handle(Action::ToggleDetails, &provider);
    let top = render(&provider, &mut app, 72, 20);
    assert!(top.contains("stable display id: source:2"), "{top}");
    assert!(
        !top.contains("command.late_result"),
        "late field unexpectedly fit: {top}"
    );
    let selected = app.view_state().unwrap().selected.clone();
    let details = app.hit_regions.details.expect("details hitbox");
    app.handle(
        Action::Mouse(mouse(
            MouseEventKind::ScrollDown,
            details.x + 1,
            details.y + 1,
        )),
        &provider,
    );
    assert_eq!(
        app.view_state().unwrap().selected,
        selected,
        "Details wheel moved the log row"
    );
    app.handle(Action::ScrollDetails(i32::MAX), &provider);
    let bottom = render(&provider, &mut app, 72, 20);
    assert!(bottom.contains("command.late_result: typed 東"), "{bottom}");
    assert!(bottom.contains("result"), "{bottom}");
    assert!(
        bottom.contains("command.status: Pending · explicit run required"),
        "{bottom}"
    );
    // §8.10: no key footer on the docked pane.
    assert!(!bottom.contains("↑/↓ scroll"), "{bottom}");

    app.handle(Action::MoveLine(-1), &provider);
    let changed = render(&provider, &mut app, 72, 20);
    assert!(changed.contains("stable display id: source:1"), "{changed}");
    assert_eq!(app.view_state().unwrap().details_scroll, 0);

    let narrow = render(&provider, &mut app, 54, 14);
    let narrow_details = app.hit_regions.details.expect("narrow details hitbox");
    // §8.10: no key footer at any size.
    assert!(!narrow.contains("↑/↓ scroll"), "{narrow}");
    assert!(app.hit_regions.log.unwrap().bottom() <= narrow_details.y);
    app.focus = Focus::Logs;
    app.handle(Action::CycleFocus, &provider);
    assert_eq!(app.focus, Focus::Details);
    // §8.11: Down moves the Details cursor, and scrolls the pane when the
    // record is not a tree or the cursor is at the end.
    assert_eq!(
        key_to_action(
            KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
            Focus::Details
        ),
        Action::DetailsCursor(1)
    );
    assert_eq!(
        key_to_action(
            KeyEvent::new(KeyCode::PageDown, KeyModifiers::ALT),
            Focus::Details
        ),
        Action::None
    );
}

#[test]
fn forbidden_navigation_keys_are_unbound_in_every_app_focus() {
    let focuses = [
        Focus::Selector,
        Focus::Logs,
        Focus::Details,
        Focus::Layer,
        Focus::Layer,
        Focus::Layer,
    ];
    for focus in focuses {
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
                    key_to_action(KeyEvent::new(code, modifiers), focus),
                    Action::None,
                    "{code:?} {modifiers:?} was bound in {focus:?}"
                );
            }
        }
    }
}

#[test]
fn editor_status_focus_blocks_mutation_and_cursor_until_focus_returns() {
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::Search), &provider);
    app.handle(Action::Raw(RawEvent::Paste("draft".into())), &provider);
    app.handle(raw_key(KeyCode::Enter), &provider);
    let request = app.take_query_requests().pop().unwrap();
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: request.view_id,
        generation: request.generation,
        revision: request.revision,
        purpose: request.purpose,
        result: Err(QueryFailure {
            purpose: request.purpose,
            message: format!("invalid expression: {}", "bounded details ".repeat(40)),
        }),
    }));
    let _ = render(&provider, &mut app, 54, 12);
    assert!(app.layers.filter.diagnostics_rect().is_some());
    assert!(app.layers.filter.scroll_limit() > 0);
    app.handle(raw_key(KeyCode::Tab), &provider);
    app.handle(raw_key(KeyCode::Tab), &provider);
    assert!(app.layers.filter.scroll_focused());
    app.handle(raw_key(KeyCode::Char('x')), &provider);
    app.handle(raw_key(KeyCode::Backspace), &provider);
    assert_eq!(app.search_state().unwrap().draft, "draft");
    let mut terminal = Terminal::new(TestBackend::new(54, 12)).unwrap();
    terminal
        .draw(|frame| ui::render(frame, &mut app, &provider))
        .unwrap();
    assert_eq!(terminal.backend().cursor_position(), Position::new(0, 0));
    app.handle(raw_key(KeyCode::Tab), &provider);
    assert!(!app.layers.filter.scroll_focused());
    assert!(!app.layers.filter.tabs_focused());
}

#[test]
fn rapid_search_edits_coalesce_and_empty_draft_retries_backpressure() {
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::Search), &provider);
    for ch in "rapid".chars() {
        app.handle(raw_key(KeyCode::Char(ch)), &provider);
        assert!(!app.flush_debounced_searches(Instant::now()));
        assert!(app.take_query_requests().is_empty());
    }
    for _ in 0..5 {
        app.handle(raw_key(KeyCode::Backspace), &provider);
    }
    assert!(!app.flush_debounced_searches(Instant::now()));
    assert!(app.flush_debounced_searches(Instant::now() + SEARCH_DEBOUNCE));
    let request = app.take_query_requests().pop().unwrap();
    assert!(request.constraints.text.is_none());
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: request.view_id.clone(),
        generation: request.generation,
        revision: request.revision,
        purpose: QueryPurpose::Search,
        result: Err(QueryFailure {
            purpose: QueryPurpose::Search,
            message: "query queue is full".into()
        }),
    }));
    assert!(app.flush_debounced_searches(Instant::now() + SEARCH_DEBOUNCE));
    let retry = app.take_query_requests().pop().unwrap();
    assert!(retry.constraints.text.is_none());
    assert!(retry.revision > request.revision);
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: retry.view_id,
        generation: retry.generation,
        revision: retry.revision,
        purpose: QueryPurpose::Search,
        result: Ok(()),
    }));
    assert!(app.search_state().unwrap().applied.is_empty());
    assert!(app.search_state().unwrap().error.is_none());
}

#[test]
fn time_dialog_timestamp_assistance_is_reviewed_enrichment_not_automatic_execution() {
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::Time), &provider);
    assert!(render(&provider, &mut app, 100, 28).contains("Recognize timestamp"));
    // Alt-T is the layer's chord; it hands off to the assistant, which is
    // still a legacy dialog (component-model.md §6.4 `Outcome::Legacy`).
    app.handle(raw_alt(KeyCode::Char('t')), &provider);
    assert_eq!(app.focus, Focus::Layer);
    assert!(!app.layers.time.is_open());
    let dialog = app.layers.ask.state().unwrap();
    assert_eq!(dialog.kind, AskAiKind::Enrichment);
    assert_eq!(dialog.stage, AskAiStage::Input);
    assert!(dialog.prompt.contains("timestamp_utc"));
    assert!(dialog.prompt.contains("Never infer"));
    assert!(app.take_query_requests().is_empty());
}

#[test]
fn search_reaffirmation_does_not_erase_invalid_draft_diagnostic() {
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::Search), &provider);
    app.handle(Action::Raw(RawEvent::Paste("valid".into())), &provider);
    app.handle(raw_key(KeyCode::Enter), &provider);
    let good = app.take_query_requests().pop().unwrap();
    app.apply_query_completion(QueryCompletion {
        view_id: good.view_id,
        generation: good.generation,
        revision: good.revision,
        purpose: good.purpose,
        result: Ok(()),
    });
    for _ in 0..5 {
        app.handle(raw_key(KeyCode::Backspace), &provider);
    }
    app.handle(Action::Raw(RawEvent::Paste("/[/".into())), &provider);
    app.handle(raw_key(KeyCode::Enter), &provider);
    let bad = app.take_query_requests().pop().unwrap();
    app.apply_query_completion(QueryCompletion {
        view_id: bad.view_id,
        generation: bad.generation,
        revision: bad.revision,
        purpose: bad.purpose,
        result: Err(QueryFailure {
            purpose: QueryPurpose::Search,
            message: "invalid search regex".into(),
        }),
    });
    let reaffirm = app.take_query_requests().pop().unwrap();
    app.apply_query_completion(QueryCompletion {
        view_id: reaffirm.view_id,
        generation: reaffirm.generation,
        revision: reaffirm.revision,
        purpose: reaffirm.purpose,
        result: Ok(()),
    });
    let search = app.search_state().unwrap();
    assert_eq!(search.applied, "valid");
    assert_eq!(search.draft, "/[/");
    assert_eq!(search.error.as_deref(), Some("invalid search regex"));
}

#[test]
fn blank_enrichment_add_keeps_all_successful_stages() {
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::Enrichment), &provider);
    enrichment_add(&mut app, &provider);
    app.handle(
        Action::Raw(RawEvent::Paste("derived = pl.lit('ok')".into())),
        &provider,
    );
    step_submit(&mut app, &provider);
    let request = app.take_query_requests().pop().unwrap();
    app.apply_query_completion(QueryCompletion {
        view_id: request.view_id,
        generation: request.generation,
        revision: request.revision,
        purpose: request.purpose,
        result: Ok(()),
    });
    let stages = app.view_state().unwrap().enrichments.clone();
    assert!(app.view_state().unwrap().enrichment.draft.is_empty());
    step_submit(&mut app, &provider);
    assert_eq!(app.view_state().unwrap().enrichments, stages);
    assert!(app.take_query_requests().is_empty());
}

#[test]
fn extracted_time_basis_is_explicit_transactional_and_persistent() {
    let (provider, mut app) = demo();
    let view_id = app.active_view_id().unwrap().to_owned();
    app.handle(Action::Open(Open::Time), &provider);
    app.handle(raw_alt(KeyCode::Char('u')), &provider);
    app.handle(
        Action::Raw(RawEvent::Paste("2026-09-05T12:30:45Z".into())),
        &provider,
    );
    app.layers.time.switch_field();
    app.handle(
        Action::Raw(RawEvent::Paste("2026-09-05T12:30:46Z".into())),
        &provider,
    );
    time_activate(&mut app, &provider, TimeControl::Apply);
    let request = app.take_query_requests().pop().unwrap();
    assert_eq!(request.constraints.time_basis, lvu::TimeBasis::Extracted);
    assert_eq!(
        app.view_state().unwrap().applied_time_basis,
        lvu::TimeBasis::Capture,
        "basis changes only after native membership publishes"
    );
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: request.view_id,
        generation: request.generation,
        revision: request.revision,
        purpose: request.purpose,
        result: Ok(()),
    }));
    assert_eq!(
        app.view_state().unwrap().applied_time_basis,
        lvu::TimeBasis::Extracted
    );
    assert_eq!(
        app.persistent_view_state(&view_id)
            .unwrap()
            .applied_time_basis,
        lvu::TimeBasis::Extracted
    );

    app.handle(Action::Open(Open::Time), &provider);
    app.handle(raw_alt(KeyCode::Char('u')), &provider);
    assert_eq!(app.layers.time.state().basis, lvu::TimeBasis::Extracted);
    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    terminal
        .draw(|frame| ui::render(frame, &mut app, &provider))
        .unwrap();
    assert!(screen(terminal.backend().buffer()).contains("Extracted timestamp_utc"));
}

#[test]
fn capture_controls_are_bounded_source_scoped_and_do_not_escape_editors() {
    let (provider, mut app) = demo();
    let original_view = app.active_view_id().unwrap().to_owned();
    app.handle(Action::StopCapture, &provider);
    app.handle(Action::RestartCapture, &provider);
    let requests = app.take_source_controls();
    assert_eq!(requests.len(), 1, "one pending operation per source");
    assert!(!requests[0].restart);
    assert_eq!(requests[0].source_id, app.views()[0].source_id);
    assert_eq!(app.active_view_id(), Some(original_view.as_str()));
    assert_eq!(
        key_to_action(
            KeyEvent::new(KeyCode::Char('s'), KeyModifiers::ALT),
            Focus::Logs
        ),
        Action::StopCapture
    );
    assert_eq!(
        key_to_action(
            KeyEvent::new(KeyCode::Char('r'), KeyModifiers::ALT),
            Focus::Selector
        ),
        Action::RestartCapture
    );
    app.handle(Action::Open(Open::Search), &provider);
    app.handle(Action::RestartCapture, &provider);
    assert!(app.take_source_controls().is_empty());
    assert_ne!(
        key_to_action(
            KeyEvent::new(KeyCode::Char('r'), KeyModifiers::ALT),
            Focus::Layer
        ),
        Action::RestartCapture
    );
}

#[test]
fn capture_control_failure_is_visible_before_long_status_and_clears_on_input() {
    let (provider, mut app) = demo();
    app.action_notice = Some("stdin cannot restart; provide a fresh pipeline".into());
    assert!(render(&provider, &mut app, 80, 24).contains("stdin cannot restart"));
    app.handle(Action::Resize(60, 20), &provider);
    assert!(app.action_notice.is_some());
    app.handle(Action::Open(Open::Search), &provider);
    assert!(app.action_notice.is_none());
}

#[test]
fn raw_context_on_a_sources_only_view_leaves_filter_and_anchor_alone() {
    let (provider, mut app) = demo();
    let mut dispatcher = provider.query_dispatcher();
    app.sync_provider(&provider, 10);
    app.handle(Action::Open(Open::Search), &provider);
    app.handle(Action::Raw(RawEvent::Paste("request 05".into())), &provider);
    finish_debounced_search(&mut app, &mut dispatcher);
    app.handle(raw_key(KeyCode::Esc), &provider);
    app.sync_provider(&provider, 10);
    // `o` is a jump to All events (raw-context-as-jump.md). This filtered
    // view is its source's only view, so there is nowhere to jump; the
    // filter, the selection and the rows stay exactly as they were.
    let anchor = app.view_state().unwrap().selected.clone().unwrap();
    let view = app.active_view_id().unwrap().to_owned();
    app.set_view_role(&view, lvu::ViewRole::Canonical);
    app.handle(
        Action::RawContext {
            anchor: None,
            layer: None,
        },
        &provider,
    );
    assert_eq!(app.focus, Focus::Logs);
    assert_eq!(app.raw_context_origin(), None);
    assert_eq!(app.view_state().unwrap().selected.as_ref(), Some(&anchor));
    assert_eq!(app.view_state().unwrap().search.applied, "request 05");
    assert_eq!(app.visible_rows(&provider).len(), 1);
    assert_eq!(
        key_to_action(
            KeyEvent::new(KeyCode::Char('o'), KeyModifiers::NONE),
            Focus::Logs
        ),
        Action::RawContext {
            anchor: None,
            layer: None
        }
    );
}

#[test]
fn recipe_export_captures_reviewed_identity_and_does_not_replace_newer_drafts() {
    use lvu::{RecipeConfig, RecipeDialogMode, RecipeItem, RecipeRequest};
    let (provider, mut app) = demo();
    app.handle(
        Action::Open(Open::Recipes {
            mode: RecipeDialogMode::Browse,
        }),
        &provider,
    );
    let RecipeRequest::List { meta } = app.take_recipe_requests().pop().unwrap() else {
        panic!("list");
    };
    app.set_recipes(
        meta,
        vec![RecipeItem {
            id: "recipe-one".into(),
            revision: "revision-one".into(),
            saved_at_unix_nanos: None,
            name: "Portable".into(),
            config: RecipeConfig::default(),
            incompatibility: None,
        }],
        None,
    );
    app.handle(raw_alt(KeyCode::Char('e')), &provider);
    app.handle(
        Action::Raw(RawEvent::Paste("/tmp/portable café.toml".into())),
        &provider,
    );
    assert!(render(&provider, &mut app, 100, 24).contains("Selected: Portable"));
    recipe_apply(&mut app, &provider);
    let RecipeRequest::Export {
        meta,
        path,
        recipe_id,
        revision,
    } = app.take_recipe_requests().pop().unwrap()
    else {
        panic!("export");
    };
    assert_eq!(path, "/tmp/portable café.toml");
    assert_eq!(recipe_id, "recipe-one");
    assert_eq!(revision, "revision-one");
    // `x` is the layer's Reject binding, so it never reached the name field;
    // the retired `Action::RecipeInput` bypassed the key table.
    app.handle(raw_char('z'), &provider);
    app.recipe_exported(meta, "exported old request".into());
    assert!(app.layers.recipes.state().name.ends_with(".tomlz"));
    assert_ne!(app.layers.recipes.state().status, "exported old request");
    assert!(app.take_query_requests().is_empty());
    // Alt-E is the layer's own binding now, so it is asserted by the mode it
    // selects rather than by the retired `Action` it used to produce.
    app.handle(raw_alt(KeyCode::Char('b')), &provider);
    app.handle(raw_alt(KeyCode::Char('e')), &provider);
    assert_eq!(app.layers.recipes.state().mode, RecipeDialogMode::Export);
}

#[test]
fn bookmarks_notes_restore_and_open_hidden_record_context_without_changing_search() {
    let (provider, mut app) = demo();
    app.sync_provider(&provider, 10);
    app.handle(Action::Top, &provider);
    let view = app.active_view_id().unwrap().to_owned();
    let fence = app.view_interaction_revision(&view).unwrap();
    app.handle(Action::ToggleBookmark, &provider);
    assert!(app.view_interaction_revision(&view).unwrap() > fence);
    app.handle(Action::Open(Open::Bookmarks), &provider);
    app.handle(raw_alt(KeyCode::Char('e')), &provider);
    let before_edit = app.view_interaction_revision(&view).unwrap();
    app.handle(
        Action::Raw(RawEvent::Paste("Café request to investigate".into())),
        &provider,
    );
    assert!(app.view_interaction_revision(&view).unwrap() > before_edit);
    assert!(render(&provider, &mut app, 70, 12).contains("Café request"));
    app.handle(raw_key(KeyCode::Enter), &provider);
    assert_eq!(
        app.bookmarks_for_view(&view)[0].note,
        "Café request to investigate"
    );
    app.handle(Action::CancelEditor, &provider);
    let saved = app.persistent_view_state(&view).unwrap();
    let mut dispatcher = provider.query_dispatcher();
    app.handle(Action::Open(Open::Search), &provider);
    app.handle(Action::Raw(RawEvent::Paste("request 05".into())), &provider);
    finish_debounced_search(&mut app, &mut dispatcher);
    app.handle(raw_key(KeyCode::Esc), &provider);
    app.sync_provider(&provider, 10);
    app.handle(Action::Open(Open::Bookmarks), &provider);
    // Explicit raw-context inspection is still reachable from its own control;
    // only the default activation now jumps to the record instead.
    bookmark_focus(
        &mut app,
        &provider,
        lvu::app::BookmarkDialogControl::Context,
    );
    // The button is a jump (raw-context-as-jump.md); this view is its
    // source's only one, so Bookmarks comes straight back with the reason.
    app.set_view_role(&view, lvu::ViewRole::Canonical);
    app.handle(raw_key(KeyCode::Enter), &provider);
    assert_eq!(app.focus, Focus::Layer);
    assert!(app.layers.bookmarks.is_open());
    assert!(render(&provider, &mut app, 80, 16).contains("Bookmarks"));
    app.handle(Action::CancelEditor, &provider);
    assert_eq!(app.search_state().unwrap().applied, "request 05");
    let (_, mut restored) = demo();
    assert!(restored.restore_persistent_view(&view, saved.clone()));
    assert_eq!(restored.bookmarks_for_view(&view), saved.bookmarks);
    let mut invalid = saved;
    invalid.bookmarks[0].note = "x".repeat(1025);
    assert!(!restored.restore_persistent_view(&view, invalid));
    assert_eq!(
        restored.bookmarks_for_view(&view)[0].note,
        "Café request to investigate"
    );
}

#[test]
fn bookmark_list_scroll_and_mouse_targets_exclude_footer_and_note_edit_is_anchored() {
    let (provider, mut app) = demo();
    let view = app.active_view_id().unwrap().to_owned();
    let saved = PersistentViewState {
        bookmarks: (0..128)
            .map(|sequence| lvu::Bookmark {
                id: RowId::new("api", sequence),
                note: format!("note {sequence}"),
            })
            .collect(),
        ..Default::default()
    };
    assert!(app.restore_persistent_view(&view, saved));
    app.handle(Action::Open(Open::Bookmarks), &provider);
    // The layer moves one row per key, so walking to the last bookmark is what
    // `Action::MoveBookmark(127)` used to do in one step.
    for _ in 0..127 {
        app.handle(raw_key(KeyCode::Down), &provider);
    }
    // §12.10 puts the note on the row's second line, so the two no longer
    // share one string; both must still be on screen for the last bookmark.
    let scrolled = render(&provider, &mut app, 80, 12);
    assert!(scrolled.contains("#127"), "{scrolled}");
    assert!(scrolled.contains("note 127"), "{scrolled}");
    let (area, index) = app.layers.bookmarks.row_rects()[0];
    assert!(area.y < 10);
    app.handle(
        Action::Raw(RawEvent::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: area.x,
            row: area.y,
            modifiers: KeyModifiers::NONE,
        })),
        &provider,
    );
    assert_eq!(app.layers.bookmarks.state().selected, index);
    app.handle(raw_alt(KeyCode::Char('e')), &provider);
    app.handle(raw_key(KeyCode::Down), &provider);
    assert_eq!(app.layers.bookmarks.state().selected, index);
    app.handle(Action::EditorPaste("x".repeat(1025)), &provider);
    assert_eq!(app.layers.bookmarks.state().draft, format!("note {index}"));
    render(&provider, &mut app, 80, 12);
    assert!(app.layers.bookmarks.row_rects().is_empty());
    app.handle(Action::CancelEditor, &provider);
    assert_eq!(
        app.bookmarks_for_view(&view)[index].note,
        format!("note {index}")
    );
}

#[test]
fn recipe_history_is_fenced_and_update_captures_reviewed_revision() {
    use lvu::{RecipeConfig, RecipeItem, RecipeRequest};
    let (provider, mut app) = demo();
    let item = RecipeItem {
        id: "recipe".into(),
        revision: "current".into(),
        saved_at_unix_nanos: None,
        name: "Saved".into(),
        config: RecipeConfig::default(),
        incompatibility: None,
    };
    app.handle(
        Action::Open(Open::Recipes {
            mode: RecipeDialogMode::Browse,
        }),
        &provider,
    );
    let RecipeRequest::List { meta } = app.take_recipe_requests().pop().unwrap() else {
        panic!("list");
    };
    app.set_recipes(meta, vec![item.clone()], None);
    // History is its own layer, reached by `Replace`: one layer is on the
    // stack at a time, and the dialog's state crosses the transition (§6.5).
    app.handle(raw_alt(KeyCode::Char('h')), &provider);
    assert_eq!(app.layers.stack_ids(), vec![LayerId::RecipeHistory]);
    let RecipeRequest::History { meta, recipe_id } = app.take_recipe_requests().pop().unwrap()
    else {
        panic!("history");
    };
    assert_eq!(recipe_id, "recipe");
    app.handle(raw_alt(KeyCode::Char('b')), &provider);
    assert_eq!(app.layers.stack_ids(), vec![LayerId::Recipes]);
    app.set_recipes(meta, Vec::new(), None);
    assert!(
        app.layers.recipes.state().loading,
        "stale history cannot replace browse request"
    );
    let RecipeRequest::List { meta } = app.take_recipe_requests().pop().unwrap() else {
        panic!("list");
    };
    app.set_recipes(meta, vec![item.clone()], None);
    app.handle(raw_alt(KeyCode::Char('u')), &provider);
    assert!(render(&provider, &mut app, 100, 24).contains("NEW revision"));
    app.handle(raw_char('x'), &provider);
    recipe_apply(&mut app, &provider);
    let RecipeRequest::Save { update, name, .. } = app.take_recipe_requests().pop().unwrap() else {
        panic!("update");
    };
    assert_eq!(update, Some(("recipe".into(), "current".into())));
    assert_eq!(name, "Saved");
    app.handle(raw_alt(KeyCode::Char('b')), &provider);
    let RecipeRequest::List { meta } = app.take_recipe_requests().pop().unwrap() else {
        panic!("list");
    };
    app.set_recipes(meta, vec![item], None);
    app.handle(raw_alt(KeyCode::Char('h')), &provider);
    let RecipeRequest::History { meta, .. } = app.take_recipe_requests().pop().unwrap() else {
        panic!("history");
    };
    app.set_recipes(
        meta,
        (0..30)
            .map(|n| RecipeItem {
                id: "recipe".into(),
                revision: format!("revision-{n}"),
                saved_at_unix_nanos: None,
                name: format!("Saved-{n}"),
                config: RecipeConfig::default(),
                incompatibility: None,
            })
            .collect(),
        None,
    );
    for _ in 0..29 {
        app.handle(raw_key(KeyCode::Down), &provider);
    }
    assert!(render(&provider, &mut app, 80, 14).contains("Saved-29"));
    recipe_apply(&mut app, &provider);
    assert_eq!(app.focus, Focus::Logs);
}

#[test]
fn recipe_adaptation_reviews_ordered_chain_and_rolls_back_atomically() {
    use lvu::{AskAiKind, EnrichmentDefinition, EnrichmentStageId, RecipeConfig};
    let (provider, mut app) = demo();
    // The recipe-adaptation entry point, rather than a hand-built dialog: the
    // layer is opened the way `AdaptRecipeSuggestion` opens it.
    app.handle(
        Action::Open(Open::Ask(AskOpen::Recipe {
            config: Box::new(RecipeConfig {
                search: "candidate".into(),
                pinned_columns: vec!["number".into()],
                ..Default::default()
            }),
            outcome: lvu::app::RecipeOutcome {
                source_id: String::new(),
                recipe_id: String::new(),
                revision: String::new(),
                accepted: true,
            },
            prompt: "adapt it".into(),
        })),
        &provider,
    );
    let dialog = app.layers.ask.state().unwrap();
    let generation = dialog.generation;
    let revision = dialog.definition_revision;
    let view = dialog.view_id.clone();
    app.handle(raw_alt(KeyCode::Char('f')), &provider);
    assert_eq!(app.layers.ask.state().unwrap().kind, AskAiKind::Recipe);
    let legacy_kind = key_to_action(
        KeyEvent::new(KeyCode::Char('e'), KeyModifiers::ALT),
        Focus::Layer,
    );
    app.handle(legacy_kind, &provider);
    assert_eq!(
        app.layers.ask.state().unwrap().kind,
        AskAiKind::Recipe,
        "legacy kind actions cannot change fixed recipe adaptation"
    );
    let stages = vec![
        EnrichmentDefinition {
            id: EnrichmentStageId("first".into()),
            source: "/(?P<code>[0-9]+)/".into(),
            command: None,
        },
        EnrichmentDefinition {
            id: EnrichmentStageId("second".into()),
            source: format!(
                "number = pl.col('code').cast(pl.Int64){} # LAST-STAGE",
                " ".repeat(600)
            ),
            command: None,
        },
    ];
    assert!(app.finish_recipe_ai(
        generation,
        &view,
        revision,
        Ok((
            "pl.col('number') > 5".into(),
            "derives and filters".into(),
            Some(stages.clone())
        ))
    ));
    let first = render(&provider, &mut app, 80, 18);
    assert!(first.contains("Proposal"), "{first}");
    assert!(first.contains("Recipe adaptation"), "{first}");
    assert!(first.contains("[ Apply ]"), "{first}");
    assert!(
        !ask_offers(&app, lvu::app::AskControl::Kind),
        "recipe adaptation kind is fixed"
    );
    app.handle(raw_key(KeyCode::Down), &provider);
    let last = render(&provider, &mut app, 80, 18);
    assert!(last.contains("are retained"), "{last}");
    app.handle(raw_key(KeyCode::Up), &provider);
    ask_submit(&mut app, &provider);
    let request = app.take_query_requests().pop().unwrap();
    assert_eq!(request.constraints.enrichments, stages);
    assert_eq!(
        request.constraints.advanced_polars.as_deref(),
        Some("pl.col('number') > 5")
    );
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: request.view_id,
        generation: request.generation,
        revision: request.revision,
        purpose: request.purpose,
        result: Err(QueryFailure {
            purpose: QueryPurpose::Enrichment,
            message: "bad candidate".into()
        })
    }));
    let state = app.view_state().unwrap();
    assert!(state.enrichments.is_empty());
    assert!(state.pinned_columns.is_empty());
    assert!(state.search.applied.is_empty());
    let rollback = app.take_query_requests().pop().unwrap();
    assert!(rollback.constraints.enrichments.is_empty());
    assert!(rollback.constraints.advanced_polars.is_none());
}

#[test]
fn merged_source_editor_scrolls_and_changes_only_accepted_membership() {
    let (provider, mut app) = demo();
    let view = app.active_view_id().unwrap().to_string();
    let primary = app.view_source_ids(&view)[0].clone();
    for index in 0..20 {
        app.sources.push(SourceItem {
            id: format!("extra-{index}"),
            name: format!("extra source {index}"),
            health: "open".into(),
        });
    }
    app.handle(Action::Open(Open::View), &provider);
    app.handle(raw_alt(KeyCode::Char('m')), &provider);
    for _ in 0..100 {
        app.handle(raw_key(KeyCode::Down), &provider);
    }
    let rendered = render(&provider, &mut app, 80, 12);
    assert!(rendered.contains("extra source 19"));
    assert!(render(&provider, &mut app, 40, 6).contains("extra source 19"));
    render(&provider, &mut app, 80, 12);
    let (_, last) = app.layers.view.source_rects().last().unwrap();
    assert_eq!(*last, app.sources.len() - 1);
    app.handle(raw_key(KeyCode::Char(' ')), &provider);
    app.handle(raw_alt(KeyCode::Up), &provider);
    app.handle(raw_key(KeyCode::Enter), &provider);
    let mutation = app.layers.view.outbox.take().pop().unwrap();
    assert_eq!(
        mutation.source_ids,
        vec!["extra-19".to_string(), primary.clone()]
    );
    let before = app.persistent_view_state(&view).unwrap();
    let query = app
        .begin_source_change(&view, mutation.source_ids.clone())
        .unwrap();
    assert_eq!(
        app.persistent_view_state(&view).unwrap().source_ids,
        before.source_ids
    );
    assert!(app.view_has_pending_query(&view));
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: view.clone(),
        generation: query.generation,
        revision: query.revision,
        purpose: query.purpose,
        result: Err(QueryFailure {
            purpose: QueryPurpose::Advanced,
            message: "source failed".into()
        })
    }));
    assert_eq!(app.persistent_view_state(&view).unwrap(), before);
    assert!(!app.view_has_pending_query(&view));
    let query = app
        .begin_source_change(&view, mutation.source_ids.clone())
        .unwrap();
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: view.clone(),
        generation: query.generation,
        revision: query.revision,
        purpose: query.purpose,
        result: Ok(())
    }));
    assert_eq!(app.view_source_ids(&view), mutation.source_ids);
    assert!(app.take_query_requests().is_empty());
    assert!(
        app.begin_source_change(&view, vec!["extra-19".into()])
            .is_err()
    );
}

#[test]
fn deferring_an_inactive_view_preserves_the_selected_view_identity() {
    let (_, mut app) = demo();
    let hidden = app.views()[0].id.clone();
    app.set_selected_view(1);
    let selected = app.active_view_id().unwrap().to_owned();
    app.defer_view_restore(&hidden);
    assert_eq!(app.active_view_id(), Some(selected.as_str()));
}

#[test]
fn enrichment_workspace_separates_data_results_and_multiline_input() {
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::Enrichment), &provider);
    enrichment_add(&mut app, &provider);
    let draft = format!(
        "normalized = pl.col('raw').str.replace('{}', 'END_EXPRESSION', literal=True)",
        "界e\u{301}".repeat(32)
    );
    app.handle(Action::Raw(RawEvent::Paste(draft)), &provider);
    let backend = TestBackend::new(120, 32);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| ui::render(frame, &mut app, &provider))
        .unwrap();
    let text = screen(terminal.backend().buffer());
    // The capped multi-line input windows on the caret, so the tail is what is
    // visible; the panes and the message keep their own rows.
    assert!(text.contains("END_EXPRESSION"), "{text}");
    assert!(text.contains("Accepted output"), "{text}");
    assert!(text.contains("Input record"), "{text}");
    assert!(text.contains("Save this step to evaluate"), "{text}");
    let cursor = terminal.backend().cursor_position();
    let line = text.lines().nth(cursor.y as usize).unwrap();
    assert!(
        !line.contains("[ Save ]"),
        "cursor must remain in the expression field"
    );
    // Head-clipped input still shows its tail rather than an empty field.
    app.handle(raw_ctrl(KeyCode::Char('a')), &provider);
    terminal
        .draw(|frame| ui::render(frame, &mut app, &provider))
        .unwrap();
    let head = screen(terminal.backend().buffer());
    assert!(head.contains("normalized = pl.col('raw')"), "{head}");
}

#[test]
fn selection_surface_tracks_visible_dialog_and_clears_on_close_or_tiny_terminal() {
    let (provider, mut app) = demo();
    for (width, height) in [(120, 32), (54, 12)] {
        app.handle(Action::Open(Open::Enrichment), &provider);
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| ui::render(frame, &mut app, &provider))
            .unwrap();
        let bounds = app
            .hit_regions
            .selection_modal
            .expect("visible editor surface");
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(bounds.x - 1, bounds.y)].symbol(), "│");
        assert_eq!(buffer[(bounds.right(), bounds.y)].symbol(), "│");
        assert_eq!(buffer[(bounds.x - 1, bounds.bottom())].symbol(), "└");
        assert!(
            !bounds.contains((0, 0).into()),
            "background header excluded"
        );
        assert!(bounds.bottom() < height);
        app.handle(raw_key(KeyCode::Esc), &provider);
        render(&provider, &mut app, width, height);
        assert!(app.hit_regions.selection_modal.is_none());
    }
    app.handle(Action::Open(Open::Enrichment), &provider);
    render(&provider, &mut app, 120, 32);
    assert!(app.hit_regions.selection_modal.is_some());
    render(&provider, &mut app, 10, 3);
    assert!(app.hit_regions.selection_modal.is_none());
}

#[test]
fn corner_heart_reserves_selector_space_without_covering_logs_or_modal() {
    use lvu::delight::{ActivityState, DelightConfig};
    let (provider, mut app) = demo();
    let original = ui::layout(ratatui::layout::Rect::new(0, 0, 100, 30), false);
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    terminal
        .draw(|frame| {
            ui::render_with_theme(
                frame,
                &mut app,
                &provider,
                lvu::theme::Theme::LOVE_DARK,
                Some((
                    Duration::from_millis(750),
                    DelightConfig::default(),
                    ActivityState::Active {
                        label: "agent working",
                    },
                )),
            )
        })
        .unwrap();
    assert_eq!(app.hit_regions.log_rows, Some(original.log_rows));
    let sidebar = app.hit_regions.sidebar.unwrap();
    assert_eq!(
        Some(sidebar),
        original.sidebar,
        "pane border must extend to full height"
    );
    assert!(
        app.hit_regions
            .sidebar_views
            .iter()
            .all(|(area, _)| area.bottom()
                <= sidebar.bottom() - 1 - lvu::delight::CORNER_HEART_HEIGHT)
    );
    assert!(!screen(terminal.backend().buffer()).contains("agent working"));
    assert!(screen(terminal.backend().buffer()).contains("FOLLOW"));
    let buffer = terminal.backend().buffer();
    for x in sidebar.x..sidebar.right() {
        assert_eq!(
            buffer[(x, original.status.y)].bg,
            lvu::theme::Theme::LOVE_DARK.base_bg,
            "main status must not paint beneath the sidebar"
        );
    }
    assert_eq!(
        buffer[(original.log.x, original.status.y)].bg,
        lvu::theme::Theme::LOVE_DARK.accent
    );
    assert_eq!(buffer[(sidebar.x, sidebar.bottom() - 1)].symbol(), "└");
    let heart_left = sidebar.x + 1;
    for y in sidebar.bottom() - 1 - lvu::delight::CORNER_HEART_HEIGHT..sidebar.bottom() - 1 {
        assert_eq!(buffer[(sidebar.x, y)].symbol(), "│");
        for x in sidebar.x + 1..sidebar.right() - 1 {
            if buffer[(x, y)].symbol() != " " {
                assert!(
                    (heart_left..heart_left + 5).contains(&x),
                    "heart must stay in the bottom-left sidebar interior"
                );
            }
        }
    }
    app.handle(Action::Open(Open::Help), &provider);
    terminal
        .draw(|frame| {
            ui::render_with_theme(
                frame,
                &mut app,
                &provider,
                lvu::theme::Theme::LOVE_DARK,
                Some((
                    Duration::from_millis(750),
                    DelightConfig::default(),
                    ActivityState::Idle,
                )),
            )
        })
        .unwrap();
    assert!(app.hit_regions.selection_modal.is_some());
}

#[test]
fn tiny_time_dialog_preserves_editing_and_explains_hidden_actions() {
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::Time), &provider);
    let mut terminal = Terminal::new(TestBackend::new(30, 10)).unwrap();
    terminal
        .draw(|frame| ui::render(frame, &mut app, &provider))
        .unwrap();
    let buffer = terminal.backend().buffer();
    let rendered = screen(buffer);
    assert!(rendered.contains("Time basis"), "{rendered}");
    assert!(!rendered.contains("Enter"), "{rendered}");
    assert!(!rendered.contains("Tab"), "{rendered}");
    assert!(!rendered.contains("Esc"), "{rendered}");
    // A 30x10 terminal cannot show every row at once. §8.8 scrolls a focused
    // control into view, so every field stays reachable by traversal alone —
    // which is what the retired Scroll up/down pseudo-buttons used to do.
    let mut seen = rendered.clone();
    for _ in 0..10 {
        app.handle(raw_key(KeyCode::Tab), &provider);
        terminal
            .draw(|frame| ui::render(frame, &mut app, &provider))
            .unwrap();
        seen.push_str(&screen(terminal.backend().buffer()));
    }
    assert!(seen.contains("Start"), "{seen}");
    assert!(seen.contains("Recognize"), "{seen}");
    assert!(!seen.contains("Scroll up"), "{seen}");
}

#[test]
fn wide_time_form_groups_bounds_and_hides_false_overflow_controls() {
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::Time), &provider);
    time_choose(&mut app, &provider, TimeControl::Window, 1);
    time_focus(&mut app, &provider, TimeControl::StartDate);
    let start_rendered = render(&provider, &mut app, 100, 28);
    let start = start_rendered
        .lines()
        .find(|line| line.contains("Start"))
        .unwrap_or_else(|| panic!("missing Start row:\n{start_rendered}"));
    time_focus(&mut app, &provider, TimeControl::EndDate);
    let rendered = render(&provider, &mut app, 100, 28);
    let end = rendered
        .lines()
        .find(|line| line.contains("End"))
        .unwrap_or_else(|| panic!("missing End row:\n{rendered}"));
    assert!(start.contains('-') && start.contains(':') && start.contains("UTC"));
    assert!(end.contains('-') && end.contains(':') && end.contains("UTC"));
    assert!(rendered.contains("[ Apply ]"));
    assert!(rendered.contains("[ Clear ]"));
    assert!(rendered.contains("Recognize timestamp"), "{rendered}");
    assert!(rendered.contains("Applied"));
    assert!(!rendered.contains("Applied:"), "{rendered}");
    assert!(!rendered.contains("Scroll up"));
    assert!(!rendered.contains("Scroll down"));
}

#[test]
fn shared_time_editing_excludes_menus_and_publishes_only_drafts() {
    use lvu::app::TimeWindowChoice;
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::Time), &provider);
    // A menu or a button takes no text, so `q` dismisses instead of typing.
    for control in [
        TimeControl::StartZoneMenu,
        TimeControl::EndZoneMenu,
        TimeControl::Apply,
    ] {
        time_focus(&mut app, &provider, control);
        assert!(app.layers.time.editing_segment().is_none());
    }
    // A preset zone is a value, not a field: Tab never lands on it at all.
    time_choose(&mut app, &provider, TimeControl::StartZoneMenu, 0);
    for _ in 0..24 {
        app.handle(raw_key(KeyCode::Tab), &provider);
        assert_ne!(app.layers.time.state().focus, TimeControl::StartZone);
    }
    let custom = lvu::components::time::time_zone_choice_count();
    time_choose(&mut app, &provider, TimeControl::StartZoneMenu, custom);
    assert_eq!(
        app.layers.time.editing_segment(),
        Some(TimeControl::StartZone)
    );
    // An open dropdown takes the keys, so the segment under it is not editing.
    time_activate(&mut app, &provider, TimeControl::StartZoneMenu);
    assert!(app.layers.time.editing_segment().is_none());
    app.handle(raw_key(KeyCode::Esc), &provider);
    time_choose(&mut app, &provider, TimeControl::Window, 3);
    time_focus(&mut app, &provider, TimeControl::StartZone);
    app.handle(raw_ctrl(KeyCode::Char('a')), &provider);
    assert_eq!(
        app.view_state().unwrap().time_window_draft,
        TimeWindowChoice::Recent(900)
    );
    app.handle(raw_ctrl(KeyCode::Char('k')), &provider);
    assert_eq!(app.layers.time.state().start_zone, "");
    assert_eq!(
        app.view_state().unwrap().time_window_draft,
        TimeWindowChoice::Absolute
    );
    assert!(app.take_query_requests().is_empty());
    app.handle(Action::CancelEditor, &provider);
    app.handle(Action::Open(Open::Time), &provider);
    assert_eq!(app.layers.time.state().window, TimeWindowChoice::Absolute);
    assert_eq!(app.layers.time.state().start_zone, "");
}

#[test]
fn zone_dropdown_stages_rolls_back_and_custom_offset_remains_exact() {
    use lvu::app::TimeWindowChoice;
    use lvu::components::time::TimeDropdown;
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::Time), &provider);
    time_focus(&mut app, &provider, TimeControl::StartZoneMenu);
    app.handle(raw_key(KeyCode::Enter), &provider);
    assert_eq!(
        app.layers.time.state().dropdown,
        Some(TimeDropdown::StartZone)
    );
    let original = app.layers.time.state().start_zone.clone();
    app.handle(raw_key(KeyCode::Down), &provider);
    assert_eq!(app.layers.time.state().start_zone, original);
    app.handle(raw_key(KeyCode::Esc), &provider);
    assert_eq!(app.layers.time.state().start_zone, original);
    assert!(app.layers.time.state().dropdown.is_none());

    app.handle(raw_key(KeyCode::Enter), &provider);
    for _ in 0..16 {
        app.handle(raw_key(KeyCode::Down), &provider);
    }
    let rendered = render(&provider, &mut app, 100, 28);
    assert!(rendered.contains("Custom offset"), "{rendered}");
    let custom = app
        .layers
        .time
        .choice_rects()
        .iter()
        .find(|(_, index)| *index == 16)
        .expect("visible custom offset hitbox")
        .0;
    app.handle(
        Action::Raw(RawEvent::Mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            custom.x,
            custom.y,
        ))),
        &provider,
    );
    assert!(app.layers.time.state().start_zone_custom);
    for _ in 0..32 {
        app.handle(raw_key(KeyCode::Backspace), &provider);
    }
    app.handle(Action::Raw(RawEvent::Paste("+12:34".into())), &provider);
    let dialog = app.layers.time.state();
    assert_eq!(dialog.start_zone, "+12:34");
    assert_eq!(dialog.window, TimeWindowChoice::Absolute);
    let state = app.view_state().unwrap();
    assert_eq!(state.time_start_zone_draft, "+12:34");
    assert_eq!(state.time_window_draft, TimeWindowChoice::Absolute);
    assert!(app.take_query_requests().is_empty());
    time_focus(&mut app, &provider, TimeControl::StartZoneMenu);
    app.handle(raw_key(KeyCode::Enter), &provider);
    let mut terminal = Terminal::new(TestBackend::new(100, 28)).unwrap();
    terminal
        .draw(|frame| ui::render(frame, &mut app, &provider))
        .unwrap();
    assert_eq!(terminal.backend().cursor_position(), Position::new(0, 0));
    app.layers.time.highlight(0);
    app.handle(raw_key(KeyCode::Enter), &provider);
    let dialog = app.layers.time.state();
    assert_eq!(dialog.start_zone, "Z");
    assert!(!dialog.start_zone_custom);
}

#[test]
fn narrow_zone_dropdowns_place_selected_rows_inside_modal_for_keyboard_and_mouse() {
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::Time), &provider);
    for control in [TimeControl::StartZoneMenu, TimeControl::EndZoneMenu] {
        time_focus(&mut app, &provider, control);
        app.handle(raw_key(KeyCode::Enter), &provider);
        for _ in 0..16 {
            app.handle(raw_key(KeyCode::Down), &provider);
        }
        let rendered = render(&provider, &mut app, 46, 12);
        assert!(
            rendered.contains("Custom offset"),
            "{control:?}\n{rendered}"
        );
        let modal = app.hit_regions.selection_modal.unwrap();
        let selected = app
            .layers
            .time
            .choice_rects()
            .iter()
            .find(|(_, index)| *index == 16)
            .expect("selected zone choice remains visible")
            .0;
        assert!(modal.contains(Position::new(selected.x, selected.y)));
        app.handle(
            Action::Raw(RawEvent::Mouse(mouse(
                MouseEventKind::Down(MouseButton::Left),
                selected.x,
                selected.y,
            ))),
            &provider,
        );
        assert!(app.layers.time.state().dropdown.is_none());
        time_focus(&mut app, &provider, control);
        app.handle(raw_key(KeyCode::Enter), &provider);
        app.handle(raw_key(KeyCode::Up), &provider);
        app.handle(raw_key(KeyCode::Enter), &provider);
        assert!(app.layers.time.state().dropdown.is_none());
    }
}

#[test]
fn narrow_time_status_is_scrollable_and_scroll_chrome_does_not_reveal_content() {
    let (provider, mut app) = demo();
    let view = app.active_view_id().unwrap().to_owned();
    let mut restored = app.persistent_view_state(&view).unwrap();
    restored.time_error = Some(format!(
        "{} final-status-marker",
        "bounded diagnostic ".repeat(30)
    ));
    assert!(app.restore_persistent_view(&view, restored));
    app.handle(Action::Open(Open::Time), &provider);
    // §9 replaces the scroll pseudo-buttons with a scrollbar; a diagnostic too
    // long for the message row becomes scrollable body content, so the whole
    // text is still reachable.
    let first = render(&provider, &mut app, 46, 12);
    assert!(first.contains("Error"), "{first}");
    assert!(!first.contains("Scroll down"), "{first}");
    assert!(app.layers.time.state().has_overflow, "{first}");
    let wheel = app.layers.time.surface().popup;
    for _ in 0..64 {
        app.handle(
            Action::Raw(RawEvent::Mouse(mouse(
                MouseEventKind::ScrollDown,
                wheel.x + 2,
                wheel.y + 2,
            ))),
            &provider,
        );
    }
    let last = render(&provider, &mut app, 46, 12);
    assert!(last.contains("final-status-marker"), "{last}");
    let scroll = app.layers.time.state().scroll;
    let _ = render(&provider, &mut app, 46, 12);
    assert_eq!(app.layers.time.state().scroll, scroll);
}

#[test]
fn command_enrichment_is_structured_fenced_and_never_runs_on_save() {
    let (provider, mut app) = demo();
    app.handle(
        Action::Open(Open::ExternalCommand {
            stage: None,
            insert_at: usize::MAX,
        }),
        &provider,
    );
    app.handle(
        Action::Raw(RawEvent::Paste("/usr/bin/enrich".into())),
        &provider,
    );
    app.handle(raw_key(KeyCode::Tab), &provider);
    app.handle(
        Action::Raw(RawEvent::Paste("--format\njson".into())),
        &provider,
    );
    app.handle(raw_key(KeyCode::Tab), &provider);
    app.handle(Action::Raw(RawEvent::Paste("/tmp/work".into())), &provider);
    app.handle(raw_key(KeyCode::Tab), &provider);
    app.handle(
        Action::Raw(RawEvent::Paste("LANG=C\nMODE=wide".into())),
        &provider,
    );
    app.handle(raw_ctrl(KeyCode::Char('s')), &provider);
    let view_id = accept_command_save(&mut app);
    let stage = command_state(&app).accepted.clone().expect("saved stage");
    let lvu_core::CommandProgram::Exec { executable, args } = &stage.definition.program else {
        panic!("structured exec")
    };
    assert_eq!(executable.to_string_lossy(), "/usr/bin/enrich");
    assert_eq!(args, &["--format", "json"]);
    assert_eq!(stage.definition.restart, lvu_core::RestartPolicy::Never);
    assert_eq!(stage.id.0, "command-1");
    assert_eq!(
        app.views
            .state(&view_id)
            .unwrap()
            .command_revision("command-1"),
        1
    );
    assert!(
        app.take_command_enrichment_requests().is_empty(),
        "saving must not run"
    );

    app.handle(raw_ctrl(KeyCode::Char('r')), &provider);
    let CommandEnrichmentRequest::PrepareRun {
        generation,
        definition_revision,
        ..
    } = app.take_command_enrichment_requests().pop().unwrap()
    else {
        panic!("prepare request")
    };
    assert!(!app.finish_command_enrichment_review(
        generation + 1,
        &view_id,
        definition_revision,
        Err("stale".into())
    ));
    assert!(app.finish_command_enrichment_review(
        generation,
        &view_id,
        definition_revision,
        Ok(CommandEnrichmentReview {
            review_token: "opaque-token".into(),
            record_count: 7,
            source_count: 2,
            executable: "/usr/bin/enrich".into(),
            arguments: vec!["--format".into(), "json".into()],
            cwd: Some("/tmp/work".into()),
            environment_keys: vec!["LANG".into(), "MODE".into()],
        })
    ));
    let review_screen = render(&provider, &mut app, 100, 28);
    assert!(review_screen.contains("1,024 records / 4 MiB input; no sampling"));
    assert!(review_screen.contains("fixed snapshot: 7 records from 2 sources"));
    // §5.2 sizes the pane to its content, so a review that fits needs no
    // scrolling. What must hold either way is that every line is reachable.
    let notes = app
        .layers
        .external_command
        .notes_rect()
        .expect("review pane scrolls");
    for _ in 0..64 {
        app.handle(
            Action::Raw(RawEvent::Mouse(mouse(
                MouseEventKind::ScrollDown,
                notes.x,
                notes.y,
            ))),
            &provider,
        );
    }
    let review_end = render(&provider, &mut app, 100, 28);
    assert!(
        review_end.contains("Environment keys: LANG, MODE"),
        "{review_end}"
    );
    command_confirm_run(&mut app, &provider);
    assert!(
        matches!(app.take_command_enrichment_requests().as_slice(), [CommandEnrichmentRequest::Execute { review_token, .. }] if review_token == "opaque-token")
    );
    assert!(app.commit_command_publication(
        &view_id,
        "command-1",
        definition_revision,
        "publication-v1".into()
    ));
    assert!(app.finish_command_enrichment_run(
        generation,
        &view_id,
        definition_revision,
        Ok("Published 7 records".into())
    ));
    let saved = app.persistent_view_state(&view_id).unwrap();
    assert_eq!(
        saved.command_steps["command-1"].publication.as_deref(),
        Some("publication-v1")
    );
    assert_eq!(saved.applied_enrichments.len(), 1);
    assert_eq!(saved.applied_enrichments[0].command_stage(), Some(stage));
    assert_eq!(saved.applied_enrichments[0].source, "command");
}

#[test]
fn command_result_save_is_immutable_and_survives_a_closed_dialog() {
    let (provider, mut app) = demo();
    app.handle(
        Action::Open(Open::ExternalCommand {
            stage: None,
            insert_at: usize::MAX,
        }),
        &provider,
    );
    app.handle(
        Action::Raw(RawEvent::Paste("/usr/bin/enrich".into())),
        &provider,
    );
    app.handle(raw_ctrl(KeyCode::Char('s')), &provider);
    let view_id = accept_command_save(&mut app);
    app.handle(raw_ctrl(KeyCode::Char('r')), &provider);
    let CommandEnrichmentRequest::PrepareRun {
        generation,
        definition_revision,
        ..
    } = app.take_command_enrichment_requests().pop().unwrap()
    else {
        panic!("prepare request")
    };
    assert!(app.finish_command_enrichment_review(
        generation,
        &view_id,
        definition_revision,
        Ok(CommandEnrichmentReview {
            review_token: "reviewed".into(),
            record_count: 1,
            source_count: 1,
            executable: "/usr/bin/enrich".into(),
            arguments: vec![],
            cwd: None,
            environment_keys: vec![],
        })
    ));
    command_confirm_run(&mut app, &provider);
    assert!(matches!(
        app.take_command_enrichment_requests().as_slice(),
        [CommandEnrichmentRequest::Execute { .. }]
    ));
    assert!(!app.begin_command_result_save(generation + 1, &view_id, definition_revision));
    assert!(app.begin_command_result_save(generation, &view_id, definition_revision));

    let original = command_state(&app).program.clone();
    for action in [
        raw_char('x'),
        raw_key(KeyCode::Backspace),
        Action::Raw(RawEvent::Paste("changed".into())),
        raw_ctrl(KeyCode::Char('s')),
        raw_alt(KeyCode::Delete),
        raw_ctrl(KeyCode::Char('r')),
    ] {
        app.handle(action, &provider);
    }
    let dialog = command_state(&app);
    assert_eq!(dialog.program, original);
    assert_eq!(
        dialog.run_state,
        lvu::app::CommandEnrichmentRunState::SavingResults
    );
    assert!(app.take_command_enrichment_requests().is_empty());
    let rendered = render(&provider, &mut app, 100, 28);
    // §7.4 draws the state word; the sentence carries only the detail.
    assert!(rendered.contains("Saving results"), "{rendered}");
    assert!(!rendered.contains("Status:"), "{rendered}");
    assert!(!rendered.contains("Esc close"), "{rendered}");
    assert!(!rendered.contains("Ctrl-S save"), "{rendered}");

    // `q` is a dismissal, not a character: the busy field has no text focus.
    app.handle(raw_char('q'), &provider);
    assert!(app.layers.external_command.state().is_none());
    assert!(
        app.take_command_enrichment_requests().is_empty(),
        "durable result saving cannot be cancelled"
    );
    assert!(app.finish_command_enrichment_run(
        generation,
        &view_id,
        definition_revision,
        Ok("Published 1 record".into())
    ));
    assert!(
        app.action_notice
            .as_deref()
            .unwrap()
            .contains("results saved")
    );
}

#[test]
fn command_failure_has_an_explicit_error_status_and_closed_notice() {
    let (provider, mut app) = demo();
    app.handle(
        Action::Open(Open::ExternalCommand {
            stage: None,
            insert_at: usize::MAX,
        }),
        &provider,
    );
    app.handle(
        Action::Raw(RawEvent::Paste("/usr/bin/enrich".into())),
        &provider,
    );
    app.handle(raw_ctrl(KeyCode::Char('s')), &provider);
    let view_id = accept_command_save(&mut app);
    app.handle(raw_ctrl(KeyCode::Char('r')), &provider);
    let CommandEnrichmentRequest::PrepareRun {
        generation,
        definition_revision,
        ..
    } = app.take_command_enrichment_requests().pop().unwrap()
    else {
        panic!("prepare request")
    };
    let diagnostic =
        "Previous published results retained: command protocol did not complete: malformed_json";
    assert!(app.finish_command_enrichment_review(
        generation,
        &view_id,
        definition_revision,
        Err(diagnostic.into())
    ));
    let rendered = render(&provider, &mut app, 78, 24);
    // §7.4 draws the state word itself; the sentence no longer repeats it.
    assert!(rendered.contains("Error"), "{rendered}");
    assert!(!rendered.contains("Status: Error"), "{rendered}");
    app.handle(Action::ScrollDialog(i32::MAX), &provider);
    let rendered = render(&provider, &mut app, 78, 24);
    assert!(rendered.contains("malformed_json"), "{rendered}");

    app.handle(raw_key(KeyCode::Esc), &provider);
    app.action_notice = None;
    // A fenced completion arriving after its dialog closes remains visible without
    // mutating a newer dialog. Model the already-dispatched run context directly.
    app.handle(
        Action::Open(Open::ExternalCommand {
            stage: None,
            insert_at: usize::MAX,
        }),
        &provider,
    );
    app.handle(raw_ctrl(KeyCode::Char('r')), &provider);
    let CommandEnrichmentRequest::PrepareRun { generation, .. } =
        app.take_command_enrichment_requests().pop().unwrap()
    else {
        panic!("second prepare request")
    };
    assert!(app.finish_command_enrichment_review(
        generation,
        &view_id,
        definition_revision,
        Ok(CommandEnrichmentReview {
            review_token: "reviewed-again".into(),
            record_count: 1,
            source_count: 1,
            executable: "/usr/bin/enrich".into(),
            arguments: vec![],
            cwd: None,
            environment_keys: vec![],
        })
    ));
    command_confirm_run(&mut app, &provider);
    app.take_command_enrichment_requests();
    app.handle(raw_key(KeyCode::Esc), &provider);
    assert!(app.finish_command_enrichment_run(
        generation,
        &view_id,
        definition_revision,
        Err(diagnostic.into())
    ));
    assert!(
        app.action_notice
            .as_deref()
            .unwrap()
            .contains("results unchanged")
    );
}

#[test]
fn command_enrichment_dialog_keeps_unicode_cursor_review_and_actions_visible() {
    let (provider, mut app) = demo();
    app.handle(
        Action::Open(Open::ExternalCommand {
            stage: None,
            insert_at: usize::MAX,
        }),
        &provider,
    );
    app.handle(
        Action::Raw(RawEvent::Paste(format!(
            "/opt/{}-e\u{301}",
            "界".repeat(24)
        ))),
        &provider,
    );
    let mut terminal = Terminal::new(TestBackend::new(54, 18)).unwrap();
    terminal
        .draw(|frame| ui::render(frame, &mut app, &provider))
        .unwrap();
    let rendered = screen(terminal.backend().buffer());
    assert!(rendered.contains("Program"), "{rendered}");
    assert!(rendered.contains("External command"), "{rendered}");
    for action in ["New line", "Save", "Review", "Remove"] {
        assert!(rendered.contains(action), "missing {action}: {rendered}");
    }
    let cursor = terminal.backend().cursor_position();
    assert!(app.hit_regions.selection_modal.unwrap().contains(cursor));
    assert_ne!(
        terminal.backend().buffer()[cursor].bg,
        ratatui::style::Color::Reset
    );

    app.handle(raw_key(KeyCode::Tab), &provider);
    app.handle(Action::Raw(RawEvent::Paste("first".into())), &provider);
    app.handle(raw_alt(KeyCode::Char('n')), &provider);
    terminal
        .draw(|frame| ui::render(frame, &mut app, &provider))
        .unwrap();
    let after_newline = screen(terminal.backend().buffer());
    // §12.6 shows the lines themselves in a multi-line field rather than
    // counting them in a help string, so the second line is what proves the
    // newline landed.
    assert!(after_newline.contains("first"), "{after_newline}");
    // §4.2 put the fields after a shared label column, so the caret sits at
    // the input column rather than at the dialog's left edge. What this
    // protects is unchanged: the trailing empty argument line owns the cursor.
    let interior = app.hit_regions.selection_modal.unwrap();
    let caret = terminal.backend().cursor_position();
    let arguments = app
        .layers
        .external_command
        .control_rects()
        .iter()
        .find(|(rect, _)| rect.y <= caret.y && caret.y < rect.bottom())
        .map(|(rect, _)| *rect)
        .expect("the caret is inside a drawn field");
    assert_eq!(
        caret.x,
        interior.x + 1 + 14,
        "the caret is at the input column"
    );
    assert_eq!(
        caret.y,
        arguments.bottom() - 1,
        "trailing empty argument line must own the cursor"
    );
}

#[test]
fn narrow_command_controls_keep_the_focused_action_visible_and_clickable() {
    use lvu::app::CommandEnrichmentControl;
    let (provider, mut app) = demo();
    app.handle(
        Action::Open(Open::ExternalCommand {
            stage: None,
            insert_at: usize::MAX,
        }),
        &provider,
    );
    for (control, label) in [
        (CommandEnrichmentControl::NewLine, "New line"),
        (CommandEnrichmentControl::Save, "Save"),
        (CommandEnrichmentControl::Review, "Review"),
        (CommandEnrichmentControl::Remove, "Remove"),
    ] {
        command_focus(&mut app, &provider, control, None);
        let output = render(&provider, &mut app, 34, 18);
        assert!(output.contains(label), "missing focused {label}: {output}");
        assert!(
            app.layers
                .external_command
                .control_rects()
                .iter()
                .any(|(_, visible)| *visible == control),
            "focused {label} has no hitbox"
        );
    }
}

#[test]
fn enrichment_caret_uses_one_exact_boundary_multiline_model() {
    let (provider, mut exact) = demo();
    exact.handle(Action::Open(Open::Enrichment), &provider);
    enrichment_add(&mut exact, &provider);
    exact.handle(
        Action::Raw(RawEvent::Paste(format!("{}\n", "a".repeat(76)))),
        &provider,
    );
    let mut exact_terminal = Terminal::new(TestBackend::new(100, 28)).unwrap();
    exact_terminal
        .draw(|frame| ui::render(frame, &mut exact, &provider))
        .unwrap();
    let exact_cursor = exact_terminal.backend().cursor_position();

    let (provider, mut combined) = demo();
    combined.handle(Action::Open(Open::Enrichment), &provider);
    enrichment_add(&mut combined, &provider);
    combined.handle(
        Action::Raw(RawEvent::Paste(format!("{}\ne\u{301}", "a".repeat(76)))),
        &provider,
    );
    let mut combined_terminal = Terminal::new(TestBackend::new(100, 28)).unwrap();
    combined_terminal
        .draw(|frame| ui::render(frame, &mut combined, &provider))
        .unwrap();
    let combined_cursor = combined_terminal.backend().cursor_position();
    let modal = combined.hit_regions.selection_modal.unwrap();
    assert!(modal.contains(exact_cursor));
    assert!(modal.contains(combined_cursor));
    assert_eq!(combined_cursor.y, exact_cursor.y);
    assert_eq!(combined_cursor.x, exact_cursor.x + 1);

    combined.handle(raw_key(KeyCode::Tab), &provider);
    combined_terminal
        .draw(|frame| ui::render(frame, &mut combined, &provider))
        .unwrap();
    // The field belongs to the layer, so the layer is what still knows whether
    // Tab took the caret off it.
    assert!(!combined.layers.enrichment_step.text_focus());
}

#[test]
fn command_save_survives_close_and_ready_review_is_invalidated_by_edits() {
    let (provider, mut app) = demo();
    let view_id = app.active_view_id().unwrap().to_owned();
    app.handle(
        Action::Open(Open::ExternalCommand {
            stage: None,
            insert_at: usize::MAX,
        }),
        &provider,
    );
    app.handle(
        Action::Raw(RawEvent::Paste("/bin/enrich".into())),
        &provider,
    );
    app.handle(raw_ctrl(KeyCode::Char('s')), &provider);
    app.handle(raw_key(KeyCode::Esc), &provider);
    assert_eq!(accept_command_save(&mut app), view_id);
    assert_eq!(
        app.persistent_view_state(&view_id).unwrap().command_steps["command-1"].revision,
        1
    );
    assert!(app.action_notice.as_deref().unwrap().contains("not run"));

    app.handle(
        Action::Open(Open::ExternalCommand {
            stage: None,
            insert_at: usize::MAX,
        }),
        &provider,
    );
    app.handle(raw_ctrl(KeyCode::Char('r')), &provider);
    let CommandEnrichmentRequest::PrepareRun { generation, .. } =
        app.take_command_enrichment_requests().pop().unwrap()
    else {
        panic!("prepare")
    };
    assert!(app.finish_command_enrichment_review(
        generation,
        &view_id,
        1,
        Ok(CommandEnrichmentReview {
            review_token: "old-program".into(),
            record_count: 1,
            source_count: 1,
            executable: "/bin/enrich".into(),
            arguments: vec![],
            cwd: None,
            environment_keys: vec![],
        })
    ));
    app.handle(raw_char('2'), &provider);
    assert!(
        command_state(&app).review.is_none(),
        "an edit invalidates the Ready review"
    );
    app.handle(raw_ctrl(KeyCode::Char('r')), &provider);
    assert!(app.take_command_enrichment_requests().is_empty());
    assert!(
        command_state(&app)
            .error
            .as_deref()
            .unwrap()
            .contains("save it")
    );
    // With no review pending, Enter in the Program field is the dialog's
    // default (§8.9: Save), never the run the stale review described.
    command_confirm_run(&mut app, &provider);
    let requests = app.take_command_enrichment_requests();
    assert!(
        !requests
            .iter()
            .any(|request| matches!(request, CommandEnrichmentRequest::Execute { .. })),
        "edited Ready review must not execute: {requests:?}"
    );
    assert!(requests.is_empty(), "{requests:?}");
    assert_eq!(app.take_query_requests().len(), 1, "Enter in Program saves");
}

#[test]
fn command_saves_go_through_the_query_seam_and_wait_for_its_answer() {
    let (provider, mut app) = demo();
    app.handle(
        Action::Open(Open::ExternalCommand {
            stage: None,
            insert_at: usize::MAX,
        }),
        &provider,
    );
    app.handle(
        Action::Raw(RawEvent::Paste("/bin/enrich".into())),
        &provider,
    );
    app.handle(raw_ctrl(KeyCode::Char('s')), &provider);
    assert!(
        app.take_command_enrichment_requests().is_empty(),
        "a save never occupies the run queue"
    );
    assert_eq!(
        command_state(&app).run_state,
        lvu::app::CommandEnrichmentRunState::Saving
    );
    // While the chain is being checked the definition is not editable and a
    // second save is refused with a reason, as every in-flight state is.
    app.handle(raw_ctrl(KeyCode::Char('s')), &provider);
    let view_id = accept_command_save(&mut app);
    assert_eq!(
        command_state(&app).run_state,
        lvu::app::CommandEnrichmentRunState::Unrun
    );
    assert_eq!(
        app.views
            .state(&view_id)
            .unwrap()
            .command_revision("command-1"),
        1
    );
    // A rejected chain keeps the accepted one and says why.
    app.handle(Action::Raw(RawEvent::Paste("-broken".into())), &provider);
    app.handle(raw_ctrl(KeyCode::Char('s')), &provider);
    let request = app.take_query_requests().pop().unwrap();
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: request.view_id,
        generation: request.generation,
        revision: request.revision,
        purpose: QueryPurpose::Enrichment,
        result: Err(QueryFailure {
            purpose: QueryPurpose::Enrichment,
            message: "program not found".into(),
        }),
    }));
    assert_eq!(
        command_state(&app).error.as_deref(),
        Some("program not found")
    );
    assert_eq!(
        app.views
            .state(&view_id)
            .unwrap()
            .command_revision("command-1"),
        1
    );
}

#[test]
fn command_enrichment_keys_match_the_action_footer() {
    // The three enrichment layers own their keymaps now (§6.3 step 13), so the
    // footer's accelerators are asserted by what they do rather than by what
    // `key_to_action` returns.
    let (provider, mut app) = demo();
    app.handle(
        Action::Open(Open::ExternalCommand {
            stage: None,
            insert_at: usize::MAX,
        }),
        &provider,
    );
    app.handle(
        Action::Raw(RawEvent::Paste("/bin/enrich".into())),
        &provider,
    );

    app.handle(raw_ctrl(KeyCode::Char('s')), &provider);
    accept_command_save(&mut app);

    // Enhanced-keyboard Enter encodings remain optional aliases.
    app.handle(raw_ctrl(KeyCode::Enter), &provider);
    assert_eq!(app.take_query_requests().len(), 1);
    app.handle(raw_key(KeyCode::Esc), &provider);
    app.handle(
        Action::Open(Open::ExternalCommand {
            stage: None,
            insert_at: usize::MAX,
        }),
        &provider,
    );
    app.handle(
        Action::Raw(RawEvent::Paste("/bin/enrich".into())),
        &provider,
    );

    // Alt-n is the newline in the multi-line fields, and only there.
    command_field(&mut app, &provider, CommandEnrichmentField::Arguments);
    app.handle(raw_alt(KeyCode::Char('n')), &provider);
    assert_eq!(command_state(&app).arguments, "\n");
    command_field(&mut app, &provider, CommandEnrichmentField::Program);
    let program = command_state(&app).program.clone();
    app.handle(raw_alt(KeyCode::Char('n')), &provider);
    assert_eq!(command_state(&app).program, program);

    // Ctrl-r and Alt-Enter both ask for the bounded review.
    for review in [raw_ctrl(KeyCode::Char('r')), raw_alt(KeyCode::Enter)] {
        app.handle(review, &provider);
        assert!(
            command_state(&app)
                .error
                .as_deref()
                .is_some_and(|error| error.contains("save it")),
            "review of an edited draft is refused, not run"
        );
    }
    app.handle(raw_key(KeyCode::Esc), &provider);

    // Alt-c opens External command from the step list, and Enter on the list
    // opens the step editor rather than doing nothing.
    app.handle(Action::Open(Open::Enrichment), &provider);
    app.handle(raw_alt(KeyCode::Char('c')), &provider);
    assert_eq!(app.layers.top(), Some(LayerId::ExternalCommand));
    app.handle(raw_key(KeyCode::Esc), &provider);

    app.handle(Action::Open(Open::Enrichment), &provider);
    enrichment_focus(&mut app, &provider, EnrichmentControl::Add);
    app.handle(raw_key(KeyCode::Enter), &provider);
    assert_eq!(app.layers.top(), Some(LayerId::EnrichmentStep));
    // Alt-n is the step editor's newline too.
    app.handle(raw_alt(KeyCode::Char('n')), &provider);
    assert_eq!(app.view_state().unwrap().enrichment.draft, "\n");
    // Enter on the expression submits it.
    app.handle(raw_key(KeyCode::Enter), &provider);
    assert!(
        app.take_query_requests().is_empty(),
        "a blank step is refused"
    );
}

#[test]
fn command_arguments_round_trip_an_intentional_trailing_empty_value() {
    let (provider, mut app) = demo();
    let view_id = app.active_view_id().unwrap().to_owned();
    app.handle(
        Action::Open(Open::ExternalCommand {
            stage: None,
            insert_at: usize::MAX,
        }),
        &provider,
    );
    app.handle(Action::Raw(RawEvent::Paste("/bin/tool".into())), &provider);
    app.handle(raw_key(KeyCode::Tab), &provider);
    app.handle(Action::Raw(RawEvent::Paste("two words".into())), &provider);
    app.handle(raw_alt(KeyCode::Char('n')), &provider);
    app.handle(raw_ctrl(KeyCode::Char('s')), &provider);
    assert_eq!(accept_command_save(&mut app), view_id);
    let stage = command_state(&app).accepted.clone().expect("saved stage");
    let lvu_core::CommandProgram::Exec { args, .. } = &stage.definition.program else {
        panic!("exec")
    };
    assert_eq!(args, &["two words", ""]);
    app.handle(raw_key(KeyCode::Esc), &provider);
    app.handle(
        Action::Open(Open::ExternalCommand {
            stage: None,
            insert_at: usize::MAX,
        }),
        &provider,
    );
    assert_eq!(command_state(&app).arguments, "two words\n");
}

#[test]
fn text_line_controls_move_without_mutation_and_q_inserts_at_the_cursor() {
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::Advanced), &provider);
    app.handle(
        Action::Raw(RawEvent::Paste("界e\u{301}tail".into())),
        &provider,
    );
    let before = app.advanced_state().unwrap().draft.clone();
    app.handle(raw_ctrl(KeyCode::Char('a')), &provider);
    assert_eq!(app.advanced_state().unwrap().draft, before);
    app.handle(raw_key(KeyCode::Char('q')), &provider);
    assert_eq!(app.advanced_state().unwrap().draft, format!("q{before}"));
    app.handle(raw_ctrl(KeyCode::Char('e')), &provider);
    app.handle(raw_ctrl(KeyCode::Char('k')), &provider);
    assert_eq!(app.advanced_state().unwrap().draft, format!("q{before}"));

    app.handle(raw_ctrl(KeyCode::Char('a')), &provider);
    app.handle(raw_ctrl(KeyCode::Char('k')), &provider);
    assert!(app.advanced_state().unwrap().draft.is_empty());
}

#[test]
fn arrow_keys_route_only_active_text_fields_and_move_multiline_carets() {
    let (provider, mut app) = demo();

    app.handle(Action::Open(Open::Search), &provider);
    app.handle(Action::Raw(RawEvent::Paste("abc".into())), &provider);
    // The layer owns the arrow keys: they move its caret, which is the bank's,
    // and the next character lands where the caret was left.
    app.handle(raw_key(KeyCode::Left), &provider);
    app.handle(raw_key(KeyCode::Char('q')), &provider);
    assert_eq!(app.search_state().unwrap().draft, "abqc");

    app.handle(raw_key(KeyCode::Esc), &provider);
    app.handle(Action::Open(Open::Enrichment), &provider);
    enrichment_add(&mut app, &provider);
    app.handle(Action::Raw(RawEvent::Paste("ab\ncd".into())), &provider);
    // The step editor owns its arrows too: Up is the caret's while the
    // expression field has the keys.
    app.handle(raw_key(KeyCode::Up), &provider);
    app.handle(raw_char('q'), &provider);
    assert_eq!(app.view_state().unwrap().enrichment.draft, "abq\ncd");

    app.handle(raw_key(KeyCode::Esc), &provider);
    app.handle(
        Action::Open(Open::ExternalCommand {
            stage: None,
            insert_at: usize::MAX,
        }),
        &provider,
    );
    app.handle(Action::Raw(RawEvent::Paste("tool".into())), &provider);
    app.handle(raw_key(KeyCode::Left), &provider);
    app.handle(raw_char('q'), &provider);
    assert_eq!(command_state(&app).program, "tooql");
    app.handle(raw_key(KeyCode::Tab), &provider);
    app.handle(Action::Raw(RawEvent::Paste("ab\ncd".into())), &provider);
    app.handle(raw_key(KeyCode::Up), &provider);
    app.handle(raw_char('q'), &provider);
    assert_eq!(command_state(&app).arguments, "abq\ncd");

    let mut source = App::new(vec![], vec![], false);
    source.handle(Action::Raw(RawEvent::Paste("abc".into())), &provider);
    // Left is the caret's while the field is taking text, and the mode
    // selector's once a control has focus — the layer decides, not the shell.
    source.handle(raw_key(KeyCode::Left), &provider);
    source.handle(raw_char('q'), &provider);
    assert_eq!(source.layers.source.state().draft, "abqc");
    source.handle(raw_key(KeyCode::Tab), &provider);
    let mode = source.layers.source.state().mode;
    source.handle(raw_key(KeyCode::Left), &provider);
    assert_ne!(source.layers.source.state().mode, mode);
}

#[test]
fn stale_command_run_completions_release_capacity_without_changing_restored_state() {
    let (provider, mut app) = demo();
    app.handle(
        Action::Open(Open::ExternalCommand {
            stage: None,
            insert_at: usize::MAX,
        }),
        &provider,
    );
    app.handle(
        Action::Raw(RawEvent::Paste("/usr/bin/enrich".into())),
        &provider,
    );
    app.handle(raw_ctrl(KeyCode::Char('s')), &provider);
    let view_id = accept_command_save(&mut app);
    for _ in 0..10 {
        app.handle(raw_ctrl(KeyCode::Char('r')), &provider);
        let CommandEnrichmentRequest::PrepareRun {
            generation,
            definition_revision,
            ..
        } = app
            .take_command_enrichment_requests()
            .pop()
            .expect("stale runs must not exhaust capacity")
        else {
            panic!("prepare")
        };
        assert!(app.finish_command_enrichment_review(
            generation,
            &view_id,
            definition_revision,
            Ok(CommandEnrichmentReview {
                review_token: "reviewed".into(),
                record_count: 1,
                source_count: 1,
                executable: "/usr/bin/enrich".into(),
                arguments: vec![],
                cwd: None,
                environment_keys: vec![],
            })
        ));
        command_confirm_run(&mut app, &provider);
        assert!(matches!(
            app.take_command_enrichment_requests().as_slice(),
            [CommandEnrichmentRequest::Execute { .. }]
        ));
        app.handle(raw_key(KeyCode::Esc), &provider);
        app.take_command_enrichment_requests();
        let mut restored = app.persistent_view_state(&view_id).unwrap();
        let run = restored.command_steps.get_mut("command-1").unwrap();
        run.revision += 1;
        run.publication = Some("last-good".into());
        app.restore_persistent_view(&view_id, restored);
        assert!(!app.finish_command_enrichment_run(
            generation,
            &view_id,
            definition_revision,
            Ok("stale success".into())
        ));
        assert_eq!(
            app.persistent_view_state(&view_id).unwrap().command_steps["command-1"]
                .publication
                .as_deref(),
            Some("last-good")
        );
        app.handle(
            Action::Open(Open::ExternalCommand {
                stage: None,
                insert_at: usize::MAX,
            }),
            &provider,
        );
    }
}

#[test]
fn narrow_source_ai_proposal_scrolls_to_every_reviewable_field_by_key() {
    use lvu::{SourceAiPreview, SourceAiPreviewItem, SourceAiRequest, SourceAiStage};

    let provider = EmptyProvider;
    let mut app = App::new(Vec::new(), Vec::new(), false);
    app.handle(
        Action::Command(
            LayerId::Source,
            lvu::command_palette::CommandId::AskAiSource,
        ),
        &provider,
    );
    app.handle(
        Action::Raw(RawEvent::Paste("follow controlled logs".into())),
        &provider,
    );
    app.handle(raw_key(KeyCode::Enter), &provider);
    let SourceAiRequest::Start { generation, .. } = app.take_source_ai_requests().pop().unwrap()
    else {
        panic!("source AI start")
    };
    let preview = SourceAiPreview {
        sources: vec![SourceAiPreviewItem {
            name: "reviewed command source".into(),
            kind: "command".into(),
            launch: r#"{"args":["-c","printf x"],"executable":"/bin/sh"}"#.into(),
            effective_path_or_cwd: "/tmp/controlled source cwd".into(),
            restart: "never".into(),
            environment: vec!["ALPHA=one".into(), "DELTA=four".into()],
        }],
        explanation: "full controlled why evidence remains reviewable".into(),
    };
    assert!(app.finish_source_ai(generation, Ok(preview.clone())));
    assert_eq!(app.layers.source.state().ai.stage, SourceAiStage::Proposal);

    // A cramped terminal must still expose every field the user has to review
    // before an irreversible launch. Render first so the pane publishes its limit.
    let mut observed = render(&provider, &mut app, 54, 16);
    assert!(
        app.layers.source.state().ai.preview_scroll_limit > 0,
        "narrow preview must report clipped content:\n{observed}"
    );

    // The real app moves focus onto the launch controls when a proposal lands,
    // which is exactly the state a user reviews from.
    app.handle(raw_key(KeyCode::Tab), &provider);
    observed.push_str(&render(&provider, &mut app, 54, 16));
    assert!(
        app.layers.source.state().controls_focused,
        "expected the launch controls to hold focus"
    );

    // Drive it the way a user does: the Down key, not a synthesised action.
    for _ in 0..24 {
        let before = app.layers.source.state().ai.preview_scroll;
        app.handle(raw_key(KeyCode::Down), &provider);
        observed.push_str(&render(&provider, &mut app, 54, 16));
        assert!(
            app.layers.source.state().ai.preview_scroll >= before,
            "Down must remain bound while reviewing a proposal:\n{observed}"
        );
    }
    for expected in [
        "Launch:",
        "Effective path/cwd:",
        "Restart:",
        "ALPHA=one",
        "DELTA=four",
        "Why:",
        "full controlled why",
    ] {
        assert!(
            observed.contains(expected),
            "{expected:?} unreachable before launch at 54x16:\n{observed}"
        );
    }

    // The wheel over the preview pane is the other ordinary way to review it.
    let pane = app
        .layers
        .source
        .scroll_rect()
        .expect("narrow proposal preview publishes a scroll hitbox");
    for _ in 0..64 {
        app.handle(
            Action::Raw(RawEvent::Mouse(mouse(
                MouseEventKind::ScrollUp,
                pane.x + 1,
                pane.y + 1,
            ))),
            &provider,
        );
    }
    assert_eq!(app.layers.source.state().ai.preview_scroll, 0);
    observed.push_str(&render(&provider, &mut app, 54, 16));
    app.handle(
        Action::Raw(RawEvent::Mouse(mouse(
            MouseEventKind::ScrollDown,
            pane.x + 1,
            pane.y + 1,
        ))),
        &provider,
    );
    assert!(
        app.layers.source.state().ai.preview_scroll > 0,
        "wheel over the proposal preview must scroll it:\n{observed}"
    );
    app.handle(
        Action::Raw(RawEvent::Mouse(mouse(
            MouseEventKind::ScrollUp,
            pane.x + 1,
            pane.y + 1,
        ))),
        &provider,
    );
    assert_eq!(app.layers.source.state().ai.preview_scroll, 0);

    // A key pressed in the same frame the proposal arrived predates the
    // rendered measurement; it must still move the pane rather than be clamped
    // against a stale zero limit. Re-delivering the proposal resets both.
    assert!(app.finish_source_ai(generation, Ok(preview.clone())));
    app.handle(raw_key(KeyCode::Down), &provider);
    let after = render(&provider, &mut app, 54, 16);
    assert_eq!(
        app.layers.source.state().ai.preview_scroll,
        1,
        "unmeasured limit must not swallow review scrolling:\n{after}"
    );

    assert!(app.take_source_requests().is_empty());
}

#[test]
fn enrichment_layers_dismiss_one_at_a_time_and_cancel_preserves_the_chain() {
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::Enrichment), &provider);
    enrichment_add(&mut app, &provider);
    app.handle(
        Action::Raw(RawEvent::Paste("kind = pl.lit('accepted')".into())),
        &provider,
    );
    step_submit(&mut app, &provider);
    let accepted = app.take_query_requests().pop().unwrap();
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: accepted.view_id,
        generation: accepted.generation,
        revision: accepted.revision,
        purpose: accepted.purpose,
        result: Ok(()),
    }));
    let chain = app.view_state().unwrap().enrichments.clone();
    assert_eq!(chain.len(), 1);
    assert_eq!(app.layers.top(), Some(LayerId::Enrichment));

    // Editing then cancelling leaves the accepted step untouched and keeps the
    // unfinished edit, which is what persistence restores after a restart.
    enrichment_edit(&mut app, &provider);
    assert_eq!(app.layers.top(), Some(LayerId::EnrichmentStep));
    app.handle(Action::Raw(RawEvent::Paste(" + 1".into())), &provider);
    app.handle(raw_key(KeyCode::Esc), &provider);
    assert_eq!(app.layers.top(), Some(LayerId::Enrichment));
    assert_eq!(app.view_state().unwrap().enrichments, chain);
    assert_eq!(
        app.view_state().unwrap().enrichment.draft,
        "kind = pl.lit('accepted') + 1"
    );
    assert_eq!(
        app.view_state().unwrap().enrichment_editing,
        Some(chain[0].id.clone())
    );
    assert!(app.take_query_requests().is_empty());

    // Opening and leaving a step without editing it reports no phantom draft.
    enrichment_edit(&mut app, &provider);
    while !app.view_state().unwrap().enrichment.draft.is_empty() {
        app.handle(raw_key(KeyCode::Backspace), &provider);
    }
    app.handle(
        Action::Raw(RawEvent::Paste(chain[0].source.clone())),
        &provider,
    );
    app.handle(raw_key(KeyCode::Esc), &provider);
    assert!(app.view_state().unwrap().enrichment.draft.is_empty());
    assert_eq!(app.view_state().unwrap().enrichment_editing, None);
    assert_eq!(app.view_state().unwrap().enrichments, chain);

    // A completion popup, then layer two, then layer one, then the workspace.
    enrichment_add(&mut app, &provider);
    app.handle(Action::Raw(RawEvent::Paste("later = ".into())), &provider);
    app.handle(raw_ctrl(KeyCode::Char(' ')), &provider);
    render(&provider, &mut app, 100, 28);
    assert!(app.layers.enrichment_step.completion().is_some());
    app.handle(raw_key(KeyCode::Esc), &provider);
    assert!(app.layers.enrichment_step.completion().is_none());
    assert_eq!(app.layers.top(), Some(LayerId::EnrichmentStep));
    // `q` stays literal while the expression is being edited.
    assert!(app.layers.enrichment_step.surface().text_focus);
    app.handle(raw_key(KeyCode::Esc), &provider);
    assert_eq!(app.layers.top(), Some(LayerId::Enrichment));
    // An unfinished new step survives as the persisted draft; an abandoned edit
    // above did not, because its accepted source is still there to reopen.
    assert_eq!(app.view_state().unwrap().enrichment.draft, "later = ");
    assert_eq!(app.view_state().unwrap().enrichment_editing, None);
    let list = render(&provider, &mut app, 100, 28);
    assert!(list.contains("unsaved draft kept"), "{list}");
    // The list takes no text, so `q` is a dismissal there.
    assert!(!app.layers.enrichment.surface().text_focus);
    app.handle(raw_key(KeyCode::Esc), &provider);
    assert_eq!(app.focus, Focus::Logs);
    assert_eq!(app.view_state().unwrap().enrichments, chain);
}

#[test]
fn enrichment_step_failure_keeps_earlier_steps_and_reopens_its_own_layer() {
    let (provider, mut app) = demo();
    app.handle(Action::Open(Open::Enrichment), &provider);
    enrichment_add(&mut app, &provider);
    app.handle(
        Action::Raw(RawEvent::Paste("good = pl.lit(1)".into())),
        &provider,
    );
    step_submit(&mut app, &provider);
    let good = app.take_query_requests().pop().unwrap();
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: good.view_id,
        generation: good.generation,
        revision: good.revision,
        purpose: good.purpose,
        result: Ok(()),
    }));
    let chain = app.view_state().unwrap().enrichments.clone();

    enrichment_add(&mut app, &provider);
    app.handle(
        Action::Raw(RawEvent::Paste("broken = pl.col(".into())),
        &provider,
    );
    step_submit(&mut app, &provider);
    let broken = app.take_query_requests().pop().unwrap();
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: broken.view_id,
        generation: broken.generation,
        revision: broken.revision,
        purpose: broken.purpose,
        result: Err(QueryFailure {
            purpose: QueryPurpose::Enrichment,
            message: "compiler rejected expression".into(),
        }),
    }));
    // The failing draft keeps its own layer so it can be corrected in place.
    assert_eq!(app.layers.top(), Some(LayerId::EnrichmentStep));
    assert_eq!(app.view_state().unwrap().enrichments, chain);
    let reaffirm = app.take_query_requests().pop().unwrap();
    assert_eq!(reaffirm.constraints.enrichments, chain);
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: reaffirm.view_id,
        generation: reaffirm.generation,
        revision: reaffirm.revision,
        purpose: reaffirm.purpose,
        result: Ok(()),
    }));
    assert_eq!(
        app.view_state().unwrap().enrichment.error.as_deref(),
        Some("compiler rejected expression")
    );
    let step = render(&provider, &mut app, 100, 28);
    assert!(step.contains("Error"), "{step}");
    assert!(step.contains("every accepted step is retained"), "{step}");

    app.handle(raw_key(KeyCode::Esc), &provider);
    let list = render(&provider, &mut app, 100, 28);
    assert!(list.contains("1  good = pl.lit(1)"), "{list}");
    assert_eq!(app.view_state().unwrap().enrichments, chain);
}

#[test]
fn enrichment_layer_hitboxes_match_what_is_drawn_at_narrow_and_wide_sizes() {
    use lvu::app::{EnrichmentControl, EnrichmentStepControl};

    for (width, height) in [(54u16, 16u16), (80, 24), (120, 34)] {
        let (provider, mut app) = demo();
        app.handle(Action::Open(Open::Enrichment), &provider);
        enrichment_add(&mut app, &provider);
        app.handle(
            Action::Raw(RawEvent::Paste("one = pl.lit(1)".into())),
            &provider,
        );
        step_submit(&mut app, &provider);
        let first = app.take_query_requests().pop().unwrap();
        assert!(app.apply_query_completion(QueryCompletion {
            view_id: first.view_id,
            generation: first.generation,
            revision: first.revision,
            purpose: first.purpose,
            result: Ok(()),
        }));
        enrichment_add(&mut app, &provider);
        app.handle(
            Action::Raw(RawEvent::Paste("two = pl.lit(2)".into())),
            &provider,
        );
        step_submit(&mut app, &provider);
        let second = app.take_query_requests().pop().unwrap();
        assert!(app.apply_query_completion(QueryCompletion {
            view_id: second.view_id,
            generation: second.generation,
            revision: second.revision,
            purpose: second.purpose,
            result: Ok(()),
        }));

        render(&provider, &mut app, width, height);
        let modal = app.hit_regions.selection_modal.unwrap();
        let rows = app.layers.enrichment.row_rects().to_vec();
        assert!(!rows.is_empty(), "{width}x{height} lists no clickable step");
        for (rect, _) in &rows {
            assert!(modal.contains(Position::new(rect.x, rect.y)));
            assert!(rect.right() <= modal.right() && rect.bottom() <= modal.bottom());
        }
        // Clicking a step selects it and gives the list focus.
        let (rect, index) = rows[0];
        app.handle(raw_click(rect.x, rect.y), &provider);
        assert_eq!(app.view_state().unwrap().enrichment_selected, index);
        assert_eq!(
            app.view_state().unwrap().enrichment_control,
            EnrichmentControl::Steps
        );

        // Clicking Edit opens layer two for that step.
        let edit = app
            .layers
            .enrichment
            .control_rects()
            .iter()
            .find(|(_, control)| *control == EnrichmentControl::Edit)
            .copied()
            .unwrap_or_else(|| panic!("{width}x{height} has no Edit hitbox"));
        app.handle(raw_click(edit.0.x, edit.0.y), &provider);
        assert_eq!(app.layers.top(), Some(LayerId::EnrichmentStep));
        assert_eq!(
            app.view_state().unwrap().enrichment.draft,
            "one = pl.lit(1)"
        );

        let step = render(&provider, &mut app, width, height);
        assert!(step.contains("Input record"), "{width}x{height}: {step}");
        assert!(step.contains("Accepted output"), "{width}x{height}: {step}");
        assert!(step.contains("[ Save ]"), "{width}x{height}: {step}");
        let modal = app.hit_regions.selection_modal.unwrap();
        for (rect, _) in app.layers.enrichment_step.control_rects() {
            assert!(modal.contains(Position::new(rect.x, rect.y)));
            assert!(rect.right() <= modal.right() && rect.bottom() <= modal.bottom());
        }

        // The input pane is a list over records: the wheel moves its selection.
        let input = app
            .layers
            .enrichment_step
            .control_rects()
            .iter()
            .find(|(_, control)| *control == EnrichmentStepControl::Input)
            .copied()
            .unwrap_or_else(|| panic!("{width}x{height} has no input pane hitbox"));
        let before = app.layers.enrichment_step.sample();
        app.handle(
            Action::Raw(RawEvent::Mouse(mouse(
                MouseEventKind::ScrollUp,
                input.0.x,
                input.0.y,
            ))),
            &provider,
        );
        assert_eq!(
            app.layers.enrichment_step.sample(),
            before.saturating_sub(1)
        );
        let moved = render(&provider, &mut app, width, height);
        assert!(
            moved.contains(&format!("{} of", before)),
            "{width}x{height}: {moved}"
        );

        // Removing from layer two returns to the list and validates the chain.
        let remove = app
            .layers
            .enrichment_step
            .control_rects()
            .iter()
            .find(|(_, control)| *control == EnrichmentStepControl::Remove)
            .copied()
            .unwrap_or_else(|| panic!("{width}x{height} has no Remove hitbox"));
        app.handle(raw_click(remove.0.x, remove.0.y), &provider);
        assert_eq!(app.layers.top(), Some(LayerId::Enrichment));
        assert_eq!(app.view_state().unwrap().enrichments.len(), 2);
    }
}

#[test]
fn unfinished_enrichment_drafts_survive_restart_for_new_and_edited_steps() {
    // Both layer-two paths write into the same working-view state, so both are
    // persisted and both are resumed by reopening the step they belong to.
    for edit in [false, true] {
        let (provider, mut app) = demo();
        let view_id = app.active_view_id().unwrap().to_owned();
        app.handle(Action::Open(Open::Enrichment), &provider);
        enrichment_add(&mut app, &provider);
        app.handle(
            Action::Raw(RawEvent::Paste("kind = pl.lit('accepted')".into())),
            &provider,
        );
        step_submit(&mut app, &provider);
        let accepted = app.take_query_requests().pop().unwrap();
        assert!(app.apply_query_completion(QueryCompletion {
            view_id: accepted.view_id,
            generation: accepted.generation,
            revision: accepted.revision,
            purpose: accepted.purpose,
            result: Ok(()),
        }));
        let chain = app.view_state().unwrap().enrichments.clone();

        let (unfinished, editing) = if edit {
            enrichment_edit(&mut app, &provider);
            app.handle(Action::Raw(RawEvent::Paste(" + 1".into())), &provider);
            (
                "kind = pl.lit('accepted') + 1".to_owned(),
                Some(chain[0].id.clone()),
            )
        } else {
            enrichment_add(&mut app, &provider);
            app.handle(
                Action::Raw(RawEvent::Paste("later = pl.lit(2)".into())),
                &provider,
            );
            ("later = pl.lit(2)".to_owned(), None)
        };
        // Leaving both layers is what a quit does; neither may discard the work.
        app.handle(raw_key(KeyCode::Esc), &provider);
        app.handle(raw_key(KeyCode::Esc), &provider);
        assert_eq!(app.focus, Focus::Logs);

        let persisted = app.persistent_view_state(&view_id).unwrap();
        assert_eq!(persisted.applied_enrichments, chain, "edit={edit}");
        assert_eq!(persisted.enrichment_draft, unfinished, "edit={edit}");
        assert_eq!(persisted.enrichment_editing, editing, "edit={edit}");

        // Restart: the accepted chain and the unfinished work both come back.
        let (provider, mut restarted) = demo();
        let view_id = restarted.active_view_id().unwrap().to_owned();
        assert!(restarted.restore_persistent_view(&view_id, persisted));
        let request = restarted.take_query_requests().pop().unwrap();
        assert!(restarted.apply_query_completion(QueryCompletion {
            view_id: request.view_id,
            generation: request.generation,
            revision: request.revision,
            purpose: request.purpose,
            result: Ok(()),
        }));
        assert_eq!(restarted.view_state().unwrap().enrichments, chain);
        assert_eq!(restarted.view_state().unwrap().enrichment.draft, unfinished);

        restarted.handle(Action::Open(Open::Enrichment), &provider);
        let list = render(&provider, &mut restarted, 100, 28);
        assert!(list.contains("unsaved draft kept"), "edit={edit}: {list}");
        if edit {
            enrichment_edit(&mut restarted, &provider);
        } else {
            enrichment_add(&mut restarted, &provider);
        }
        assert_eq!(restarted.layers.top(), Some(LayerId::EnrichmentStep));
        assert_eq!(
            restarted.view_state().unwrap().enrichment.draft,
            unfinished,
            "edit={edit}: reopening the step must resume the restored draft"
        );
        let step = render(&provider, &mut restarted, 100, 28);
        assert!(step.contains("pl.lit("), "edit={edit}: {step}");
        assert_eq!(restarted.view_state().unwrap().enrichments, chain);
    }
}

/// A provider that folds, so the terminal's half of folding can be exercised
/// without the native view stack. It collapses the three `flood` rows into one.
struct FoldingProvider {
    rows: Vec<DisplayRow>,
    request: RefCell<lvu::FoldRequest>,
    /// Columns a still-arriving source would add while a picker is open
    /// (§5.2.1). Zero is the fixture's own two.
    extra_columns: RefCell<usize>,
    /// Stream rows the fold feed has not reached yet. Folding is incremental,
    /// so a large view spends time in this state with the rest of the stream
    /// rendering individually.
    pending_rows: RefCell<usize>,
}

impl FoldingProvider {
    fn new() -> Self {
        let make = |sequence: u64, text: &str| DisplayRow {
            id: RowId::new("api", sequence),
            timestamp: format!("12:00:{sequence:02}"),
            captured_at_unix_nanos: Some(sequence as i64),
            level: "INFO".into(),
            text: text.into(),
            details: vec![],
            // Two columns a fold key can name, so the Folding dialog's picker
            // has something to offer besides the derived pattern column.
            fields: vec![
                (
                    "service".into(),
                    if sequence == 4 { "indexer" } else { "shipper" }.into(),
                ),
                ("host".into(), format!("node-{}", sequence % 2)),
            ],
        };
        Self {
            rows: vec![
                make(0, "service started"),
                make(1, "retry connect failed"),
                make(2, "retry connect failed"),
                make(3, "retry connect failed"),
                make(4, "ready"),
            ],
            request: RefCell::new(lvu::FoldRequest::default()),
            extra_columns: RefCell::new(0),
            pending_rows: RefCell::new(0),
        }
    }

    /// Make the sampled column set change under an open picker.
    fn set_extra_columns(&self, count: usize) {
        *self.extra_columns.borrow_mut() = count;
    }

    fn folding(&self) -> bool {
        self.enabled() && !self.expanded()
    }

    fn enabled(&self) -> bool {
        self.request.borrow().enabled
    }

    /// The run is a run either way; this is only whether it is shown collapsed.
    fn expanded(&self) -> bool {
        let request = self.request.borrow();
        request.enabled && request.expanded.contains(&RowId::new("api", 1))
    }

    fn display(&self) -> Vec<DisplayRow> {
        let extra = *self.extra_columns.borrow();
        let rows: Vec<DisplayRow> = self
            .rows
            .iter()
            .cloned()
            .map(|mut row| {
                for index in 0..extra {
                    row.fields
                        .push((format!("arrived_{index:02}"), "value".into()));
                }
                row
            })
            .collect();
        if self.expanded() {
            // The run rendered as its members, each marked with where it sits.
            let mut out = vec![rows[0].clone()];
            for (offset, place) in ["first", "middle", "last"].iter().enumerate() {
                let mut member = rows[offset + 1].clone();
                member
                    .details
                    .push(("fold_member".into(), (*place).to_string()));
                member.details.push(("fold_count".into(), "3".to_string()));
                out.push(member);
            }
            out.push(rows[4].clone());
            return out;
        }
        if !self.folding() {
            return rows;
        }
        let mut collapsed = rows[1].clone();
        collapsed.text = format!("{}  [x3 repeated]", collapsed.text);
        collapsed
            .details
            .push(("fold_count".into(), "3".to_string()));
        collapsed
            .details
            .push(("fold_entry".into(), "collapsed".to_string()));
        collapsed.details.push((
            "fold_pattern".into(),
            "retry connect failed after <num>ms".to_string(),
        ));
        collapsed
            .details
            .push(("fold_last_time".into(), "12:00:03".to_string()));
        vec![rows[0].clone(), collapsed, rows[4].clone()]
    }
}

impl RowProvider for FoldingProvider {
    fn page(&self, _view_id: &str, request: ViewportRequest) -> RowPage {
        let rows = self.display();
        RowPage {
            total: rows.len(),
            rows: rows
                .into_iter()
                .skip(request.start)
                .take(request.len)
                .collect(),
        }
    }

    fn row_by_id(&self, _view_id: &str, id: &RowId) -> Option<DisplayRow> {
        self.rows.iter().find(|row| &row.id == id).cloned()
    }

    fn index_of_id(&self, _view_id: &str, id: &RowId) -> Option<usize> {
        self.display().iter().position(|row| &row.id == id)
    }

    fn revision(&self, _view_id: &str) -> u64 {
        u64::from(self.folding())
    }

    fn set_fold(&self, _view_id: &str, request: &lvu::FoldRequest) {
        *self.request.borrow_mut() = request.clone();
    }

    fn fold_summary(&self, _view_id: &str) -> Option<lvu::FoldSummary> {
        let request = self.request.borrow();
        if !request.enabled {
            return Some(lvu::FoldSummary::default());
        }
        Some(lvu::FoldSummary {
            enabled: true,
            entries: 3,
            runs: usize::from(self.enabled()),
            folded_entries: usize::from(self.folding() && !self.expanded()),
            hidden_rows: if self.folding() && !self.expanded() {
                2
            } else {
                0
            },
            evicted_entries: 0,
            pending_rows: *self.pending_rows.borrow(),
        })
    }

    fn fold_members(&self, _view_id: &str, id: &RowId) -> Vec<RowId> {
        if id == &RowId::new("api", 1) {
            return (1..4).map(|sequence| RowId::new("api", sequence)).collect();
        }
        vec![id.clone()]
    }
}

fn folding_app() -> (FoldingProvider, App) {
    let mut app = App::new(
        vec![SourceItem {
            id: "api".into(),
            name: "api".into(),
            health: "ok".into(),
        }],
        vec![ViewItem {
            id: "view".into(),
            source_id: "api".into(),
            name: "view".into(),
        }],
        false,
    );
    let provider = FoldingProvider::new();
    app.sync_provider(&provider, 8);
    (provider, app)
}

/// Folding a large view is incremental: the window the user is looking at folds
/// first and the feed continues from there, so the pane is usable throughout
/// instead of blanking until the whole stream has been walked. The rows the feed
/// has not reached render individually, and the indicator says how many they are
/// rather than leaving them looking like a toggle that did nothing.
#[test]
fn an_unfinished_fold_reports_what_it_has_not_reached() {
    let (provider, mut app) = folding_app();
    enable_folding_via_dialog(&mut app, &provider);
    app.handle(Action::Top, &provider);
    *provider.pending_rows.borrow_mut() = 4_806_126;
    app.sync_provider(&provider, 8);
    let folding = render(&provider, &mut app, 100, 18);
    assert!(folding.contains("fold:1 runs, 2 hidden"), "{folding}");
    assert!(
        folding.contains("fold:1 runs, 2 hidden, +4806126"),
        "{folding}"
    );
    // The position counter keeps its place in front of the fold indicator, so a
    // narrow terminal loses the progress note before it loses where the user is.
    let counter = folding.find("/").expect("a position counter");
    assert!(
        counter < folding.find("fold:").expect("a fold indicator"),
        "{folding}"
    );

    // Once the feed has consumed the stream the note goes away on its own; the
    // counts it was reporting towards stay.
    *provider.pending_rows.borrow_mut() = 0;
    app.sync_provider(&provider, 8);
    let done = render(&provider, &mut app, 100, 18);
    assert!(done.contains("fold:1 runs, 2 hidden"), "{done}");
    assert!(!done.contains("folding "), "{done}");
}

/// The log pane's own row containing `needle`, from the event column onwards.
/// Assertions about the gutter have to be about this and not the whole screen,
/// which also carries the sources pane and the pane borders.
fn event_column(screen: &str) -> usize {
    let header = screen
        .lines()
        .find(|line| line.contains("time") && line.contains("level") && line.contains("event"))
        .expect("log column header");
    header[..header.find("event").unwrap()].chars().count()
}

fn event_cell(screen: &str, needle: &str) -> String {
    let line = screen
        .lines()
        .find(|line| line.contains(needle))
        .unwrap_or_else(|| panic!("no row containing {needle:?} in\n{screen}"));
    line.chars()
        .skip(event_column(screen))
        .collect::<String>()
        .trim_end_matches(['│', ' '])
        .to_owned()
}

/// A fold has to read as a fold. Collapsed, the entry showed its first member's
/// raw text with an "[xN repeated]" suffix that is off the right edge of an
/// eighty-column terminal, so it looked like one more log line. Expanded, its
/// members looked like every other row, so scrolling through a long run gave no
/// sign you were inside one.
#[test]
fn a_collapsed_fold_reads_as_a_fold_rather_than_a_log_line() {
    let (provider, mut app) = folding_app();
    enable_folding_via_dialog(&mut app, &provider);
    app.handle(Action::Top, &provider);
    let folded = render(&provider, &mut app, 100, 12);
    // The gutter marks it, the shape is shown with its placeholders rather than
    // one member's literal text, and the count and span are on their own line.
    let entry = event_cell(&folded, "retry connect failed after");
    assert!(
        entry.contains("› retry connect failed after <num>ms"),
        "{entry}"
    );
    let summary = event_cell(&folded, "×3 events");
    assert!(summary.contains("│ ×3 events"), "{summary}");
    assert!(
        summary.contains("12:00:01 → 12:00:03"),
        "the span names both ends: {summary}"
    );
    // Rows that are not part of a run keep the pane exactly as it was.
    let plain = event_cell(&folded, "service started");
    assert_eq!(plain.trim(), "service started", "{plain}");
}

/// Expanded, the run is bracketed from its first member to its last, so a long
/// run is visibly a run however far into it you have scrolled.
#[test]
fn an_expanded_run_is_bracketed_from_its_first_member_to_its_last() {
    let (provider, mut app) = folding_app();
    enable_folding_via_dialog(&mut app, &provider);
    app.handle(Action::Top, &provider);
    app.handle(Action::MoveLine(1), &provider);
    app.handle(Action::ToggleExpandedGroup, &provider);
    let expanded = render(&provider, &mut app, 100, 12);
    let gutters: Vec<char> = expanded
        .lines()
        .filter(|line| line.contains("retry connect"))
        .filter_map(|line| {
            line.chars()
                .skip(event_column(&expanded))
                .find(|ch| !ch.is_whitespace())
        })
        .collect();
    assert_eq!(gutters, vec!['┌', '│', '└'], "{expanded}");
    // The status keeps saying the view has a run in it while one is expanded,
    // which is exactly when the extent matters.
    assert!(expanded.contains("fold:1 runs"), "{expanded}");
}

/// The same three shapes in ASCII: a mark, a continuing line, and a cap at each
/// end. A terminal without box drawing must still show where a run begins and
/// ends.
#[test]
fn the_fold_gutter_has_an_ascii_form() {
    let (provider, mut app) = folding_app();
    app.appearance.ascii = true;
    enable_folding_via_dialog(&mut app, &provider);
    app.handle(Action::Top, &provider);
    let folded = render(&provider, &mut app, 100, 12);
    let entry = event_cell(&folded, "retry connect failed after");
    assert!(
        entry.contains("> retry connect failed after <num>ms"),
        "{entry}"
    );
    let summary = event_cell(&folded, "x3 events");
    assert!(summary.contains("| x3 events"), "{summary}");
    assert!(summary.contains("12:00:01 -> 12:00:03"), "{summary}");
    assert!(!entry.contains('›'), "{entry}");
    assert!(!summary.contains('×'), "{summary}");

    app.handle(Action::MoveLine(1), &provider);
    app.handle(Action::ToggleExpandedGroup, &provider);
    let expanded = render(&provider, &mut app, 100, 12);
    let gutters: Vec<char> = expanded
        .lines()
        .filter(|line| line.contains("retry connect"))
        .filter_map(|line| {
            line.chars()
                .skip(event_column(&expanded))
                .find(|ch| !ch.is_whitespace())
        })
        .collect();
    assert_eq!(gutters, vec!['+', '|', '+'], "{expanded}");
}

#[test]
fn folding_is_off_until_asked_for_and_then_says_so() {
    let (provider, mut app) = folding_app();
    assert!(!app.view_state().unwrap().fold_enabled);
    let plain = render(&provider, &mut app, 100, 18);
    assert!(plain.contains("retry connect failed"), "{plain}");
    assert!(!plain.contains("fold:"), "{plain}");
    assert!(!plain.contains("repeated"), "{plain}");

    // Exact keys only: name the service column first, then enable from the
    // dialog checkbox. The request plumbing below proves the exact key
    // reaches the provider; this fixture double keeps rendering its fixed
    // legacy Pattern shape, which is what the rest of this test reads.
    app.handle(Action::Open(Open::Folding), &provider);
    folding_pick_key(&mut app, &provider, 1);
    app.handle(raw_key(KeyCode::Esc), &provider);
    enable_folding_via_dialog(&mut app, &provider);
    render(&provider, &mut app, 100, 18);
    assert_eq!(
        provider.request.borrow().key_column.as_deref(),
        Some("service")
    );
    let folded = render(&provider, &mut app, 100, 18);
    // The collapse is shown as a fold — gutter mark, shape, count — rather than
    // as an "[x3 repeated]" suffix on one member's text, which sat off the right
    // edge of a narrow terminal. `row.text` still carries the suffix for
    // anything reading the row rather than the pane.
    assert!(folded.contains("› retry connect failed"), "{folded}");
    assert!(folded.contains("×3 events"), "{folded}");

    // Enter expands the run into its original events, in the original order.
    app.handle(Action::Top, &provider);
    app.handle(Action::MoveLine(1), &provider);
    let indicated = render(&provider, &mut app, 100, 18);
    // The status says folding is on and how much is hiding.
    assert!(indicated.contains("fold:1 runs, 2 hidden"), "{indicated}");
    assert_eq!(
        app.view_state().unwrap().selected,
        Some(RowId::new("api", 1))
    );
    app.handle(Action::ToggleExpandedGroup, &provider);
    assert_eq!(
        app.view_state().unwrap().fold_expanded,
        vec![RowId::new("api", 1)]
    );
    let expanded = render(&provider, &mut app, 100, 18);
    assert!(!expanded.contains("repeated"), "{expanded}");
    assert_eq!(
        provider
            .page("view", ViewportRequest { start: 0, len: 8 })
            .rows
            .len(),
        5
    );

    // Collapsing all restores the counted line without touching the records.
    app.handle(Action::CollapseAllFolds, &provider);
    assert!(app.view_state().unwrap().fold_expanded.is_empty());
    let recollapsed = render(&provider, &mut app, 100, 18);
    assert!(
        recollapsed.contains("› retry connect failed"),
        "{recollapsed}"
    );
    assert!(recollapsed.contains("×3 events"), "{recollapsed}");

    // Turning folding off restores every row and clears the indicator.
    // The toggle routes the unified Grouping UI, so the dialog checkbox that
    // enabled the fold disables it here.
    app.handle(Action::Open(Open::Folding), &provider);
    folding_focus(&mut app, &provider, FoldingControl::Enabled);
    app.handle(raw_key(KeyCode::Enter), &provider);
    assert!(!app.view_state().unwrap().fold_enabled);
    app.handle(raw_key(KeyCode::Esc), &provider);
    let off = render(&provider, &mut app, 100, 18);
    assert!(!off.contains("fold:"), "{off}");
    assert!(!off.contains("repeated"), "{off}");
    for sequence in 0..5 {
        assert!(
            provider
                .row_by_id("view", &RowId::new("api", sequence))
                .is_some(),
            "every record stays addressable"
        );
    }
}

#[test]
fn folding_configuration_and_expansion_survive_restart() {
    let (provider, mut app) = folding_app();
    let view_id = app.active_view_id().unwrap().to_owned();
    app.handle(Action::Open(Open::Folding), &provider);
    folding_pick_key(&mut app, &provider, 1);
    app.handle(raw_key(KeyCode::Esc), &provider);
    enable_folding_via_dialog(&mut app, &provider);
    render(&provider, &mut app, 100, 18);
    app.handle(Action::Top, &provider);
    app.handle(Action::MoveLine(1), &provider);
    assert_eq!(
        app.view_state().unwrap().selected,
        Some(RowId::new("api", 1))
    );
    app.handle(Action::ToggleExpandedGroup, &provider);

    let persisted = app.persistent_view_state(&view_id).unwrap();
    assert!(persisted.fold_enabled);
    assert_eq!(persisted.fold_key_column.as_deref(), Some("service"));
    assert_eq!(persisted.fold_minimum_run, 3);
    assert_eq!(persisted.fold_expanded, vec![RowId::new("api", 1)]);

    let (provider, mut restarted) = folding_app();
    let view_id = restarted.active_view_id().unwrap().to_owned();
    assert!(restarted.restore_persistent_view(&view_id, persisted));
    let state = restarted.view_state().unwrap();
    assert!(state.fold_enabled);
    assert_eq!(state.fold_minimum_run, 3);
    assert_eq!(state.fold_expanded, vec![RowId::new("api", 1)]);
    // The restored policy reaches the provider on the next frame.
    render(&provider, &mut restarted, 100, 18);
    assert!(provider.request.borrow().enabled);
    assert_eq!(
        provider.request.borrow().expanded,
        vec![RowId::new("api", 1)]
    );
}

#[test]
fn grouping_is_reachable_from_the_command_palette() {
    use lvu::command_palette::{CommandId, Palette, PaletteContext};
    let mut palette = Palette::new();
    palette.open(PaletteContext::new(Focus::Logs, true));
    for character in "grouping".chars() {
        let context = palette.context();
        palette.handle_key(
            KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE),
            context.clone(),
        );
    }
    assert_eq!(
        palette.selected_command().map(|command| command.id),
        Some(CommandId::Grouping),
        "grouping must be discoverable without a memorised key"
    );
    let mut collapse = Palette::new();
    collapse.open(PaletteContext::new(Focus::Logs, true));
    for character in "collapse expanded".chars() {
        let context = collapse.context();
        collapse.handle_key(
            KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE),
            context.clone(),
        );
    }
    assert_eq!(
        collapse.selected_command().map(|command| command.id),
        Some(CommandId::CollapseAllFolds)
    );
}

/// Drives one fork from an edit on a canonical view through to an installed
/// derived view, the way the runtime does: register, query, persist, install.
fn settle_fork(app: &mut App, provider: &FixtureProvider) -> Option<String> {
    let request = app.take_view_fork_requests().pop()?;
    let candidate = request.candidate_view_id.clone();
    // The runtime registers the candidate before its query may be submitted.
    provider.derive_view(&request.origin_view_id, &candidate);
    assert!(app.begin_fork_query(&candidate));
    let mut dispatcher = provider.query_dispatcher();
    submit_query_requests(app, &mut dispatcher);
    poll_query_completions(app, &mut dispatcher);
    let ready = app.take_ready_forks().pop()?;
    assert_eq!(ready.candidate_view_id, candidate);
    assert!(app.install_fork(&candidate));
    Some(candidate)
}

fn canonical_demo() -> (FixtureProvider, App, String) {
    let (provider, mut app) = demo();
    let view = app.active_view_id().unwrap().to_owned();
    app.set_view_role(&view, lvu::ViewRole::Canonical);
    (provider, app, view)
}

#[test]
fn the_canonical_view_cannot_be_filtered_in_place_and_the_filter_becomes_a_new_view() {
    let (provider, mut app, canonical) = canonical_demo();
    let views_before = app.views().len();
    app.handle(Action::Open(Open::Search), &provider);
    app.handle(Action::Raw(RawEvent::Paste("request 01".into())), &provider);
    app.handle(raw_key(KeyCode::Enter), &provider);

    // Nothing was queued against the canonical view, and its definition is
    // untouched even while the candidate is being prepared.
    assert!(
        app.take_query_requests().is_empty(),
        "an edit to All events never queries All events"
    );
    assert_eq!(app.views().len(), views_before, "no view is visible yet");
    assert!(app.search_state().unwrap().applied.is_empty());

    let candidate = settle_fork(&mut app, &provider).expect("one derived view");
    assert_eq!(app.views().len(), views_before + 1);
    assert_eq!(
        app.active_view_id(),
        Some(candidate.as_str()),
        "the new view is selected"
    );
    assert_eq!(app.view_role(&candidate), lvu::ViewRole::Derived);
    assert_eq!(
        app.persistent_view_state(&candidate)
            .unwrap()
            .applied_search,
        "request 01"
    );
    // The canonical view is back to unfiltered, drafts included.
    assert_eq!(app.view_role(&canonical), lvu::ViewRole::Canonical);
    let origin = app.persistent_view_state(&canonical).unwrap();
    assert!(origin.applied_search.is_empty());
    assert!(origin.search_draft.is_empty());

    // The derived view is an ordinary editable view from here on, including
    // its live debounced search.
    app.handle(Action::Open(Open::Search), &provider);
    app.handle(Action::Raw(RawEvent::Paste(" more".into())), &provider);
    assert!(app.flush_debounced_searches(Instant::now() + SEARCH_DEBOUNCE));
    assert!(
        app.take_view_fork_requests().is_empty(),
        "editing a derived view edits it in place"
    );
    assert_eq!(
        app.take_query_requests().pop().unwrap().view_id,
        candidate,
        "the query targets the derived view itself"
    );
}

#[test]
fn typing_on_the_canonical_view_applies_nothing_until_it_is_submitted() {
    let (provider, mut app, canonical) = canonical_demo();
    let views_before = app.views().len();
    app.handle(Action::Open(Open::Search), &provider);
    for character in "request 01".chars() {
        app.handle(raw_key(KeyCode::Char(character)), &provider);
        // The live search settles after every keystroke, and settles into
        // nothing: a draft on All events is only a draft.
        app.flush_debounced_searches(Instant::now() + SEARCH_DEBOUNCE);
        assert!(
            app.take_view_fork_requests().is_empty(),
            "typing proposed a view before it was applied"
        );
        assert!(
            app.take_query_requests().is_empty(),
            "typing queried the canonical view"
        );
        assert_eq!(app.views().len(), views_before);
        assert!(app.search_state().unwrap().applied.is_empty());
    }
    assert_eq!(
        app.search_state().unwrap().draft,
        "request 01",
        "the draft is kept for the moment it is applied"
    );

    // One apply, one view.
    app.handle(raw_key(KeyCode::Enter), &provider);
    let requests = app.take_view_fork_requests();
    assert_eq!(requests.len(), 1, "one apply proposes one view");
    let candidate = requests[0].candidate_view_id.clone();
    provider.derive_view(&canonical, &candidate);
    assert!(app.begin_fork_query(&candidate));
    let mut dispatcher = provider.query_dispatcher();
    submit_query_requests(&mut app, &mut dispatcher);
    poll_query_completions(&mut app, &mut dispatcher);
    let ready = app.take_ready_forks();
    assert_eq!(ready.len(), 1);
    assert!(app.install_fork(&ready[0].candidate_view_id));
    assert_eq!(app.views().len(), views_before + 1);
    assert_eq!(
        app.persistent_view_state(&candidate)
            .unwrap()
            .applied_search,
        "request 01"
    );
}

#[test]
fn a_rejected_filter_leaves_no_view_and_reports_on_the_view_being_edited() {
    let (provider, mut app, canonical) = canonical_demo();
    let views_before = app.views().len();
    app.handle(Action::Open(Open::Advanced), &provider);
    app.handle(
        Action::Raw(RawEvent::Paste("pl.col('level') == 'ERROR'".into())),
        &provider,
    );
    app.handle(raw_key(KeyCode::Enter), &provider);
    let request = app.take_view_fork_requests().pop().expect("fork request");
    let candidate = request.candidate_view_id.clone();
    assert!(app.begin_fork_query(&candidate));

    // The demo dispatcher rejects advanced expressions.
    let mut dispatcher = provider.query_dispatcher();
    submit_query_requests(&mut app, &mut dispatcher);
    poll_query_completions(&mut app, &mut dispatcher);

    assert!(app.take_ready_forks().is_empty());
    assert_eq!(app.views().len(), views_before, "no phantom view");
    assert!(app.persistent_view_state(&candidate).is_none());
    assert_eq!(
        app.take_fork_discards(),
        vec![candidate],
        "the runtime is told to unregister the candidate"
    );
    assert_eq!(app.active_view_id(), Some(canonical.as_str()));
    let error = app
        .view_state()
        .unwrap()
        .advanced
        .error
        .clone()
        .expect("the failure is reported where the user is looking");
    assert!(error.contains("advanced Polars adapter"), "{error}");
    assert!(
        app.persistent_view_state(&canonical)
            .unwrap()
            .applied_advanced
            .is_empty(),
        "All events is still unfiltered"
    );
}

#[test]
fn dismissing_an_editor_does_not_retract_a_filter_the_user_already_applied() {
    // Escape closes the surface in front of the user; it is not an undo. The
    // fork is asynchronous, so dismissing the editor before it settles used to
    // discard it, and `/error` Enter Escape — an ordinary sequence — left the
    // user on an unfiltered All events with no view and no diagnostic.
    let (provider, mut app, canonical) = canonical_demo();
    app.handle(Action::Open(Open::Search), &provider);
    app.handle(Action::Raw(RawEvent::Paste("request 01".into())), &provider);
    app.handle(raw_key(KeyCode::Enter), &provider);
    let candidate = app
        .take_view_fork_requests()
        .pop()
        .expect("fork request")
        .candidate_view_id;

    app.handle(raw_key(KeyCode::Esc), &provider);
    assert!(
        app.take_fork_discards().is_empty(),
        "an applied candidate survives the editor closing"
    );
    assert!(
        app.begin_fork_query(&candidate),
        "it still has a query to run"
    );
    // All events itself is still untouched while its candidate settles.
    assert!(
        app.persistent_view_state(&canonical)
            .unwrap()
            .applied_search
            .is_empty()
    );
    assert!(app.view_state().unwrap().search.error.is_none());
}

#[test]
fn dismissing_an_unapplied_draft_on_the_canonical_view_leaves_nothing_behind() {
    // The counterpart: typing alone never forks (`enqueue_live_query` refuses
    // on a canonical view), so there is no candidate for Escape to clean up.
    let (provider, mut app, canonical) = canonical_demo();
    let views_before = app.views().len();
    app.handle(Action::Open(Open::Search), &provider);
    app.handle(Action::Raw(RawEvent::Paste("request 01".into())), &provider);

    app.handle(raw_key(KeyCode::Esc), &provider);
    assert!(app.take_view_fork_requests().is_empty());
    assert!(app.take_fork_discards().is_empty());
    assert!(app.take_ready_forks().is_empty());
    assert_eq!(app.views().len(), views_before);
    assert!(
        app.persistent_view_state(&canonical)
            .unwrap()
            .applied_search
            .is_empty()
    );
}

#[test]
fn presentation_stays_editable_on_the_canonical_view() {
    let (provider, mut app, canonical) = canonical_demo();
    let views_before = app.views().len();
    app.sync_provider(&provider, 10);
    app.handle(Action::ToggleFollow, &provider);
    app.handle(Action::Top, &provider);
    app.handle(Action::Open(Open::Grouping), &provider);
    app.handle(Action::Raw(RawEvent::Paste("fixture".into())), &provider);
    app.handle(raw_key(KeyCode::Enter), &provider);
    let request = app
        .take_query_requests()
        .pop()
        .expect("grouping stays in place");
    assert_eq!(request.view_id, canonical);
    assert_eq!(request.purpose, QueryPurpose::Grouping);
    assert!(
        app.take_view_fork_requests().is_empty(),
        "display-only grouping does not create a view"
    );
    assert_eq!(app.views().len(), views_before);

    // Source membership, which does change what the view contains, is refused.
    let error = app
        .begin_source_change(&canonical, vec!["api".into(), "worker".into()])
        .expect_err("membership is fixed");
    assert!(error.contains("All events"), "{error}");
}

#[test]
fn a_bookmark_jumps_into_the_canonical_view_even_when_another_view_hides_the_record() {
    let (provider, mut app, canonical) = canonical_demo();
    app.sync_provider(&provider, 10);

    // Filtering All events produces the derived view the bookmark is taken in.
    app.handle(Action::Open(Open::Search), &provider);
    app.handle(Action::Raw(RawEvent::Paste("request 05".into())), &provider);
    app.handle(raw_key(KeyCode::Enter), &provider);
    let derived = settle_fork(&mut app, &provider).expect("derived view");
    // The editor stays open on the view the edit created; close it to work in
    // the log surface.
    app.handle(raw_key(KeyCode::Esc), &provider);
    app.sync_provider(&provider, 10);
    app.handle(Action::Top, &provider);
    app.handle(Action::ToggleBookmark, &provider);
    let bookmark = app.bookmarks_for_view(&derived)[0].id.clone();

    // Narrow that view further so its own bookmark no longer matches it.
    let mut dispatcher = provider.query_dispatcher();
    app.handle(Action::Open(Open::Search), &provider);
    app.handle(Action::Raw(RawEvent::Paste("1".into())), &provider);
    finish_debounced_search(&mut app, &mut dispatcher);
    app.handle(raw_key(KeyCode::Esc), &provider);
    app.sync_provider(&provider, 10);
    assert!(
        provider.index_of_id(&derived, &bookmark).is_none(),
        "the record is filtered out of the view holding the bookmark"
    );

    app.handle(Action::Open(Open::Bookmarks), &provider);
    app.handle(raw_key(KeyCode::Enter), &provider);
    assert_eq!(app.focus, Focus::Logs, "no raw-context detour");
    assert!(!app.layers.bookmarks.is_open());
    assert_eq!(
        app.active_view_id(),
        Some(canonical.as_str()),
        "the jump lands in the view that always holds the record"
    );
    app.sync_provider(&provider, 10);
    let state = app.view_state().unwrap();
    assert_eq!(state.selected.as_ref(), Some(&bookmark));
    assert!(!state.follow, "a jump stops following the tail");
    let index = provider.index_of_id(&canonical, &bookmark).unwrap();
    assert!(
        state.top <= index && index < state.top + 10,
        "the record is on screen: top {} index {index}",
        state.top
    );
    // The view the bookmark came from keeps its own filter and its bookmark.
    assert_eq!(
        app.persistent_view_state(&derived).unwrap().applied_search,
        "request 051"
    );
    assert_eq!(app.bookmarks_for_view(&derived).len(), 1);
}

#[test]
fn a_restart_reopens_the_view_last_used_and_all_events_only_until_one_is() {
    let (provider, mut app, canonical) = canonical_demo();
    let source = app
        .views()
        .iter()
        .find(|view| view.id == canonical)
        .unwrap()
        .source_id
        .clone();
    app.handle(Action::Open(Open::Search), &provider);
    app.handle(Action::Raw(RawEvent::Paste("request 01".into())), &provider);
    app.handle(raw_key(KeyCode::Enter), &provider);
    let derived = settle_fork(&mut app, &provider).expect("derived view");
    let canonical_state = app.persistent_view_state(&canonical).unwrap();
    let derived_state = app.persistent_view_state(&derived).unwrap();
    assert!(
        derived_state.selected_at > canonical_state.selected_at,
        "the view an apply creates becomes the one in use: {} vs {}",
        derived_state.selected_at,
        canonical_state.selected_at
    );

    // Restart: a fresh workspace restores both views and their positions.
    let (_provider, mut restarted) = demo();
    restarted.set_view_role(&canonical, lvu::ViewRole::Canonical);
    restarted.add_view(lvu::ViewItem {
        id: derived.clone(),
        source_id: source.clone(),
        name: "request 01".into(),
    });
    assert_eq!(restarted.active_view_id(), Some(canonical.as_str()));
    assert!(restarted.restore_persistent_view(&canonical, canonical_state));
    assert!(restarted.restore_persistent_view(&derived, derived_state));
    assert!(restarted.restore_source_selection(&source));
    assert_eq!(
        restarted.active_view_id(),
        Some(derived.as_str()),
        "a restart reopens the view that was in use"
    );
    assert!(
        !restarted.restore_source_selection(&source),
        "the remembered selection is applied once per source"
    );

    // A source whose views have never been chosen stays on All events.
    let (_provider, mut untouched) = demo();
    untouched.set_view_role(&canonical, lvu::ViewRole::Canonical);
    untouched.add_view(lvu::ViewItem {
        id: derived.clone(),
        source_id: source.clone(),
        name: "request 01".into(),
    });
    assert!(!untouched.restore_source_selection(&source));
    assert_eq!(untouched.active_view_id(), Some(canonical.as_str()));
}

#[test]
fn a_view_chosen_during_startup_is_not_pulled_back_by_the_restore() {
    let (provider, mut app) = demo();
    let canonical = app.active_view_id().unwrap().to_owned();
    let source = app
        .views()
        .iter()
        .find(|view| view.id == canonical)
        .unwrap()
        .source_id
        .clone();
    app.set_view_role(&canonical, lvu::ViewRole::Canonical);
    let derived = "derived-view".to_owned();
    app.add_view(lvu::ViewItem {
        id: derived.clone(),
        source_id: source.clone(),
        name: "derived".into(),
    });
    // The workspace remembers the derived view.
    app.set_view_selection_stamp(&derived, 7);
    // The user picks All events before the load finishes.
    app.select_view(&canonical);
    app.handle(Action::Top, &provider);
    assert!(
        !app.restore_source_selection(&source),
        "a choice made in this run wins over the remembered one"
    );
    assert_eq!(app.active_view_id(), Some(canonical.as_str()));
}

#[test]
fn a_correlated_view_keeps_its_constraint_through_a_rejected_later_edit() {
    let (provider, mut app) = demo();
    let correlation = lvu_core::FieldCorrelation::new(
        "request_id",
        lvu_core::ExactScalar::string("req-7").unwrap(),
        [
            ("api".to_owned(), "request_id".to_owned()),
            ("worker".to_owned(), "req".to_owned()),
        ]
        .into_iter()
        .collect(),
    )
    .unwrap();
    app.add_view(ViewItem {
        id: "correlated".into(),
        source_id: "api".into(),
        name: "request_id = \"req-7\"".into(),
    });
    assert!(app.restore_persistent_view(
        "correlated",
        PersistentViewState {
            source_ids: vec!["api".into(), "worker".into()],
            view_name: "request_id = \"req-7\"".into(),
            exact_field: Some(correlation.clone()),
            pinned_columns: correlation.fields(),
            ..PersistentViewState::default()
        },
    ));
    app.select_view("correlated");
    let restore = app.take_query_requests().pop().unwrap();
    assert_eq!(restore.constraints.exact_field, Some(correlation.clone()));
    assert_eq!(
        app.persistent_view_state("correlated")
            .unwrap()
            .pinned_columns,
        vec!["req".to_owned(), "request_id".to_owned()]
    );

    app.handle(Action::Open(Open::Search), &provider);
    app.handle(Action::Raw(RawEvent::Paste("boom".into())), &provider);
    app.handle(raw_key(KeyCode::Enter), &provider);
    let failed = app.take_query_requests().pop().unwrap();
    assert_eq!(
        failed.base_constraints.exact_field,
        Some(correlation.clone())
    );
    assert_eq!(failed.constraints.exact_field, Some(correlation.clone()));
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: failed.view_id,
        generation: failed.generation,
        revision: failed.revision,
        purpose: failed.purpose,
        result: Err(QueryFailure {
            purpose: QueryPurpose::Search,
            message: "candidate rejected".into(),
        }),
    }));
    // The correlation is a view constraint, so a rejected search leaves it,
    // its sources and its pins intact.
    let state = app.persistent_view_state("correlated").unwrap();
    assert_eq!(state.exact_field, Some(correlation));
    assert_eq!(
        state.source_ids,
        vec!["api".to_owned(), "worker".to_owned()]
    );
}

#[test]
fn a_correlation_naming_a_source_the_view_does_not_carry_is_refused() {
    let (_provider, mut app) = demo();
    app.add_view(ViewItem {
        id: "correlated".into(),
        source_id: "api".into(),
        name: "narrow".into(),
    });
    let elsewhere = lvu_core::FieldCorrelation::new(
        "request_id",
        lvu_core::ExactScalar::string("req-7").unwrap(),
        [("worker".to_owned(), "req".to_owned())]
            .into_iter()
            .collect(),
    )
    .unwrap();
    assert!(!app.restore_persistent_view(
        "correlated",
        PersistentViewState {
            source_ids: vec!["api".into()],
            exact_field: Some(elsewhere),
            ..PersistentViewState::default()
        },
    ));
    assert!(
        app.persistent_view_state("correlated")
            .unwrap()
            .exact_field
            .is_none()
    );
}

#[test]
fn an_invalid_union_restore_is_transactional() {
    let (_provider, mut app) = demo();
    let view = app.active_view_id().unwrap().to_owned();
    let accepted_correlation = lvu_core::FieldCorrelation::new(
        "request_id",
        lvu_core::ExactScalar::string("req-7").unwrap(),
        [("api".to_owned(), "request_id".to_owned())]
            .into_iter()
            .collect(),
    )
    .unwrap();
    let accepted = PersistentViewState {
        view_name: "accepted name".into(),
        applied_search: "request".into(),
        search_draft: "accepted draft".into(),
        severity_column: Some("severity".into()),
        timestamp_column: Some("event_time".into()),
        exact_field: Some(accepted_correlation),
        ..PersistentViewState::default()
    };
    assert!(app.restore_persistent_view(&view, accepted));
    let _ = app.take_query_requests();
    let before = app.persistent_view_state(&view).unwrap();
    let definition_revision = app.view_definition_revision(&view).unwrap();

    let mut invalid = before.clone();
    invalid.view_name = "must not land".into();
    invalid.applied_search = "must not land".into();
    invalid.search_draft = "must not land".into();
    invalid.severity_column = None;
    invalid.timestamp_column = None;
    invalid.exact_field = None;
    invalid.union = Some(lvu::PersistentUnion {
        // The count is valid, but the duplicate/self-referential definition
        // is not. Rejection must still precede cancellation, revision changes,
        // or accepted-state edits.
        inputs: vec![
            lvu::PersistentUnionInput {
                view_id: view.clone(),
                accepted_revision: 7,
                applied_generation: 9,
            },
            lvu::PersistentUnionInput {
                view_id: view.clone(),
                accepted_revision: 7,
                applied_generation: 9,
            },
        ],
        filter: "invalid".into(),
        advanced_filter: String::new(),
        exact_key: None,
    });
    assert!(!app.restore_persistent_view(&view, invalid));
    assert_eq!(app.persistent_view_state(&view), Some(before));
    assert_eq!(
        app.view_definition_revision(&view),
        Some(definition_revision)
    );
    assert!(app.take_query_requests().is_empty());
}

// ---------------------------------------------------------------------------
// The Folding dialog (W23): the fold key is one column, per view
// ---------------------------------------------------------------------------

/// Walk the Folding dialog's focus ring to a named control.
fn folding_focus<P: RowProvider>(app: &mut App, provider: &P, control: FoldingControl) {
    for _ in 0..8 {
        if app.layers.folding.control() == Some(control) {
            return;
        }
        app.handle(raw_key(KeyCode::Tab), provider);
    }
    panic!("{control:?} is not reachable by Tab");
}

/// Enable folding with the stored key through the dialog checkbox. The
/// ToggleFolding action routes the unified Grouping UI, so tests that pin
/// legacy fold rendering enable here instead of through it.
fn enable_folding_via_dialog<P: RowProvider>(app: &mut App, provider: &P) {
    app.handle(Action::Open(Open::Folding), provider);
    folding_focus(app, provider, FoldingControl::Enabled);
    app.handle(raw_key(KeyCode::Enter), provider);
    assert!(app.view_state().unwrap().fold_enabled);
    app.handle(raw_key(KeyCode::Esc), provider);
}

/// Open the key-column list and pick the row at `index`.
fn folding_pick_key<P: RowProvider>(app: &mut App, provider: &P, index: usize) {
    folding_focus(app, provider, FoldingControl::KeyColumn);
    // The list opens on the row that is currently the value, so walk from there.
    app.handle(raw_key(KeyCode::Enter), provider);
    while app.layers.folding.highlighted() > index {
        app.handle(raw_key(KeyCode::Up), provider);
    }
    while app.layers.folding.highlighted() < index {
        app.handle(raw_key(KeyCode::Down), provider);
    }
    app.handle(raw_key(KeyCode::Enter), provider);
}

#[test]
fn the_grouping_dialog_opens_from_either_multiline_key_and_the_palette() {
    use lvu::command_palette::{CommandId, Palette, PaletteContext};

    // `m` and `z` route the same unified Grouping UI: runs and starts are
    // defined once, on enrichment columns.
    for code in [KeyCode::Char('m'), KeyCode::Char('z')] {
        assert_eq!(
            key_to_action(KeyEvent::new(code, KeyModifiers::NONE), Focus::Logs),
            Action::Open(Open::Grouping)
        );
    }
    let (provider, mut app) = folding_app();
    app.handle(
        key_to_action(
            KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE),
            Focus::Logs,
        ),
        &provider,
    );
    assert!(app.layers.grouping.is_open());
    let dialog = render(&provider, &mut app, 100, 28);
    assert!(dialog.contains("Multiline grouping"), "{dialog}");
    assert!(dialog.contains("Run"), "{dialog}");
    app.handle(raw_key(KeyCode::Esc), &provider);

    let mut palette = Palette::new();
    palette.open(PaletteContext::new(Focus::Logs, true));
    for character in "grouping".chars() {
        let context = palette.context();
        palette.handle_key(
            KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE),
            context.clone(),
        );
    }
    assert!(
        palette
            .selected_command()
            .map(|command| command.id == CommandId::Grouping
                || command.id == CommandId::FoldingDialog)
            .unwrap_or(false),
        "grouping must be reachable without a memorised key"
    );
}

#[test]
fn choosing_a_key_column_is_what_the_view_asks_its_provider_for() {
    let (provider, mut app) = folding_app();
    app.views.active_mut().unwrap().fold_enabled = true; // Restored legacy presentation.
    render(&provider, &mut app, 100, 28);
    assert_eq!(provider.request.borrow().key_column, None);

    app.handle(Action::Open(Open::Folding), &provider);
    // Row 0 is the first sampled column, sorted; the derived pattern is
    // restore-only and never offered for new selection.
    folding_pick_key(&mut app, &provider, 0);
    assert_eq!(
        app.view_state().unwrap().fold_key_column.as_deref(),
        Some("host")
    );
    render(&provider, &mut app, 100, 28);
    assert_eq!(
        provider.request.borrow().key_column.as_deref(),
        Some("host")
    );

    // Normalisation is only offered while the key is the derived column,
    // because it is the only key it can affect.
    let column = render(&provider, &mut app, 100, 28);
    assert!(!column.contains("Normalisation"), "{column}");
    assert!(
        column.contains("nothing is normalised"),
        "the help must say what a column key does: {column}"
    );
    // The pattern row is gone for good: reopening the list offers exact
    // columns, so row 0 keeps naming the first sampled column instead of
    // switching back to normalisation.
    folding_pick_key(&mut app, &provider, 0);
    assert_eq!(
        app.view_state().unwrap().fold_key_column.as_deref(),
        Some("host")
    );
    let exact = render(&provider, &mut app, 100, 28);
    assert!(!exact.contains("Normalisation"), "{exact}");
    assert!(!exact.contains("Message pattern"), "{exact}");
    render(&provider, &mut app, 100, 28);
    assert_eq!(
        provider.request.borrow().key_column.as_deref(),
        Some("host")
    );

    // A stored pattern key still restores through the engine default: a fresh
    // dialog names the plain default value while the list stays exact-only.
    let (legacy_provider, mut legacy_app) = folding_app();
    legacy_app.handle(Action::Open(Open::Folding), &legacy_provider);
    let legacy = render(&legacy_provider, &mut legacy_app, 100, 28);
    assert!(legacy.contains("Message pattern"), "{legacy}");
    assert!(legacy.contains("timestamps, ids and numbers"), "{legacy}");
}

#[test]
fn folding_picker_never_offers_the_pattern_row() {
    use lvu::QueryCompletion;

    let (provider, mut app) = folding_app();
    // Even ungrouped, the list names exact columns: the derived pattern is
    // restore-only through the engine default, never a new selection.
    app.handle(Action::Open(Open::Folding), &provider);
    folding_focus(&mut app, &provider, FoldingControl::KeyColumn);
    app.handle(raw_key(KeyCode::Enter), &provider);
    let legacy = render(&provider, &mut app, 100, 28);
    // The stored default still names itself in the value row, but no picker
    // row offers it: every line naming the pattern is the value display.
    for line in legacy.lines() {
        assert!(
            !line.contains("Message pattern") || line.contains("Key column"),
            "{line}\n{legacy}"
        );
    }
    assert!(legacy.contains("host"), "{legacy}");
    // Pick the first sampled column to close the list.
    app.handle(raw_key(KeyCode::Enter), &provider);
    assert_eq!(
        app.view_state().unwrap().fold_key_column.as_deref(),
        Some("host")
    );
    app.handle(raw_key(KeyCode::Esc), &provider);

    // Group the view by runs of that column through the normal control.
    app.handle(Action::Open(Open::Grouping), &provider);
    app.handle(raw_key(KeyCode::Down), &provider);
    assert_eq!(
        app.view_state().unwrap().grouping.draft,
        lvu::grouping::run_rule("host")
    );
    app.handle(raw_key(KeyCode::Enter), &provider);
    let request = app.take_query_requests().pop().unwrap();
    assert_eq!(request.purpose, QueryPurpose::Grouping);
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: request.view_id,
        generation: request.generation,
        revision: request.revision,
        purpose: request.purpose,
        result: Ok(()),
    }));
    app.handle(raw_key(KeyCode::Esc), &provider);

    // Beside configured grouping the pattern row is gone: recognition lives
    // in Enrichment, so normalisation is not newly selectable. The stored
    // pattern would still restore through the engine default.
    app.handle(Action::Open(Open::Folding), &provider);
    folding_focus(&mut app, &provider, FoldingControl::KeyColumn);
    app.handle(raw_key(KeyCode::Enter), &provider);
    let exact = render(&provider, &mut app, 100, 28);
    assert!(!exact.contains("Message pattern"), "{exact}");
    assert!(exact.contains("host"), "{exact}");
    app.handle(raw_key(KeyCode::Esc), &provider);
}

#[test]
fn minimum_run_scope_and_normalisation_reach_the_provider() {
    let (provider, mut app) = folding_app();
    app.views.active_mut().unwrap().fold_enabled = true; // Restored legacy presentation.
    app.handle(Action::Open(Open::Folding), &provider);

    folding_focus(&mut app, &provider, FoldingControl::MinimumRun);
    app.handle(raw_key(KeyCode::Enter), &provider);
    app.handle(raw_key(KeyCode::Down), &provider);
    app.handle(raw_key(KeyCode::Enter), &provider);
    assert_eq!(app.view_state().unwrap().fold_minimum_run, 5);

    folding_focus(&mut app, &provider, FoldingControl::Scope);
    app.handle(raw_key(KeyCode::Enter), &provider);
    app.handle(raw_key(KeyCode::Down), &provider);
    app.handle(raw_key(KeyCode::Enter), &provider);
    assert_eq!(app.view_state().unwrap().fold_lookback, 2);

    folding_focus(&mut app, &provider, FoldingControl::Normalisation);
    app.handle(raw_key(KeyCode::Enter), &provider);
    app.handle(raw_key(KeyCode::Down), &provider);
    app.handle(raw_key(KeyCode::Enter), &provider);
    assert_eq!(
        app.view_state().unwrap().fold_normalisation,
        lvu::FoldNormalisation::Aggressive
    );

    render(&provider, &mut app, 100, 28);
    let request = provider.request.borrow().clone();
    assert_eq!(request.minimum_run, 5);
    assert_eq!(request.scope, lvu::FoldScopeRequest::Lookback(2));
    assert_eq!(request.normalisation, lvu::FoldNormalisation::Aggressive);

    // The on/off toggle that exists today lives in the dialog too, and the
    // status line still reports what the fold is doing.
    folding_focus(&mut app, &provider, FoldingControl::Enabled);
    app.handle(raw_key(KeyCode::Char(' ')), &provider);
    assert!(!app.view_state().unwrap().fold_enabled);
    let off = render(&provider, &mut app, 100, 28);
    assert!(off.contains("every row is listed individually"), "{off}");
}

#[test]
fn the_new_column_action_opens_the_step_editor_on_a_concatenation() {
    let (provider, mut app) = folding_app();
    app.handle(Action::Open(Open::Folding), &provider);
    folding_focus(&mut app, &provider, FoldingControl::KeyColumn);
    app.handle(raw_key(KeyCode::Enter), &provider);
    // Past the two sampled columns sits `[ New column… ]`: no derived row.
    for _ in 0..2 {
        app.handle(raw_key(KeyCode::Down), &provider);
    }
    let picker = render(&provider, &mut app, 100, 28);
    assert!(picker.contains("New column"), "{picker}");
    app.handle(raw_key(KeyCode::Enter), &provider);
    assert_eq!(app.layers.folding.composing(), Some(&[][..]));

    // The picker becomes a checkbox list of the fields the new column combines.
    let composing = render(&provider, &mut app, 100, 28);
    assert!(composing.contains("[ ]"), "{composing}");
    assert_eq!(app.layers.folding.highlighted(), 0, "{composing}");
    app.handle(raw_key(KeyCode::Char(' ')), &provider);
    app.handle(raw_key(KeyCode::Down), &provider);
    app.handle(raw_key(KeyCode::Char(' ')), &provider);
    assert_eq!(
        app.layers.folding.composing(),
        Some(&["host".to_owned(), "service".to_owned()][..])
    );
    app.handle(raw_key(KeyCode::Enter), &provider);

    // One enrichment step, pre-filled, in the ordinary step editor: there is no
    // second field-combination mechanism.
    assert!(app.layers.enrichment_step.is_open());
    let draft = app.view_state().unwrap().enrichment.draft.clone();
    assert_eq!(
        draft,
        "fold_key = pl.concat_str([pl.col('host').cast(pl.String), \
         pl.col('service').cast(pl.String)], separator=\"|\", ignore_nulls=True)"
    );
    let editor = render(&provider, &mut app, 100, 28);
    assert!(editor.contains("concat_str"), "{editor}");

    // Saving the step selects the column it created, and the fold key follows.
    step_submit(&mut app, &provider);
    let request = app
        .take_query_requests()
        .pop()
        .expect("an enrichment query");
    assert_eq!(request.purpose, QueryPurpose::Enrichment);
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: request.view_id,
        generation: request.generation,
        revision: request.revision,
        purpose: QueryPurpose::Enrichment,
        result: Ok(()),
    }));
    assert!(!app.layers.enrichment_step.is_open());
    let state = app.view_state().unwrap();
    assert_eq!(state.fold_key_column.as_deref(), Some("fold_key"));
    assert!(
        state.fold_enabled,
        "the column was built in order to fold on it"
    );
    render(&provider, &mut app, 100, 28);
    assert_eq!(
        provider.request.borrow().key_column.as_deref(),
        Some("fold_key")
    );
}

#[test]
fn a_cancelled_generated_column_never_changes_the_fold_key() {
    let (provider, mut app) = folding_app();
    app.handle(Action::Open(Open::Folding), &provider);
    folding_focus(&mut app, &provider, FoldingControl::KeyColumn);
    app.handle(raw_key(KeyCode::Enter), &provider);
    for _ in 0..2 {
        app.handle(raw_key(KeyCode::Down), &provider);
    }
    app.handle(raw_key(KeyCode::Enter), &provider);
    app.handle(raw_key(KeyCode::Char(' ')), &provider);
    app.handle(raw_key(KeyCode::Enter), &provider);
    assert!(app.layers.enrichment_step.is_open());

    // Escape out of the editor without saving, then let an unrelated enrichment
    // land. The key column must not move: nothing produced `fold_key`.
    app.handle(raw_key(KeyCode::Esc), &provider);
    assert!(!app.layers.enrichment_step.is_open());
    assert!(app.layers.folding.is_open());
    let view_id = app.active_view_id().unwrap().to_owned();
    if let Some(state) = app.views.state_mut(&view_id) {
        state.enrichments.push(lvu::EnrichmentDefinition {
            id: lvu::EnrichmentStageId("other".into()),
            source: "latency = pl.col('raw')".into(),
            command: None,
        });
    }
    assert!(!app.apply_query_completion(QueryCompletion {
        view_id,
        generation: 99,
        revision: 99,
        purpose: QueryPurpose::Enrichment,
        result: Ok(()),
    }));
    assert_eq!(app.view_state().unwrap().fold_key_column, None);

    // Escape closes the picker, then the dialog: the innermost surface first.
    app.handle(raw_key(KeyCode::Esc), &provider);
    assert!(!app.layers.folding.is_open());
}

#[test]
fn the_folding_key_column_and_policy_survive_a_restart() {
    let (provider, mut app) = folding_app();
    let view_id = app.active_view_id().unwrap().to_owned();
    app.handle(Action::Open(Open::Folding), &provider);
    folding_pick_key(&mut app, &provider, 1);
    folding_focus(&mut app, &provider, FoldingControl::Scope);
    app.handle(raw_key(KeyCode::Enter), &provider);
    app.handle(raw_key(KeyCode::Down), &provider);
    app.handle(raw_key(KeyCode::Enter), &provider);

    let persisted = app.persistent_view_state(&view_id).unwrap();
    assert_eq!(persisted.fold_key_column.as_deref(), Some("service"));
    assert_eq!(persisted.fold_lookback, 2);

    let (provider, mut restarted) = folding_app();
    let view_id = restarted.active_view_id().unwrap().to_owned();
    assert!(restarted.restore_persistent_view(&view_id, persisted));
    let state = restarted.view_state().unwrap();
    assert_eq!(state.fold_key_column.as_deref(), Some("service"));
    assert_eq!(state.fold_lookback, 2);
    // The provider only hears the key while folding is enabled: enable from
    // the dialog checkbox, as the retired toggle used to.
    restarted.handle(Action::Open(Open::Folding), &provider);
    folding_focus(&mut restarted, &provider, FoldingControl::Enabled);
    restarted.handle(raw_key(KeyCode::Enter), &provider);
    assert!(restarted.view_state().unwrap().fold_enabled);
    restarted.handle(raw_key(KeyCode::Esc), &provider);
    render(&provider, &mut restarted, 100, 28);
    assert_eq!(
        provider.request.borrow().key_column.as_deref(),
        Some("service")
    );

    // A view written before this dialog existed folds exactly as it did: the
    // absent column reads as the derived pattern column.
    let mut legacy = restarted.persistent_view_state(&view_id).unwrap();
    legacy.fold_key_column = None;
    legacy.fold_lookback = 0;
    assert!(restarted.restore_persistent_view(&view_id, legacy));
    render(&provider, &mut restarted, 100, 28);
    let request = provider.request.borrow().clone();
    assert_eq!(request.key_column, None);
    assert_eq!(request.scope, lvu::FoldScopeRequest::Adjacent);
    assert_eq!(request.normalisation, lvu::FoldNormalisation::Standard);
}

#[test]
fn the_folding_dialog_fits_a_narrow_terminal() {
    let (provider, mut app) = folding_app();
    app.handle(Action::Open(Open::Folding), &provider);
    for (width, height) in [(80u16, 24u16), (54, 16)] {
        let screen = render(&provider, &mut app, width, height);
        assert!(screen.contains("Folding"), "{width}x{height}: {screen}");
        assert!(screen.contains("Key column"), "{width}x{height}: {screen}");
        assert!(
            screen.contains("Message pattern"),
            "{width}x{height}: {screen}"
        );
        for line in screen.lines() {
            assert!(
                line.chars().count() <= usize::from(width),
                "{width}x{height} overflows: {line:?}"
            );
        }
    }
}

/// §5.2.1: the key-column picker is a live region. Its content is a bounded
/// sample of the view's rows, so a source that is still arriving can add a
/// column while the list is open — and the popup must not resize under the
/// cursor when it does.
///
/// The list is anchored class A, whose height `anchored_rect` takes from its
/// item count; passing the reservation instead is what makes the rect the same
/// on every frame. An overlong list says how much it is holding.
#[test]
fn the_key_column_picker_keeps_one_rectangle_while_the_column_set_changes() {
    for (width, height) in [(80u16, 24u16), (54, 16)] {
        let (provider, mut app) = folding_app();
        app.handle(Action::Open(Open::Folding), &provider);
        folding_focus(&mut app, &provider, FoldingControl::KeyColumn);
        app.handle(raw_key(KeyCode::Enter), &provider);

        let mut rects = Vec::new();
        // Two columns, then none, then more than the reservation can show.
        for extra in [0usize, 0, 6, 40, 0] {
            provider.set_extra_columns(extra);
            app.sync_provider(&provider, 8);
            let screen = render(&provider, &mut app, width, height);
            rects.push((extra, app.layers.folding.surface().popup));
            if extra == 40 {
                // The affordance for a region with no pane heading.
                assert!(screen.contains("more"), "{width}x{height}: {screen}");
            }
        }
        let first = rects[0].1;
        assert!(
            rects.iter().all(|(_, rect)| *rect == first),
            "{width}x{height}: the picker moved: {rects:?}"
        );
        assert!(first.height >= 4, "{width}x{height}: {first:?}");
    }
}

/// §8.9: Folding is a settings dialog — every field takes effect where it
/// stands — so its one verb is its default, filled, and Enter reaches it from
/// the button. Every other control consumes Enter itself, per §8.9's table.
#[test]
fn folding_declares_collapse_as_its_default_and_every_control_consumes_enter() {
    let (provider, mut app) = folding_app();
    app.handle(Action::Open(Open::Folding), &provider);
    folding_pick_key(&mut app, &provider, 0);

    // A checkbox toggles with Enter and back with Space.
    folding_focus(&mut app, &provider, FoldingControl::Enabled);
    app.handle(raw_key(KeyCode::Enter), &provider);
    assert!(app.view_state().unwrap().fold_enabled);
    app.handle(raw_key(KeyCode::Char(' ')), &provider);
    assert!(!app.view_state().unwrap().fold_enabled);

    // A closed dropdown opens; Escape closes the list, not the dialog.
    folding_focus(&mut app, &provider, FoldingControl::Scope);
    app.handle(raw_key(KeyCode::Enter), &provider);
    let open = render(&provider, &mut app, 100, 28);
    assert!(open.contains("Lookback"), "{open}");
    app.handle(raw_key(KeyCode::Esc), &provider);
    assert!(app.layers.folding.is_open());

    // §8.2: Space never presses a button. Enter on the default runs it.
    // A disabled Collapse is skipped by focus navigation, so re-enable
    // alongside the seeded expansion: this half tests the button, not the
    // checkbox above.
    if let Some(state) = app.views.active_mut() {
        state.fold_enabled = true;
        state.fold_expanded.push(RowId::new("api", 1));
    }
    folding_focus(&mut app, &provider, FoldingControl::Collapse);
    app.handle(raw_key(KeyCode::Char(' ')), &provider);
    assert_eq!(
        app.view_state().unwrap().fold_expanded,
        vec![RowId::new("api", 1)],
        "Space must not press the default"
    );
    app.handle(raw_key(KeyCode::Enter), &provider);
    assert!(app.view_state().unwrap().fold_expanded.is_empty());
}
