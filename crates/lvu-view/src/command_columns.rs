//! Command results as columns of the evaluation frame (docs/command-enrichment.md).
//!
//! A command step publishes one JSON object per record. Once published those
//! values are read by later expression steps, by filters and by searches as
//! `<name>.<field>` columns, joined into every batch frame before the chain
//! runs. The join is by stable record identity; a record the command has not
//! answered reads as null, so a filter over a command's output is valid before
//! the run and simply matches nothing until results exist. The raw bytes and
//! the protected `_lvu_*` columns are never touched.

use lvu_core::RawRecord;
use polars::prelude::{DataFrame, IntoColumn, NamedFrom, PolarsResult, Series};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashMap};

/// One command step's published results, keyed by record identity.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CommandColumns {
    /// The step's output prefix.
    pub name: String,
    /// `(source id, sequence)` → the record's result object.
    pub rows: HashMap<(String, u64), BTreeMap<String, Value>>,
    /// Every field any row carries, so a column exists for each of them.
    pub fields: BTreeSet<String>,
}

impl CommandColumns {
    pub fn new(name: String, rows: HashMap<(String, u64), BTreeMap<String, Value>>) -> Self {
        let fields = rows
            .values()
            .flat_map(|fields| fields.keys().cloned())
            .collect();
        Self { name, rows, fields }
    }

    pub fn column_name(&self, field: &str) -> String {
        format!("{}.{field}", self.name)
    }
}

/// The `<name>.` prefixes of `names` that `dependency` reads, if any.
pub fn command_prefix<'a>(
    dependency: &str,
    names: impl Iterator<Item = &'a str>,
) -> Option<&'a str> {
    names.into_iter().find(|name| {
        dependency.len() > name.len() + 1
            && dependency.starts_with(name)
            && dependency.as_bytes()[name.len()] == b'.'
    })
}

/// Adds every command column the batch needs: each published field of each
/// step, plus any `required` column an expression names that no row has yet,
/// as a null column so the expression stays valid. Existing columns are never
/// replaced, so a record field that happens to be named `command.x` wins over
/// the join and the raw record stays authoritative.
pub fn join_command_columns(
    frame: &mut DataFrame,
    records: &[RawRecord],
    steps: &[std::sync::Arc<CommandColumns>],
    required: &BTreeSet<String>,
) -> PolarsResult<()> {
    let existing: BTreeSet<String> = frame
        .get_column_names()
        .into_iter()
        .map(|name| name.to_string())
        .collect();
    let mut added = BTreeSet::new();
    for step in steps {
        let mut fields = step.fields.clone();
        for column in required {
            if let Some(field) = column
                .strip_prefix(step.name.as_str())
                .and_then(|rest| rest.strip_prefix('.'))
            {
                fields.insert(field.to_owned());
            }
        }
        for field in fields {
            let column = step.column_name(&field);
            if existing.contains(&column) || !added.insert(column.clone()) {
                continue;
            }
            let values: Vec<Option<&Value>> = records
                .iter()
                .map(|record| {
                    step.rows
                        .get(&(
                            record.record_id.source_id.0.to_string(),
                            record.record_id.sequence,
                        ))
                        .and_then(|fields| fields.get(&field))
                        .filter(|value| !value.is_null())
                })
                .collect();
            frame.with_column(series_for(&column, &values).into_column())?;
        }
    }
    // A column an expression names for a step with no publication yet.
    for column in required {
        if existing.contains(column) || added.contains(column) {
            continue;
        }
        if steps.iter().any(|step| {
            column
                .strip_prefix(step.name.as_str())
                .is_some_and(|rest| rest.starts_with('.'))
        }) {
            continue;
        }
        // Typed as null, not as text: a comparison or arithmetic over it is
        // null rather than a type error, so the step or filter stays valid.
        frame.with_column(Series::new_null(column.as_str().into(), records.len()).into_column())?;
        added.insert(column.clone());
    }
    Ok(())
}

