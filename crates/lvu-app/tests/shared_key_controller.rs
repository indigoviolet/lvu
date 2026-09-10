use lvu_core::{ExactFieldConstraint, ExactScalar, RecordId, SourceId};
use uuid::Uuid;

#[path = "../src/shared_key_controller.rs"]
mod shared_key_controller;

use shared_key_controller::{
    MAX_SHARED_KEY_DIAGNOSTIC_BYTES, SharedKeyCompletion, SharedKeyController, SharedKeyOrigin,
};

fn origin(revision: u64, generation: u64) -> SharedKeyOrigin {
    SharedKeyOrigin {
        view_id: "accepted-input".into(),
        accepted_revision: revision,
        applied_generation: generation,
        record_id: RecordId {
            source_id: SourceId(Uuid::from_u128(1)),
            sequence: 7,
        },
        field: "request_key".into(),
    }
}

fn key(value: i64) -> ExactFieldConstraint {
    ExactFieldConstraint::new("request_key", ExactScalar::SignedInteger(value)).unwrap()
}

fn outputs() -> Vec<String> {
    vec!["request_key".into()]
}

#[test]
fn accepts_only_the_current_precise_origin_fence() {
    let mut controller = SharedKeyController::default();
    let generation = controller.begin(origin(11, 4), &outputs()).unwrap();
    let completion = controller.complete(
        generation,
        Some((11, 4)),
        Ok((key(42), "Int64".into(), true)),
    );
    assert!(matches!(completion, SharedKeyCompletion::Accepted(_)));
    let proven = controller.last_proven().unwrap();
    assert_eq!(proven.constraint, key(42));
    assert_eq!(proven.native_dtype, "Int64");
    assert_eq!(proven.origin.record_id.sequence, 7);
}

#[test]
fn stale_missing_and_invalid_keys_preserve_last_good() {
    let mut controller = SharedKeyController::default();
    let first = controller.begin(origin(1, 2), &outputs()).unwrap();
    controller.complete(first, Some((1, 2)), Ok((key(41), "Int64".into(), true)));

    let stale = controller.begin(origin(2, 2), &outputs()).unwrap();
    assert_eq!(
        controller.complete(stale, Some((3, 2)), Ok((key(42), "Int64".into(), true))),
        SharedKeyCompletion::Stale
    );
    assert_eq!(controller.last_proven().unwrap().constraint, key(41));

    let missing = controller.begin(origin(3, 2), &outputs()).unwrap();
    assert!(matches!(
        controller.complete(missing, Some((3, 2)), Err("key is missing".into())),
        SharedKeyCompletion::Rejected(_)
    ));
    assert_eq!(controller.last_proven().unwrap().constraint, key(41));

    let null = controller.begin(origin(3, 2), &outputs()).unwrap();
    let null_key = ExactFieldConstraint::new("request_key", ExactScalar::Null).unwrap();
    assert!(matches!(
        controller.complete(null, Some((3, 2)), Ok((null_key, "Int64".into(), true))),
        SharedKeyCompletion::Rejected(_)
    ));
    assert_eq!(controller.last_proven().unwrap().constraint, key(41));
}

#[test]
fn superseded_completion_is_ignored_and_diagnostic_is_bounded() {
    let mut controller = SharedKeyController::default();
    let old = controller.begin(origin(1, 1), &outputs()).unwrap();
    let current = controller.begin(origin(1, 1), &outputs()).unwrap();
    assert_eq!(
        controller.complete(old, Some((1, 1)), Ok((key(9), "Int64".into(), true))),
        SharedKeyCompletion::Ignored
    );
    assert_eq!(controller.pending().unwrap().0, current);

    let long = "é".repeat(MAX_SHARED_KEY_DIAGNOSTIC_BYTES);
    assert!(matches!(
        controller.complete(current, Some((1, 1)), Err(long)),
        SharedKeyCompletion::Rejected(_)
    ));
    let error = controller.error().unwrap();
    assert!(error.len() <= MAX_SHARED_KEY_DIAGNOSTIC_BYTES);
    assert!(error.is_char_boundary(error.len()));
}

#[test]
fn mismatched_field_or_missing_dtype_evidence_rejects() {
    let mut controller = SharedKeyController::default();
    let generation = controller.begin(origin(1, 1), &outputs()).unwrap();
    let wrong = ExactFieldConstraint::new("other", ExactScalar::Bool(true)).unwrap();
    assert!(matches!(
        controller.complete(
            generation,
            Some((1, 1)),
            Ok((wrong, "Boolean".into(), true))
        ),
        SharedKeyCompletion::Rejected(_)
    ));

    let generation = controller.begin(origin(1, 1), &outputs()).unwrap();
    assert!(matches!(
        controller.complete(generation, Some((1, 1)), Ok((key(1), String::new(), true))),
        SharedKeyCompletion::Rejected(_)
    ));
}

#[test]
fn raw_same_name_and_unready_derived_value_have_no_authority() {
    let mut controller = SharedKeyController::default();
    assert!(controller.begin(origin(1, 1), &[]).is_err());
    assert!(controller.pending().is_none());

    let generation = controller.begin(origin(1, 1), &outputs()).unwrap();
    assert!(matches!(
        controller.complete(
            generation,
            Some((1, 1)),
            Ok((key(42), "Int64".into(), false))
        ),
        SharedKeyCompletion::Rejected(_)
    ));
    assert!(controller.last_proven().is_none());
}

#[test]
fn cancel_drops_only_pending_resolution() {
    let mut controller = SharedKeyController::default();
    let first = controller.begin(origin(1, 1), &outputs()).unwrap();
    controller.complete(first, Some((1, 1)), Ok((key(42), "Int64".into(), true)));
    controller.begin(origin(2, 1), &outputs()).unwrap();
    controller.cancel();
    assert!(controller.pending().is_none());
    assert!(controller.error().is_none());
    assert_eq!(controller.last_proven().unwrap().constraint, key(42));
}
