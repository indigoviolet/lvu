use std::{
    cell::RefCell,
    collections::HashMap,
    time::{Duration, Instant},
};

use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};
use lvu::{
    Action, App, AskAiKind, AskAiRequest, AskAiStage, DisplayRow, Focus, InvestigationItem,
    InvestigationRequest, InvestigationStage, PersistentViewState, QueryCompletion,
    QueryConstraints, QueryFailure, QueryPurpose, QueryRequest, RowId, RowPage, RowProvider,
    SourceKind, StorageCategory, StorageEntry, StorageSnapshot, ViewportRequest,
    app::{MAX_EDITOR_BYTES, SEARCH_DEBOUNCE, SourceItem, ViewItem, key_to_action},
    fixture::FixtureProvider,
    terminal::{QueryDispatcher, poll_query_completions, submit_query_requests},
    ui,
};
use pretty_assertions::assert_eq;
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};

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
        truncated: false,
        errors: vec![],
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
    app.handle(
        Action::EditorPaste("x".repeat(MAX_EDITOR_BYTES + 100)),
        &provider,
    );
    assert_eq!(
        app.search_state().expect("search").draft.len(),
        MAX_EDITOR_BYTES
    );
}

#[test]
fn enrichment_editor_emits_composite_request_and_failed_draft_preserves_applied() {
    let (provider, mut app) = demo();
    app.handle(Action::OpenEnrichment, &provider);
    let editor = render(&provider, &mut app, 100, 28);
    assert!(editor.contains("Representative before:"));
    assert!(editor.contains("Applied after: no enrichment"));
    app.handle(
        Action::EditorPaste("status = pl.lit(200)".into()),
        &provider,
    );
    app.handle(Action::SubmitDraft, &provider);
    let request = app.take_query_requests().pop().unwrap();
    assert_eq!(request.purpose, QueryPurpose::Enrichment);
    assert_eq!(
        request.constraints.enrichment.as_deref(),
        Some("status = pl.lit(200)")
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

    app.handle(Action::EditorBackspace, &provider);
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
fn restored_constraints_are_pending_until_real_dispatch_completion() {
    let (provider, mut app) = demo();
    let view_id = app.active_view_id().unwrap().to_owned();
    assert!(app.restore_persistent_view(
        &view_id,
        PersistentViewState {
            view_name: "All events".into(),
            applied_search: "request 01".into(),
            search_draft: "unfinished literal".into(),
            search_error: None,
            applied_advanced: "pl.col('raw').str.contains('completed')".into(),
            advanced_draft: "invalid (".into(),
            advanced_error: Some("invalid expression".into()),
            applied_enrichment: String::new(),
            enrichment_draft: String::new(),
            enrichment_error: None,
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
    assert!(app.restore_persistent_view(
        &view_id,
        PersistentViewState {
            applied_enrichment: "code = pl.lit(200)".into(),
            enrichment_draft: "code = pl.col(".into(),
            enrichment_error: Some("unfinished".into()),
            ..PersistentViewState::default()
        }
    ));
    assert!(app.view_has_pending_query(&view_id));
    assert!(
        app.persistent_view_state(&view_id)
            .unwrap()
            .applied_enrichment
            .is_empty(),
        "autosave must not replace the stored recipe while restoration compiles"
    );
    let request = app.take_query_requests().pop().unwrap();
    assert_eq!(request.purpose, QueryPurpose::Enrichment);
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: view_id.clone(),
        generation: request.generation,
        revision: request.revision,
        purpose: request.purpose,
        result: Ok(()),
    }));
    assert!(!app.view_has_pending_query(&view_id));
    assert_eq!(
        app.persistent_view_state(&view_id)
            .unwrap()
            .applied_enrichment,
        "code = pl.lit(200)"
    );
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
        latest.constraints.enrichment.as_deref(),
        Some("code = pl.lit(200)")
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
        rebased.constraints.enrichment.as_deref(),
        Some("code = pl.lit(200)")
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
        rollback.constraints.enrichment.as_deref(),
        Some("old_field = pl.lit('ok')")
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
    assert!(dialog.loading);

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
    assert!(dialog.contains("Ask AI (local Paseo)"));
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
    assert!(render(&provider, &mut app, 120, 30).contains("Investigate with local Paseo"));
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
        Action::CompleteSourcePath
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
    assert!(output.contains("Choices"));
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
    assert!(help.contains("m grouping"), "{help}");
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
