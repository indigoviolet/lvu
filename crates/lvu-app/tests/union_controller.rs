//! Union controller fencing: stale candidates fail, priors survive.
//!
//! Path-includes `crates/lvu-app/src/union_controller.rs` following the
//! `tests/settings.rs` precedent, so no worker or manifest change is needed
//! before the primary assigns the composition-tick hooks. Candidates and
//! persisted shapes are the single `lvu_view::union` definitions.

#[path = "../src/union_controller.rs"]
#[allow(dead_code)]
mod union_controller;

use lvu_view::union::{StoredUnionInput, StoredUnionShape, UnionCandidateSpec, UnionFilterSpec};
use std::collections::HashMap;
use union_controller::{UnionController, UnionControllerError};

/// Accepted (revision, generation) per input view.
fn states(pairs: &[(&str, u64, u64)]) -> HashMap<String, (u64, u64)> {
    pairs
        .iter()
        .map(|(view, revision, generation)| ((*view).to_owned(), (*revision, *generation)))
        .collect()
}

fn current(map: &HashMap<String, (u64, u64)>) -> impl Fn(&str) -> Option<(u64, u64)> + '_ {
    move |view| map.get(view).copied()
}

fn candidate(view: &str, revision: u64, inputs: &[(&str, u64, u64)]) -> UnionCandidateSpec {
    UnionCandidateSpec {
        union_view_id: view.into(),
        union_revision: revision,
        generation: 1,
        inputs: inputs
            .iter()
            .map(|(id, rev, generation)| StoredUnionInput {
                view_id: (*id).into(),
                accepted_revision: *rev,
                applied_generation: *generation,
            })
            .collect(),
        filter: UnionFilterSpec::default(),
        color_rules: Vec::new(),
    }
}

#[test]
fn publish_fences_every_input_and_serves_the_union() {
    let map = states(&[("view-a", 3, 1), ("view-b", 5, 1)]);
    let mut controller = UnionController::default();
    controller.propose(candidate("union", 1, &[("view-a", 3, 1), ("view-b", 5, 1)]));
    let published = controller
        .note_union_published("union", current(&map))
        .unwrap();
    assert_eq!(published.revision, 1);
    assert_eq!(published.inputs.len(), 2);
    assert!(controller.pending("union").is_none());
    let accepted = controller.accepted("union").unwrap();
    assert_eq!(accepted, &published);
    assert_eq!(
        controller.dependency_inputs("union").unwrap(),
        vec!["view-a".to_owned(), "view-b".to_owned()]
    );
}

#[test]
fn stale_candidate_is_rejected_and_prior_union_survives() {
    let map = states(&[("view-a", 3, 1), ("view-b", 5, 1)]);
    let mut controller = UnionController::default();
    controller.propose(candidate("union", 1, &[("view-a", 3, 1), ("view-b", 5, 1)]));
    controller
        .note_union_published("union", current(&map))
        .unwrap();
    // An input advances while the next candidate is in flight.
    controller.propose(candidate("union", 2, &[("view-a", 3, 1), ("view-b", 5, 1)]));
    let moved = states(&[("view-a", 4, 1), ("view-b", 5, 1)]);
    let error = controller
        .note_union_published("union", current(&moved))
        .unwrap_err();
    assert!(
        matches!(error, UnionControllerError::StaleCandidate { .. }),
        "{error:?}"
    );
    // A source restart bumps generation without touching revisions: still
    // stale, because the membership underneath changed identity.
    controller.propose(candidate("union", 2, &[("view-a", 3, 1), ("view-b", 5, 1)]));
    let restarted = states(&[("view-a", 3, 2), ("view-b", 5, 1)]);
    let error = controller
        .note_union_published("union", current(&restarted))
        .unwrap_err();
    assert!(
        matches!(error, UnionControllerError::StaleCandidate { .. }),
        "{error:?}"
    );
    // The prior accepted union keeps serving; nothing was half-published.
    let accepted = controller.accepted("union").unwrap();
    assert_eq!(accepted.revision, 1);
    assert!(controller.pending("union").is_none());
}

