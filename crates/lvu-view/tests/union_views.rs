//! Union view semantics over accepted input views.
//!
//! Exercised against `crates/lvu-view/src/union.rs` by path include, so no
//! shared module or manifest changes are needed before the primary assigns
//! the registration hooks. Identity-level tests assert the deterministic
//! merge contract; typed tests execute real Polars and assert extracted typed
//! values — never string comparisons or truncated projections. The frozen
//! tests pin the authoritative entry point: typed JSON values plus native
//! dtype evidence from the frozen accepted evaluation, NOT the
//! `Membership.derived` display map (truncated strings).

#[path = "../src/union.rs"]
#[allow(dead_code)]
mod union;

use lvu_core::{RecordId, SourceId};
use polars::prelude::*;
use std::collections::BTreeMap;
use union::{
    INPUT_COLUMN, MergedUnionRow, SEQUENCE_COLUMN, SOURCE_ID_COLUMN, StoredUnionInput,
    StoredUnionShape, UNION_TS_COLUMN, UnionError, UnionFrozenInput, UnionFrozenRow, UnionInputRow,
    UnionInputSnapshot, UnionLimits, detect_union_cycle, frozen_identity_snapshot,
    merge_union_rows, union_frozen_inputs, union_input_stale, union_typed_frames,
    validate_union_spec,
};
use uuid::Uuid;

/// Deterministic canonical source identities: `Uuid::from_u128` needs no
/// randomness and orders numerically, so tiebreaks are reproducible.
fn source(n: u128) -> SourceId {
    SourceId(Uuid::from_u128(n))
}

fn row(source: u128, sequence: u64, timestamp_nanos: Option<i64>) -> UnionInputRow {
    UnionInputRow {
        id: RecordId {
            source_id: self::source(source),
            sequence,
        },
        timestamp_nanos,
    }
}

fn snapshot(view_id: &str, revision: u64, rows: Vec<UnionInputRow>) -> UnionInputSnapshot {
    UnionInputSnapshot {
        view_id: view_id.into(),
        accepted_revision: revision,
        rows,
    }
}

fn order(merged: &[MergedUnionRow]) -> Vec<(u128, u64)> {
    merged
        .iter()
        .map(|row| (row.id.source_id.0.as_u128(), row.id.sequence))
        .collect()
}

#[test]
fn out_of_order_times_merge_in_ts_order() {
    let api = snapshot(
        "view-api",
        3,
        vec![row(1, 1, Some(30)), row(1, 2, Some(10))],
    );
    let worker = snapshot(
        "view-worker",
        5,
        vec![row(2, 1, Some(20)), row(2, 2, Some(40))],
    );
    let merged = merge_union_rows(&[api, worker]).unwrap();
    assert_eq!(order(&merged), vec![(1, 2), (2, 1), (1, 1), (2, 2),]);
}

#[test]
fn ties_break_by_input_order_then_sequence_and_repeat_deterministically() {
    let first = snapshot(
        "view-one",
        1,
        vec![row(3, 2, Some(50)), row(3, 1, Some(50))],
    );
    let second = snapshot("view-two", 1, vec![row(4, 9, Some(50))]);
    let merged = merge_union_rows(&[first.clone(), second.clone()]).unwrap();
    // Equal times: input one's rows before input two's, then original
    // sequence within the input.
    assert_eq!(order(&merged), vec![(3, 1), (3, 2), (4, 9),]);
    assert_eq!(merged[0].input, 0);
    assert_eq!(merged[2].input, 1);
    // Restarting from the same snapshots reproduces the identical order.
    let again = merge_union_rows(&[first, second]).unwrap();
    assert_eq!(merged, again);
}

#[test]
fn missing_times_sort_last_without_dropping_records() {
    let timed = snapshot("view-timed", 2, vec![row(5, 1, Some(7))]);
    let untimed = snapshot("view-untimed", 2, vec![row(6, 1, None), row(6, 2, Some(3))]);
    let merged = merge_union_rows(&[timed, untimed]).unwrap();
    assert_eq!(order(&merged), vec![(6, 2), (5, 1), (6, 1),]);
    assert_eq!(merged.last().unwrap().timestamp_nanos, None);
}