/// The narrowest Polars type the values share: integers, then floats, then
/// booleans, else text (with structured values kept as their JSON text).
fn series_for(name: &str, values: &[Option<&Value>]) -> Series {
    let present = values.iter().flatten();
    let all_int = present.clone().all(|value| value.as_i64().is_some());
    let all_number = present.clone().all(|value| value.is_number());
    let all_bool = present.clone().all(|value| value.is_boolean());
    let any = values.iter().any(Option::is_some);
    if any && all_int {
        let column: Vec<Option<i64>> = values
            .iter()
            .map(|value| value.and_then(Value::as_i64))
            .collect();
        Series::new(name.into(), column)
    } else if any && all_number {
        let column: Vec<Option<f64>> = values
            .iter()
            .map(|value| value.and_then(Value::as_f64))
            .collect();
        Series::new(name.into(), column)
    } else if any && all_bool {
        let column: Vec<Option<bool>> = values
            .iter()
            .map(|value| value.and_then(Value::as_bool))
            .collect();
        Series::new(name.into(), column)
    } else {
        let column: Vec<Option<String>> = values
            .iter()
            .map(|value| {
                value.map(|value| match value {
                    Value::String(text) => text.clone(),
                    other => other.to_string(),
                })
            })
            .collect();
        Series::new(name.into(), column)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lvu_core::{ChunkPosition, RecordId, SourceId, StreamKind};
    use polars::prelude::{DataType, IntoColumn};
    use serde_json::json;
    use std::sync::Arc;

    fn record(source: SourceId, sequence: u64) -> RawRecord {
        RawRecord {
            record_id: RecordId {
                source_id: source,
                sequence,
            },
            captured_at_unix_nanos: 0,
            stream: StreamKind::Stdout,
            bytes: b"{}".to_vec().into(),
            delimiter: b"\n".to_vec().into(),
            acquisition_id: uuid::Uuid::nil(),
            chunk: ChunkPosition::End,
        }
    }

    #[test]
    fn joins_by_record_identity_with_typed_columns_and_nulls_for_unanswered_rows() {
        let source = SourceId::new();
        let records = vec![record(source, 1), record(source, 2), record(source, 3)];
        let step = Arc::new(CommandColumns::new(
            "geo".into(),
            HashMap::from([
                (
                    (source.0.to_string(), 1),
                    BTreeMap::from([("score".into(), json!(7)), ("city".into(), json!("Oslo"))]),
                ),
                (
                    (source.0.to_string(), 3),
                    BTreeMap::from([("score".into(), json!(9)), ("flag".into(), json!(true))]),
                ),
            ]),
        ));
        let mut frame = DataFrame::new(
            3,
            vec![Series::new("raw".into(), vec!["a", "b", "c"]).into_column()],
        )
        .unwrap();
        let required = BTreeSet::from(["geo.missing".to_owned(), "other.x".to_owned()]);
        join_command_columns(&mut frame, &records, &[step], &required).unwrap();
        assert_eq!(frame.column("geo.score").unwrap().dtype(), &DataType::Int64);
        assert_eq!(
            frame
                .column("geo.score")
                .unwrap()
                .i64()
                .unwrap()
                .iter()
                .collect::<Vec<_>>(),
            vec![Some(7), None, Some(9)]
        );
        assert_eq!(frame.column("geo.city").unwrap().dtype(), &DataType::String);
        assert_eq!(
            frame.column("geo.flag").unwrap().dtype(),
            &DataType::Boolean
        );
        // A field named by an expression but published by no row is a null
        // column of no particular type.
        assert_eq!(frame.column("geo.missing").unwrap().null_count(), 3);
        assert_eq!(frame.column("other.x").unwrap().dtype(), &DataType::Null);
        assert_eq!(
            frame.column("raw").unwrap().str().unwrap().get(0),
            Some("a")
        );
    }

    #[test]
    fn a_record_field_with_the_same_name_is_not_replaced() {
        let source = SourceId::new();
        let records = vec![record(source, 1)];
        let step = Arc::new(CommandColumns::new(
            "cmd".into(),
            HashMap::from([(
                (source.0.to_string(), 1),
                BTreeMap::from([("x".into(), json!(1))]),
            )]),
        ));
        let mut frame = DataFrame::new(
            1,
            vec![Series::new("cmd.x".into(), vec!["theirs"]).into_column()],
        )
        .unwrap();
        join_command_columns(&mut frame, &records, &[step], &BTreeSet::new()).unwrap();
        assert_eq!(
            frame.column("cmd.x").unwrap().str().unwrap().get(0),
            Some("theirs")
        );
    }

    #[test]
    fn prefix_detection_needs_the_dot() {
        let names = ["cmd", "geo"];
        assert_eq!(command_prefix("cmd.x", names.iter().copied()), Some("cmd"));
        assert_eq!(command_prefix("cmdx", names.iter().copied()), None);
        assert_eq!(command_prefix("cmd", names.iter().copied()), None);
        assert_eq!(
            command_prefix("geo.a.b", names.iter().copied()),
            Some("geo")
        );
    }
}
