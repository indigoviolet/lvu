//! Union dialog draft transitions.
//!
//! Path-includes `crates/lvu/src/components/union.rs` so no shell, layer or
//! manifest change is needed before the primary assigns the registration
//! hooks. These pin the draft/accepted separation: rejecting a candidate
//! preserves the last-good union, staleness is a refresh signal, and the
//! dialog never invents input revisions.

#[path = "../src/components/union.rs"]
#[allow(dead_code)]
mod union_dialog;

use std::collections::HashMap;
use union_dialog::{AcceptedUnion, UnionDialog};

/// Accepted (revision, generation) per input view.
fn states(pairs: &[(&str, u64, u64)]) -> HashMap<String, (u64, u64)> {
    pairs
        .iter()
        .map(|(view, revision, generation)| ((*view).to_owned(), (*revision, *generation)))
        .collect()
}

fn current_revision(map: &HashMap<String, (u64, u64)>) -> impl Fn(&str) -> Option<u64> + '_ {
    move |view| map.get(view).map(|state| state.0)
}

fn current_generation(map: &HashMap<String, (u64, u64)>) -> impl Fn(&str) -> Option<u64> + '_ {
    move |view| map.get(view).map(|state| state.1)
}

fn draft_with(inputs: &[&str]) -> UnionDialog {
    let mut dialog = UnionDialog::new();
    for input in inputs {
        dialog.add_input(*input).unwrap();
    }
    dialog
}

#[test]
fn draft_add_remove_bounds_and_duplicates() {
    let mut dialog = UnionDialog::new();
    assert!(dialog.validate_for_create("union").is_err());
    dialog.add_input("view-a").unwrap();
    assert!(dialog.validate_for_create("union").is_err());
    assert_eq!(
        dialog.add_input("view-a").unwrap_err(),
        "'view-a' is already an input of this union"
    );
    dialog.add_input("view-b").unwrap();
    assert!(dialog.validate_for_create("union").is_ok());
    assert!(dialog.remove_input("view-a"));
    assert!(!dialog.remove_input("view-a"));
    assert_eq!(dialog.inputs(), &["view-b".to_owned()]);
}

#[test]
fn draft_rejects_self_reference() {
    let dialog = draft_with(&["view-a", "union"]);
    let error = dialog.validate_for_create("union").unwrap_err();
    assert!(error.contains("itself"), "{error}");
}

#[test]
fn accept_fences_on_current_revisions() {
    let map = states(&[("view-a", 3, 1), ("view-b", 5, 1)]);
    let mut dialog = draft_with(&["view-a", "view-b"]);
    let accepted = dialog
        .accept("union", 1, current_revision(&map), current_generation(&map))
        .unwrap();
    assert_eq!(
        accepted,
        AcceptedUnion {
            union_view_id: "union".into(),
            inputs: vec![
                union_dialog::UnionInputRef {
                    view_id: "view-a".into(),
                    accepted_revision: 3,
                    applied_generation: 1,
                },
                union_dialog::UnionInputRef {
                    view_id: "view-b".into(),
                    accepted_revision: 5,
                    applied_generation: 1,
                },
            ],
            revision: 1,
        }
    );
    assert_eq!(dialog.error(), None);
}

#[test]
fn accept_rejects_unavailable_inputs_and_keeps_the_draft() {
    let map = states(&[("view-a", 3, 1)]);
    let mut dialog = draft_with(&["view-a", "view-gone"]);
    let error = dialog
        .accept("union", 1, current_revision(&map), current_generation(&map))
        .unwrap_err();
    assert!(error.contains("view-gone"), "{error}");
    assert!(dialog.error().is_some());
    // The draft is intact for correction, not cleared by the failure.
    assert_eq!(
        dialog.inputs(),
        &["view-a".to_owned(), "view-gone".to_owned()]
    );
}

#[test]
fn rejection_preserves_accepted_and_stays_editable() {
    let map = states(&[("view-a", 3, 1), ("view-b", 5, 1)]);
    let mut dialog = draft_with(&["view-a", "view-b"]);
    let accepted = dialog
        .accept("union", 1, current_revision(&map), current_generation(&map))
        .unwrap();
    dialog.mark_pending(7);
    dialog.reject_candidate("engine refused the timestamp column");
    assert_eq!(dialog.error(), Some("engine refused the timestamp column"));
    assert_eq!(dialog.pending_generation(), None);
    // The accepted union the view keeps serving is untouched by the failure.
    assert!(!UnionDialog::accepted_is_stale(
        &accepted,
        current_revision(&map),
        current_generation(&map)
    ));
    let mut moved = map;
    moved.insert("view-a".into(), (4, 1));
    assert!(UnionDialog::accepted_is_stale(
        &accepted,
        current_revision(&moved),
        current_generation(&moved)
    ));
    // A generation move alone (restart at the same revision) is stale too.
    let mut restarted = states(&[("view-a", 3, 1), ("view-b", 5, 1)]);
    restarted.insert("view-b".into(), (5, 2));
    assert!(UnionDialog::accepted_is_stale(
        &accepted,
        current_revision(&restarted),
        current_generation(&restarted)
    ));
}

#[test]
fn dialog_cycle_check_rejects_transitive_loops() {
    let dialog = draft_with(&["union-b", "base"]);
    let resolve = |view: &str| match view {
        "union-b" => Some(vec!["union".to_owned()]),
        _ => None,
    };
    let error = dialog.validate_no_cycle("union", resolve).unwrap_err();
    assert!(error.contains("union"), "{error}");
    let resolve_ok = |_: &str| None;
    assert!(dialog.validate_no_cycle("union", resolve_ok).is_ok());
}

#[test]
fn double_submit_while_pending_is_refused() {
    let mut dialog = draft_with(&["view-a", "view-b"]);
    assert!(dialog.mark_pending(1));
    assert!(!dialog.mark_pending(2));
    assert_eq!(dialog.pending_generation(), Some(1));
}