#[test]
fn overlapping_identity_emits_once_with_first_input_precedence() {
    // The same stable RecordId in both inputs, with DIFFERING derived
    // projections (here the timestamp): the first input wins outright.
    let first = snapshot("view-one", 1, vec![row(7, 7, Some(100))]);
    let second = snapshot("view-two", 1, vec![row(7, 7, Some(10))]);
    let merged = merge_union_rows(&[first, second]).unwrap();
    assert_eq!(merged.len(), 1);
    assert_eq!(
        merged[0].id,
        RecordId {
            source_id: source(7),
            sequence: 7
        }
    );
    assert_eq!(merged[0].input, 0);
    assert_eq!(merged[0].timestamp_nanos, Some(100));
}

#[test]
fn spec_validation_rejects_empty_duplicate_self_and_oversized() {
    assert_eq!(
        validate_union_spec("union", &[]),
        Err(UnionError::EmptyInputs)
    );
    assert_eq!(
        validate_union_spec("union", &["only-one"]),
        Err(UnionError::EmptyInputs)
    );
    assert_eq!(
        validate_union_spec("union", &["a", "a"]),
        Err(UnionError::DuplicateInput {
            view_id: "a".into()
        })
    );
    assert_eq!(
        validate_union_spec("union", &["a", "union"]),
        Err(UnionError::SelfReference {
            view_id: "union".into()
        })
    );
    let many: Vec<String> = (0..20).map(|index| format!("view-{index}")).collect();
    let many_refs: Vec<&str> = many.iter().map(String::as_str).collect();
    assert!(matches!(
        validate_union_spec("union", &many_refs),
        Err(UnionError::TooManyInputs { .. })
    ));
    assert!(validate_union_spec("union", &["a", "b"]).is_ok());
}

#[test]
fn transitive_cycles_are_rejected_with_a_path() {
    // union-c = [union-b], union-b = [union-a], union-a = [union-c]: creating
    // union-c over union-b must fail.
    let resolve = |view: &str| match view {
        "union-b" => Some(vec!["union-a".to_owned()]),
        "union-a" => Some(vec!["union-c".to_owned()]),
        _ => None,
    };
    let error = detect_union_cycle("union-c", &["union-b"], resolve).unwrap_err();
    assert!(matches!(error, UnionError::UnionCycle { .. }), "{error:?}");
    // A cycle between older unions is reported even though the new union is
    // not on it: creating anything over a cyclic graph must fail.
    let resolve_old = |view: &str| match view {
        "union-a" => Some(vec!["union-b".to_owned()]),
        "union-b" => Some(vec!["union-a".to_owned()]),
        _ => None,
    };
    let error = detect_union_cycle("union-new", &["union-a"], resolve_old).unwrap_err();
    match error {
        UnionError::UnionCycle { path } => {
            assert!(
                path.contains("union-a") && path.contains("union-b"),
                "{path}"
            );
        }
        other => panic!("expected a cycle path, got {other:?}"),
    }
    // A stored union wider than the bound is an invalid graph, not a prefix
    // to validate: fail closed, never truncate edges.
    let wide: Vec<String> = (0..20).map(|index| format!("old-{index}")).collect();
    let resolve_wide = |view: &str| match view {
        "union-wide" => Some(wide.clone()),
        _ => None,
    };
    assert!(matches!(
        detect_union_cycle("union-new", &["union-wide"], resolve_wide),
        Err(UnionError::TooManyInputs { .. })
    ));
    // An acyclic diamond is fine.
    let resolve_ok = |view: &str| match view {
        "union-b" => Some(vec!["base-one".to_owned()]),
        _ => None,
    };
    assert!(detect_union_cycle("union-c", &["union-b", "base-two"], resolve_ok).is_ok());
}

#[test]
fn row_limit_fences_before_any_merge_work() {
    let rows: Vec<UnionInputRow> = (0..10).map(|i| row(7, i, Some(i as i64))).collect();
    // Ten rows are fine; the fence is about the bound, exercised here by
    // asserting small inputs pass and the error carries real counts.
    let merged = merge_union_rows(&[
        snapshot("a", 1, rows[..5].to_vec()),
        snapshot("b", 1, rows[5..].to_vec()),
    ])
    .unwrap();
    assert_eq!(merged.len(), 10);
}

