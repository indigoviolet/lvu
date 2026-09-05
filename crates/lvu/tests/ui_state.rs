use std::{cell::RefCell, collections::HashMap, time::Instant};

use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};
use lvu::{
    Action, App, DisplayRow, Focus, PersistentViewState, QueryCompletion, QueryConstraints,
    QueryFailure, QueryPurpose, QueryRequest, RowId, RowPage, RowProvider, SourceKind,
    ViewportRequest,
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
            applied_search: "request 01".into(),
            search_draft: "unfinished literal".into(),
            search_error: None,
            applied_advanced: "pl.col('raw').str.contains('completed')".into(),
            advanced_draft: "invalid (".into(),
            advanced_error: Some("invalid expression".into()),
            applied_enrichment: String::new(),
            enrichment_draft: String::new(),
            enrichment_error: None,
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
    assert!(render(&provider, &mut app, 70, 16).contains("left click exact row/view"));
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
