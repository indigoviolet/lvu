//! Whole-view column statistics: the engine counts, the caller names.

use lvu_query::column_stats::{ColumnAggregate, ColumnAggregator};
use polars::prelude::*;

fn text_frame(values: &[Option<&str>]) -> DataFrame {
    DataFrame::new(
        values.len(),
        vec![Column::from(Series::new("field".into(), values.to_vec()))],
    )
    .unwrap()
}

fn number_frame(values: &[Option<i64>]) -> DataFrame {
    DataFrame::new(
        values.len(),
        vec![Column::from(Series::new("field".into(), values.to_vec()))],
    )
    .unwrap()
}

fn aggregate(
    frames: &[DataFrame],
    predicate: Option<Expr>,
    top: usize,
    cap: usize,
) -> ColumnAggregate {
    let mut aggregator = ColumnAggregator::new("field", predicate, top, cap);
    for frame in frames {
        aggregator.push(frame).unwrap();
    }
    aggregator.finish().unwrap()
}

#[test]
fn counts_span_every_batch() {
    let result = aggregate(
        &[
            text_frame(&[Some("a"), Some("b"), Some("a")]),
            text_frame(&[Some("a"), None, Some("c")]),
        ],
        None,
        5,
        4096,
    );
    assert_eq!(result.rows, 6);
    assert_eq!(result.present, 5);
    assert_eq!(result.distinct, 3);
    assert!(!result.distinct_capped);
    assert_eq!(result.top[0], ("a".to_owned(), 3));
    assert_eq!(
        result.top.iter().map(|(_, count)| *count).sum::<u64>(),
        5,
        "every present value is counted under some value"
    );
}

#[test]
fn a_null_is_absent_not_a_value() {
    let result = aggregate(&[text_frame(&[None, None, Some("x")])], None, 5, 4096);
    assert_eq!(result.rows, 3);
    assert_eq!(result.present, 1);
    assert_eq!(result.distinct, 1);
    assert_eq!(result.top, vec![("x".to_owned(), 1)]);
}

#[test]
fn a_column_of_nothing_answers_without_inventing_values() {
    let result = aggregate(&[text_frame(&[None, None])], None, 5, 4096);
    assert_eq!(result.rows, 2);
    assert_eq!(result.present, 0);
    assert_eq!(result.distinct, 0);
    assert!(result.top.is_empty());
    assert_eq!(result.minimum, None);
    assert_eq!(result.maximum, None);
}

#[test]
fn the_caller_predicate_is_what_decides_the_type_share() {
    // The app has already decided this field is an integer, from the record's
    // own bytes. It hands that verdict over as an expression; the engine only
    // counts the rows satisfying it, so the two cannot disagree.
    let integers = col("field").cast(DataType::Int64).is_not_null();
    let result = aggregate(
        &[text_frame(&[
            Some("1"),
            Some("2"),
            Some("not a number"),
            None,
        ])],
        Some(integers),
        5,
        4096,
    );
    assert_eq!(result.present, 3);
    assert_eq!(result.matching, 2);
}

#[test]
fn extremes_follow_the_type_the_caller_cast_to_not_the_spelling() {
    // Lexically "1000" < "9", which is the wrong answer for a number and is
    // exactly what the cast exists to prevent.
    let numeric = aggregate(
        &[
            number_frame(&[Some(9), Some(1000)]),
            number_frame(&[Some(37)]),
        ],
        None,
        5,
        4096,
    );
    assert_eq!(numeric.minimum.as_deref(), Some("9"));
    assert_eq!(numeric.maximum.as_deref(), Some("1000"));

    let lexical = aggregate(
        &[
            text_frame(&[Some("apple"), Some("pear")]),
            text_frame(&[Some("fig")]),
        ],
        None,
        5,
        4096,
    );
    assert_eq!(lexical.minimum.as_deref(), Some("apple"));
    assert_eq!(lexical.maximum.as_deref(), Some("pear"));
}

#[test]
fn the_cap_bounds_memory_and_says_the_count_is_a_floor() {
    let many: Vec<Option<String>> = (0..500).map(|index| Some(format!("v{index}"))).collect();
    let borrowed: Vec<Option<&str>> = many.iter().map(|value| value.as_deref()).collect();
    let result = aggregate(&[text_frame(&borrowed)], None, 5, 64);
    assert!(result.distinct_capped, "past the cap the count is a floor");
    assert!(
        result.distinct <= 64,
        "the aggregator kept {} distinct values against a cap of 64",
        result.distinct
    );
    assert_eq!(result.rows, 500);
    assert_eq!(result.present, 500);
}

#[test]
fn the_most_frequent_values_come_first() {
    let mut values: Vec<Option<&str>> = Vec::new();
    values.extend(std::iter::repeat_n(Some("common"), 30));
    values.extend(std::iter::repeat_n(Some("middling"), 10));
    values.extend(std::iter::repeat_n(Some("rare"), 2));
    let result = aggregate(&[text_frame(&values)], None, 2, 4096);
    assert_eq!(
        result.top,
        vec![("common".to_owned(), 30), ("middling".to_owned(), 10)]
    );
    assert_eq!(
        result.distinct, 3,
        "the count is of every value, not the top ones"
    );
}

#[test]
fn batching_does_not_change_any_figure() {
    let whole = text_frame(&[
        Some("a"),
        Some("b"),
        Some("a"),
        None,
        Some("c"),
        Some("b"),
        Some("a"),
    ]);
    let split = [
        text_frame(&[Some("a"), Some("b")]),
        text_frame(&[Some("a"), None]),
        text_frame(&[Some("c"), Some("b"), Some("a")]),
    ];
    let one = aggregate(std::slice::from_ref(&whole), None, 5, 4096);
    let many = aggregate(&split, None, 5, 4096);
    assert_eq!(
        one, many,
        "a figure that depends on batch geometry is wrong"
    );
}