#[test]
fn live_append_remerges_with_prior_order_preserved() {
    let api = snapshot("view-api", 1, vec![row(1, 1, Some(10))]);
    let worker = snapshot("view-worker", 1, vec![row(2, 1, Some(30))]);
    let before = merge_union_rows(&[api.clone(), worker.clone()]).unwrap();
    assert_eq!(order(&before), vec![(1, 1), (2, 1)]);
    // A live append lands in the middle of the merged order.
    let mut api_grown = api;
    api_grown.rows.push(row(1, 2, Some(20)));
    api_grown.accepted_revision = 2;
    assert!(union_input_stale(1, api_grown.accepted_revision));
    let after = merge_union_rows(&[api_grown, worker]).unwrap();
    assert_eq!(order(&after), vec![(1, 1), (1, 2), (2, 1),]);
    // The previously published rows keep their relative order.
    let before_ids = order(&before);
    let after_ids = order(&after);
    assert!(before_ids.iter().all(|id| after_ids.contains(id)));
}

#[test]
fn stale_fencing_compares_fenced_and_current_revisions() {
    assert!(!union_input_stale(4, 4));
    assert!(union_input_stale(4, 5));
}

fn u64_at(frame: &DataFrame, name: &str, index: usize) -> u64 {
    frame
        .column(name)
        .unwrap()
        .get(index)
        .unwrap()
        .try_extract::<u64>()
        .unwrap()
}

fn i64_opt_at(frame: &DataFrame, name: &str, index: usize) -> Option<i64> {
    match frame.column(name).unwrap().get(index).unwrap() {
        AnyValue::Null => None,
        value => Some(value.try_extract::<i64>().unwrap()),
    }
}

fn str_at(frame: &DataFrame, name: &str, index: usize) -> String {
    frame
        .column(name)
        .unwrap()
        .get(index)
        .unwrap()
        .str_value()
        .to_string()
}

fn typed_frame(
    source: &str,
    sequences: &[u64],
    times: &[Option<i64>],
    levels: &[&str],
    counts: Option<&[i64]>,
) -> DataFrame {
    let height = sequences.len();
    let mut columns = vec![
        Column::new("source".into(), vec![source; height]),
        Column::new("seq".into(), sequences.to_vec()),
        Column::new("ts".into(), times.to_vec()),
        Column::new(
            "level".into(),
            levels.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        ),
    ];
    if let Some(counts) = counts {
        columns.push(Column::new("n".into(), counts.to_vec()));
    }
    DataFrame::new_infer_height(columns).unwrap()
}

#[test]
fn typed_union_aligns_schemas_with_nulls_in_ts_order() {
    // Input two lacks `n`: diagonal union keeps the column with nulls rather
    // than failing or dropping rows. Values asserted as typed extractions.
    let api = typed_frame(
        "api",
        &[1, 2],
        &[Some(30), Some(10)],
        &["info", "error"],
        Some(&[1, 2]),
    );
    let worker = typed_frame(
        "worker",
        &[1, 2],
        &[Some(20), Some(40)],
        &["warn", "info"],
        None,
    );
    let merged = union_typed_frames(vec![api, worker], "ts", "source", "seq").unwrap();
    assert_eq!(merged.height(), 4);
    assert_eq!(merged.column("n").unwrap().null_count(), 2);
    // ts order: 10 (api/2), 20 (worker/1), 30 (api/1), 40 (worker/2).
    let sequences: Vec<u64> = (0..4).map(|i| u64_at(&merged, "seq", i)).collect();
    assert_eq!(sequences, vec![2, 1, 1, 2]);
    let sources: Vec<String> = (0..4).map(|i| str_at(&merged, "source", i)).collect();
    assert_eq!(sources, vec!["api", "worker", "api", "worker"]);
    let counts: Vec<Option<i64>> = (0..4).map(|i| i64_opt_at(&merged, "n", i)).collect();
    assert_eq!(counts, vec![Some(2), None, Some(1), None]);
    let inputs: Vec<u64> = (0..4).map(|i| u64_at(&merged, INPUT_COLUMN, i)).collect();
    assert_eq!(inputs, vec![0, 1, 0, 1]);
}

