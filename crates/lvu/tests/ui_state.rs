use std::{
    cell::RefCell,
    collections::HashMap,
    time::{Duration, Instant},
};

use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};
use lvu::theme::{Theme, ThemeId};
use lvu::{
    Action, App, AskAiKind, AskAiRequest, AskAiStage, DisplayRow, Focus, InvestigationItem,
    InvestigationRequest, InvestigationStage, PersistentViewState, QueryCompletion,
    QueryConstraints, QueryFailure, QueryPurpose, QueryRequest, RowId, RowPage, RowProvider,
    SettingsContext, SettingsValues, SourceKind, StorageCategory, StorageEntry, StorageSnapshot,
    ViewportRequest,
    app::{
        CommandEnrichmentRequest, CommandEnrichmentReview, MAX_EDITOR_BYTES, SEARCH_DEBOUNCE,
        SourceItem, ViewItem, key_to_action,
    },
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
    app.handle(Action::OpenStorage, &provider);
    let request = app.take_storage_requests().pop().unwrap();
    assert_eq!(app.focus, Focus::Storage);
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
    assert!(!app.update_storage(
        request.generation + 1,
        snapshot.clone(),
        "stale".into(),
        true
    ));
    assert!(app.update_storage(request.generation, snapshot, "complete".into(), true));
    let screen = render(&provider, &mut app, 100, 25);
    assert!(screen.contains("unused.rows.idx"));
    assert!(screen.contains("not a process RSS limit"));
    assert!(screen.contains("r refresh"));
    assert!(screen.contains("↑/↓ active pane"));
    assert!(app.dialog_scroll_limit > 0);
    let status = app
        .hit_regions
        .dialog_scroll
        .expect("storage status hitbox");
    let selected = app.storage_dialog.as_ref().unwrap().selected;
    app.dialog_scroll = 0;
    app.dialog_scroll_focused = false;
    app.handle(
        Action::Mouse(mouse(
            MouseEventKind::ScrollDown,
            status.x + 1,
            status.y + 1,
        )),
        &provider,
    );
    assert!(
        app.dialog_scroll > 0,
        "wheel scrolls the hovered status pane"
    );
    assert_eq!(app.storage_dialog.as_ref().unwrap().selected, selected);
    app.handle(Action::ScrollDialog(i32::MAX), &provider);
    let scrolled = render(&provider, &mut app, 100, 25);
    assert!(scrolled.contains("bounded detail"));
    assert!(scrolled.contains("unused.rows.idx"));
    app.handle(Action::ClearStorage, &provider);
    assert!(app.take_storage_requests().is_empty());
    assert!(app.storage_dialog.as_ref().unwrap().confirm_clear);
    app.handle(Action::ClearStorage, &provider);
    assert!(matches!(
        app.take_storage_requests()[0].kind,
        lvu::StorageRequestKind::ClearUnusedDerived
    ));
    app.handle(Action::CancelEditor, &provider);
    assert!(matches!(
        app.take_storage_requests()[0].kind,
        lvu::StorageRequestKind::Cancel
    ));
    assert_eq!(app.focus, Focus::Logs);
}

fn demo() -> (FixtureProvider, App) {
    let (provider, sources, views) = FixtureProvider::demo();
    (provider, App::new(sources, views, true))
}

fn settings_context() -> SettingsContext {
    SettingsContext {
        saved: SettingsValues {
            provider: "codex/old".into(),
            mode: "full-access".into(),
            thinking: "medium".into(),
            theme: ThemeId::Terminal,
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
    app.handle(Action::OpenSettings, &provider);
    let first_generation = app.settings_dialog.as_ref().unwrap().generation;
    app.handle(Action::MoveSettings(3), &provider);
    app.handle(Action::CycleSetting, &provider);
    assert_eq!(
        app.theme_id,
        ThemeId::LoveDark,
        "theme previews immediately"
    );
    app.handle(Action::SaveSettings, &provider);
    let request = app.take_settings_requests().pop().unwrap();
    assert_eq!(request.generation, first_generation);

    app.handle(Action::CancelEditor, &provider);
    assert_eq!(
        app.theme_id,
        ThemeId::Terminal,
        "cancel restores effective theme"
    );
    app.handle(Action::OpenSettings, &provider);
    assert_ne!(
        app.settings_dialog.as_ref().unwrap().generation,
        first_generation
    );
    assert!(app.complete_settings_save(first_generation, Ok(settings_context())));
    assert_eq!(
        app.settings_dialog.as_ref().unwrap().draft.theme,
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
        app.theme_id.theme().cursor,
        "focused editable settings field has a visible semantic cursor"
    );

    let settings_screen = render(&provider, &mut app, 110, 28);
    assert!(settings_screen.contains("environment LVU_AI_PROVIDER"));
    assert!(settings_screen.contains("global settings.toml"));

    for _ in 0..10 {
        app.handle(Action::MoveSettings(1), &provider);
    }
    let backend = TestBackend::new(54, 12);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| ui::render(frame, &mut app, &provider))
        .unwrap();
    let cursor = terminal.backend().cursor_position();
    let rendered = screen(terminal.backend().buffer());
    assert!(rendered.contains("Index/source MiB"), "{rendered}");
    assert!(rendered.contains("Space toggle"), "{rendered}");
    assert!(cursor.y < 10, "cursor must stay above the reserved footer");
}

#[test]
fn long_unicode_editor_uses_scrolled_input_surface_and_keeps_footer_clear() {
    let (provider, mut app) = demo();
    app.handle(Action::OpenAdvanced, &provider);
    let draft = "前置き".repeat(40) + " visible-tail";
    app.handle(Action::EditorPaste(draft), &provider);
    let backend = TestBackend::new(54, 12);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| ui::render(frame, &mut app, &provider))
        .unwrap();
    let cursor = terminal.backend().cursor_position();
    let buffer = terminal.backend().buffer();
    assert_eq!(buffer[(cursor.x, cursor.y)].bg, app.theme_id.theme().cursor);
    let rendered = screen(buffer);
    assert!(
        rendered.contains("visible-tail"),
        "the tail nearest the cursor stays visible"
    );
    assert!(!rendered.contains("Enter apply"), "{rendered}");
    assert!(usize::from(cursor.y) < rendered.lines().count());

    app.handle(Action::ToggleEditorCompletion, &provider);
    let backend = TestBackend::new(54, 12);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| ui::render(frame, &mut app, &provider))
        .unwrap();
    assert!(screen(terminal.backend().buffer()).contains("Complete field"));
    assert_ne!(
        terminal.backend().buffer()[terminal.backend().cursor_position()].bg,
        app.theme_id.theme().cursor,
        "completion overlay owns focus instead of leaving the editor cursor painted above it"
    );
}

#[test]
fn long_source_path_scrolls_inside_padded_body_above_footer() {
    let (provider, mut app) = demo();
    app.handle(Action::OpenSource, &provider);
    let path = format!("/tmp/{}/visible.log", "長い path ".repeat(20));
    for character in path.chars() {
        app.handle(Action::SourceInput(character), &provider);
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
    app.handle(Action::OpenSearch, &provider);
    app.handle(Action::EditorPaste("request 01".into()), &provider);
    app.handle(Action::CancelEditor, &provider);
    app.handle(Action::OpenSearch, &provider);
    assert_eq!(app.search_state().expect("search").draft, "request 01");

    app.handle(Action::SubmitDraft, &provider);
    app.handle(Action::EditorInput('2'), &provider);
    app.handle(Action::SubmitDraft, &provider);
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
    app.handle(Action::CancelEditor, &provider);
    app.handle(Action::NextView, &provider);
    app.handle(Action::OpenSearch, &provider);
    app.handle(Action::EditorPaste("queue".into()), &provider);
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
    app.handle(Action::CancelEditor, &provider);
    app.handle(Action::PreviousView, &provider);
    assert_eq!(app.search_state().expect("search").applied, "request 012");

    app.handle(Action::OpenSearch, &provider);
    let before_oversized_paste = app.search_state().expect("search").draft.clone();
    app.handle(
        Action::EditorPaste("x".repeat(MAX_EDITOR_BYTES + 100)),
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
    app.handle(Action::OpenEnrichment, &provider);
    let editor = render(&provider, &mut app, 100, 28);
    assert!(editor.contains("Raw input before enrichment:"));
    assert!(editor.contains("No accepted outputs yet."));
    app.handle(
        Action::EditorPaste("status = pl.lit(200)".into()),
        &provider,
    );
    app.handle(Action::SubmitDraft, &provider);
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

    app.handle(Action::AddEnrichment, &provider);
    app.handle(Action::EditorPaste("invalid expression".into()), &provider);
    app.handle(Action::SubmitDraft, &provider);
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
    app.handle(Action::OpenEnrichment, &provider);
    app.handle(
        Action::EditorPaste("status = pl.lit('ready')".into()),
        &provider,
    );
    app.handle(Action::SubmitDraft, &provider);
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

    app.handle(
        Action::EditorPaste("upper = pl.col('status').str.to_uppercase()".into()),
        &provider,
    );
    app.handle(Action::SubmitDraft, &provider);
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

    app.handle(Action::EditEnrichment, &provider);
    while !app.view_state().unwrap().enrichment.draft.is_empty() {
        app.handle(Action::EditorBackspace, &provider);
    }
    app.handle(Action::EditorPaste("upper = invalid".into()), &provider);
    app.handle(Action::SubmitDraft, &provider);
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

    app.handle(Action::RemoveEnrichment, &provider);
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

    app.handle(Action::AddEnrichment, &provider);
    app.handle(
        Action::EditorPaste(r"/(?P<code>\d+) (?P<message>.*)/".into()),
        &provider,
    );
    app.handle(Action::SubmitDraft, &provider);
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
    assert!(
        rendered.contains("Saved steps · kept when you add"),
        "{rendered}"
    );
    assert!(rendered.contains("(?P<code>"), "{rendered}");
    let first_row = app.hit_regions.enrichment_rows[0];
    app.handle(
        Action::Mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            first_row.0.x,
            first_row.0.y,
        )),
        &provider,
    );
    assert_eq!(app.view_state().unwrap().enrichment_selected, first_row.1);
    app.handle(Action::MoveEnrichment(1), &provider);
    app.handle(Action::RemoveEnrichment, &provider);
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
    app.handle(Action::OpenEnrichment, &provider);
    let normal = render(&provider, &mut app, 100, 28);
    assert!(normal.contains("derived.id: 42"), "{normal}");
    assert!(normal.contains("derived.message: hello"), "{normal}");
    assert!(!normal.contains("Derived outputs after accepted stages — /id"));

    let backend = TestBackend::new(54, 12);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| ui::render(frame, &mut app, &provider))
        .unwrap();
    let rendered = screen(terminal.backend().buffer());
    let cursor = terminal.backend().cursor_position();
    assert!(rendered.contains("(?P<id>"), "{rendered}");
    assert!(cursor.y < 10, "draft cursor must remain above the footer");
    assert!(!rendered.contains("Native"), "{rendered}");
}

#[test]
fn enrichment_dependency_failure_restores_chain_and_accepted_advanced_filter() {
    let (provider, mut app) = demo();
    app.handle(Action::OpenEnrichment, &provider);
    app.handle(
        Action::EditorPaste("status = pl.lit('ready')".into()),
        &provider,
    );
    app.handle(Action::SubmitDraft, &provider);
    let stage = app.take_query_requests().pop().unwrap();
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: stage.view_id,
        generation: stage.generation,
        revision: stage.revision,
        purpose: stage.purpose,
        result: Ok(()),
    }));
    let accepted = app.view_state().unwrap().enrichments.clone();

    app.handle(Action::CancelEditor, &provider);
    app.handle(Action::OpenAdvanced, &provider);
    app.handle(
        Action::EditorPaste("pl.col('status') == 'ready'".into()),
        &provider,
    );
    app.handle(Action::SubmitDraft, &provider);
    let advanced = app.take_query_requests().pop().unwrap();
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: advanced.view_id,
        generation: advanced.generation,
        revision: advanced.revision,
        purpose: advanced.purpose,
        result: Ok(()),
    }));
    app.handle(Action::CancelEditor, &provider);

    app.handle(Action::OpenEnrichment, &provider);
    app.handle(Action::RemoveEnrichment, &provider);
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

    app.handle(Action::EditEnrichment, &provider);
    while !app.view_state().unwrap().enrichment.draft.is_empty() {
        app.handle(Action::EditorBackspace, &provider);
    }
    app.handle(
        Action::EditorPaste("renamed = pl.lit('ready')".into()),
        &provider,
    );
    app.handle(Action::SubmitDraft, &provider);
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
            view_name: "All events".into(),
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
            command_enrichment: None,
            command_enrichment_revision: 0,
            command_publication: None,
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
        },
        lvu::EnrichmentDefinition {
            id: lvu::EnrichmentStageId("label".into()),
            source: "label = pl.col('code').cast(pl.String)".into(),
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
    app.handle(Action::OpenEnrichment, &_provider);
    app.handle(Action::CancelEditor, &_provider);
    app.handle(Action::OpenEnrichment, &_provider);
    assert_eq!(
        app.view_state().unwrap().enrichment_editing,
        Some(lvu::EnrichmentStageId("label".into()))
    );
    app.handle(Action::SubmitDraft, &_provider);
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

    app.handle(Action::OpenSearch, &provider);
    app.handle(Action::EditorPaste("new filter".into()), &provider);
    app.handle(Action::SubmitDraft, &provider);
    let request = app.take_query_requests().pop().unwrap();
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: view_id.clone(),
        generation: request.generation,
        revision: request.revision,
        purpose: request.purpose,
        result: Ok(()),
    }));
    app.handle(Action::CancelEditor, &provider);
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
    app.handle(Action::OpenAdvanced, &provider);
    app.handle(
        Action::EditorPaste("pl.col('level') == 'ERROR'".into()),
        &provider,
    );
    app.handle(Action::SubmitDraft, &provider);
    let valid = app.take_query_requests().pop().expect("advanced request");
    assert_eq!(valid.purpose, QueryPurpose::Advanced);
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: valid.view_id,
        generation: valid.generation,
        revision: valid.revision,
        purpose: QueryPurpose::Advanced,
        result: Ok(()),
    }));

    app.handle(Action::CancelEditor, &provider);
    app.handle(Action::OpenSearch, &provider);
    app.handle(Action::EditorPaste("request".into()), &provider);
    app.handle(Action::SubmitDraft, &provider);
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

    app.handle(Action::CancelEditor, &provider);
    app.handle(Action::OpenAdvanced, &provider);
    app.handle(Action::EditorPaste(" invalid".into()), &provider);
    app.handle(Action::SubmitDraft, &provider);
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

    app.handle(Action::OpenSearch, &provider);
    app.handle(Action::EditorPaste("LATE".into()), &provider);
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
        app.handle(Action::EditorBackspace, &provider);
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
    app.handle(Action::OpenSearch, provider);
    app.handle(Action::EditorPaste("request".into()), provider);
    app.handle(Action::SubmitDraft, provider);
    assert!(submit_query_requests(app, dispatcher));
    let search = dispatcher.submitted.last().expect("search").clone();

    app.handle(Action::CancelEditor, provider);
    app.handle(Action::OpenAdvanced, provider);
    app.handle(Action::EditorPaste("level == 'INFO'".into()), provider);
    app.handle(Action::SubmitDraft, provider);
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
    app.handle(Action::OpenAdvanced, provider);
    app.handle(Action::EditorPaste("level == 'INFO'".into()), provider);
    app.handle(Action::SubmitDraft, provider);
    assert!(submit_query_requests(app, dispatcher));
    let advanced = dispatcher.submitted.last().expect("advanced").clone();

    app.handle(Action::CancelEditor, provider);
    app.handle(Action::OpenSearch, provider);
    app.handle(Action::EditorPaste("request".into()), provider);
    app.handle(Action::SubmitDraft, provider);
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
    app.handle(Action::OpenSearch, &provider);
    app.handle(Action::EditorPaste(" unsaved".into()), &provider);

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
    app.handle(Action::OpenEnrichment, &provider);
    app.handle(Action::EditorPaste("code = pl.lit(200)".into()), &provider);
    app.handle(Action::SubmitDraft, &provider);
    app.handle(Action::CancelEditor, &provider);
    app.handle(Action::OpenAdvanced, &provider);
    app.handle(Action::EditorPaste("invalid advanced".into()), &provider);
    app.handle(Action::SubmitDraft, &provider);
    app.handle(Action::CancelEditor, &provider);
    app.handle(Action::OpenSearch, &provider);
    app.handle(Action::EditorPaste("request".into()), &provider);
    app.handle(Action::SubmitDraft, &provider);

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

    app.handle(Action::EditorPaste(" unfinished".into()), &provider);
    app.handle(Action::CancelEditor, &provider);
    app.handle(Action::OpenEnrichment, &provider);
    app.handle(Action::EditorPaste(" unfinished".into()), &provider);
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

    app.handle(Action::OpenAdvanced, &provider);
    app.handle(Action::EditorPaste("invalid advanced".into()), &provider);
    app.handle(Action::SubmitDraft, &provider);
    assert!(submit_query_requests(&mut app, &mut dispatcher));
    app.handle(Action::CancelEditor, &provider);
    app.handle(Action::OpenSearch, &provider);
    app.handle(Action::EditorPaste("request 05".into()), &provider);
    app.handle(Action::SubmitDraft, &provider);
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
    app.handle(Action::OpenSearch, &provider);
    app.handle(Action::EditorPaste("slow".into()), &provider);
    app.handle(Action::SubmitDraft, &provider);
    let mut dispatcher = DelayedDispatcher::default();
    assert!(submit_query_requests(&mut app, &mut dispatcher));
    assert_eq!(dispatcher.submitted.len(), 1);
    assert!(!poll_query_completions(&mut app, &mut dispatcher));

    app.handle(Action::CancelEditor, &provider);
    app.handle(Action::Resize(55, 9), &provider);
    app.handle(Action::MoveLine(-1), &provider);
    assert_eq!(app.terminal_size, (55, 9));
    assert!(!app.should_quit);

    app.handle(Action::OpenSearch, &provider);
    app.handle(Action::EditorInput('2'), &provider);
    app.handle(Action::SubmitDraft, &provider);
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
    app.handle(Action::OpenSearch, &provider);
    for index in 0..33 {
        app.handle(Action::SubmitDraft, &provider);
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
        app.handle(Action::SubmitDraft, &provider);
        app.handle(Action::NextView, &provider);
    }
    app.handle(Action::OpenRecipes, &provider);
    let RecipeRequest::List { meta } = app.take_recipe_requests().pop().unwrap() else {
        panic!("recipe list")
    };
    app.set_recipes(
        meta,
        vec![lvu::RecipeItem {
            id: "full".into(),
            revision: "one".into(),
            name: "Queued".into(),
            config: lvu::RecipeConfig {
                pinned_columns: vec!["must-not-apply".into()],
                ..lvu::RecipeConfig::default()
            },
            incompatibility: None,
        }],
        None,
    );
    app.handle(Action::SubmitRecipe, &provider);
    assert_eq!(app.take_query_requests().len(), 32);
    assert!(
        app.recipe_dialog
            .as_ref()
            .unwrap()
            .status
            .contains("queue is full")
    );
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
    assert!(output.contains("No view selected"));
    assert!(output.contains("add or discover a source"));
    assert!(output.contains("Add source"));
    assert!(output.contains("FILE PATH"));
    assert_eq!(app.active_view_id(), None);
}

