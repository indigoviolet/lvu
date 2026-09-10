//! New -> shipped-old read/run/save -> new compatibility for colour rules.
//!
//! A shipped old binary knows `color_rules` entries as `{predicate, color}`
//! only. This simulates that reader purely through serde shapes: parse a new
//! save while dropping unknown keys (old read), re-serialize the old shape
//! (old save), restore with new code. Classifier loss across the downgrade
//! is allowed; what is required is that the underlying view and the legacy
//! rules stay valid and no empty active predicate ever appears.

use lvu_memory::*;
use serde::{Deserialize, Serialize};

/// The colour-rule shape a shipped old binary understands.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct OldColorRule {
    predicate: String,
    color: String,
}

/// The stored view shape a shipped old binary understands: the legacy-only
/// rule list. Unknown keys are ignored on read and absent on save — there is
/// deliberately no flatten remainder, which would round-trip what a real old
/// binary drops.
#[derive(Clone, Debug, Deserialize)]
struct OldPresentation {
    #[serde(default)]
    color_rules: Vec<OldColorRule>,
}

/// A new save carries classifiers beside a legacy-only rule list, and the
/// legacy list never contains an empty predicate standing in for one.
#[test]
fn new_save_keeps_a_valid_legacy_projection_beside_classifiers() {
    let state = PresentationState {
        color_rules: vec![StoredColorRule {
            predicate: "level: ERROR".into(),
            color: "red".into(),
        }],
        color_classifiers: vec![StoredColorClassifier {
            position: 1,
            column: "severity".into(),
            value: Some("ERROR".into()),
            color: "green".into(),
        }],
        ..PresentationState::default()
    };
    let stored = serde_json::to_vec(&state).unwrap();
    let old: OldPresentation = serde_json::from_slice(&stored).unwrap();
    assert_eq!(old.color_rules.len(), 1);
    assert_eq!(old.color_rules[0].predicate, "level: ERROR");
    // The core guarantee: nothing the old binary reads here is an empty
    // active predicate, so it can neither fail on it nor save one.
    assert!(
        old.color_rules
            .iter()
            .all(|rule| !rule.predicate.trim().is_empty()),
        "old readers must never see an empty predicate: {:?}",
        old.color_rules
    );
}

/// Old read/save drops the sibling classifiers; the new restore keeps a
/// valid view with its legacy rules and no empty active predicate.
/// Classifier loss is the allowed, documented cost.
#[test]
fn old_round_trip_preserves_view_and_legacy_rules_without_empty_predicates() {
    let state = PresentationState {
        color_rules: vec![
            StoredColorRule {
                predicate: "level: ERROR".into(),
                color: "red".into(),
            },
            StoredColorRule {
                predicate: "ready".into(),
                color: "blue".into(),
            },
        ],
        color_classifiers: vec![StoredColorClassifier {
            position: 0,
            column: "severity".into(),
            value: Some("ERROR".into()),
            color: "green".into(),
        }],
        ..PresentationState::default()
    };
    // New save -> old read -> old save (unknown keys dropped: emulate the
    // old save by removing the sibling key the old struct never knew).
    let stored = serde_json::to_vec(&state).unwrap();
    let old: OldPresentation = serde_json::from_slice(&stored).unwrap();
    assert_eq!(old.color_rules.len(), 2);
    let mut old_saved: serde_json::Value = serde_json::from_slice(&stored).unwrap();
    old_saved
        .as_object_mut()
        .expect("stored presentation is an object")
        .remove("color_classifiers");
    // The old binary runs and saves: legacy rules evaluate as before (a
    // real engine check lives in lvu-view); here the save must round-trip.
    let resaved = serde_json::to_vec(&old_saved).unwrap();
    // Old save -> new read.
    let restored: PresentationState = serde_json::from_slice(&resaved).unwrap();
    let predicates: Vec<&str> = restored
        .color_rules
        .iter()
        .map(|rule| rule.predicate.as_str())
        .collect();
    assert_eq!(predicates, vec!["level: ERROR", "ready"]);
    assert!(
        restored.color_classifiers.is_empty(),
        "classifiers are allowed to be lost, nothing else may be"
    );
    assert!(
        restored
            .color_rules
            .iter()
            .all(|rule| !rule.predicate.trim().is_empty()),
        "no empty active predicate after the downgrade round trip"
    );
}

/// Rows written before classifiers existed restore legacy-only, unchanged.
#[test]
fn legacy_rows_restore_without_classifiers() {
    let stored = serde_json::json!({
        "color_rules": [{"predicate": "timeout", "color": "yellow"}],
    });
    let restored: PresentationState =
        serde_json::from_value(stored).expect("legacy shape restores");
    assert_eq!(restored.color_rules.len(), 1);
    assert_eq!(restored.color_rules[0].predicate, "timeout");
    assert!(restored.color_classifiers.is_empty());
}