#[test]
fn typed_union_sorts_null_times_last_and_breaks_ties_by_input() {
    let first = typed_frame("s1", &[1, 2], &[None, Some(5)], &["a", "b"], None);
    let second = typed_frame("s2", &[1, 2], &[Some(5), None], &["c", "d"], None);
    let merged = union_typed_frames(vec![first, second], "ts", "source", "seq").unwrap();
    let pairs: Vec<(String, u64)> = (0..4)
        .map(|i| (str_at(&merged, "source", i), u64_at(&merged, "seq", i)))
        .collect();
    assert_eq!(
        pairs,
        vec![
            ("s1".to_owned(), 2),
            ("s2".to_owned(), 1),
            ("s1".to_owned(), 1),
            ("s2".to_owned(), 2),
        ]
    );
}

#[test]
fn typed_union_dedups_identity_with_first_input_projection() {
    // Same record, disagreeing derived projections AND disagreeing times: the
    // first input's row (level "alpha" at ts 100) survives, not the earlier
    // timestamp's row.
    let first = typed_frame("s", &[7], &[Some(100)], &["alpha"], Some(&[1]));
    let second = typed_frame("s", &[7], &[Some(50)], &["beta"], Some(&[2]));
    let merged = union_typed_frames(vec![first, second], "ts", "source", "seq").unwrap();
    assert_eq!(merged.height(), 1);
    assert_eq!(str_at(&merged, "level", 0), "alpha");
    assert_eq!(i64_opt_at(&merged, "ts", 0), Some(100));
    assert_eq!(i64_opt_at(&merged, "n", 0), Some(1));
    assert_eq!(u64_at(&merged, INPUT_COLUMN, 0), 0);
}

#[test]
fn typed_union_rejects_conflicting_dtypes_and_protected_tags() {
    // Same column name, different dtypes across inputs: the candidate fails
    // (normalization belongs upstream in enrichment) instead of coercing.
    let ints = typed_frame("a", &[1], &[Some(1)], &["x"], Some(&[1]));
    let mut texts = typed_frame("b", &[1], &[Some(2)], &["y"], None);
    texts
        .with_column(Column::new("n".into(), vec!["not-a-number".to_string()]))
        .unwrap();
    let error = union_typed_frames(vec![ints, texts], "ts", "source", "seq").unwrap_err();
    assert!(matches!(error, UnionError::Engine { .. }), "{error:?}");

    // A frame smuggling the provenance tag is rejected, not merged.
    let mut smuggled = typed_frame("a", &[1], &[Some(1)], &["x"], None);
    smuggled
        .with_column(Column::new(INPUT_COLUMN.into(), vec![99u32]))
        .unwrap();
    let clean = typed_frame("b", &[1], &[Some(2)], &["y"], None);
    let error = union_typed_frames(vec![smuggled, clean], "ts", "source", "seq").unwrap_err();
    assert_eq!(
        error,
        UnionError::ProtectedColumn {
            name: INPUT_COLUMN.into()
        }
    );

    // No timestamp column anywhere is an actionable rejection.
    let no_ts = DataFrame::new_infer_height(vec![
        Column::new("source".into(), vec!["a"]),
        Column::new("seq".into(), vec![1u64]),
    ])
    .unwrap();
    let error = union_typed_frames(vec![no_ts.clone(), no_ts], "ts", "source", "seq").unwrap_err();
    assert!(
        matches!(error, UnionError::BadTimestampColumn { .. }),
        "{error:?}"
    );
}

