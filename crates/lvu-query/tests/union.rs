//! Engine union compute: diagonal concat, stable dedup, total timestamp order.
//!
//! Path-includes `crates/lvu-query/src/union.rs` until the coordinated
//! `pub mod union;` export lands in `lib.rs`. Real Polars execution with
//! typed assertions throughout.

#[path = "../src/union.rs"]
#[allow(dead_code)]
mod engine_union;

use engine_union::union_sorted_frames;
use polars::prelude::*;

const TS: &str = "ts";
const SOURCE: &str = "src";
const SEQ: &str = "seq";
const TAG: &str = "_lvu_union_input";

fn frame(
    sources: &[&str],
    sequences: &[u64],
    times: &[Option<i64>],
    values: &[Option<i64>],
) -> DataFrame {
    DataFrame::new_infer_height(vec![
        Column::new(SOURCE.into(), sources.to_vec()),
        Column::new(SEQ.into(), sequences.to_vec()),
        Column::new(TS.into(), times.to_vec()),
        Column::new("v".into(), values.to_vec()),
    ])
    .unwrap()
}

fn i64_opt_at(frame: &DataFrame, name: &str, index: usize) -> Option<i64> {
    match frame.column(name).unwrap().get(index).unwrap() {
        AnyValue::Null => None,
        value => Some(value.try_extract::<i64>().unwrap()),
    }
}

#[test]
fn dedup_keeps_first_input_projection_then_sorts() {
    let first = frame(
        &["s", "s"],
        &[1, 2],
        &[Some(30), Some(10)],
        &[Some(1), Some(2)],
    );
    let second = frame(
        &["s", "t"],
        &[1, 1],
        &[Some(10), Some(20)],
        &[Some(99), Some(3)],
    );
    let merged = union_sorted_frames(vec![first, second], TS, SOURCE, SEQ, TAG).unwrap();
    // (s,1) dedups to the first input's row (ts 30, v 1), not the earlier
    // duplicate's (ts 10, v 99); survivors sort by (ts, input, seq, source).
    assert_eq!(merged.height(), 3);
    let values: Vec<Option<i64>> = (0..3).map(|i| i64_opt_at(&merged, "v", i)).collect();
    assert_eq!(values, vec![Some(2), Some(3), Some(1)]);
    let tags: Vec<u64> = (0..3)
        .map(|i| {
            merged
                .column(TAG)
                .unwrap()
                .get(i)
                .unwrap()
                .try_extract::<u64>()
                .unwrap()
        })
        .collect();
    assert_eq!(tags, vec![0, 1, 0]);
}

#[test]
fn source_breaks_dead_ties_deterministically() {
    // One input, two sources, identical sequence AND time: only the source
    // distinguishes the rows, and the order must be total and repeatable.
    let first = frame(
        &["b", "a"],
        &[5, 5],
        &[Some(7), Some(7)],
        &[Some(1), Some(2)],
    );
    let second = frame(&["c"], &[0], &[Some(1)], &[Some(3)]);
    let merged = union_sorted_frames(vec![first, second], TS, SOURCE, SEQ, TAG).unwrap();
    let sources: Vec<String> = (0..3)
        .map(|i| {
            merged
                .column(SOURCE)
                .unwrap()
                .get(i)
                .unwrap()
                .str_value()
                .to_string()
        })
        .collect();
    assert_eq!(sources, vec!["c", "a", "b"]);
    let again = merged
        .column(SOURCE)
        .unwrap()
        .get(1)
        .unwrap()
        .str_value()
        .to_string();
    assert_eq!(again, "a");
}

#[test]
fn diagonal_concat_keeps_missing_as_null_with_nulls_last() {
    let with_extra = DataFrame::new_infer_height(vec![
        Column::new(SOURCE.into(), vec!["a"]),
        Column::new(SEQ.into(), vec![1u64]),
        Column::new(TS.into(), vec![None::<i64>]),
        Column::new("extra".into(), vec!["kept"]),
    ])
    .unwrap();
    let plain = frame(&["b"], &[1], &[Some(4)], &[Some(9)]);
    let merged = union_sorted_frames(vec![with_extra, plain], TS, SOURCE, SEQ, TAG).unwrap();
    assert_eq!(merged.height(), 2);
    // Timed row first, null time last; the one-sided column is null elsewhere.
    assert_eq!(i64_opt_at(&merged, TS, 0), Some(4));
    assert_eq!(i64_opt_at(&merged, TS, 1), None);
    assert_eq!(merged.column("extra").unwrap().null_count(), 1);
}

#[test]
fn protected_tag_and_shape_conflicts_reject() {
    let mut smuggled = frame(&["a"], &[1], &[Some(1)], &[Some(1)]);
    smuggled
        .with_column(Column::new(TAG.into(), vec![0u32]))
        .unwrap();
    let clean = frame(&["b"], &[1], &[Some(2)], &[Some(2)]);
    assert!(union_sorted_frames(vec![smuggled, clean], TS, SOURCE, SEQ, TAG,).is_err());
    let strings = DataFrame::new_infer_height(vec![
        Column::new(SOURCE.into(), vec!["a"]),
        Column::new(SEQ.into(), vec![1u64]),
        Column::new(TS.into(), vec![Some(1)]),
        Column::new("v".into(), vec!["text"]),
    ])
    .unwrap();
    let ints = frame(&["b"], &[1], &[Some(2)], &[Some(2)]);
    assert!(union_sorted_frames(vec![strings, ints], TS, SOURCE, SEQ, TAG,).is_err());
}
