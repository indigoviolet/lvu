//! The shell's side of whole-view field statistics: what it asks for, what it
//! keeps, and what it refuses to show.
//!
//! The pass itself is `lvu-view`'s. What is tested here is the contract the
//! dialog depends on: one question outstanding at a time, an answer to a
//! superseded question dropped, and figures never shown against a field they do
//! not describe.

use lvu::{
    App, FieldStatsRequest, WholeViewStats, field_stats::ValueType, fixture::FixtureProvider,
};

fn app() -> App {
    let (_, sources, views) = FixtureProvider::demo();
    App::new(sources, views, true)
}

fn answer(generation: u64, view_id: &str, path: &str) -> WholeViewStats {
    WholeViewStats {
        generation,
        view_id: view_id.to_owned(),
        path: path.to_owned(),
        records: 619_272,
        present: 619_000,
        matching: 618_000,
        distinct: 4,
        distinct_capped: false,
        top: vec![("200".into(), 500_000), ("404".into(), 100_000)],
        minimum: Some("200".into()),
        maximum: Some("503".into()),
    }
}

#[test]
fn asking_queues_one_question_and_reports_it_pending() {
    let mut app = app();
    assert!(!app.field_stats_pending());
    app.request_field_stats("view", "http.status", ValueType::Integer);
    assert!(app.field_stats_pending());
    let requests = app.take_field_stats_requests();
    assert_eq!(
        requests,
        vec![FieldStatsRequest::Resolve {
            generation: 1,
            view_id: "view".into(),
            path: "http.status".into(),
            kind: ValueType::Integer,
        }]
    );
    assert!(app.take_field_stats_requests().is_empty(), "drained once");
}

#[test]
fn moving_to_another_field_cancels_the_question_it_replaces() {
    let mut app = app();
    app.request_field_stats("view", "http.status", ValueType::Integer);
    let _ = app.take_field_stats_requests();
    app.request_field_stats("view", "service", ValueType::String);
    let requests = app.take_field_stats_requests();
    assert_eq!(
        requests,
        vec![
            FieldStatsRequest::Cancel { generation: 1 },
            FieldStatsRequest::Resolve {
                generation: 2,
                view_id: "view".into(),
                path: "service".into(),
                kind: ValueType::String,
            },
        ],
        "the pass still running is cancelled before the next is asked for"
    );
}

#[test]
fn an_answer_to_a_superseded_question_is_dropped() {
    let mut app = app();
    app.request_field_stats("view", "http.status", ValueType::Integer);
    app.request_field_stats("view", "service", ValueType::String);
    let _ = app.take_field_stats_requests();
    // The first pass finishes late, after the selection has moved.
    app.finish_field_stats(Ok(answer(1, "view", "http.status")));
    assert!(
        app.whole_view_stats("view", "http.status").is_none(),
        "a late answer about the field the user left is not shown"
    );
    assert!(app.field_stats_pending(), "the live question is still out");
    app.finish_field_stats(Ok(answer(2, "view", "service")));
    assert!(!app.field_stats_pending());
    assert!(app.whole_view_stats("view", "service").is_some());
}

#[test]
fn figures_are_only_shown_against_the_field_they_describe() {
    let mut app = app();
    app.request_field_stats("view", "http.status", ValueType::Integer);
    let _ = app.take_field_stats_requests();
    app.finish_field_stats(Ok(answer(1, "view", "http.status")));
    assert!(app.whole_view_stats("view", "http.status").is_some());
    assert!(
        app.whole_view_stats("view", "service").is_none(),
        "another path is another question"
    );
    assert!(
        app.whole_view_stats("other", "http.status").is_none(),
        "another view is another question"
    );
}

#[test]
fn a_failed_pass_leaves_the_sample_showing_and_says_no_more() {
    let mut app = app();
    app.request_field_stats("view", "http.status", ValueType::Integer);
    let _ = app.take_field_stats_requests();
    app.finish_field_stats(Err((1, "membership went away".into())));
    assert!(
        !app.field_stats_pending(),
        "the question is answered, badly"
    );
    assert!(
        app.whole_view_stats("view", "http.status").is_none(),
        "nothing replaces the sample the pane already shows"
    );
}

#[test]
fn cancelling_withdraws_the_question_and_the_answer() {
    let mut app = app();
    app.request_field_stats("view", "http.status", ValueType::Integer);
    let _ = app.take_field_stats_requests();
    app.finish_field_stats(Ok(answer(1, "view", "http.status")));
    assert!(app.whole_view_stats("view", "http.status").is_some());
    app.cancel_field_stats();
    assert!(!app.field_stats_pending());
    assert!(
        app.whole_view_stats("view", "http.status").is_none(),
        "closing the dialog drops figures nobody is looking at"
    );
}

#[test]
fn cancelling_an_outstanding_question_tells_the_worker() {
    let mut app = app();
    app.request_field_stats("view", "http.status", ValueType::Integer);
    let _ = app.take_field_stats_requests();
    app.cancel_field_stats();
    assert_eq!(
        app.take_field_stats_requests(),
        vec![FieldStatsRequest::Cancel { generation: 1 }]
    );
}