#[test]
fn stored_shape_round_trips_additively() {
    // Persistence contract: serde defaults everywhere, unknown fields ignored,
    // so no schema bump is needed to carry union inputs.
    let shape = StoredUnionShape {
        inputs: vec![
            StoredUnionInput {
                view_id: "view-api".into(),
                accepted_revision: 3,
                applied_generation: 1,
            },
            StoredUnionInput {
                view_id: "view-worker".into(),
                accepted_revision: 5,
                applied_generation: 1,
            },
        ],
    };
    let json = serde_json::to_string(&shape).unwrap();
    let back: StoredUnionShape = serde_json::from_str(&json).unwrap();
    assert_eq!(shape, back);
    let legacy: StoredUnionShape = serde_json::from_str("{}").unwrap();
    assert!(legacy.inputs.is_empty());
    let future: StoredUnionShape =
        serde_json::from_str(r#"{"inputs":[],"union_v99":{"x":1}}"#).unwrap();
    assert!(future.inputs.is_empty());
}

fn frozen_row(
    source: u128,
    sequence: u64,
    timestamp_nanos: Option<i64>,
    typed: &[(&str, &str, serde_json::Value)],
) -> UnionFrozenRow {
    UnionFrozenRow {
        record_id: RecordId {
            source_id: self::source(source),
            sequence,
        },
        timestamp_nanos,
        fields: typed
            .iter()
            .map(|(name, _, value)| ((*name).to_owned(), value.clone()))
            .collect::<BTreeMap<_, _>>(),
        // Mirrors the frozen replay's native dtype evidence
        // (`format!("{:?}")` spellings): the decoder's authority.
        field_types: typed
            .iter()
            .map(|(name, dtype, _)| ((*name).to_owned(), (*dtype).to_owned()))
            .collect::<BTreeMap<_, _>>(),
    }
}

fn frozen_input(view_id: &str, revision: u64, rows: Vec<UnionFrozenRow>) -> UnionFrozenInput {
    UnionFrozenInput {
        view_id: view_id.into(),
        applied_revision: revision,
        applied_generation: 1,
        timestamp_column: "ts".into(),
        rows,
    }
}

fn limits() -> UnionLimits {
    UnionLimits::default()
}

#[test]
fn frozen_values_keep_dtype_and_full_length() {
    use serde_json::json;
    // The display map would truncate this to its byte bound and stringify the
    // numbers; the frozen path preserves both exactly under dtype authority.
    let long = "x".repeat(600);
    let api = frozen_input(
        "view-api",
        3,
        vec![
            frozen_row(
                1,
                1,
                Some(30),
                &[("n", "Int64", json!(1000)), ("msg", "String", json!(long))],
            ),
            frozen_row(
                1,
                2,
                Some(10),
                &[("n", "Int64", json!(9)), ("msg", "String", json!("short"))],
            ),
        ],
    );
    let worker = frozen_input(
        "view-worker",
        5,
        vec![frozen_row(2, 1, Some(20), &[("n", "Int64", json!(80))])],
    );
    let merged = union_frozen_inputs("union", &[api, worker], &limits()).unwrap();
    assert_eq!(merged.height(), 3);
    assert_eq!(merged.column("n").unwrap().dtype(), &DataType::Int64);
    // ts order: 10 (n=9), 20 (n=80), 30 (n=1000).
    let values: Vec<i64> = (0..3)
        .map(|i| i64_opt_at(&merged, "n", i).unwrap())
        .collect();
    assert_eq!(values, vec![9, 80, 1000]);
    assert_eq!(str_at(&merged, "msg", 2).len(), 600);
    // A typed Polars filter over the union sees numbers, not text: n > 10
    // matches 80 and 1000, where a lexical comparison would also match "9".
    let filtered = merged
        .lazy()
        .filter(col("n").gt(lit(10)))
        .collect()
        .unwrap();
    assert_eq!(filtered.height(), 2);
}

#[test]
fn frozen_unsupported_values_reject_explicitly() {
    use serde_json::json;
    // A temporal native type has no union encoding: stringifying it would
    // silently change time-predicate semantics downstream, so it rejects.
    let temporal = frozen_input(
        "view-a",
        1,
        vec![frozen_row(
            1,
            1,
            Some(1),
            &[("t", "Datetime(Milliseconds, None)", json!("2026-01-01"))],
        )],
    );
    let plain = frozen_input(
        "view-b",
        1,
        vec![frozen_row(
            2,
            1,
            Some(2),
            &[("t", "String", json!("later"))],
        )],
    );
    let error = union_frozen_inputs("union", &[temporal, plain.clone()], &limits()).unwrap_err();
    match error {
        UnionError::UnsupportedValue { view_id, field, .. } => {
            assert_eq!(view_id, "view-a");
            assert_eq!(field, "t");
        }
        other => panic!("expected an explicit unsupported-value rejection, got {other:?}"),
    }
    // A real nested value is likewise rejected, never stringified: encoding
    // an array as text would turn a null into the string "null" and change
    // validity, grouping keys and every predicate over the field.
    let nested = frozen_input(
        "view-a",
        1,
        vec![frozen_row(
            1,
            1,
            Some(1),
            &[("v", "List(String)", json!(["a"]))],
        )],
    );
    let error = union_frozen_inputs("union", &[nested, plain.clone()], &limits()).unwrap_err();
    assert!(
        matches!(error, UnionError::UnsupportedValue { .. }),
        "{error:?}"
    );
    // And a value contradicting its declared dtype is a corrupt handoff, not
    // a coercion: an object where a scalar was declared.
    let corrupt = frozen_input(
        "view-a",
        1,
        vec![frozen_row(
            1,
            1,
            Some(1),
            &[("n", "Int64", json!({"kind": "i64", "decimal": "7"}))],
        )],
    );
    let error = union_frozen_inputs("union", &[corrupt, plain], &limits()).unwrap_err();
    assert!(matches!(error, UnionError::Engine { .. }), "{error:?}");
}

#[test]
fn frozen_value_kind_conflicts_reject() {
    use serde_json::json;
    // A float where an integer was declared is a schema conflict, not a
    // coercion (the precise frozen path refuses the same lossy mix).
    let first = frozen_input(
        "view-a",
        1,
        vec![frozen_row(1, 1, Some(1), &[("v", "Int64", json!(7))])],
    );
    let second = frozen_input(
        "view-b",
        1,
        vec![frozen_row(2, 1, Some(2), &[("v", "Int64", json!(7.5))])],
    );
    let error = union_frozen_inputs("union", &[first, second], &limits()).unwrap_err();
    assert!(matches!(error, UnionError::Engine { .. }), "{error:?}");
    // And across inputs with honest dtypes, the diagonal schema conflict
    // rejects the same way rather than coercing one side.
    let ints = frozen_input(
        "view-a",
        1,
        vec![frozen_row(1, 1, Some(1), &[("v", "Int64", json!(7))])],
    );
    let texts = frozen_input(
        "view-b",
        1,
        vec![frozen_row(2, 1, Some(2), &[("v", "String", json!("7"))])],
    );
    let error = union_frozen_inputs("union", &[ints, texts], &limits()).unwrap_err();
    assert!(matches!(error, UnionError::Engine { .. }), "{error:?}");
}

#[test]
fn frozen_all_null_typed_column_survives_with_schema() {
    use serde_json::json;
    // Primary review case: a present-but-all-null Int64 column must keep its
    // dtype so `is_null` filters work — not vanish from the schema.
    let first = frozen_input(
        "view-a",
        1,
        vec![
            frozen_row(1, 1, Some(1), &[("x", "Int64", json!(null))]),
            frozen_row(1, 2, Some(2), &[("x", "Int64", json!(null))]),
        ],
    );
    let second = frozen_input(
        "view-b",
        1,
        vec![frozen_row(2, 1, Some(3), &[("n", "Int64", json!(2))])],
    );
    let merged = union_frozen_inputs("union", &[first, second.clone()], &limits()).unwrap();
    assert_eq!(merged.height(), 3);
    assert_eq!(merged.column("x").unwrap().dtype(), &DataType::Int64);
    let nulls = merged.lazy().filter(col("x").is_null()).collect().unwrap();
    assert_eq!(nulls.height(), 3);
    // A field whose declaration is genuinely unknown (`Null`) carries no
    // schema and contributes no column.
    let unknown = frozen_input(
        "view-a",
        1,
        vec![frozen_row(1, 1, Some(1), &[("ghost", "Null", json!(null))])],
    );
    let merged = union_frozen_inputs("union", &[unknown, second], &limits()).unwrap();
    assert!(merged.column("ghost").is_err());
}

#[test]
fn frozen_missing_fields_arrive_as_null() {
    use serde_json::json;
    let first = frozen_input(
        "view-a",
        1,
        vec![
            frozen_row(1, 1, Some(1), &[("n", "Int64", json!(1))]),
            frozen_row(1, 2, Some(2), &[]),
        ],
    );
    let second = frozen_input(
        "view-b",
        1,
        vec![frozen_row(2, 1, Some(3), &[("n", "Int64", json!(2))])],
    );
    let merged = union_frozen_inputs("union", &[first, second], &limits()).unwrap();
    assert_eq!(merged.height(), 3);
    // `n` is absent on a/2: null, not an error and not a zero.
    assert_eq!(i64_opt_at(&merged, "n", 1), None);
}

#[test]
fn frozen_empty_input_contributes_nothing_but_schema() {
    use serde_json::json;
    let empty = frozen_input("view-a", 2, vec![]);
    let full = frozen_input(
        "view-b",
        1,
        vec![frozen_row(2, 1, Some(5), &[("n", "Int64", json!(1))])],
    );
    let merged = union_frozen_inputs("union", &[empty, full], &limits()).unwrap();
    assert_eq!(merged.height(), 1);
    assert_eq!(u64_at(&merged, SEQUENCE_COLUMN, 0), 1);
    assert_eq!(
        str_at(&merged, SOURCE_ID_COLUMN, 0),
        source(2).0.to_string()
    );
}

#[test]
fn frozen_identity_snapshot_feeds_the_same_merge() {
    use serde_json::json;
    let api = frozen_input(
        "view-api",
        3,
        vec![
            frozen_row(1, 1, Some(30), &[("n", "Int64", json!(1))]),
            frozen_row(1, 2, None, &[("n", "Int64", json!(2))]),
        ],
    );
    let worker = frozen_input(
        "view-worker",
        5,
        vec![
            frozen_row(2, 1, Some(20), &[("n", "Int64", json!(3))]),
            // Overlapping identity with a disagreeing projection: first input wins.
            frozen_row(1, 1, Some(10), &[("n", "Int64", json!(99))]),
        ],
    );
    let snapshots = [
        frozen_identity_snapshot(&api),
        frozen_identity_snapshot(&worker),
    ];
    assert_eq!(snapshots[0].accepted_revision, 3);
    let merged = merge_union_rows(&snapshots).unwrap();
    // worker/1 (20), api/1 once at its FIRST-input time (30), api/2 null last.
    assert_eq!(order(&merged), vec![(2, 1), (1, 1), (1, 2),]);
    assert_eq!(merged[1].input, 0);
    assert_eq!(merged[1].timestamp_nanos, Some(30));
    // And the typed entry agrees on membership: one row per surviving identity.
    let typed = union_frozen_inputs("union", &[api, worker], &limits()).unwrap();
    assert_eq!(typed.height(), 3);
    assert_eq!(i64_opt_at(&typed, UNION_TS_COLUMN, 1), Some(30));
}

#[test]
fn engine_and_contract_agree_on_the_full_order() {
    use serde_json::json;
    // Fix (3): two sources in ONE input sharing sequence AND time — the
    // source tiebreak must match between the contract and the engine — plus
    // cross-input ties, nulls and a duplicate, in one fixture.
    let first = frozen_input(
        "view-a",
        1,
        vec![
            frozen_row(2, 5, Some(100), &[("n", "Int64", json!(1))]),
            frozen_row(1, 5, Some(100), &[("n", "Int64", json!(2))]),
            frozen_row(1, 6, None, &[("n", "Int64", json!(3))]),
        ],
    );
    let second = frozen_input(
        "view-b",
        1,
        vec![
            frozen_row(3, 5, Some(100), &[("n", "Int64", json!(4))]),
            frozen_row(2, 5, Some(50), &[("n", "Int64", json!(99))]),
        ],
    );
    let snapshots = [
        frozen_identity_snapshot(&first),
        frozen_identity_snapshot(&second),
    ];
    let contract = merge_union_rows(&snapshots).unwrap();
    let contract_order = order(&contract);
    // (2,5) dedups to the first input's time (100); the rest interleave by
    // (time, input, sequence, source): (1,5),(2,5),(3,5) at 100, then null.
    assert_eq!(contract_order, vec![(1, 5), (2, 5), (3, 5), (1, 6),]);
    let engine = union_frozen_inputs("union", &[first, second], &limits()).unwrap();
    // Map the engine's source strings back without parsing: the worker wrote
    // canonical `SourceId` spellings, so exact string equality is the check.
    let spellings: BTreeMap<String, u128> = [1u128, 2, 3]
        .into_iter()
        .map(|n| (source(n).0.to_string(), n))
        .collect();
    let engine_order: Vec<(u128, u64)> = (0..engine.height())
        .map(|i| {
            (
                spellings[&str_at(&engine, SOURCE_ID_COLUMN, i)],
                u64_at(&engine, SEQUENCE_COLUMN, i),
            )
        })
        .collect();
    assert_eq!(engine_order, contract_order);
}

#[test]
fn frozen_wide_integers_decode_without_wrapping() {
    use serde_json::json;
    let big = (u64::MAX).to_string();
    let first = frozen_input(
        "view-a",
        1,
        vec![frozen_row(
            1,
            1,
            Some(1),
            &[("n", "UInt64", serde_json::from_str(&big).unwrap())],
        )],
    );
    let second = frozen_input(
        "view-b",
        1,
        vec![frozen_row(2, 1, Some(2), &[("n", "UInt64", json!(7))])],
    );
    let merged = union_frozen_inputs("union", &[first, second.clone()], &limits()).unwrap();
    assert_eq!(merged.column("n").unwrap().dtype(), &DataType::UInt64);
    // A negative where unsigned was declared is corruption, not wrapping.
    let negative = frozen_input(
        "view-a",
        1,
        vec![frozen_row(1, 1, Some(1), &[("n", "UInt64", json!(-1))])],
    );
    let error = union_frozen_inputs("union", &[negative, second], &limits()).unwrap_err();
    assert!(matches!(error, UnionError::Engine { .. }), "{error:?}");
}

#[test]
fn frozen_budgets_fence_rows_and_bytes() {
    use serde_json::json;
    let first = frozen_input(
        "view-a",
        1,
        vec![
            frozen_row(1, 1, Some(1), &[("n", "Int64", json!(1))]),
            frozen_row(1, 2, Some(2), &[("n", "Int64", json!(2))]),
        ],
    );
    let second = frozen_input(
        "view-b",
        1,
        vec![frozen_row(2, 1, Some(3), &[("n", "Int64", json!(3))])],
    );
    let tight_rows = UnionLimits {
        maximum_rows: 2,
        ..UnionLimits::default()
    };
    assert!(matches!(
        union_frozen_inputs("union", &[first.clone(), second.clone()], &tight_rows).unwrap_err(),
        UnionError::RowLimit {
            rows: 3,
            maximum: 2
        }
    ));
    let tight_bytes = UnionLimits {
        maximum_bytes: 1,
        ..UnionLimits::default()
    };
    assert!(matches!(
        union_frozen_inputs("union", &[first, second], &tight_bytes).unwrap_err(),
        UnionError::ByteLimit { .. }
    ));
}

#[test]
fn frozen_entry_rejects_self_reference_and_protected_fields() {
    use serde_json::json;
    let mine = frozen_input(
        "union",
        1,
        vec![frozen_row(1, 1, Some(1), &[("n", "Int64", json!(1))])],
    );
    let other = frozen_input(
        "view-b",
        1,
        vec![frozen_row(2, 1, Some(2), &[("n", "Int64", json!(2))])],
    );
    assert_eq!(
        union_frozen_inputs("union", &[mine, other], &limits()).unwrap_err(),
        UnionError::SelfReference {
            view_id: "union".into()
        }
    );
    let smuggled = frozen_input(
        "view-a",
        1,
        vec![frozen_row(
            1,
            1,
            Some(1),
            &[("_lvu_union_input", "UInt32", json!(0))],
        )],
    );
    let clean = frozen_input(
        "view-b",
        1,
        vec![frozen_row(2, 1, Some(2), &[("n", "Int64", json!(2))])],
    );
    assert!(matches!(
        union_frozen_inputs("union", &[smuggled, clean], &limits()).unwrap_err(),
        UnionError::ProtectedColumn { .. }
    ));
}
