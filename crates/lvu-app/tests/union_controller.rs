//! Union controller fencing: stale candidates fail, priors survive.
//!
//! Path-includes `crates/lvu-app/src/union_controller.rs` following the
//! `tests/settings.rs` precedent, so no worker or manifest change is needed
//! before the primary assigns the composition-tick hooks.

#[path = "../src/union_controller.rs"]
#[allow(dead_code)]
mod union_controller;

use std::collections::HashMap;
use union_controller::{
    StoredUnion, StoredUnionInput, UnionCandidate, UnionController, UnionControllerError,
};

fn revisions(pairs: &[(&str, u64)]) -> HashMap<String, u64> {
    pairs
        .iter()
        .map(|(view, revision)| ((*view).to_owned(), *revision))
        .collect()
}

fn current(map: &HashMap<String, u64>) -> impl Fn(&str) -> Option<u64> + '_ {
    move |view| map.get(view).copied()
}

fn candidate(view: &str, revision: u64, inputs: &[(&str, u64)]) -> UnionCandidate {
    UnionCandidate {
        union_view_id: view.into(),
        union_revision: revision,
        generation: 1,
        inputs: inputs
            .iter()
            .map(|(id, rev)| StoredUnionInput {
                view_id: (*id).into(),
                accepted_revision: *rev,
            })
            .collect(),
    }
}

#[test]
fn publish_fences_every_input_and_serves_the_union() {
    let map = revisions(&[("view-a", 3), ("view-b", 5)]);
    let mut controller = UnionController::default();
    controller.propose(candidate("union", 1, &[("view-a", 3), ("view-b", 5)]));
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
    let map = revisions(&[("view-a", 3), ("view-b", 5)]);
    let mut controller = UnionController::default();
    controller.propose(candidate("union", 1, &[("view-a", 3), ("view-b", 5)]));
    controller
        .note_union_published("union", current(&map))
        .unwrap();
    // An input advances while the next candidate is in flight.
    controller.propose(candidate("union", 2, &[("view-a", 3), ("view-b", 5)]));
    let moved = revisions(&[("view-a", 4), ("view-b", 5)]);
    let error = controller
        .note_union_published("union", current(&moved))
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
    let map = revisions(&[("view-a", 3)]);
    let mut controller = UnionController::default();
    controller.propose(candidate("union", 1, &[("view-a", 3), ("view-gone", 1)]));
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
    let map = revisions(&[("view-a", 3), ("view-b", 5)]);
    let mut controller = UnionController::default();
    controller.propose(candidate("union", 1, &[("view-a", 3), ("view-b", 5)]));
    controller
        .note_union_published("union", current(&map))
        .unwrap();
    controller.propose(candidate("union", 2, &[("view-a", 3), ("view-b", 5)]));
    controller.reject("union");
    controller.reject("union");
    assert_eq!(controller.accepted("union").unwrap().revision, 1);
}

#[test]
fn input_advance_requests_refresh_only_for_dependent_unions() {
    let map = revisions(&[("view-a", 3), ("view-b", 5), ("view-c", 1)]);
    let mut controller = UnionController::default();
    controller.propose(candidate("union-one", 1, &[("view-a", 3), ("view-b", 5)]));
    controller
        .note_union_published("union-one", current(&map))
        .unwrap();
    controller.propose(candidate("union-two", 1, &[("view-c", 1), ("view-b", 5)]));
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
        StoredUnion {
            inputs: vec![
                StoredUnionInput {
                    view_id: "view-a".into(),
                    accepted_revision: 9,
                },
                StoredUnionInput {
                    view_id: "view-b".into(),
                    accepted_revision: 2,
                },
            ],
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
fn stored_shape_matches_view_layer_json() {
    // The controller and view persistence shapes must stay in lockstep: the
    // same JSON key carries both. Field-for-field JSON equality, not parallel
    // structs drifting apart.
    let stored = StoredUnion {
        inputs: vec![StoredUnionInput {
            view_id: "view-a".into(),
            accepted_revision: 3,
        }],
    };
    let json = serde_json::to_string(&stored).unwrap();
    assert_eq!(
        json,
        r#"{"inputs":[{"view_id":"view-a","accepted_revision":3}]}"#
    );
    let legacy: StoredUnion = serde_json::from_str("{}").unwrap();
    assert!(legacy.inputs.is_empty());
}