#[test]
fn named_recipe_dialog_saves_accepted_state_and_applies_through_query_request() {
    use lvu::{RecipeConfig, RecipeDialogMode, RecipeItem, RecipeRequest};
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
    app.handle(Action::OpenRecipes, &provider);
    assert!(matches!(
        app.take_recipe_requests().as_slice(),
        [RecipeRequest::List { .. }]
    ));
    app.handle(Action::SelectRecipeMode(RecipeDialogMode::Save), &provider);
    app.handle(Action::RecipeInput('E'), &provider);
    app.handle(Action::SubmitRecipe, &provider);
    assert!(
        matches!(app.take_recipe_requests().as_slice(), [RecipeRequest::Save { name, view_id, config, .. }] if name == "E" && view_id == &target && config.search == "old" && config.enrichment == "old_field = pl.lit('ok')")
    );
    app.handle(
        Action::SelectRecipeMode(RecipeDialogMode::Import),
        &provider,
    );
    app.handle(Action::RecipeBackspace, &provider);
    for character in "/tmp/recipe.toml".chars() {
        app.handle(Action::RecipeInput(character), &provider);
    }
    app.handle(Action::SubmitRecipe, &provider);
    assert!(matches!(
        app.take_recipe_requests().as_slice(),
        [RecipeRequest::Import { path, .. }] if path == "/tmp/recipe.toml"
    ));

    let dialog = app.recipe_dialog.as_ref().unwrap();
    let response = lvu::RecipeRequestMeta {
        request_id: 99,
        dialog_id: dialog.id,
        dialog_revision: dialog.interaction_revision,
    };
    app.recipe_dialog.as_mut().unwrap().pending_request_id = Some(response.request_id);
    app.set_recipes(
        response,
        vec![lvu::RecipeItem {
            id: "r".into(),
            revision: "rev".into(),
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
    app.handle(
        Action::SelectRecipeMode(RecipeDialogMode::Browse),
        &provider,
    );
    app.handle(Action::SubmitRecipe, &provider);
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

    app.handle(Action::OpenRecipes, &provider);
    app.take_recipe_requests();
    let dialog = app.recipe_dialog.as_ref().unwrap();
    let response = lvu::RecipeRequestMeta {
        request_id: 100,
        dialog_id: dialog.id,
        dialog_revision: dialog.interaction_revision,
    };
    app.recipe_dialog.as_mut().unwrap().pending_request_id = Some(response.request_id);
    app.set_recipes(
        response,
        vec![RecipeItem {
            id: "unsupported".into(),
            revision: "rev2".into(),
            name: "Command recipe".into(),
            config: RecipeConfig::default(),
            incompatibility: Some("command enrichment recipes are not supported".into()),
        }],
        None,
    );
    app.handle(Action::SubmitRecipe, &provider);
    assert!(
        app.recipe_dialog
            .as_ref()
            .unwrap()
            .status
            .contains("not supported")
    );
    assert!(app.take_query_requests().is_empty());
}

#[test]
fn recipe_results_are_fenced_from_reopened_or_edited_dialogs() {
    use lvu::{RecipeDialogMode, RecipeRequest, RecipeRequestMeta};
    let (provider, mut app) = demo();
    app.handle(Action::OpenRecipes, &provider);
    let RecipeRequest::List { meta: stale } = app.take_recipe_requests().pop().unwrap() else {
        panic!("list request")
    };
    app.handle(Action::CancelEditor, &provider);
    app.handle(Action::OpenRecipes, &provider);
    let RecipeRequest::List { meta: current } = app.take_recipe_requests().pop().unwrap() else {
        panic!("second list request")
    };
    assert_ne!(stale.dialog_id, current.dialog_id);
    app.set_recipes(stale, Vec::new(), Some("stale".into()));
    assert!(app.recipe_dialog.as_ref().unwrap().loading);

    app.handle(Action::SelectRecipeMode(RecipeDialogMode::Save), &provider);
    app.handle(Action::RecipeInput('N'), &provider);
    app.set_recipes(current, Vec::new(), None);
    let dialog = app.recipe_dialog.as_ref().unwrap();
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
    app.handle(Action::OpenRecipes, &provider);
    let RecipeRequest::List { meta } = app.take_recipe_requests().pop().unwrap() else {
        panic!("list request")
    };
    let item = RecipeItem {
        id: "00000000-0000-0000-0000-000000000041".into(),
        revision: "00000000-0000-0000-0000-000000000042".into(),
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
    app.handle(Action::RejectRecipeSuggestion, &provider);
    let RecipeRequest::Outcome(outcome) = app.take_recipe_requests().pop().unwrap() else {
        panic!("outcome request")
    };
    assert!(!outcome.accepted);
    assert!(app.recipe_dialog.as_ref().unwrap().suggestions.is_empty());

    let meta = lvu::RecipeRequestMeta {
        request_id: 77,
        dialog_id: app.recipe_dialog.as_ref().unwrap().id,
        dialog_revision: app.recipe_dialog.as_ref().unwrap().interaction_revision,
    };
    app.recipe_dialog.as_mut().unwrap().pending_request_id = Some(77);
    app.set_recipes_with_suggestions(meta, vec![item], vec![suggestion], None);
    app.handle(Action::AdaptRecipeSuggestion, &provider);
    let dialog = app.ask_ai_dialog.as_ref().expect("adaptation dialog");
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
    app.handle(Action::OpenRecipes, &provider);
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
    app.handle(Action::SubmitRecipe, &provider);
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
        app.handle(Action::OpenRecipes, &provider);
        let RecipeRequest::List { meta } = app.take_recipe_requests().pop().unwrap() else {
            panic!()
        };
        let id = "00000000-0000-0000-0000-000000000061".to_owned();
        app.set_recipes_with_suggestions(
            meta,
            vec![RecipeItem {
                id: id.clone(),
                revision: "00000000-0000-0000-0000-000000000062".into(),
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
        app.handle(Action::SubmitRecipe, &provider);
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
    app.handle(Action::OpenSearch, &provider);
    app.handle(Action::EditorInput('x'), &provider);
    app.handle(Action::SubmitDraft, &provider);
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
    app.handle(Action::OpenRecipes, &provider);
    let RecipeRequest::List { meta } = app.take_recipe_requests().pop().unwrap() else {
        panic!("list request")
    };
    app.set_recipes(
        meta,
        vec![RecipeItem {
            id: "presentation".into(),
            revision: "one".into(),
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
    app.handle(Action::SubmitRecipe, &provider);
    let request = app.take_query_requests().pop().unwrap();
    app.handle(Action::OpenFieldPicker, &provider);
    app.handle(Action::TogglePinnedField, &provider);
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
    app.handle(Action::OpenTime, &provider);
    app.handle(
        Action::EditorPaste("1970-01-01T00:00:01Z".into()),
        &provider,
    );
    app.handle(Action::SwitchTimeField, &provider);
    app.handle(
        Action::EditorPaste("1970-01-01T00:00:03Z".into()),
        &provider,
    );
    app.handle(Action::SubmitTime, &provider);
    let request = app.take_query_requests().pop().unwrap();
    assert_eq!(
        request.constraints.capture_time,
        Some(lvu::CaptureTimeRange {
            start_unix_nanos: 1_000_000_000,
            end_unix_nanos: 3_000_000_000,
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
    app.handle(Action::OpenTime, &provider);
    for _ in 0..32 {
        app.handle(Action::TimeBackspace, &provider);
    }
    app.handle(
        Action::EditorPaste("2026-02-30T00:00:00Z".into()),
        &provider,
    );
    app.handle(Action::SubmitTime, &provider);
    assert!(app.time_dialog.is_some());
    assert!(app.view_state().unwrap().time_error.is_some());
    assert_eq!(
        app.view_state()
            .unwrap()
            .applied_capture_time
            .unwrap()
            .start_unix_nanos,
        1_000_000_000
    );
    app.handle(Action::AroundSelected, &provider);
    app.handle(Action::SubmitTime, &provider);
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
    app.handle(Action::OpenTime, &provider);
    app.handle(Action::TimeFocus(lvu::app::TimeControl::Window), &provider);
    app.handle(Action::TimeOpenFocused, &provider);
    app.handle(Action::TimeMoveChoice(2), &provider);
    app.handle(Action::TimeChoose, &provider);
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
    use lvu::app::TimeControl;
    let (provider, mut app) = demo();
    app.handle(Action::OpenTime, &provider);
    app.handle(
        Action::EditorPaste("2026-09-06T12:34:56.123456789+05:45".into()),
        &provider,
    );
    app.handle(Action::TimeFocus(TimeControl::StartClock), &provider);
    for _ in 0..32 {
        app.handle(Action::TimeBackspace, &provider);
    }
    app.handle(Action::EditorPaste("01:02:03.987654321".into()), &provider);
    let dialog = app.time_dialog.as_ref().unwrap();
    assert_eq!(dialog.start_date, "2026-09-06");
    assert_eq!(dialog.start_clock, "01:02:03.987654321");
    assert_eq!(dialog.start_zone, "+05:45");
    let accepted_draft = app.view_state().unwrap().time_start_draft.clone();
    app.handle(Action::EditorPaste("9".repeat(65)), &provider);
    assert_eq!(app.view_state().unwrap().time_start_draft, accepted_draft);
    assert!(app.view_state().unwrap().time_error.is_some());
    assert!(app.take_query_requests().is_empty());
}

#[test]
fn untouched_time_reopen_refreshes_visible_segments_with_opening_selection() {
    let (mut provider, mut app) = demo();
    app.sync_provider(&provider, 4);
    app.handle(Action::OpenTime, &provider);
    let first = app.view_state().unwrap().time_start_draft.clone();
    app.handle(Action::CancelEditor, &provider);
    assert!(provider.advance());
    app.sync_provider(&provider, 4);
    app.handle(Action::OpenTime, &provider);
    let current = &app.view_state().unwrap().time_start_draft;
    assert_ne!(current, &first);
    let (date, clock, zone) = lvu::app::split_time_draft(current);
    let dialog = app.time_dialog.as_ref().unwrap();
    assert_eq!(
        (&dialog.start_date, &dialog.start_clock, &dialog.start_zone),
        (&date, &clock, &zone)
    );
    assert!(app.take_query_requests().is_empty());
}

#[test]
fn basis_and_window_only_drafts_keep_seeded_segments_on_reopen() {
    let (provider, mut app) = demo();
    app.handle(Action::OpenTime, &provider);
    let seeded = app.view_state().unwrap().time_start_draft.clone();
    app.handle(Action::SetTimeBasis(lvu::TimeBasis::Event), &provider);
    app.handle(Action::CancelEditor, &provider);
    app.handle(Action::OpenTime, &provider);
    assert_eq!(app.view_state().unwrap().time_start_draft, seeded);
    assert!(!app.time_dialog.as_ref().unwrap().start_date.is_empty());
    app.handle(Action::TimeFocus(lvu::app::TimeControl::Window), &provider);
    app.handle(Action::TimeOpenFocused, &provider);
    app.handle(Action::TimeMoveChoice(2), &provider);
    app.handle(Action::TimeChoose, &provider);
    app.handle(Action::CancelEditor, &provider);
    app.handle(Action::OpenTime, &provider);
    assert_eq!(app.view_state().unwrap().time_start_draft, seeded);
    assert_eq!(
        app.time_dialog.as_ref().unwrap().window,
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
    app.handle(Action::OpenTime, &provider);
    let dialog = app.time_dialog.as_ref().unwrap();
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
    app.handle(Action::OpenTime, &provider);
    let dialog = app.time_dialog.as_ref().unwrap();
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
    app.handle(Action::OpenTime, &provider);
    app.handle(Action::TimeFocus(lvu::app::TimeControl::Window), &provider);
    app.handle(Action::TimeOpenFocused, &provider);
    app.handle(Action::TimeMoveChoice(5), &provider);
    let rendered = render(&provider, &mut app, 46, 12);
    assert!(rendered.contains("Around selected"), "{rendered}");
    let area = app
        .hit_regions
        .time_choices
        .iter()
        .find(|(_, index)| *index == 5)
        .unwrap()
        .0;
    app.handle(
        Action::Mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            area.x,
            area.y,
        )),
        &provider,
    );
    assert_eq!(
        app.time_dialog.as_ref().unwrap().window,
        lvu::app::TimeWindowChoice::AroundSelected
    );
}

#[test]
fn rolling_capture_time_expires_idle_rows_without_changing_definition_revision() {
    let (mut provider, mut app) = demo();
    let view_id = app.active_view_id().unwrap().to_owned();
    let mut dispatcher = provider.query_dispatcher();
    let elapsed = Instant::now();
    assert!(!app.refresh_rolling_capture_times(20_000_000_000, elapsed));
    app.handle(Action::OpenSearch, &provider);
    app.handle(Action::EditorPaste("fixture".into()), &provider);
    app.handle(Action::SubmitDraft, &provider);
    let search = app.take_query_requests().pop().unwrap();
    dispatcher.submit(search).unwrap();
    assert!(app.apply_query_completion(dispatcher.poll().unwrap()));

    app.handle(Action::OpenTime, &provider);
    app.handle(Action::SetRecentTime(5 * 60), &provider);
    let recent = app.take_query_requests().pop().unwrap();
    assert_eq!(recent.constraints.text.as_ref().unwrap().literal, "fixture");
    assert_eq!(
        recent.constraints.capture_time,
        Some(lvu::CaptureTimeRange {
            start_unix_nanos: -280_000_000_000,
            end_unix_nanos: 20_000_000_000,
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
    assert!(app.refresh_rolling_capture_times(320_000_000_000, elapsed + Duration::from_secs(1)));
    let refresh = app.take_query_requests().pop().unwrap();
    assert_eq!(
        refresh.constraints.text.as_ref().unwrap().literal,
        "fixture"
    );
    assert_eq!(
        refresh.constraints.capture_time,
        Some(lvu::CaptureTimeRange {
            start_unix_nanos: 20_000_000_000,
            end_unix_nanos: 320_000_000_000,
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
    assert!(
        !app.refresh_rolling_capture_times(320_500_000_000, elapsed + Duration::from_millis(1500))
    );
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
    app.handle(Action::OpenRecipes, &provider);
    let lvu::RecipeRequest::List { meta } = app.take_recipe_requests().pop().unwrap() else {
        panic!("recipe list")
    };
    app.set_recipes(
        meta,
        vec![lvu::RecipeItem {
            id: "recent".into(),
            revision: "one".into(),
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
    app.handle(Action::SubmitRecipe, &provider);
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
    app.handle(Action::OpenRecipes, &provider);
    let lvu::RecipeRequest::List { meta } = app.take_recipe_requests().pop().unwrap() else {
        panic!("recipe list")
    };
    app.set_recipes(
        meta,
        vec![lvu::RecipeItem {
            id: "recent".into(),
            revision: "one".into(),
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
    app.handle(Action::SubmitRecipe, &provider);
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
    app.handle(Action::OpenTime, &provider);
    app.handle(Action::SetRecentTime(300), &provider);
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
    app.handle(Action::OpenRecipes, &provider);
    let lvu::RecipeRequest::List { meta } = app.take_recipe_requests().pop().unwrap() else {
        panic!("recipe list")
    };
    app.set_recipes(
        meta,
        vec![lvu::RecipeItem {
            id: "invalid".into(),
            revision: "one".into(),
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
    app.handle(Action::SubmitRecipe, &provider);
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
        start_unix_nanos: 1_000_000_000,
        end_unix_nanos: 3_000_000_000,
    };
    app.handle(Action::OpenTime, &provider);
    app.handle(
        Action::EditorPaste("1970-01-01T00:00:01Z".into()),
        &provider,
    );
    app.handle(Action::SwitchTimeField, &provider);
    app.handle(
        Action::EditorPaste("1970-01-01T00:00:03Z".into()),
        &provider,
    );
    app.handle(Action::SubmitTime, &provider);
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
        app.handle(Action::OpenTime, &provider);
        app.handle(
            Action::TimeFocus(lvu::app::TimeControl::StartDate),
            &provider,
        );
        for _ in 0..64 {
            app.handle(Action::TimeBackspace, &provider);
        }
        app.handle(Action::EditorPaste(invalid.into()), &provider);
        app.handle(Action::SubmitTime, &provider);
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
    app.handle(Action::OpenAskAi, &provider);
    app.handle(Action::EditorPaste("suggest a filter".into()), &provider);
    app.handle(Action::SubmitAskAi, &provider);
    let AskAiRequest::Start {
        generation,
        definition_revision,
        ..
    } = app.take_ask_ai_requests().pop().unwrap()
    else {
        panic!("AI start")
    };

    app.handle(Action::OpenTime, &provider);
    app.handle(
        Action::TimeFocus(lvu::app::TimeControl::StartDate),
        &provider,
    );
    let anchored = app.view_state().unwrap().selected.clone().unwrap();
    let anchored_time = provider
        .row_by_id(&view_id, &anchored)
        .unwrap()
        .captured_at_unix_nanos
        .unwrap();
    app.handle(Action::TimeInput('2'), &provider);
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
    app.handle(Action::AroundSelected, &provider);
    app.handle(Action::SubmitTime, &provider);
    let request = app.take_query_requests().pop().unwrap();
    let window = request.constraints.capture_time.unwrap();
    assert_eq!(window.start_unix_nanos, anchored_time - 30_000_000_000);
    assert_eq!(window.end_unix_nanos, anchored_time + 30_000_000_000);
}

#[test]
fn event_time_basis_is_explicit_transactional_and_persistent() {
    let (provider, mut app) = demo();
    let view_id = app.active_view_id().unwrap().to_owned();
    app.handle(Action::OpenTime, &provider);
    app.handle(Action::SetTimeBasis(lvu::TimeBasis::Event), &provider);
    app.handle(
        Action::EditorPaste("2026-09-05T12:30:45Z".into()),
        &provider,
    );
    app.handle(Action::SwitchTimeField, &provider);
    app.handle(
        Action::EditorPaste("2026-09-05T12:30:46Z".into()),
        &provider,
    );
    app.handle(Action::SubmitTime, &provider);
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
    app.handle(Action::OpenTime, &provider);
    app.handle(Action::AroundSelected, &provider);
    app.handle(Action::SubmitTime, &provider);
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
            app.handle(Action::OpenAdvanced, &provider);
            app.handle(Action::EditorPaste("pl.lit(True)".into()), &provider);
            app.handle(Action::SubmitDraft, &provider);
        };
        let submit_time = |app: &mut App| {
            app.handle(Action::OpenTime, &provider);
            app.handle(
                Action::EditorPaste("1970-01-01T00:00:01Z".into()),
                &provider,
            );
            app.handle(Action::SwitchTimeField, &provider);
            app.handle(
                Action::EditorPaste("1970-01-01T00:00:03Z".into()),
                &provider,
            );
            app.handle(Action::SubmitTime, &provider);
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
            app.handle(Action::OpenAdvanced, &provider);
            app.handle(Action::EditorPaste("pl.lit(True)".into()), &provider);
            app.handle(Action::SubmitDraft, &provider);
        };
        let submit_time = |app: &mut App| {
            app.handle(Action::OpenTime, &provider);
            app.handle(
                Action::EditorPaste("1970-01-01T00:00:01Z".into()),
                &provider,
            );
            app.handle(Action::SwitchTimeField, &provider);
            app.handle(
                Action::EditorPaste("1970-01-01T00:00:03Z".into()),
                &provider,
            );
            app.handle(Action::SubmitTime, &provider);
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
    app.handle(Action::OpenTime, &provider);
    app.handle(
        Action::EditorPaste("1970-01-01T00:00:01Z".into()),
        &provider,
    );
    app.handle(Action::SwitchTimeField, &provider);
    app.handle(
        Action::EditorPaste("1970-01-01T00:00:03Z".into()),
        &provider,
    );
    app.handle(Action::SubmitTime, &provider);
    app.handle(Action::OpenAdvanced, &provider);
    app.handle(Action::EditorPaste("invalid advanced".into()), &provider);
    app.handle(Action::SubmitDraft, &provider);
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
    app.handle(Action::OpenViewDialog, &provider);
    assert_eq!(app.focus, Focus::ViewDialog);
    assert!(render(&provider, &mut app, 90, 24).contains("CLONE SETTINGS"));
    app.handle(
        Action::SelectViewDialogMode(lvu::ViewDialogMode::Blank),
        &provider,
    );
    for _ in 0.."New view".len() {
        app.handle(Action::ViewBackspace, &provider);
    }
    for character in "Errors".chars() {
        app.handle(Action::ViewInput(character), &provider);
    }
    app.handle(Action::SubmitViewDialog, &provider);
    let blank = app.take_view_requests().pop().unwrap();
    assert_eq!(blank.mode, lvu::ViewDialogMode::Blank);
    assert_eq!(blank.view_id, selected);
    assert_eq!(blank.name, "Errors");

    app.handle(
        Action::SelectViewDialogMode(lvu::ViewDialogMode::Rename),
        &provider,
    );
    app.handle(Action::SubmitViewDialog, &provider);
    assert_eq!(
        app.take_view_requests().pop().unwrap().mode,
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
    app.handle(Action::OpenSearch, &provider);
    app.handle(Action::EditorPaste("new draft".into()), &provider);
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
    assert_eq!(app.views[0].name, "Restored first");
    assert_eq!(app.views[1].name, "Second");
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
    app.handle(Action::OpenSearch, &provider);
    app.handle(Action::EditorPaste("user filter".into()), &provider);
    app.handle(Action::SubmitDraft, &provider);
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
    assert_eq!(app.views[0].name, "User name");
    assert_eq!(app.search_state().unwrap().draft, "user filter");
    assert_eq!(app.search_state().unwrap().applied, "user filter");
    assert!(!app.rename_view("first", "Second".into()));
    assert_eq!(app.views[0].name, "User name");
}

#[test]
fn ask_ai_proposal_is_fenced_and_applies_through_native_editor_request() {
    let (mut provider, mut app) = demo();
    let view_id = app.active_view_id().unwrap().to_owned();
    app.handle(Action::OpenAskAi, &provider);
    let dialog = render(&provider, &mut app, 120, 28);
    assert!(dialog.contains("Ask 🧠"));
    assert!(dialog.contains("codex/gpt-5.6-sol"));
    app.handle(Action::EditorPaste("only errors".into()), &provider);
    app.handle(Action::SubmitAskAi, &provider);
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
    app.handle(Action::SubmitAskAi, &provider);
    let query = app
        .take_query_requests()
        .pop()
        .expect("native query request");
    assert_eq!(query.purpose, QueryPurpose::Advanced);
    assert_eq!(
        query.constraints.advanced_polars.as_deref(),
        Some("pl.col('level') == 'ERROR'")
    );
    assert_eq!(app.focus, Focus::AdvancedEditor);
}

#[test]
fn unsubmitted_editor_draft_invalidates_an_inflight_ai_proposal() {
    let (provider, mut app) = demo();
    let view_id = app.active_view_id().unwrap().to_owned();
    app.handle(Action::OpenAskAi, &provider);
    app.handle(Action::EditorPaste("suggest a filter".into()), &provider);
    app.handle(Action::SubmitAskAi, &provider);
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
    app.handle(Action::OpenAdvanced, &provider);
    app.handle(
        Action::EditorPaste("pl.col('message').is_not_null()".into()),
        &provider,
    );
    assert!(!app.finish_ask_ai(
        generation,
        &view_id,
        definition_revision,
        Ok(("pl.lit(True)".into(), "stale proposal".into())),
    ));
    assert_eq!(
        app.active_editor_state().expect("advanced editor").draft,
        "pl.col('message').is_not_null()"
    );
}

#[test]
fn investigation_starts_follows_up_and_explicitly_resumes_saved_session() {
    let (provider, mut app) = demo();
    app.handle(Action::OpenInvestigation, &provider);
    assert_eq!(app.focus, Focus::Investigation);
    assert!(render(&provider, &mut app, 120, 30).contains("Investigate with local agent"));
    app.handle(
        Action::EditorPaste("explain the failures".into()),
        &provider,
    );
    app.handle(Action::SubmitInvestigation, &provider);
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
    let dialog = app.investigation_dialog.as_ref().unwrap();
    assert_eq!(dialog.messages.len(), 64);
    assert!(
        dialog
            .messages
            .iter()
            .all(|message| message.len() <= 16_387)
    );
    assert_eq!(dialog.stage, InvestigationStage::Conversation);
    app.handle(Action::EditorPaste("show the first one".into()), &provider);
    app.handle(Action::SubmitInvestigation, &provider);
    assert!(matches!(
        app.take_investigation_requests().as_slice(),
        [InvestigationRequest::Send { session_id, prompt, .. }]
            if session_id == "session-1" && prompt == "show the first one"
    ));

    app.handle(Action::CancelEditor, &provider);
    app.take_investigation_requests();
    app.set_investigations(vec![item]);
    app.handle(Action::OpenInvestigation, &provider);
    app.handle(Action::SubmitInvestigation, &provider);
    assert!(matches!(
        app.take_investigation_requests().as_slice(),
        [InvestigationRequest::Resume { item, .. }] if item.session_id == "session-1"
    ));
    assert_eq!(
        app.view_definition_revision(app.active_view_id().unwrap()),
        Some(definition_revision)
    );
}

#[test]
fn delayed_investigation_load_merges_with_session_created_in_memory() {
    let (provider, mut app) = demo();
    app.handle(Action::OpenInvestigation, &provider);
    app.handle(Action::EditorPaste("new question".into()), &provider);
    app.handle(Action::SubmitInvestigation, &provider);
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
    app.handle(Action::OpenInvestigation, &provider);
    let screen = render(&provider, &mut app, 120, 30);
    assert!(screen.contains("new question"));
    assert!(screen.contains("older question"));
}

#[test]
fn cancelled_or_definition_stale_ai_cannot_overwrite_later_edits() {
    let (provider, mut app) = demo();
    let view_id = app.active_view_id().unwrap().to_owned();
    app.handle(Action::OpenAskAi, &provider);
    app.handle(Action::EditorPaste("derive status".into()), &provider);
    app.handle(Action::SelectAskAiKind(AskAiKind::Enrichment), &provider);
    app.handle(Action::SubmitAskAi, &provider);
    let AskAiRequest::Start {
        generation,
        definition_revision,
        ..
    } = app.take_ask_ai_requests().pop().unwrap()
    else {
        panic!("start request")
    };
    app.handle(Action::CancelEditor, &provider);
    assert!(matches!(
        app.take_ask_ai_requests().as_slice(),
        [AskAiRequest::Cancel { generation: value }] if *value == generation
    ));
    app.handle(Action::OpenEnrichment, &provider);
    app.handle(
        Action::EditorPaste("status = pl.lit('user')".into()),
        &provider,
    );
    assert!(!app.finish_ask_ai(
        generation,
        &view_id,
        definition_revision,
        Ok(("status = pl.lit('agent')".into(), "stale".into())),
    ));
    assert_eq!(
        app.active_editor_state().unwrap().draft,
        "status = pl.lit('user')"
    );
}

#[test]
fn ai_proposal_cannot_cross_views_or_a_new_definition_revision() {
    let (provider, mut app) = demo();
    let original = app.active_view_id().unwrap().to_owned();
    app.handle(Action::OpenAskAi, &provider);
    app.handle(Action::EditorPaste("errors".into()), &provider);
    app.handle(Action::SubmitAskAi, &provider);
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
    app.handle(Action::SubmitAskAi, &provider);
    assert!(app.take_query_requests().is_empty());
    assert!(
        app.ask_ai_dialog
            .as_ref()
            .is_some_and(|dialog| dialog.stage == AskAiStage::Error)
    );

    app.handle(Action::CancelEditor, &provider);
    app.handle(Action::PreviousView, &provider);
    app.handle(Action::OpenAskAi, &provider);
    app.handle(Action::EditorPaste("fresh".into()), &provider);
    app.handle(Action::SubmitAskAi, &provider);
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
    assert_eq!(app.focus, Focus::SourceDialog);
    app.handle(Action::EditorPaste("./events.log".into()), &provider);
    app.handle(Action::ToggleSourceKind, &provider);
    assert_eq!(
        app.source_dialog.as_ref().expect("dialog").kind,
        SourceKind::Command
    );
    assert_eq!(
        app.source_dialog.as_ref().expect("dialog").draft,
        "./events.log"
    );
    app.handle(Action::SubmitSource, &provider);
    let request = app.take_source_requests().pop().expect("request");
    assert_eq!(request.kind, SourceKind::Command);
    assert_eq!(request.text, "./events.log");
}

#[test]
fn empty_start_source_ai_requires_review_and_fences_stale_results() {
    use lvu::{SourceAiPreview, SourceAiRequest, SourceAiStage};

    let provider = EmptyProvider;
    let mut app = App::new(Vec::new(), Vec::new(), false);
    app.handle(Action::ToggleSourceAi, &provider);
    app.handle(
        Action::EditorPaste("follow backend docker logs".into()),
        &provider,
    );
    app.handle(Action::SubmitSource, &provider);
    let SourceAiRequest::Start {
        generation,
        instruction,
        ..
    } = app.take_source_ai_requests().pop().unwrap()
    else {
        panic!("source AI start")
    };
    assert_eq!(instruction, "follow backend docker logs");
    assert_eq!(
        app.source_dialog.as_ref().unwrap().ai.stage,
        SourceAiStage::Preparing
    );
    assert!(!app.finish_source_ai(generation - 1, Err("stale".into())));
    assert!(app.finish_source_ai(
        generation,
        Ok(SourceAiPreview {
            name: "backend".into(),
            kind: "command".into(),
            launch: r#"{"executable":"docker","args":["logs","-f","backend"]}"#.into(),
            effective_path_or_cwd: "/project".into(),
            restart: "never".into(),
            environment: (0..16).map(|index| format!("KEY{index}=value")).collect(),
            explanation: "matched Compose service".into(),
        })
    ));
    let screen = render(&provider, &mut app, 120, 30);
    assert!(screen.contains("preview never executes"));
    assert!(screen.contains("docker"));
    app.handle(Action::MovePathCompletion(20), &provider);
    assert!(render(&provider, &mut app, 120, 30).contains("KEY15=value"));
    assert!(app.take_source_requests().is_empty());
    app.handle(Action::SubmitSource, &provider);
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
    app.handle(Action::EditorPaste("manual path.log".into()), &provider);
    app.handle(Action::ToggleSourceAi, &provider);
    app.handle(
        Action::EditorPaste("slow source suggestion".into()),
        &provider,
    );
    app.handle(Action::SubmitSource, &provider);
    let SourceAiRequest::Start { generation, .. } = app.take_source_ai_requests().pop().unwrap()
    else {
        panic!("start")
    };
    app.handle(Action::ToggleSourceAi, &provider);
    assert_eq!(app.source_dialog.as_ref().unwrap().draft, "manual path.log");
    app.handle(Action::CancelEditor, &provider);
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
    app.handle(Action::EditorPaste("logs/app".into()), &provider);
    app.handle(Action::CompleteSourcePath, &provider);
    let first = app
        .take_path_completion_requests()
        .pop()
        .expect("completion request");
    assert_eq!(first.draft, "logs/app");

    app.handle(Action::SourceInput('x'), &provider);
    assert_eq!(app.active_path_completion_generation(), None);
    assert!(!app.apply_path_completion_result(
        first.generation,
        &first.draft,
        Some("logs/application.log".into()),
        vec!["logs/application.log".into()],
        None,
    ));
    assert_eq!(
        app.source_dialog.as_ref().expect("dialog").draft,
        "logs/appx"
    );

    assert_eq!(
        key_to_action(
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::ALT),
            Focus::SourceDialog,
        ),
        Action::SelectSourceKind(SourceKind::Command)
    );
    assert_eq!(
        key_to_action(
            KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE),
            Focus::SourceDialog
        ),
        Action::ToggleSourceControlFocus
    );
    app.handle(Action::SelectSourceKind(SourceKind::Command), &provider);
    app.handle(Action::CompleteSourcePath, &provider);
    assert!(app.take_path_completion_requests().is_empty());

    app.handle(Action::SelectSourceKind(SourceKind::File), &provider);
    app.handle(Action::CompleteSourcePath, &provider);
    let current = app
        .take_path_completion_requests()
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
    assert!(output.contains("Path matches"));
    assert!(output.contains("logs/appx ünicode"));
    app.handle(Action::MovePathCompletion(1), &provider);
    app.handle(Action::CompleteSourcePath, &provider);
    assert_eq!(
        app.source_dialog.as_ref().expect("dialog").draft,
        "logs/appx ünicode"
    );

    let mut reopened = App::new(vec![], vec![], false);
    reopened.handle(Action::EditorPaste("same/path".into()), &provider);
    reopened.handle(Action::CompleteSourcePath, &provider);
    let old_dialog = reopened
        .take_path_completion_requests()
        .pop()
        .expect("old dialog request");
    reopened.handle(Action::CancelEditor, &provider);
    reopened.handle(Action::OpenSource, &provider);
    reopened.handle(Action::EditorPaste("same/path".into()), &provider);
    reopened.handle(Action::CompleteSourcePath, &provider);
    let new_dialog = reopened
        .take_path_completion_requests()
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
    assert_eq!(
        reopened.source_dialog.as_ref().expect("dialog").draft,
        "same/path"
    );

    assert!(reopened.apply_path_completion_result(
        new_dialog.generation,
        &new_dialog.draft,
        Some("same/path/".into()),
        vec!["same/path/".into()],
        None,
    ));
    assert!(
        reopened
            .source_dialog
            .as_ref()
            .expect("dialog")
            .path_completion
            .candidates
            .is_empty()
    );
    reopened.handle(Action::CompleteSourcePath, &provider);
    assert_eq!(
        reopened
            .take_path_completion_requests()
            .pop()
            .expect("directory contents request")
            .draft,
        "same/path/"
    );
}

#[test]
fn narrow_source_controls_keep_each_workflow_action_visible_and_live() {
    use lvu::app::{SourceControl, SourceDialogMode};
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
        (SourceDialogMode::Manual, SourceControl::Open, "Open"),
        (
            SourceDialogMode::Discovery,
            SourceControl::Refresh,
            "Refresh",
        ),
    ];
    for (mode, control, label) in cases {
        let dialog = app.source_dialog.as_mut().expect("source dialog");
        dialog.mode = mode;
        dialog.control = control;
        dialog.controls_focused = true;
        let output = render(&provider, &mut app, 34, 18);
        assert!(output.contains(label), "missing focused {label}: {output}");
        assert!(
            app.hit_regions
                .source_controls
                .iter()
                .any(|(_, visible)| *visible == control),
            "focused {label} has no hitbox"
        );
    }

    app.source_dialog.as_mut().unwrap().mode = SourceDialogMode::Manual;
    app.handle(
        Action::FocusSourceControl(SourceControl::Command),
        &provider,
    );
    assert_eq!(
        app.source_dialog.as_ref().unwrap().kind,
        SourceKind::Command
    );
    app.handle(
        Action::FocusSourceControl(SourceControl::Discovery),
        &provider,
    );
    assert!(matches!(
        app.take_discovery_requests().as_slice(),
        [lvu::DiscoveryUiRequest::Scan { .. }]
    ));
}

#[test]
fn async_source_results_preserve_newer_dialog_input_and_reopen_dismissed_errors() {
    let provider = EmptyProvider;
    let mut app = App::new(vec![], vec![], false);
    app.handle(Action::EditorPaste("first.log".into()), &provider);
    app.handle(Action::SubmitSource, &provider);
    let first = app.take_source_requests().pop().expect("first request");
    app.handle(Action::EditorPaste(".newer".into()), &provider);

    app.source_request_succeeded(&first, "unrelated-view");
    assert_eq!(app.focus, Focus::SourceDialog);
    assert_eq!(
        app.source_dialog.as_ref().expect("dialog").draft,
        "first.log.newer"
    );
    app.source_request_failed(first.clone(), "old failure".into());
    assert!(app.source_dialog.as_ref().expect("dialog").error.is_none());
    assert_eq!(
        app.source_notice.as_deref(),
        Some("source error: old failure")
    );

    app.handle(Action::CancelEditor, &provider);
    app.source_request_failed(first, "visible failure".into());
    assert_eq!(app.focus, Focus::SourceDialog);
    assert_eq!(
        app.source_dialog.as_ref().expect("dialog").draft,
        "first.log"
    );
    assert_eq!(
        app.source_dialog.as_ref().expect("dialog").error.as_deref(),
        Some("visible failure")
    );
}

#[test]
fn discovery_diagnostics_keep_readable_text_when_focus_changes() {
    let provider = EmptyProvider;
    let mut app = App::new(vec![], vec![], false);
    app.handle(Action::ToggleDiscovery, &provider);
    for theme in [Theme::LOVE_DARK, Theme::LOVE_LIGHT] {
        for focused in [false, true] {
            app.dialog_scroll_focused = focused;
            let mut terminal = Terminal::new(TestBackend::new(100, 28)).unwrap();
            terminal
                .draw(|frame| ui::render_with_theme(frame, &mut app, &provider, theme, None))
                .unwrap();
            let area = app.hit_regions.dialog_scroll.unwrap();
            let buffer = terminal.backend().buffer();
            assert_eq!(buffer[(area.x + 1, area.y + 1)].fg, theme.base_fg);
            assert_eq!(
                buffer[(area.x, area.y)].fg,
                if focused {
                    theme.focused_input_border
                } else {
                    theme.border
                }
            );
        }
    }
}

#[test]
fn discovery_dialog_filters_selects_and_fences_cancelled_scans() {
    use lvu::{DiscoveryItem, DiscoveryUiRequest, SourceDialogMode};

    let provider = EmptyProvider;
    let mut app = App::new(vec![], vec![], false);
    app.handle(Action::ToggleDiscovery, &provider);
    let first = app.take_discovery_requests();
    assert_eq!(first, vec![DiscoveryUiRequest::Scan { generation: 1 }]);
    assert_eq!(
        app.source_dialog.as_ref().expect("dialog").mode,
        SourceDialogMode::Discovery
    );

    app.handle(Action::RefreshDiscovery, &provider);
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
    assert!(discovered.contains("api service [Docker High Available]"));
    assert!(discovered.contains("compose service api"));
    assert!(discovered.contains("2 candidates, complete"));
    assert!(discovered.contains("Manual"));
    assert!(discovered.contains("Discover"));
    assert!(!discovered.contains("wheel select"));
    app.handle(Action::SourceInput('t'), &provider);
    app.handle(Action::SourceInput('e'), &provider);
    app.handle(Action::SourceInput('e'), &provider);
    app.handle(Action::SubmitSource, &provider);
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
    app.handle(Action::OpenSearch, &provider);
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
    assert!(rendered.find("No filter applied.").unwrap() < rendered.find("Examples:").unwrap());
    let help_row = rendered
        .lines()
        .position(|line| line.contains("Examples:"))
        .unwrap() as u16;
    let help_column = (0..buffer.area.width)
        .find(|x| buffer[(*x, help_row)].symbol() == "E")
        .unwrap();
    assert_eq!(
        buffer[(help_column, help_row)].fg,
        lvu::theme::Theme::LOVE_LIGHT.base_fg
    );
    assert!(
        rendered.contains("Status  No filter applied."),
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

    app.handle(Action::OpenTime, &provider);
    let time = render(&provider, &mut app, 54, 14);
    assert!(time.contains("Time basis: Capture"), "{time}");
    assert!(time.contains("Window: All time"), "{time}");
    assert!(time.contains("▼ Scroll down"), "{time}");
    assert!(!time.contains("Enter"), "{time}");
    assert!(!time.contains("Tab"), "{time}");
    assert!(!time.contains("Esc"), "{time}");
    assert_eq!(
        key_to_action(
            KeyEvent::new(KeyCode::Char('p'), KeyModifiers::ALT),
            Focus::TimeEditor
        ),
        Action::SetTimeBasis(lvu::TimeBasis::Capture)
    );
    app.handle(Action::CancelEditor, &provider);

    app.handle(Action::OpenRecipes, &provider);
    let recipes = render(&provider, &mut app, 54, 20);
    for label in [
        "Alt-B browse",
        "Alt-S save",
        "Alt-I import",
        "Alt-E export",
        "Alt-H history",
        "Alt-U update",
        "Alt-G refresh",
        "Alt-A adapt",
        "x reject",
    ] {
        assert!(recipes.contains(label), "missing {label}: {recipes}");
    }
    app.handle(Action::CancelEditor, &provider);

    app.handle(Action::OpenAskAi, &provider);
    let ask = render(&provider, &mut app, 54, 17);
    for label in ["Alt-F filter", "Alt-E enrichment", "Alt-T timestamp", "↑/↓"] {
        assert!(ask.contains(label), "missing {label}: {ask}");
    }
    assert_eq!(
        key_to_action(
            KeyEvent::new(KeyCode::Char('e'), KeyModifiers::ALT),
            Focus::AskAi
        ),
        Action::SelectAskAiKind(AskAiKind::Enrichment)
    );
}

#[test]
fn search_error_keeps_last_accepted_filter_and_scrolls_diagnostics() {
    let (provider, mut app) = demo();
    app.handle(Action::OpenSearch, &provider);
    app.handle(Action::EditorPaste("accepted needle".into()), &provider);
    app.handle(Action::SubmitDraft, &provider);
    let request = app.take_query_requests().pop().unwrap();
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: request.view_id,
        generation: request.generation,
        revision: request.revision,
        purpose: request.purpose,
        result: Ok(()),
    }));
    for _ in 0.."accepted needle".chars().count() {
        app.handle(Action::EditorBackspace, &provider);
    }
    app.handle(Action::EditorPaste("broken draft".into()), &provider);
    app.handle(Action::SubmitDraft, &provider);
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
    assert!(top.contains("Status · ↑/↓"), "{top}");
    assert!(app.dialog_scroll_limit > 0);
    app.handle(Action::ScrollDialog(i32::MAX), &provider);
    let bottom = render(&provider, &mut app, 54, 12);
    assert!(bottom.contains("Last accepted"), "{bottom}");
    assert!(bottom.contains("accepted needle"), "{bottom}");
    assert!(!bottom.contains("Enter apply"), "{bottom}");
}

#[test]
fn discovery_fixed_rows_keep_last_candidate_visible_highlighted_and_clickable() {
    use lvu::{DiscoveryItem, DiscoveryUiRequest};

    let provider = EmptyProvider;
    let mut app = App::new(vec![], vec![], false);
    app.handle(Action::ToggleDiscovery, &provider);
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
    app.handle(Action::MoveDiscovery(39), &provider);

    let backend = TestBackend::new(72, 12);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| ui::render(frame, &mut app, &provider))
        .unwrap();
    let selected_region = app
        .hit_regions
        .discovery_rows
        .iter()
        .find(|(_, index)| *index == 39)
        .copied()
        .expect("last selected candidate has a visible one-line hitbox");
    assert_eq!(selected_region.0.height, 1);
    assert_eq!(
        terminal.backend().buffer()[(selected_region.0.x, selected_region.0.y)].bg,
        app.theme_id.theme().selection_bg
    );
    assert!(screen(terminal.backend().buffer()).contains("candidate 39"));
    assert!(
        app.source_dialog
            .as_ref()
            .unwrap()
            .discovery
            .status_scroll_limit
            > 0
    );
    app.handle(Action::ScrollDiscoveryStatus(i32::MAX), &provider);
    terminal
        .draw(|frame| ui::render(frame, &mut app, &provider))
        .unwrap();
    assert!(screen(terminal.backend().buffer()).contains("candidate 39"));

    let first_visible = app.hit_regions.discovery_rows[0];
    app.handle(
        Action::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: first_visible.0.x,
            row: first_visible.0.y,
            modifiers: KeyModifiers::NONE,
        }),
        &provider,
    );
    assert_eq!(
        app.source_dialog.as_ref().unwrap().discovery.selected,
        first_visible.1
    );
    app.handle(Action::SubmitSource, &provider);
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
    app.handle(Action::OpenAdvanced, &provider);
    app.handle(Action::ToggleEditorCompletion, &provider);
    let field = app
        .editor_completion
        .as_ref()
        .unwrap()
        .items
        .iter()
        .position(|item| item.insertion == "pl.col('say \\'hi\\' 東京')")
        .unwrap();
    assert!(
        app.editor_completion
            .as_ref()
            .unwrap()
            .items
            .iter()
            .any(|item| item.insertion == "pl.col('raw')"),
        "the authoritative raw column remains available for unstructured logs"
    );
    app.editor_completion.as_mut().unwrap().selected = field;
    app.handle(Action::SubmitDraft, &provider);
    assert_eq!(
        app.advanced_state().unwrap().draft,
        "pl.col('say \\'hi\\' 東京')"
    );
    assert!(app.take_query_requests().is_empty());

    app.handle(Action::ToggleEditorCompletion, &provider);
    app.handle(Action::ToggleEditorCompletion, &provider);
    let value = app
        .editor_completion
        .as_ref()
        .unwrap()
        .items
        .iter()
        .position(|item| item.insertion.contains("a\\\\b"))
        .unwrap();
    app.editor_completion.as_mut().unwrap().selected = value;
    app.handle(Action::SubmitDraft, &provider);
    assert!(
        app.advanced_state()
            .unwrap()
            .draft
            .ends_with("'a\\\\b\\'c\\n東京'")
    );
    assert!(app.take_query_requests().is_empty());

    app.handle(Action::CancelEditor, &provider);
    app.handle(Action::OpenEnrichment, &provider);
    app.handle(Action::EditorPaste("copied = ".into()), &provider);
    app.handle(Action::ToggleEditorCompletion, &provider);
    render(&provider, &mut app, 100, 24);
    let space_field = app
        .editor_completion
        .as_ref()
        .unwrap()
        .items
        .iter()
        .position(|item| item.insertion == "pl.col('space field')")
        .unwrap();
    let row = app
        .hit_regions
        .editor_completion_rows
        .iter()
        .find(|(_, index)| *index == space_field)
        .unwrap()
        .0;
    app.handle(
        Action::Mouse(mouse(MouseEventKind::Down(MouseButton::Left), row.x, row.y)),
        &provider,
    );
    app.handle(Action::SubmitDraft, &provider);
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
    app.handle(Action::OpenAdvanced, &provider);
    app.handle(Action::ToggleEditorCompletion, &provider);
    assert_eq!(
        app.editor_completion.as_ref().unwrap().items[0].insertion,
        "pl.col('raw')"
    );
    let generation = app.editor_completion.as_ref().unwrap().generation;
    app.handle(Action::CancelEditor, &provider);
    assert!(app.editor_completion.is_none());
    app.handle(Action::ToggleEditorCompletion, &provider);
    assert!(app.editor_completion.as_ref().unwrap().generation > generation);
    app.handle(Action::EditorInput('x'), &provider);
    assert!(app.editor_completion.is_none());
    app.handle(Action::ToggleEditorCompletion, &provider);
    app.selected_view = 1;
    app.handle(Action::SubmitDraft, &provider);
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
    app.handle(Action::ToggleHelp, &provider);
    app.handle(
        Action::Mouse(mouse(MouseEventKind::ScrollDown, rows.x, rows.y)),
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
    app.handle(Action::OpenGrouping, &fixture);
    assert_eq!(app.focus, Focus::GroupingEditor);
    assert_eq!(
        app.active_editor_state().unwrap().draft,
        r"^(\s+|Caused by:)"
    );
    app.handle(Action::SubmitDraft, &fixture);
    let request = app.take_query_requests().pop().unwrap();
    assert_eq!(request.purpose, QueryPurpose::Grouping);
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: request.view_id,
        generation: request.generation,
        revision: request.revision,
        purpose: request.purpose,
        result: Ok(()),
    }));
    assert!(!app.view_state().unwrap().grouping.applied.is_empty());
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
fn invalid_grouping_recipe_rolls_back_every_constraint_and_keeps_failed_draft() {
    let (provider, mut app) = demo();
    app.handle(Action::OpenGrouping, &provider);
    app.handle(Action::SubmitDraft, &provider);
    let accepted = app.take_query_requests().pop().unwrap();
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: accepted.view_id,
        generation: accepted.generation,
        revision: accepted.revision,
        purpose: accepted.purpose,
        result: Ok(()),
    }));
    let prior = app.view_state().unwrap().grouping.applied.clone();

    app.handle(Action::OpenRecipes, &provider);
    let lvu::RecipeRequest::List { meta } = app.take_recipe_requests().pop().unwrap() else {
        panic!("recipe list")
    };
    app.set_recipes(
        meta,
        vec![lvu::RecipeItem {
            id: "invalid-group".into(),
            revision: "one".into(),
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
    app.handle(Action::SubmitRecipe, &provider);
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
    app.handle(Action::ToggleHelp, &provider);
    let help = render(&provider, &mut app, 70, 16);
    assert!(help.contains("EVERYWHERE"), "{help}");
    assert_eq!(app.focus, Focus::Help);
    assert!(app.help_scroll_limit > 0);
}

#[test]
fn help_is_grouped_styled_scrollable_and_does_not_move_background() {
    let (provider, mut app) = demo();
    app.handle(Action::Top, &provider);
    app.handle(Action::MoveLine(2), &provider);
    render(&provider, &mut app, 72, 16);
    let selected = app.view_state().unwrap().selected.clone();
    app.handle(Action::ToggleHelp, &provider);

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
    assert_eq!(buffer[header].fg, lvu::theme::Theme::LOVE_LIGHT.accent);
    assert!(
        buffer[header]
            .modifier
            .contains(ratatui::style::Modifier::BOLD)
    );

    app.handle(Action::ScrollHelp(i32::MAX), &provider);
    let bottom = render(&provider, &mut app, 72, 16);
    assert!(bottom.contains("ASSISTANCE"), "{bottom}");
    assert!(bottom.contains("Alt-N"), "{bottom}");
    let complete = render(&provider, &mut app, 160, 70);
    for removed in [
        "MOUSE & SELECTION",
        "j/k · ↑/↓",
        "explicit review and apply",
    ] {
        assert!(!complete.contains(removed), "{complete}");
    }
    assert_eq!(app.view_state().unwrap().selected, selected);
    app.handle(Action::ToggleHelp, &provider);
    assert_eq!(app.focus, Focus::Logs);
}

#[test]
fn field_picker_pins_colors_and_preserves_per_view_presentation() {
    let (provider, mut app) = demo();
    app.sync_provider(&provider, 10);
    app.handle(Action::OpenFieldPicker, &provider);
    assert_eq!(app.focus, Focus::FieldPicker);
    let picker = render(&provider, &mut app, 88, 24);
    assert!(picker.contains("Event fields"));
    assert!(picker.contains("service"));
    app.handle(Action::MoveFieldPicker(1), &provider);
    let first_field = app.hit_regions.field_picker_rows[0].0;
    app.handle(
        Action::Mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            first_field.x,
            first_field.y,
        )),
        &provider,
    );
    assert_eq!(app.view_state().unwrap().field_picker_selected, 0);
    app.handle(Action::TogglePinnedField, &provider);
    app.handle(Action::ToggleColorField, &provider);
    app.handle(Action::CancelEditor, &provider);
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
    empty_app.handle(Action::OpenFieldPicker, &provider);
    let empty_app_screen = render(&provider, &mut empty_app, 72, 16);
    assert_eq!(empty_app.focus, Focus::FieldPicker);
    assert!(empty_app_screen.contains("No event selected."));

    let mut no_selection = make_app();
    no_selection.handle(Action::OpenFieldPicker, &provider);
    let no_selection_screen = render(&provider, &mut no_selection, 72, 16);
    assert_eq!(no_selection.focus, Focus::FieldPicker);
    assert!(no_selection_screen.contains("No event selected."));
    assert!(!no_selection_screen.contains("Space pin"));
    assert!(!no_selection_screen.contains("o raw context"));
    assert!(no_selection.hit_regions.field_picker_rows.is_empty());

    let mut app = make_app();
    app.sync_provider(&provider, 8);
    let selected = app.view_state().unwrap().selected.clone().unwrap();
    provider.rows.borrow_mut().clear();
    app.handle(Action::OpenFieldPicker, &provider);
    assert_eq!(
        app.view_state().unwrap().field_picker_row.as_ref(),
        Some(&selected)
    );
    let loading = render(&provider, &mut app, 72, 16);
    assert!(
        loading.contains("Field data is not available yet."),
        "{loading}"
    );
    assert!(loading.contains("o raw context"), "{loading}");
    assert!(!loading.contains("Space pin"));
    assert!(app.hit_regions.field_picker_rows.is_empty());

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
                line.find("Field data").map(|x| (x as u16, y))
            })
            .expect("availability status remains visible");
        assert_eq!(buffer[status_cell].fg, Theme::LOVE_LIGHT.base_fg);
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
        empty_screen.contains("No fields found for this event"),
        "{empty_screen}"
    );
    assert_eq!(
        app.view_state().unwrap().field_picker_row.as_ref(),
        Some(&selected)
    );
    assert!(!empty_screen.contains("Space pin"));
    assert!(!empty_screen.contains("Color rows by this field"));
    assert!(!empty_screen.contains("r correlate"));
    assert!(app.hit_regions.field_picker_rows.is_empty());

    app.handle(Action::OpenContext, &provider);
    assert_eq!(app.focus, Focus::Context);
    assert_eq!(app.context_dialog.as_ref().unwrap().anchor, selected);
    app.handle(Action::CancelEditor, &provider);
    assert_eq!(app.focus, Focus::FieldPicker);
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
        vec![ViewItem {
            id: "view".into(),
            source_id: "source".into(),
            name: "view".into(),
        }],
        false,
    );
    app.sync_provider(&provider, 8);
    app.handle(Action::OpenFieldPicker, &provider);
    for _ in 0..15 {
        app.handle(Action::MoveFieldPicker(1), &provider);
    }
    let picker = render(&provider, &mut app, 56, 12);
    assert!(picker.contains("field_15"));
    assert!(!picker.contains(&"x".repeat(80)));
    assert!(app.hit_regions.field_picker_rows.len() < 12);
    assert!(
        app.hit_regions
            .field_picker_rows
            .iter()
            .all(|(area, _)| area.y < 10 && area.x > 0),
        "picker hitboxes stay in the padded body above the footer"
    );
    let last_visible = *app.hit_regions.field_picker_rows.last().unwrap();
    app.handle(
        Action::Mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            last_visible.0.x,
            last_visible.0.y,
        )),
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
    assert_eq!(app.field_picker_row(&provider).unwrap().id.sequence, 1);
    let first_visible = app.hit_regions.field_picker_rows[0];
    app.handle(
        Action::Mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            first_visible.0.x,
            first_visible.0.y,
        )),
        &provider,
    );
    assert_eq!(
        app.view_state().unwrap().field_picker_selected,
        first_visible.1
    );

    app.handle(Action::CancelEditor, &provider);
    app.handle(Action::End, &provider);
    app.handle(Action::OpenFieldPicker, &provider);
    assert_eq!(app.view_state().unwrap().field_picker_selected, 0);
    assert_eq!(app.field_picker_row(&provider).unwrap().id.sequence, 2);
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
    assert!(bottom.contains("↑/↓ scroll"), "{bottom}");

    app.handle(Action::MoveLine(-1), &provider);
    let changed = render(&provider, &mut app, 72, 20);
    assert!(changed.contains("stable display id: source:1"), "{changed}");
    assert_eq!(app.view_state().unwrap().details_scroll, 0);

    let narrow = render(&provider, &mut app, 54, 14);
    let narrow_details = app.hit_regions.details.expect("narrow details hitbox");
    assert!(
        narrow.contains("↑/↓ scroll") || narrow_details.height <= 2,
        "{narrow}"
    );
    assert!(app.hit_regions.log.unwrap().bottom() <= narrow_details.y);
    app.focus = Focus::Logs;
    app.handle(Action::CycleFocus, &provider);
    assert_eq!(app.focus, Focus::Details);
    assert_eq!(
        key_to_action(
            KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
            Focus::Details
        ),
        Action::ScrollDetails(1)
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
        Focus::SearchEditor,
        Focus::AdvancedEditor,
        Focus::EnrichmentEditor,
        Focus::CommandEnrichment,
        Focus::GroupingEditor,
        Focus::SourceDialog,
        Focus::Help,
        Focus::ViewDialog,
        Focus::FieldPicker,
        Focus::AskAi,
        Focus::Investigation,
        Focus::Storage,
        Focus::Settings,
        Focus::Recipes,
        Focus::TimeEditor,
        Focus::Context,
        Focus::Bookmarks,
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
    app.handle(Action::OpenSearch, &provider);
    app.handle(Action::EditorPaste("draft".into()), &provider);
    let _ = render(&provider, &mut app, 54, 12);
    assert!(app.hit_regions.dialog_scroll.is_some());
    app.handle(Action::ToggleEditorCompletion, &provider);
    assert!(app.dialog_scroll_focused);
    app.handle(Action::EditorInput('x'), &provider);
    app.handle(Action::EditorBackspace, &provider);
    assert_eq!(app.active_editor_state().unwrap().draft, "draft");
    let mut terminal = Terminal::new(TestBackend::new(54, 12)).unwrap();
    terminal
        .draw(|frame| ui::render(frame, &mut app, &provider))
        .unwrap();
    assert_eq!(terminal.backend().cursor_position(), Position::new(0, 0));
    app.handle(Action::ToggleEditorCompletion, &provider);
    assert!(!app.dialog_scroll_focused);
}

#[test]
fn rapid_search_edits_coalesce_and_empty_draft_retries_backpressure() {
    let (provider, mut app) = demo();
    app.handle(Action::OpenSearch, &provider);
    for ch in "rapid".chars() {
        app.handle(Action::EditorInput(ch), &provider);
        assert!(!app.flush_debounced_searches(Instant::now()));
        assert!(app.take_query_requests().is_empty());
    }
    for _ in 0..5 {
        app.handle(Action::EditorBackspace, &provider);
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
    app.handle(Action::OpenTime, &provider);
    assert!(render(&provider, &mut app, 100, 28).contains("Recognize timestamp"));
    let action = key_to_action(
        KeyEvent::new(KeyCode::Char('t'), KeyModifiers::ALT),
        app.focus,
    );
    assert_eq!(action, Action::OpenTimestampAssistant);
    app.handle(action, &provider);
    let dialog = app.ask_ai_dialog.as_ref().unwrap();
    assert_eq!(dialog.kind, AskAiKind::Enrichment);
    assert_eq!(dialog.stage, AskAiStage::Input);
    assert!(dialog.prompt.contains("timestamp_utc"));
    assert!(dialog.prompt.contains("Never infer"));
    assert!(app.take_query_requests().is_empty());
}

#[test]
fn search_reaffirmation_does_not_erase_invalid_draft_diagnostic() {
    let (provider, mut app) = demo();
    app.handle(Action::OpenSearch, &provider);
    app.handle(Action::EditorPaste("valid".into()), &provider);
    app.handle(Action::SubmitDraft, &provider);
    let good = app.take_query_requests().pop().unwrap();
    app.apply_query_completion(QueryCompletion {
        view_id: good.view_id,
        generation: good.generation,
        revision: good.revision,
        purpose: good.purpose,
        result: Ok(()),
    });
    for _ in 0..5 {
        app.handle(Action::EditorBackspace, &provider);
    }
    app.handle(Action::EditorPaste("/[/".into()), &provider);
    app.handle(Action::SubmitDraft, &provider);
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
    app.handle(Action::OpenEnrichment, &provider);
    app.handle(
        Action::EditorPaste("derived = pl.lit('ok')".into()),
        &provider,
    );
    app.handle(Action::SubmitDraft, &provider);
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
    app.handle(Action::SubmitDraft, &provider);
    assert_eq!(app.view_state().unwrap().enrichments, stages);
    assert!(app.take_query_requests().is_empty());
}

#[test]
fn extracted_time_basis_is_explicit_transactional_and_persistent() {
    let (provider, mut app) = demo();
    let view_id = app.active_view_id().unwrap().to_owned();
    app.handle(Action::OpenTime, &provider);
    app.handle(Action::SetTimeBasis(lvu::TimeBasis::Extracted), &provider);
    app.handle(
        Action::EditorPaste("2026-09-05T12:30:45Z".into()),
        &provider,
    );
    app.handle(Action::SwitchTimeField, &provider);
    app.handle(
        Action::EditorPaste("2026-09-05T12:30:46Z".into()),
        &provider,
    );
    app.handle(Action::SubmitTime, &provider);
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

    app.handle(Action::OpenTime, &provider);
    assert_eq!(
        key_to_action(
            KeyEvent::new(KeyCode::Char('u'), KeyModifiers::ALT),
            Focus::TimeEditor
        ),
        Action::SetTimeBasis(lvu::TimeBasis::Extracted)
    );
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
    assert_eq!(requests[0].source_id, app.views[0].source_id);
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
    app.handle(Action::OpenSearch, &provider);
    app.handle(Action::RestartCapture, &provider);
    assert!(app.take_source_controls().is_empty());
    assert_ne!(
        key_to_action(
            KeyEvent::new(KeyCode::Char('r'), KeyModifiers::ALT),
            Focus::SearchEditor
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
    app.handle(Action::OpenSearch, &provider);
    assert!(app.action_notice.is_none());
}

#[test]
fn raw_context_retains_filter_and_anchor_across_arrivals_and_scrolls_on_small_terminal() {
    let (mut provider, mut app) = demo();
    let mut dispatcher = provider.query_dispatcher();
    app.sync_provider(&provider, 10);
    app.handle(Action::OpenSearch, &provider);
    app.handle(Action::EditorPaste("request 05".into()), &provider);
    finish_debounced_search(&mut app, &mut dispatcher);
    app.handle(Action::CancelEditor, &provider);
    app.sync_provider(&provider, 10);
    let anchor = app.view_state().unwrap().selected.clone().unwrap();
    app.handle(Action::OpenContext, &provider);
    assert_eq!(app.focus, Focus::Context);
    let output = render(&provider, &mut app, 70, 12);
    assert!(output.contains("fixture request 04 completed"), "{output}");
    assert!(output.contains("fixture request 05 completed"), "{output}");
    assert!(output.contains("↑/↓ scroll"));
    provider.advance();
    app.sync_provider(&provider, 10);
    assert_eq!(app.context_dialog.as_ref().unwrap().anchor, anchor);
    app.handle(Action::MoveContext(10), &provider);
    assert!(render(&provider, &mut app, 70, 12).contains("fixture request 16 completed"));
    app.handle(Action::CancelEditor, &provider);
    assert_eq!(app.view_state().unwrap().search.applied, "request 05");
    assert_eq!(app.visible_rows(&provider).len(), 1);
    assert_eq!(
        key_to_action(
            KeyEvent::new(KeyCode::Char('o'), KeyModifiers::NONE),
            Focus::Logs
        ),
        Action::OpenContext
    );
}

#[test]
fn recipe_export_captures_reviewed_identity_and_does_not_replace_newer_drafts() {
    use lvu::{RecipeConfig, RecipeDialogMode, RecipeItem, RecipeRequest};
    let (provider, mut app) = demo();
    app.handle(Action::OpenRecipes, &provider);
    let RecipeRequest::List { meta } = app.take_recipe_requests().pop().unwrap() else {
        panic!("list");
    };
    app.set_recipes(
        meta,
        vec![RecipeItem {
            id: "recipe-one".into(),
            revision: "revision-one".into(),
            name: "Portable".into(),
            config: RecipeConfig::default(),
            incompatibility: None,
        }],
        None,
    );
    app.handle(
        Action::SelectRecipeMode(RecipeDialogMode::Export),
        &provider,
    );
    app.handle(
        Action::EditorPaste("/tmp/portable café.toml".into()),
        &provider,
    );
    assert!(render(&provider, &mut app, 100, 24).contains("Selected: Portable"));
    app.handle(Action::SubmitRecipe, &provider);
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
    app.handle(Action::RecipeInput('x'), &provider);
    app.recipe_exported(meta, "exported old request".into());
    assert!(app.recipe_dialog.as_ref().unwrap().name.ends_with(".tomlx"));
    assert_ne!(
        app.recipe_dialog.as_ref().unwrap().status,
        "exported old request"
    );
    assert!(app.take_query_requests().is_empty());
    assert_eq!(
        key_to_action(
            KeyEvent::new(KeyCode::Char('e'), KeyModifiers::ALT),
            Focus::Recipes
        ),
        Action::SelectRecipeMode(RecipeDialogMode::Export)
    );
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
    let id = app.bookmarks_for_view(&view)[0].id.clone();
    app.handle(Action::OpenBookmarks, &provider);
    app.handle(Action::EditBookmarkNote, &provider);
    let before_edit = app.view_interaction_revision(&view).unwrap();
    app.handle(
        Action::EditorPaste("Café request to investigate".into()),
        &provider,
    );
    assert!(app.view_interaction_revision(&view).unwrap() > before_edit);
    assert!(render(&provider, &mut app, 70, 12).contains("Café request"));
    app.handle(Action::SubmitBookmark, &provider);
    assert_eq!(
        app.bookmarks_for_view(&view)[0].note,
        "Café request to investigate"
    );
    app.handle(Action::CancelEditor, &provider);
    let saved = app.persistent_view_state(&view).unwrap();
    let mut dispatcher = provider.query_dispatcher();
    app.handle(Action::OpenSearch, &provider);
    app.handle(Action::EditorPaste("request 05".into()), &provider);
    finish_debounced_search(&mut app, &mut dispatcher);
    app.handle(Action::CancelEditor, &provider);
    app.sync_provider(&provider, 10);
    app.handle(Action::OpenBookmarks, &provider);
    app.handle(Action::SubmitBookmark, &provider);
    assert_eq!(app.focus, Focus::Context);
    assert_eq!(app.context_dialog.as_ref().unwrap().anchor, id);
    assert!(render(&provider, &mut app, 80, 16).contains("fixture request 01"));
    app.handle(Action::CancelEditor, &provider);
    assert_eq!(app.focus, Focus::Bookmarks);
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
    app.handle(Action::OpenBookmarks, &provider);
    app.handle(Action::MoveBookmark(127), &provider);
    assert!(render(&provider, &mut app, 80, 12).contains("#127 note 127"));
    let (area, index) = app.hit_regions.bookmark_rows[0];
    assert!(area.y < 10);
    app.handle(
        Action::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: area.x,
            row: area.y,
            modifiers: KeyModifiers::NONE,
        }),
        &provider,
    );
    assert_eq!(app.bookmark_dialog.as_ref().unwrap().selected, index);
    app.handle(Action::EditBookmarkNote, &provider);
    app.handle(Action::MoveBookmark(1), &provider);
    assert_eq!(app.bookmark_dialog.as_ref().unwrap().selected, index);
    app.handle(Action::EditorPaste("x".repeat(1025)), &provider);
    assert_eq!(
        app.bookmark_dialog.as_ref().unwrap().draft,
        format!("note {index}")
    );
    render(&provider, &mut app, 80, 12);
    assert!(app.hit_regions.bookmark_rows.is_empty());
    app.handle(Action::CancelEditor, &provider);
    assert_eq!(
        app.bookmarks_for_view(&view)[index].note,
        format!("note {index}")
    );
}

#[test]
fn recipe_history_is_fenced_and_update_captures_reviewed_revision() {
    use lvu::{RecipeConfig, RecipeDialogMode, RecipeItem, RecipeRequest};
    let (provider, mut app) = demo();
    let item = RecipeItem {
        id: "recipe".into(),
        revision: "current".into(),
        name: "Saved".into(),
        config: RecipeConfig::default(),
        incompatibility: None,
    };
    app.handle(Action::OpenRecipes, &provider);
    let RecipeRequest::List { meta } = app.take_recipe_requests().pop().unwrap() else {
        panic!("list");
    };
    app.set_recipes(meta, vec![item.clone()], None);
    app.handle(
        Action::SelectRecipeMode(RecipeDialogMode::History),
        &provider,
    );
    let RecipeRequest::History { meta, recipe_id } = app.take_recipe_requests().pop().unwrap()
    else {
        panic!("history");
    };
    assert_eq!(recipe_id, "recipe");
    app.handle(
        Action::SelectRecipeMode(RecipeDialogMode::Browse),
        &provider,
    );
    app.set_recipes(meta, Vec::new(), None);
    assert!(
        app.recipe_dialog.as_ref().unwrap().loading,
        "stale history cannot replace browse request"
    );
    let RecipeRequest::List { meta } = app.take_recipe_requests().pop().unwrap() else {
        panic!("list");
    };
    app.set_recipes(meta, vec![item.clone()], None);
    app.handle(
        Action::SelectRecipeMode(RecipeDialogMode::Update),
        &provider,
    );
    assert!(render(&provider, &mut app, 100, 24).contains("NEW revision"));
    app.handle(Action::RecipeInput('x'), &provider);
    app.handle(Action::SubmitRecipe, &provider);
    let RecipeRequest::Save { update, name, .. } = app.take_recipe_requests().pop().unwrap() else {
        panic!("update");
    };
    assert_eq!(update, Some(("recipe".into(), "current".into())));
    assert_eq!(name, "Saved");
    app.handle(
        Action::SelectRecipeMode(RecipeDialogMode::Browse),
        &provider,
    );
    let RecipeRequest::List { meta } = app.take_recipe_requests().pop().unwrap() else {
        panic!("list");
    };
    app.set_recipes(meta, vec![item], None);
    app.handle(
        Action::SelectRecipeMode(RecipeDialogMode::History),
        &provider,
    );
    let RecipeRequest::History { meta, .. } = app.take_recipe_requests().pop().unwrap() else {
        panic!("history");
    };
    app.set_recipes(
        meta,
        (0..30)
            .map(|n| RecipeItem {
                id: "recipe".into(),
                revision: format!("revision-{n}"),
                name: format!("Saved-{n}"),
                config: RecipeConfig::default(),
                incompatibility: None,
            })
            .collect(),
        None,
    );
    for _ in 0..29 {
        app.handle(Action::MoveRecipe(1), &provider);
    }
    assert!(render(&provider, &mut app, 80, 14).contains("Saved-29"));
    app.handle(Action::SubmitRecipe, &provider);
    assert_eq!(app.focus, Focus::Logs);
}

#[test]
fn recipe_adaptation_reviews_ordered_chain_and_rolls_back_atomically() {
    use lvu::{AskAiKind, EnrichmentDefinition, EnrichmentStageId, RecipeConfig};
    let (provider, mut app) = demo();
    app.handle(Action::OpenAskAi, &provider);
    let dialog = app.ask_ai_dialog.as_mut().unwrap();
    dialog.kind = AskAiKind::Recipe;
    dialog.recipe = Some(RecipeConfig {
        search: "candidate".into(),
        pinned_columns: vec!["number".into()],
        ..Default::default()
    });
    let generation = dialog.generation;
    let revision = dialog.definition_revision;
    let view = dialog.view_id.clone();
    let stages = vec![
        EnrichmentDefinition {
            id: EnrichmentStageId("first".into()),
            source: "/(?P<code>[0-9]+)/".into(),
        },
        EnrichmentDefinition {
            id: EnrichmentStageId("second".into()),
            source: format!(
                "number = pl.col('code').cast(pl.Int64){} # LAST-STAGE",
                " ".repeat(600)
            ),
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
    assert!(first.contains("Proposal:"));
    app.handle(Action::ScrollAskAi(65535), &provider);
    let last = render(&provider, &mut app, 80, 18);
    assert!(last.contains("retained."));
    app.handle(Action::ScrollAskAi(-65535), &provider);
    app.handle(Action::ApplyAskAi, &provider);
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
    app.handle(Action::OpenViewDialog, &provider);
    app.handle(
        Action::SelectViewDialogMode(lvu::ViewDialogMode::Sources),
        &provider,
    );
    app.handle(Action::MoveViewSource(100), &provider);
    let rendered = render(&provider, &mut app, 80, 12);
    assert!(rendered.contains("extra source 19"));
    assert!(render(&provider, &mut app, 40, 6).contains("extra source 19"));
    render(&provider, &mut app, 80, 12);
    let (_, last) = app.hit_regions.view_source_rows.last().unwrap();
    assert_eq!(*last, app.sources.len() - 1);
    app.handle(Action::ToggleViewSource, &provider);
    app.handle(Action::ReorderViewSource(-1), &provider);
    app.handle(Action::SubmitViewDialog, &provider);
    let mutation = app.take_view_requests().pop().unwrap();
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
    let hidden = app.views[0].id.clone();
    app.selected_view = 1;
    let selected = app.active_view_id().unwrap().to_owned();
    app.defer_view_restore(&hidden);
    assert_eq!(app.active_view_id(), Some(selected.as_str()));
}

#[test]
fn enrichment_workspace_separates_data_results_and_multiline_input() {
    let (provider, mut app) = demo();
    app.handle(Action::OpenEnrichment, &provider);
    let draft = format!(
        "normalized = pl.col('raw').str.replace('{}', 'END_EXPRESSION', literal=True)",
        "界e\u{301}".repeat(32)
    );
    app.handle(Action::EditorPaste(draft), &provider);
    let backend = TestBackend::new(120, 32);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| ui::render(frame, &mut app, &provider))
        .unwrap();
    let text = screen(terminal.backend().buffer());
    assert!(text.contains("normalized = pl.col('raw')"), "{text}");
    assert!(text.contains("END_EXPRESSION"), "{text}");
    assert!(text.contains("Saved steps"), "{text}");
    assert!(text.contains("Accepted output"), "{text}");
    assert!(text.contains("Raw input before enrichment"), "{text}");
    let cursor = terminal.backend().cursor_position();
    let line = text.lines().nth(cursor.y as usize).unwrap();
    assert!(
        !line.contains("Enter apply"),
        "cursor must remain in the expression field"
    );
}

#[test]
fn selection_surface_tracks_visible_dialog_and_clears_on_close_or_tiny_terminal() {
    let (provider, mut app) = demo();
    for (width, height) in [(120, 32), (54, 12)] {
        app.handle(Action::OpenEnrichment, &provider);
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
        app.handle(Action::CancelEditor, &provider);
        render(&provider, &mut app, width, height);
        assert!(app.hit_regions.selection_modal.is_none());
    }
    app.handle(Action::OpenEnrichment, &provider);
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
    app.handle(Action::ToggleHelp, &provider);
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
    app.handle(Action::OpenTime, &provider);
    let mut terminal = Terminal::new(TestBackend::new(30, 10)).unwrap();
    terminal
        .draw(|frame| ui::render(frame, &mut app, &provider))
        .unwrap();
    let buffer = terminal.backend().buffer();
    let rendered = screen(buffer);
    assert!(rendered.contains("Start"), "{rendered}");
    assert!(rendered.contains("▼ Scroll down"), "{rendered}");
    assert!(!rendered.contains("Enter"), "{rendered}");
    assert!(!rendered.contains("Tab"), "{rendered}");
    assert!(!rendered.contains("Esc"), "{rendered}");
    for _ in 0..10 {
        app.handle(Action::TimeMoveFocus(1), &provider);
    }
    terminal
        .draw(|frame| ui::render(frame, &mut app, &provider))
        .unwrap();
    let scrolled = screen(terminal.backend().buffer());
    assert!(scrolled.contains("Recognize"), "{scrolled}");
    assert!(scrolled.contains("▲ Scroll up"), "{scrolled}");
}

#[test]
fn wide_time_form_groups_bounds_and_hides_false_overflow_controls() {
    use lvu::app::TimeControl;
    let (provider, mut app) = demo();
    app.handle(Action::OpenTime, &provider);
    let rendered = render(&provider, &mut app, 100, 28);
    let start = rendered
        .lines()
        .find(|line| line.contains("Start"))
        .unwrap();
    let end = rendered.lines().find(|line| line.contains("End")).unwrap();
    assert!(start.contains('-') && start.contains(':') && start.contains("UTC"));
    assert!(end.contains('-') && end.contains(':') && end.contains("UTC"));
    assert!(rendered.contains("[ Apply ]"));
    assert!(rendered.contains("[ Clear ]"));
    assert!(rendered.contains("Recognize timestamp"), "{rendered}");
    assert!(rendered.contains("Applied:"));
    assert!(!rendered.contains("Scroll up"));
    assert!(!rendered.contains("Scroll down"));
    assert!(
        app.hit_regions
            .time_controls
            .iter()
            .all(|(_, control)| !matches!(
                control,
                TimeControl::ScrollUp | TimeControl::ScrollDown
            ))
    );
}

#[test]
fn shared_time_editing_excludes_menus_and_publishes_only_drafts() {
    use lvu::app::{TimeControl, TimeDropdown, TimeWindowChoice};
    let (provider, mut app) = demo();
    app.handle(Action::OpenTime, &provider);
    for control in [
        TimeControl::StartZoneMenu,
        TimeControl::EndZoneMenu,
        TimeControl::Apply,
    ] {
        app.handle(Action::TimeFocus(control), &provider);
        assert!(!app.is_text_editing());
    }
    app.handle(Action::TimeFocus(TimeControl::StartZone), &provider);
    app.time_dialog.as_mut().unwrap().start_zone_custom = false;
    assert!(!app.is_text_editing());
    app.time_dialog.as_mut().unwrap().start_zone_custom = true;
    assert_eq!(app.active_text_target().unwrap().field, "start-zone");
    app.time_dialog.as_mut().unwrap().dropdown = Some(TimeDropdown::StartZone);
    assert!(!app.is_text_editing());
    app.time_dialog.as_mut().unwrap().dropdown = None;
    app.handle(Action::TimeFocus(TimeControl::Window), &provider);
    app.handle(Action::TimeOpenFocused, &provider);
    app.handle(Action::TimeChooseIndex(3), &provider);
    app.handle(Action::TimeFocus(TimeControl::StartZone), &provider);
    app.handle(Action::TextStartOfLine, &provider);
    assert_eq!(
        app.view_state().unwrap().time_window_draft,
        TimeWindowChoice::Recent(900)
    );
    app.handle(Action::TextKillToEndOfLine, &provider);
    assert_eq!(app.time_dialog.as_ref().unwrap().start_zone, "");
    assert_eq!(
        app.view_state().unwrap().time_window_draft,
        TimeWindowChoice::Absolute
    );
    assert!(app.take_query_requests().is_empty());
    app.handle(Action::CancelEditor, &provider);
    app.handle(Action::OpenTime, &provider);
    assert_eq!(
        app.time_dialog.as_ref().unwrap().window,
        TimeWindowChoice::Absolute
    );
    assert_eq!(app.time_dialog.as_ref().unwrap().start_zone, "");
}

#[test]
fn zone_dropdown_stages_rolls_back_and_custom_offset_remains_exact() {
    use lvu::app::{TimeControl, TimeDropdown, TimeWindowChoice};
    let (provider, mut app) = demo();
    app.handle(Action::OpenTime, &provider);
    app.handle(Action::TimeFocus(TimeControl::StartZoneMenu), &provider);
    app.handle(Action::TimeOpenFocused, &provider);
    assert_eq!(
        app.time_dialog.as_ref().unwrap().dropdown,
        Some(TimeDropdown::StartZone)
    );
    let original = app.time_dialog.as_ref().unwrap().start_zone.clone();
    app.handle(Action::TimeMoveChoice(1), &provider);
    assert_eq!(app.time_dialog.as_ref().unwrap().start_zone, original);
    app.handle(Action::CancelEditor, &provider);
    assert_eq!(app.time_dialog.as_ref().unwrap().start_zone, original);
    assert!(app.time_dialog.as_ref().unwrap().dropdown.is_none());

    app.handle(Action::TimeOpenFocused, &provider);
    for _ in 0..16 {
        app.handle(Action::TimeMoveChoice(1), &provider);
    }
    let rendered = render(&provider, &mut app, 100, 28);
    assert!(rendered.contains("Custom offset…"), "{rendered}");
    let custom = app
        .hit_regions
        .time_choices
        .iter()
        .find(|(_, index)| *index == 16)
        .expect("visible custom offset hitbox")
        .0;
    app.handle(
        Action::Mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            custom.x,
            custom.y,
        )),
        &provider,
    );
    assert!(app.time_dialog.as_ref().unwrap().start_zone_custom);
    for _ in 0..32 {
        app.handle(Action::TimeBackspace, &provider);
    }
    app.handle(Action::EditorPaste("+12:34".into()), &provider);
    let dialog = app.time_dialog.as_ref().unwrap();
    assert_eq!(dialog.start_zone, "+12:34");
    assert_eq!(dialog.window, TimeWindowChoice::Absolute);
    let state = app.view_state().unwrap();
    assert_eq!(state.time_start_zone_draft, "+12:34");
    assert_eq!(state.time_window_draft, TimeWindowChoice::Absolute);
    assert!(app.take_query_requests().is_empty());
    app.handle(Action::TimeFocus(TimeControl::StartZoneMenu), &provider);
    app.handle(Action::TimeOpenFocused, &provider);
    let mut terminal = Terminal::new(TestBackend::new(100, 28)).unwrap();
    terminal
        .draw(|frame| ui::render(frame, &mut app, &provider))
        .unwrap();
    assert_eq!(terminal.backend().cursor_position(), Position::new(0, 0));
    app.handle(Action::TimeChooseIndex(0), &provider);
    let dialog = app.time_dialog.as_ref().unwrap();
    assert_eq!(dialog.start_zone, "Z");
    assert!(!dialog.start_zone_custom);
}

#[test]
fn narrow_zone_dropdowns_place_selected_rows_inside_modal_for_keyboard_and_mouse() {
    use lvu::app::TimeControl;
    let (provider, mut app) = demo();
    app.handle(Action::OpenTime, &provider);
    for control in [TimeControl::StartZoneMenu, TimeControl::EndZoneMenu] {
        app.handle(Action::TimeFocus(control), &provider);
        app.handle(Action::TimeOpenFocused, &provider);
        app.handle(Action::TimeMoveChoice(16), &provider);
        let rendered = render(&provider, &mut app, 46, 12);
        assert!(
            rendered.contains("Custom offset…"),
            "{control:?}\n{rendered}"
        );
        let modal = app.hit_regions.selection_modal.unwrap();
        let selected = app
            .hit_regions
            .time_choices
            .iter()
            .find(|(_, index)| *index == 16)
            .expect("selected zone choice remains visible")
            .0;
        assert!(modal.contains(Position::new(selected.x, selected.y)));
        app.handle(
            Action::Mouse(mouse(
                MouseEventKind::Down(MouseButton::Left),
                selected.x,
                selected.y,
            )),
            &provider,
        );
        assert!(app.time_dialog.as_ref().unwrap().dropdown.is_none());
        app.handle(Action::TimeFocus(control), &provider);
        app.handle(Action::TimeOpenFocused, &provider);
        app.handle(Action::TimeMoveChoice(-1), &provider);
        app.handle(Action::TimeChoose, &provider);
        assert!(app.time_dialog.as_ref().unwrap().dropdown.is_none());
    }
}

#[test]
fn narrow_time_status_is_scrollable_and_scroll_chrome_does_not_reveal_content() {
    use lvu::app::TimeControl;
    let (provider, mut app) = demo();
    let view = app.active_view_id().unwrap().to_owned();
    let mut restored = app.persistent_view_state(&view).unwrap();
    restored.time_error = Some(format!(
        "{} final-status-marker",
        "bounded diagnostic ".repeat(30)
    ));
    assert!(app.restore_persistent_view(&view, restored));
    app.handle(Action::OpenTime, &provider);
    let first = render(&provider, &mut app, 46, 12);
    assert!(first.contains("Scroll down"), "{first}");
    app.handle(Action::TimeScroll(i32::MAX), &provider);
    let last = render(&provider, &mut app, 46, 12);
    assert!(last.contains("final-status-marker"), "{last}");
    let scroll = app.time_dialog.as_ref().unwrap().scroll;
    app.handle(Action::TimeFocus(TimeControl::ScrollDown), &provider);
    let _ = render(&provider, &mut app, 46, 12);
    assert_eq!(app.time_dialog.as_ref().unwrap().scroll, scroll);
}

#[test]
fn command_enrichment_is_structured_fenced_and_never_runs_on_save() {
    let (provider, mut app) = demo();
    app.handle(Action::OpenCommandEnrichment, &provider);
    app.handle(Action::EditorPaste("/usr/bin/enrich".into()), &provider);
    app.handle(Action::CommandEnrichmentNextField, &provider);
    app.handle(Action::EditorPaste("--format\njson".into()), &provider);
    app.handle(Action::CommandEnrichmentNextField, &provider);
    app.handle(Action::EditorPaste("/tmp/work".into()), &provider);
    app.handle(Action::CommandEnrichmentNextField, &provider);
    app.handle(Action::EditorPaste("LANG=C\nMODE=wide".into()), &provider);
    app.handle(Action::SaveCommandEnrichment, &provider);
    let request = app.take_command_enrichment_requests().pop().unwrap();
    let CommandEnrichmentRequest::Save {
        generation,
        view_id,
        candidate: Some(stage),
        ..
    } = request
    else {
        panic!("save request")
    };
    let lvu_core::CommandProgram::Exec { executable, args } = &stage.definition.program else {
        panic!("structured exec")
    };
    assert_eq!(executable.to_string_lossy(), "/usr/bin/enrich");
    assert_eq!(args, &["--format", "json"]);
    assert_eq!(stage.definition.restart, lvu_core::RestartPolicy::Never);
    assert!(
        app.take_command_enrichment_requests().is_empty(),
        "saving must not run"
    );
    assert!(app.finish_command_enrichment_save(generation, &view_id, 1, Ok(Some(stage.clone()))));
    assert!(!app.finish_command_enrichment_save(generation, &view_id, 2, Err("stale".into())));

    app.handle(Action::PrepareCommandEnrichmentRun, &provider);
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
    assert!(review_screen.contains("Fixed snapshot: 7 records from 2 sources"));
    assert!(app.dialog_scroll_limit > 0);
    app.handle(Action::ScrollDialog(i32::MAX), &provider);
    let review_end = render(&provider, &mut app, 100, 28);
    assert!(review_end.contains("Environment keys: LANG, MODE"));
    app.handle(Action::ConfirmCommandEnrichmentRun, &provider);
    assert!(
        matches!(app.take_command_enrichment_requests().as_slice(), [CommandEnrichmentRequest::Execute { review_token, .. }] if review_token == "opaque-token")
    );
    assert!(app.commit_command_publication(&view_id, definition_revision, "publication-v1".into()));
    assert!(app.finish_command_enrichment_run(
        generation,
        &view_id,
        definition_revision,
        Ok("Published 7 records".into())
    ));
    let saved = app.persistent_view_state(&view_id).unwrap();
    assert_eq!(saved.command_publication.as_deref(), Some("publication-v1"));
    assert_eq!(saved.command_enrichment, Some(stage));
}

#[test]
fn command_result_save_is_immutable_and_survives_a_closed_dialog() {
    let (provider, mut app) = demo();
    app.handle(Action::OpenCommandEnrichment, &provider);
    app.handle(Action::EditorPaste("/usr/bin/enrich".into()), &provider);
    app.handle(Action::SaveCommandEnrichment, &provider);
    let CommandEnrichmentRequest::Save {
        generation,
        view_id,
        candidate,
        ..
    } = app.take_command_enrichment_requests().pop().unwrap()
    else {
        panic!("save request")
    };
    assert!(app.finish_command_enrichment_save(generation, &view_id, 1, Ok(candidate)));
    app.handle(Action::PrepareCommandEnrichmentRun, &provider);
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
    app.handle(Action::ConfirmCommandEnrichmentRun, &provider);
    assert!(matches!(
        app.take_command_enrichment_requests().as_slice(),
        [CommandEnrichmentRequest::Execute { .. }]
    ));
    assert!(!app.begin_command_result_save(generation + 1, &view_id, definition_revision));
    assert!(app.begin_command_result_save(generation, &view_id, definition_revision));

    let original = app
        .command_enrichment_dialog
        .as_ref()
        .unwrap()
        .program
        .clone();
    for action in [
        Action::CommandEnrichmentInput('x'),
        Action::EditorBackspace,
        Action::EditorPaste("changed".into()),
        Action::SaveCommandEnrichment,
        Action::RemoveCommandEnrichment,
        Action::PrepareCommandEnrichmentRun,
    ] {
        app.handle(action, &provider);
    }
    let dialog = app.command_enrichment_dialog.as_ref().unwrap();
    assert_eq!(dialog.program, original);
    assert_eq!(
        dialog.run_state,
        lvu::app::CommandEnrichmentRunState::SavingResults
    );
    assert!(app.take_command_enrichment_requests().is_empty());
    let rendered = render(&provider, &mut app, 100, 28);
    assert!(rendered.contains("Status: Saving results…"), "{rendered}");
    assert!(!rendered.contains("Esc close"), "{rendered}");
    assert!(!rendered.contains("Ctrl-S save"), "{rendered}");

    app.handle(Action::CancelEditor, &provider);
    assert!(app.command_enrichment_dialog.is_none());
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
    app.handle(Action::OpenCommandEnrichment, &provider);
    app.handle(Action::EditorPaste("/usr/bin/enrich".into()), &provider);
    app.handle(Action::SaveCommandEnrichment, &provider);
    let CommandEnrichmentRequest::Save {
        generation,
        view_id,
        candidate,
        ..
    } = app.take_command_enrichment_requests().pop().unwrap()
    else {
        panic!("save request")
    };
    assert!(app.finish_command_enrichment_save(generation, &view_id, 1, Ok(candidate)));
    app.handle(Action::PrepareCommandEnrichmentRun, &provider);
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
    assert!(rendered.contains("Status: Error ·"), "{rendered}");
    app.handle(Action::ScrollDialog(i32::MAX), &provider);
    let rendered = render(&provider, &mut app, 78, 24);
    assert!(rendered.contains("malformed_json"), "{rendered}");

    app.handle(Action::CancelEditor, &provider);
    app.action_notice = None;
    // A fenced completion arriving after its dialog closes remains visible without
    // mutating a newer dialog. Model the already-dispatched run context directly.
    app.handle(Action::OpenCommandEnrichment, &provider);
    app.handle(Action::PrepareCommandEnrichmentRun, &provider);
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
    app.handle(Action::ConfirmCommandEnrichmentRun, &provider);
    app.take_command_enrichment_requests();
    app.handle(Action::CancelEditor, &provider);
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
    app.handle(Action::OpenCommandEnrichment, &provider);
    app.handle(
        Action::EditorPaste(format!("/opt/{}-e\u{301}", "界".repeat(24))),
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

    app.handle(Action::CommandEnrichmentNextField, &provider);
    app.handle(Action::EditorPaste("first".into()), &provider);
    app.handle(Action::CommandEnrichmentInput('\n'), &provider);
    terminal
        .draw(|frame| ui::render(frame, &mut app, &provider))
        .unwrap();
    let after_newline = screen(terminal.backend().buffer());
    assert!(after_newline.contains("2 line(s)"), "{after_newline}");
    assert_eq!(
        terminal.backend().cursor_position().x,
        app.hit_regions.selection_modal.unwrap().x + 1,
        "trailing empty argument line must own the cursor"
    );
}

#[test]
fn narrow_command_controls_keep_the_focused_action_visible_and_clickable() {
    use lvu::app::CommandEnrichmentControl;
    let (provider, mut app) = demo();
    app.handle(Action::OpenCommandEnrichment, &provider);
    for (control, label) in [
        (CommandEnrichmentControl::NewLine, "New line"),
        (CommandEnrichmentControl::Save, "Save"),
        (CommandEnrichmentControl::Review, "Review"),
        (CommandEnrichmentControl::Remove, "Remove"),
    ] {
        app.handle(Action::FocusCommandEnrichmentControl(control), &provider);
        let output = render(&provider, &mut app, 34, 18);
        assert!(output.contains(label), "missing focused {label}: {output}");
        assert!(
            app.hit_regions
                .command_enrichment_controls
                .iter()
                .any(|(_, visible)| *visible == control),
            "focused {label} has no hitbox"
        );
    }
}

#[test]
fn enrichment_caret_uses_one_exact_boundary_multiline_model() {
    let (provider, mut exact) = demo();
    exact.handle(Action::OpenEnrichment, &provider);
    exact.handle(
        Action::EditorPaste(format!("{}\n", "a".repeat(76))),
        &provider,
    );
    let mut exact_terminal = Terminal::new(TestBackend::new(100, 28)).unwrap();
    exact_terminal
        .draw(|frame| ui::render(frame, &mut exact, &provider))
        .unwrap();
    let exact_cursor = exact_terminal.backend().cursor_position();

    let (provider, mut combined) = demo();
    combined.handle(Action::OpenEnrichment, &provider);
    combined.handle(
        Action::EditorPaste(format!("{}\ne\u{301}", "a".repeat(76))),
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

    combined.handle(Action::MoveEnrichmentControl(1), &provider);
    combined_terminal
        .draw(|frame| ui::render(frame, &mut combined, &provider))
        .unwrap();
    assert!(!combined.is_text_editing());
}

#[test]
fn command_save_survives_close_and_ready_review_is_invalidated_by_edits() {
    let (provider, mut app) = demo();
    let view_id = app.active_view_id().unwrap().to_owned();
    app.handle(Action::OpenCommandEnrichment, &provider);
    app.handle(Action::EditorPaste("/bin/enrich".into()), &provider);
    app.handle(Action::SaveCommandEnrichment, &provider);
    let CommandEnrichmentRequest::Save {
        generation,
        candidate,
        ..
    } = app.take_command_enrichment_requests().pop().unwrap()
    else {
        panic!("save")
    };
    app.handle(Action::CancelEditor, &provider);
    assert!(app.finish_command_enrichment_save(generation, &view_id, 1, Ok(candidate)));
    assert_eq!(
        app.persistent_view_state(&view_id)
            .unwrap()
            .command_enrichment_revision,
        1
    );
    assert!(app.action_notice.as_deref().unwrap().contains("not run"));

    app.handle(Action::OpenCommandEnrichment, &provider);
    app.handle(Action::PrepareCommandEnrichmentRun, &provider);
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
    app.handle(Action::CommandEnrichmentInput('2'), &provider);
    app.handle(Action::ConfirmCommandEnrichmentRun, &provider);
    assert!(
        app.take_command_enrichment_requests().is_empty(),
        "edited Ready review must not execute"
    );
    app.handle(Action::PrepareCommandEnrichmentRun, &provider);
    assert!(app.take_command_enrichment_requests().is_empty());
    assert!(
        app.command_enrichment_dialog
            .as_ref()
            .unwrap()
            .error
            .as_deref()
            .unwrap()
            .contains("save it")
    );
}

#[test]
fn command_request_admission_is_bounded_while_saves_wait_for_acknowledgement() {
    let (provider, mut app) = demo();
    let mut acknowledgements = Vec::new();
    for index in 0..8 {
        app.handle(Action::OpenCommandEnrichment, &provider);
        app.handle(
            Action::EditorPaste(format!("/bin/enrich-{index}")),
            &provider,
        );
        app.handle(Action::SaveCommandEnrichment, &provider);
        let requests = app.take_command_enrichment_requests();
        assert!(matches!(
            requests.as_slice(),
            [CommandEnrichmentRequest::Save { .. }]
        ));
        acknowledgements.extend(requests);
        app.handle(Action::CancelEditor, &provider);
    }
    app.handle(Action::OpenCommandEnrichment, &provider);
    app.handle(Action::EditorPaste("/bin/overflow".into()), &provider);
    app.handle(Action::SaveCommandEnrichment, &provider);
    assert!(app.take_command_enrichment_requests().is_empty());
    assert!(
        app.command_enrichment_dialog
            .as_ref()
            .unwrap()
            .error
            .as_deref()
            .unwrap()
            .contains("queue is full")
    );
    for (index, request) in acknowledgements.into_iter().enumerate() {
        let CommandEnrichmentRequest::Save {
            generation,
            view_id,
            candidate,
            ..
        } = request
        else {
            unreachable!()
        };
        assert_eq!(
            app.finish_command_enrichment_save(generation, &view_id, 1, Ok(candidate)),
            index == 0
        );
    }
    app.handle(Action::CancelEditor, &provider);
    app.handle(Action::OpenCommandEnrichment, &provider);
    app.handle(Action::SaveCommandEnrichment, &provider);
    assert!(matches!(
        app.take_command_enrichment_requests().as_slice(),
        [CommandEnrichmentRequest::Save { .. }]
    ));
}

#[test]
fn command_enrichment_keys_match_the_action_footer() {
    let key = |code, modifiers| KeyEvent::new(code, modifiers);
    assert_eq!(
        key_to_action(
            key(KeyCode::Char('s'), KeyModifiers::CONTROL),
            Focus::CommandEnrichment
        ),
        Action::SaveCommandEnrichment
    );
    assert_eq!(
        key_to_action(
            key(KeyCode::Char('r'), KeyModifiers::CONTROL),
            Focus::CommandEnrichment
        ),
        Action::PrepareCommandEnrichmentRun
    );
    assert_eq!(
        key_to_action(
            key(KeyCode::Char('n'), KeyModifiers::ALT),
            Focus::CommandEnrichment
        ),
        Action::CommandEnrichmentInput('\n')
    );
    // Enhanced-keyboard Enter encodings remain optional aliases.
    assert_eq!(
        key_to_action(
            key(KeyCode::Char('c'), KeyModifiers::ALT),
            Focus::EnrichmentEditor
        ),
        Action::OpenCommandEnrichment
    );
    assert_eq!(
        key_to_action(
            key(KeyCode::Enter, KeyModifiers::CONTROL),
            Focus::CommandEnrichment
        ),
        Action::SaveCommandEnrichment
    );
    assert_eq!(
        key_to_action(
            key(KeyCode::Enter, KeyModifiers::ALT),
            Focus::CommandEnrichment
        ),
        Action::PrepareCommandEnrichmentRun
    );
    assert_eq!(
        key_to_action(
            key(KeyCode::Enter, KeyModifiers::NONE),
            Focus::CommandEnrichment
        ),
        Action::ActivateCommandEnrichmentControl
    );
    assert_eq!(
        key_to_action(
            key(KeyCode::Char('n'), KeyModifiers::ALT),
            Focus::EnrichmentEditor
        ),
        Action::EditorInput('\n')
    );
}

#[test]
fn command_arguments_round_trip_an_intentional_trailing_empty_value() {
    let (provider, mut app) = demo();
    let view_id = app.active_view_id().unwrap().to_owned();
    app.handle(Action::OpenCommandEnrichment, &provider);
    app.handle(Action::EditorPaste("/bin/tool".into()), &provider);
    app.handle(Action::CommandEnrichmentNextField, &provider);
    app.handle(Action::EditorPaste("two words".into()), &provider);
    app.handle(Action::CommandEnrichmentInput('\n'), &provider);
    app.handle(Action::SaveCommandEnrichment, &provider);
    let CommandEnrichmentRequest::Save {
        generation,
        candidate: Some(stage),
        ..
    } = app.take_command_enrichment_requests().pop().unwrap()
    else {
        panic!("save")
    };
    let lvu_core::CommandProgram::Exec { args, .. } = &stage.definition.program else {
        panic!("exec")
    };
    assert_eq!(args, &["two words", ""]);
    assert!(app.finish_command_enrichment_save(generation, &view_id, 1, Ok(Some(stage))));
    app.handle(Action::CancelEditor, &provider);
    app.handle(Action::OpenCommandEnrichment, &provider);
    assert_eq!(
        app.command_enrichment_dialog.as_ref().unwrap().arguments,
        "two words\n"
    );
}

#[test]
fn text_line_controls_move_without_mutation_and_q_inserts_at_the_cursor() {
    let (provider, mut app) = demo();
    app.handle(Action::OpenAdvanced, &provider);
    app.handle(Action::EditorPaste("界e\u{301}tail".into()), &provider);
    let before = app.active_editor_state().unwrap().draft.clone();
    app.handle(Action::TextStartOfLine, &provider);
    assert_eq!(app.active_editor_state().unwrap().draft, before);
    app.handle(Action::EditorInput('q'), &provider);
    assert_eq!(
        app.active_editor_state().unwrap().draft,
        format!("q{before}")
    );
    app.handle(Action::TextEndOfLine, &provider);
    app.handle(Action::TextKillToEndOfLine, &provider);
    assert_eq!(
        app.active_editor_state().unwrap().draft,
        format!("q{before}")
    );

    app.handle(Action::TextStartOfLine, &provider);
    app.handle(Action::TextKillToEndOfLine, &provider);
    assert!(app.active_editor_state().unwrap().draft.is_empty());
}

#[test]
fn arrow_keys_route_only_active_text_fields_and_move_multiline_carets() {
    let plain = |code| KeyEvent::new(code, KeyModifiers::NONE);
    let (provider, mut app) = demo();

    app.handle(Action::OpenSearch, &provider);
    app.handle(Action::EditorPaste("abc".into()), &provider);
    let action = app.key_to_action(plain(KeyCode::Left));
    assert_eq!(action, Action::TextMoveLeft);
    app.handle(action, &provider);
    app.handle(Action::EditorInput('q'), &provider);
    assert_eq!(app.active_editor_state().unwrap().draft, "abqc");

    app.handle(Action::CancelEditor, &provider);
    app.handle(Action::OpenEnrichment, &provider);
    app.handle(Action::EditorPaste("ab\ncd".into()), &provider);
    let action = app.key_to_action(plain(KeyCode::Up));
    assert_eq!(action, Action::TextMoveUp);
    app.handle(action, &provider);
    app.handle(Action::EditorInput('q'), &provider);
    assert_eq!(app.active_editor_state().unwrap().draft, "abq\ncd");

    app.handle(Action::CancelEditor, &provider);
    app.handle(Action::OpenCommandEnrichment, &provider);
    app.handle(Action::EditorPaste("tool".into()), &provider);
    let action = app.key_to_action(plain(KeyCode::Left));
    app.handle(action, &provider);
    app.handle(Action::CommandEnrichmentInput('q'), &provider);
    assert_eq!(
        app.command_enrichment_dialog.as_ref().unwrap().program,
        "tooql"
    );
    app.handle(Action::CommandEnrichmentNextField, &provider);
    app.handle(Action::EditorPaste("ab\ncd".into()), &provider);
    let action = app.key_to_action(plain(KeyCode::Up));
    app.handle(action, &provider);
    app.handle(Action::CommandEnrichmentInput('q'), &provider);
    assert_eq!(
        app.command_enrichment_dialog.as_ref().unwrap().arguments,
        "abq\ncd"
    );

    let mut source = App::new(vec![], vec![], false);
    source.handle(Action::EditorPaste("abc".into()), &provider);
    let action = source.key_to_action(plain(KeyCode::Left));
    assert_eq!(action, Action::TextMoveLeft);
    source.handle(action, &provider);
    source.handle(Action::SourceInput('q'), &provider);
    assert_eq!(source.source_dialog.as_ref().unwrap().draft, "abqc");
    source.handle(Action::ToggleSourceControlFocus, &provider);
    assert!(!source.is_text_editing());
    assert_eq!(
        source.key_to_action(plain(KeyCode::Left)),
        Action::MoveSourceMode(-1)
    );
}

#[test]
fn stale_command_run_completions_release_capacity_without_changing_restored_state() {
    let (provider, mut app) = demo();
    app.handle(Action::OpenCommandEnrichment, &provider);
    app.handle(Action::EditorPaste("/usr/bin/enrich".into()), &provider);
    app.handle(Action::SaveCommandEnrichment, &provider);
    let CommandEnrichmentRequest::Save {
        generation,
        view_id,
        candidate,
        ..
    } = app.take_command_enrichment_requests().pop().unwrap()
    else {
        panic!("save")
    };
    assert!(app.finish_command_enrichment_save(generation, &view_id, 1, Ok(candidate)));
    for _ in 0..10 {
        app.handle(Action::PrepareCommandEnrichmentRun, &provider);
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
        app.handle(Action::ConfirmCommandEnrichmentRun, &provider);
        assert!(matches!(
            app.take_command_enrichment_requests().as_slice(),
            [CommandEnrichmentRequest::Execute { .. }]
        ));
        app.handle(Action::CancelEditor, &provider);
        app.take_command_enrichment_requests();
        let mut restored = app.persistent_view_state(&view_id).unwrap();
        restored.command_enrichment_revision += 1;
        restored.command_publication = Some("last-good".into());
        app.restore_persistent_view(&view_id, restored);
        assert!(!app.finish_command_enrichment_run(
            generation,
            &view_id,
            definition_revision,
            Ok("stale success".into())
        ));
        assert_eq!(
            app.persistent_view_state(&view_id)
                .unwrap()
                .command_publication
                .as_deref(),
            Some("last-good")
        );
        app.handle(Action::OpenCommandEnrichment, &provider);
    }
}
