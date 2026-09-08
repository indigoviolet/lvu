//! Acceptance for the fork subsystem as part of the query seam
//! (docs/component-model.md §2.3). W15 recorded the follow-through when it
//! extracted `Views`: "`Views` is the query seam plus the roles that gate it,
//! not the whole view lifecycle", with forking to migrate as its own step.
//!
//! What is asserted here is that the seam now owns the whole of it — no layer
//! and no `Action` hands a refusal back to the shell — and that the invariants
//! that made forking delicate are unchanged:
//!
//! * a fork happens on an explicit apply and never mid-word;
//! * the candidate lands directly after the view it came from;
//! * Escape closes the editor and never retracts a fork the user applied;
//! * a rejected query leaves no phantom view, and the origin keeps its
//!   diagnostic;
//! * one candidate per editing burst, superseded rather than accumulated.

use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use lvu::{
    Action, App, QueryCompletion, QueryFailure, QueryPurpose, ViewRole,
    app::{RecipeConfig, SEARCH_DEBOUNCE},
    component::{Open, RawEvent},
    fixture::FixtureProvider,
};

fn demo() -> (FixtureProvider, App) {
    let (provider, sources, views) = FixtureProvider::demo();
    (provider, App::new(sources, views, true))
}

fn key(app: &mut App, provider: &FixtureProvider, code: KeyCode) {
    app.handle(
        Action::Raw(RawEvent::Key(KeyEvent::new(code, KeyModifiers::NONE))),
        provider,
    );
}

fn type_text(app: &mut App, provider: &FixtureProvider, text: &str) {
    for character in text.chars() {
        key(app, provider, KeyCode::Char(character));
    }
}

/// Make the active view canonical, which is what makes every edit fork.
fn canonical(app: &mut App) -> String {
    let id = app.active_view_id().unwrap().to_owned();
    app.set_view_role(&id, ViewRole::Canonical);
    id
}

/// Drive a staged candidate all the way to an installed view, the way
/// `lvu-app`'s fork loop does.
fn settle(app: &mut App, provider: &FixtureProvider) -> String {
    let requests = app.take_view_fork_requests();
    assert_eq!(requests.len(), 1, "one candidate per burst: {requests:?}");
    let candidate = requests[0].candidate_view_id.clone();
    assert!(app.begin_fork_query(&candidate));
    let query = app
        .take_query_requests()
        .into_iter()
        .find(|request| request.view_id == candidate)
        .expect("the candidate queries for itself");
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: candidate.clone(),
        generation: query.generation,
        revision: query.revision,
        purpose: query.purpose,
        result: Ok(()),
    }));
    let ready = app.take_ready_forks();
    assert_eq!(ready.len(), 1, "{ready:?}");
    assert!(app.install_fork(&candidate));
    app.sync_provider(provider, 8);
    candidate
}

#[test]
fn an_explicit_apply_forks_and_a_pause_in_typing_never_does() {
    let (provider, mut app) = demo();
    canonical(&mut app);
    let before = app.views().len();

    app.handle(Action::Open(Open::Search), &provider);
    type_text(&mut app, &provider, "request");
    // The debounce settles into nothing: forking mid-word would hand the user a
    // view they did not ask for, and one more per pause in typing.
    assert!(app.flush_debounced_searches(Instant::now() + SEARCH_DEBOUNCE));
    assert!(app.take_view_fork_requests().is_empty());
    assert!(app.take_query_requests().is_empty());
    assert_eq!(app.views().len(), before);

    key(&mut app, &provider, KeyCode::Enter);
    assert_eq!(
        app.take_view_fork_requests().len(),
        1,
        "applying is what asks for a view"
    );
}

#[test]
fn a_forked_view_is_inserted_directly_after_its_origin() {
    let (provider, mut app) = demo();
    let origin = canonical(&mut app);
    let origin_index = app
        .views()
        .iter()
        .position(|view| view.id == origin)
        .unwrap();

    app.handle(Action::Open(Open::Search), &provider);
    type_text(&mut app, &provider, "request");
    key(&mut app, &provider, KeyCode::Enter);
    let candidate = settle(&mut app, &provider);

    // The sidebar groups views under their source, so appending would leave
    // cycling order disagreeing with what is on screen.
    assert_eq!(
        app.views()[origin_index + 1].id,
        candidate,
        "the fork sits directly after the view it came from"
    );
    assert_eq!(app.active_view_id(), Some(candidate.as_str()));
    assert_eq!(app.view_role(&candidate), ViewRole::Derived);
    // The origin is unfiltered again, drafts included: what the user typed now
    // lives in the view it created.
    assert!(app.views.state(&origin).unwrap().search.draft.is_empty());
    assert!(app.views.state(&origin).unwrap().search.applied.is_empty());
    assert_eq!(app.views.state(&candidate).unwrap().search.draft, "request");
}

#[test]
fn escape_closes_the_editor_and_never_retracts_an_applied_fork() {
    // `7b002b5`: Escape closes the surface in front of the user; it is not an
    // undo. `cancel_fork_for_origin` has exactly one legitimate caller, and
    // dismissal is not it.
    let (provider, mut app) = demo();
    canonical(&mut app);
    app.handle(Action::Open(Open::Search), &provider);
    type_text(&mut app, &provider, "request");
    key(&mut app, &provider, KeyCode::Enter);

    key(&mut app, &provider, KeyCode::Esc);
    assert!(!app.layers.filter.is_open());
    assert!(
        app.take_fork_discards().is_empty(),
        "a fork the user applied survives the editor closing"
    );
    let candidate = settle(&mut app, &provider);
    assert_eq!(app.active_view_id(), Some(candidate.as_str()));
}