#[test]
fn missing_input_rejects_without_touching_accepted() {
    let map = states(&[("view-a", 3, 1)]);
    let mut controller = UnionController::default();
    controller.propose(candidate(
        "union",
        1,
        &[("view-a", 3, 1), ("view-gone", 1, 1)],
    ));
    let error = controller
        .note_union_published("union", current(&map))
        .unwrap_err();
    assert!(
        matches!(error, UnionControllerError::MissingInput { .. }),
        "{error:?}"
    );
    assert!(controller.accepted("union").is_none());
}

#[test]
fn runtime_failure_rejects_idempotently_and_preserves_accepted() {
    let map = states(&[("view-a", 3, 1), ("view-b", 5, 1)]);
    let mut controller = UnionController::default();
    controller.propose(candidate("union", 1, &[("view-a", 3, 1), ("view-b", 5, 1)]));
    controller
        .note_union_published("union", current(&map))
        .unwrap();
    controller.propose(candidate("union", 2, &[("view-a", 3, 1), ("view-b", 5, 1)]));
    controller.reject("union");
    controller.reject("union");
    assert_eq!(controller.accepted("union").unwrap().revision, 1);
}

#[test]
fn input_advance_requests_refresh_only_for_dependent_unions() {
    let map = states(&[("view-a", 3, 1), ("view-b", 5, 1), ("view-c", 1, 1)]);
    let mut controller = UnionController::default();
    controller.propose(candidate(
        "union-one",
        1,
        &[("view-a", 3, 1), ("view-b", 5, 1)],
    ));
    controller
        .note_union_published("union-one", current(&map))
        .unwrap();
    controller.propose(candidate(
        "union-two",
        1,
        &[("view-c", 1, 1), ("view-b", 5, 1)],
    ));
    controller
        .note_union_published("union-two", current(&map))
        .unwrap();
    let refreshes = controller.note_input_revision("view-a");
    assert_eq!(refreshes.len(), 1);
    assert_eq!(refreshes[0].union_view_id, "union-one");
    assert_eq!(refreshes[0].moved_view_id, "view-a");
    assert_eq!(controller.note_input_revision("view-unknown").len(), 0);
}

#[test]
fn restore_rehydrates_inputs_without_launching_anything() {
    let mut controller = UnionController::default();
    controller.restore(
        "union",
        4,
        StoredUnionShape {
            inputs: vec![
                StoredUnionInput {
                    view_id: "view-a".into(),
                    accepted_revision: 9,
                    applied_generation: 2,
                },
                StoredUnionInput {
                    view_id: "view-b".into(),
                    accepted_revision: 2,
                    applied_generation: 1,
                },
            ],
            filter: UnionFilterSpec::default(),
        },
    );
    // Restore installs a known baseline: an input at its stored revision is
    // fresh; anything newer is a refresh, never a failure.
    let accepted = controller.accepted("union").unwrap();
    assert_eq!(accepted.revision, 4);
    assert_eq!(accepted.inputs.len(), 2);
    assert_eq!(controller.note_input_revision("view-a").len(), 1);
    controller.forget("union");
    assert!(controller.accepted("union").is_none());
    assert!(controller.dependency_inputs("union").is_none());
}

#[test]
fn stored_shape_is_the_single_view_definition() {
    // No parallel DTOs: the controller persists exactly
    // `lvu_view::union::StoredUnionShape`, generation fence included, and old
    // rows without it read as generation 0 and refresh once.
    let stored = StoredUnionShape {
        inputs: vec![StoredUnionInput {
            view_id: "view-a".into(),
            accepted_revision: 3,
            applied_generation: 1,
        }],
        filter: UnionFilterSpec::default(),
    };
    let json = serde_json::to_string(&stored).unwrap();
    assert_eq!(
        json,
        r#"{"inputs":[{"view_id":"view-a","accepted_revision":3,"applied_generation":1}],"filter":{"search":""}}"#
    );
    let legacy: StoredUnionShape =
        serde_json::from_str(r#"{"inputs":[{"view_id":"view-a","accepted_revision":3}]}"#).unwrap();
    assert_eq!(legacy.inputs[0].applied_generation, 0);
    let empty: StoredUnionShape = serde_json::from_str("{}").unwrap();
    assert!(empty.inputs.is_empty());
}