#[test]
fn an_editing_burst_supersedes_its_candidate_rather_than_accumulating_views() {
    let (provider, mut app) = demo();
    canonical(&mut app);
    app.handle(Action::Open(Open::Search), &provider);
    type_text(&mut app, &provider, "one");
    key(&mut app, &provider, KeyCode::Enter);
    let first = app.take_view_fork_requests();
    assert_eq!(first.len(), 1);
    let candidate = first[0].candidate_view_id.clone();

    // A second apply before the candidate is registered reuses its identity.
    type_text(&mut app, &provider, "two");
    key(&mut app, &provider, KeyCode::Enter);
    assert!(
        app.take_view_fork_requests().is_empty(),
        "the candidate is restaged, not proposed again"
    );
    assert!(app.begin_fork_query(&candidate));
    let query = app
        .take_query_requests()
        .into_iter()
        .find(|request| request.view_id == candidate)
        .unwrap();
    assert_eq!(
        query.constraints.text.as_ref().unwrap().literal,
        "onetwo",
        "the newest draft is what the candidate queries for"
    );
}

#[test]
fn an_edit_that_changes_nothing_cancels_the_candidate_instead_of_proposing_one() {
    let (provider, mut app) = demo();
    canonical(&mut app);
    app.handle(Action::Open(Open::Search), &provider);
    type_text(&mut app, &provider, "request");
    key(&mut app, &provider, KeyCode::Enter);
    let candidate = app.take_view_fork_requests()[0].candidate_view_id.clone();

    // Clearing the box on an already-unfiltered view is the common case: it
    // must cancel the candidate rather than propose a second unfiltered view.
    for _ in 0.."request".len() {
        key(&mut app, &provider, KeyCode::Backspace);
    }
    key(&mut app, &provider, KeyCode::Enter);
    assert_eq!(app.take_fork_discards(), vec![candidate]);
    assert!(app.take_view_fork_requests().is_empty());
}

#[test]
fn a_rejected_candidate_leaves_no_view_and_the_origin_keeps_the_diagnostic() {
    let (provider, mut app) = demo();
    let origin = canonical(&mut app);
    let before = app.views().len();

    app.handle(Action::Open(Open::Advanced), &provider);
    app.handle(Action::Raw(RawEvent::Paste("pl.col(".into())), &provider);
    key(&mut app, &provider, KeyCode::Enter);
    let candidate = app.take_view_fork_requests()[0].candidate_view_id.clone();
    assert!(app.begin_fork_query(&candidate));
    let query = app
        .take_query_requests()
        .into_iter()
        .find(|request| request.view_id == candidate)
        .unwrap();
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: candidate.clone(),
        generation: query.generation,
        revision: query.revision,
        purpose: QueryPurpose::Advanced,
        result: Err(QueryFailure {
            purpose: QueryPurpose::Advanced,
            message: "compiler rejected expression".into(),
        }),
    }));

    assert_eq!(app.views().len(), before, "no phantom view");
    assert!(app.take_ready_forks().is_empty());
    assert_eq!(app.take_fork_discards(), vec![candidate]);
    assert_eq!(
        app.views.state(&origin).unwrap().advanced.error.as_deref(),
        Some("compiler rejected expression"),
        "the diagnostic goes back to the view the user is looking at"
    );
    assert!(
        app.views
            .state(&origin)
            .unwrap()
            .advanced
            .applied
            .is_empty()
    );
}

#[test]
fn a_recipe_and_a_time_window_fork_through_the_same_seam() {
    for which in ["recipe", "time"] {
        let (_provider, mut app) = demo();
        let origin = canonical(&mut app);
        let now = 1_700_000_000_000_000_000;
        match which {
            "recipe" => {
                let config = RecipeConfig {
                    search: "beta".into(),
                    ..RecipeConfig::default()
                };
                // The seam stages the derived view itself: no `Outcome::Legacy`
                // hand-back, and the caller only sees an accepted revision.
                assert!(app.views.apply_recipe(&origin, config, now).is_ok());
            }
            _ => {
                let window = lvu::CaptureTimeRange {
                    start_unix_nanos: 1_000,
                    end_unix_nanos: 2_000,
                };
                assert!(
                    app.views
                        .submit_capture_time(&origin, Some(window), None, lvu::TimeBasis::Capture)
                        .is_ok()
                );
            }
        }
        let requests = app.take_view_fork_requests();
        assert_eq!(requests.len(), 1, "{which}: {requests:?}");
        assert_eq!(requests[0].origin_view_id, origin);
        assert!(
            app.take_query_requests()
                .iter()
                .all(|request| request.view_id != origin),
            "{which}: the canonical view is never queried for an edit it refused"
        );
    }
}

#[test]
fn a_candidate_whose_origin_stopped_being_canonical_is_discarded() {
    let (provider, mut app) = demo();
    let origin = canonical(&mut app);
    app.handle(Action::Open(Open::Search), &provider);
    type_text(&mut app, &provider, "one");
    key(&mut app, &provider, KeyCode::Enter);
    let candidate = app.take_view_fork_requests()[0].candidate_view_id.clone();
    assert!(app.begin_fork_query(&candidate));
    let query = app
        .take_query_requests()
        .into_iter()
        .find(|request| request.view_id == candidate)
        .unwrap();

    // The candidate only exists because its origin refuses to be filtered. An
    // origin that is no longer canonical would have applied the edit itself, so
    // installing the candidate would add a view nobody asked for.
    app.set_view_role(&origin, ViewRole::Derived);
    assert!(app.apply_query_completion(QueryCompletion {
        view_id: candidate.clone(),
        generation: query.generation,
        revision: query.revision,
        purpose: query.purpose,
        result: Ok(()),
    }));
    assert!(app.take_ready_forks().is_empty());
    assert_eq!(app.take_fork_discards(), vec![candidate]);
}
