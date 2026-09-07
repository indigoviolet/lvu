//! Type and value exploration for one field path over a bounded sample
//! (docs/dialog-system.md §8.12).
//!
//! The sample is the first [`MAX_STATS_ROWS`] rows of the view's unfolded
//! stream — folding is presentation and must not change what is counted —
//! and every figure says how many rows it rests on. Values are read the way
//! the screen reads them: through `JsonTree` for a JSON record and through
//! the recognised `fields` otherwise, so a number counted here is a number
//! the user can see.

use std::collections::HashMap;

use crate::json_spans::JsonKind;
use crate::json_tree::{JsonTree, NodeKind, top_level_key};
use crate::provider::{RowId, RowProvider, ViewportRequest};

/// Rows the statistics read, from the top of the view.
pub const MAX_STATS_ROWS: usize = 2048;
/// Distinct values counted before the count is reported as a floor.
pub const MAX_DISTINCT_VALUES: usize = 4096;
/// Top values listed.
pub const TOP_VALUES: usize = 5;
/// Longest value counted as itself; longer ones count under a marker.
const MAX_VALUE_BYTES: usize = 512;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ValueType {
    Null,
    Bool,
    Integer,
    Float,
    Timestamp,
    String,
    Object,
    Array,
}

impl ValueType {
    pub fn label(self) -> &'static str {
        match self {
            ValueType::Null => "null",
            ValueType::Bool => "boolean",
            ValueType::Integer => "integer",
            ValueType::Float => "number",
            ValueType::Timestamp => "timestamp",
            ValueType::String => "string",
            ValueType::Object => "object",
            ValueType::Array => "array",
        }
    }
}

/// What the sampled values look like, and the evidence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TypeGuess {
    pub kind: ValueType,
    /// Present values that are of `kind`, out of every present value.
    pub matching: usize,
    /// The first value of that kind, as the record spelled it, and where.
    pub sample: String,
    pub sample_row: RowId,
}

impl TypeGuess {
    /// 0–100, over the present values.
    pub fn confidence_percent(&self, present: usize) -> usize {
        self.matching
            .saturating_mul(100)
            .checked_div(present)
            .unwrap_or(0)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FieldStats {
    pub path: String,
    /// Rows read.
    pub sampled: usize,
    /// Rows where the path had a value.
    pub present: usize,
    pub guess: Option<TypeGuess>,
    pub distinct: usize,
    /// `distinct` stopped counting at [`MAX_DISTINCT_VALUES`].
    pub distinct_capped: bool,
    /// Value and its count, most frequent first, ties by first appearance.
    pub top: Vec<(String, usize)>,
    /// Smallest and largest value, for numbers and timestamps only.
    pub range: Option<(String, String)>,
}

/// One value as the sample saw it: its display text and its type.
#[derive(Clone, Debug)]
struct Seen {
    text: String,
    kind: ValueType,
}

/// The type of one scalar token, from its JSON kind where the record is JSON
/// and from its spelling otherwise.
pub fn infer_type(text: &str, kind: Option<&JsonKind>) -> ValueType {
    match kind {
        Some(JsonKind::Null) => ValueType::Null,
        Some(JsonKind::Boolean) => ValueType::Bool,
        Some(JsonKind::Number) => {
            if text.parse::<i64>().is_ok() {
                ValueType::Integer
            } else {
                ValueType::Float
            }
        }
        Some(JsonKind::String) => {
            if looks_like_timestamp(text.trim_matches('"')) {
                ValueType::Timestamp
            } else {
                ValueType::String
            }
        }
        Some(JsonKind::Key(_) | JsonKind::Punctuation) => ValueType::String,
        None => {
            if text == "null" {
                ValueType::Null
            } else if text == "true" || text == "false" {
                ValueType::Bool
            } else if text.parse::<i64>().is_ok() {
                ValueType::Integer
            } else if text.parse::<f64>().is_ok() {
                ValueType::Float
            } else if looks_like_timestamp(text) {
                ValueType::Timestamp
            } else {
                ValueType::String
            }
        }
    }
}

/// `2026-09-07T…`, `2026-09-07 …` or a bare date: the shapes the Time dialog
/// recognises, judged lexically.
pub fn looks_like_timestamp(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() >= 10
        && bytes[..4].iter().all(u8::is_ascii_digit)
        && bytes[4] == b'-'
        && bytes[5..7].iter().all(u8::is_ascii_digit)
        && bytes[7] == b'-'
        && bytes[8..10].iter().all(u8::is_ascii_digit)
        && (bytes.len() == 10 || matches!(bytes[10], b'T' | b' '))
}

/// The value of `path` in one row, or `None` when the row has none.
fn value_in_row(row: &crate::provider::DisplayRow, path: &str) -> Option<Seen> {
    if let Some(tree) = JsonTree::parse(&row.text) {
        let id = tree.find(path)?;
        return Some(match &tree.node(id).kind {
            NodeKind::Object(_) => Seen {
                text: tree.summary(id).unwrap_or_default(),
                kind: ValueType::Object,
            },
            NodeKind::Array(_) => Seen {
                text: tree.summary(id).unwrap_or_default(),
                kind: ValueType::Array,
            },
            NodeKind::Scalar(kind) => {
                let raw = tree.scalar_text(&row.text, id)?;
                Seen {
                    text: raw.to_owned(),
                    kind: infer_type(raw, Some(kind)),
                }
            }
        });
    }
    // Not a JSON record: only top-level recognised fields exist.
    if top_level_key(path) != path {
        return None;
    }
    let (_, value) = row.fields.iter().find(|(key, _)| key == path)?;
    Some(Seen {
        kind: infer_type(value, None),
        text: value.clone(),
    })
}

/// Statistics for `path` over the first [`MAX_STATS_ROWS`] unfolded rows.
pub fn field_stats(provider: &dyn RowProvider, view_id: &str, path: &str) -> FieldStats {
    let page = provider.unfolded_page(
        view_id,
        ViewportRequest {
            start: 0,
            len: MAX_STATS_ROWS,
        },
    );
    let sampled = page.rows.len();
    let mut present = 0usize;
    let mut by_kind: HashMap<ValueType, (usize, String, RowId)> = HashMap::new();
    let mut counts: HashMap<String, (usize, usize)> = HashMap::new(); // value → (count, first index)
    let mut capped = false;
    let mut numeric: Option<(f64, String, f64, String)> = None;
    let mut lexical: Option<(String, String)> = None;
    let mut timestamps = 0usize;
    for row in &page.rows {
        let Some(seen) = value_in_row(row, path) else {
            continue;
        };
        present += 1;
        let entry = by_kind
            .entry(seen.kind)
            .or_insert_with(|| (0, seen.text.clone(), row.id.clone()));
        entry.0 += 1;
        let key = if seen.text.len() > MAX_VALUE_BYTES {
            "(long value)".to_owned()
        } else {
            seen.text.clone()
        };
        if let Some(slot) = counts.get_mut(&key) {
            slot.0 += 1;
        } else if counts.len() < MAX_DISTINCT_VALUES {
            counts.insert(key, (1, present));
        } else {
            capped = true;
        }
        match seen.kind {
            ValueType::Integer | ValueType::Float => {
                if let Ok(number) = seen.text.parse::<f64>() {
                    numeric = Some(match numeric.take() {
                        None => (number, seen.text.clone(), number, seen.text.clone()),
                        Some((min, min_text, max, max_text)) => {
                            let (min, min_text) = if number < min {
                                (number, seen.text.clone())
                            } else {
                                (min, min_text)
                            };
                            let (max, max_text) = if number > max {
                                (number, seen.text.clone())
                            } else {
                                (max, max_text)
                            };
                            (min, min_text, max, max_text)
                        }
                    });
                }
            }
            ValueType::Timestamp => {
                timestamps += 1;
                let text = seen.text.trim_matches('"').to_owned();
                lexical = Some(match lexical.take() {
                    None => (text.clone(), text),
                    Some((min, max)) => (
                        if text < min { text.clone() } else { min },
                        if text > max { text } else { max },
                    ),
                });
            }
            _ => {}
        }
    }
    let guess = by_kind
        .into_iter()
        .max_by(|a, b| a.1.0.cmp(&b.1.0).then_with(|| b.1.2.cmp(&a.1.2)))
        .map(|(kind, (matching, sample, sample_row))| TypeGuess {
            kind,
            matching,
            sample,
            sample_row,
        });
    let mut top: Vec<(String, usize, usize)> = counts
        .into_iter()
        .map(|(value, (count, first))| (value, count, first))
        .collect();
    top.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.2.cmp(&b.2)));
    let distinct = top.len();
    let range = match guess.as_ref().map(|guess| guess.kind) {
        Some(ValueType::Integer | ValueType::Float) => numeric.map(|(_, min, _, max)| (min, max)),
        Some(ValueType::Timestamp) if timestamps > 0 => lexical,
        _ => None,
    };
    FieldStats {
        path: path.to_owned(),
        sampled,
        present,
        guess,
        distinct,
        distinct_capped: capped,
        top: top
            .into_iter()
            .take(TOP_VALUES)
            .map(|(value, count, _)| (value, count))
            .collect(),
        range,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn types_are_read_from_json_kinds_and_from_spelling() {
        assert_eq!(
            infer_type("42", Some(&JsonKind::Number)),
            ValueType::Integer
        );
        assert_eq!(infer_type("4.2", Some(&JsonKind::Number)), ValueType::Float);
        assert_eq!(
            infer_type("\"2026-09-07T12:00:00Z\"", Some(&JsonKind::String)),
            ValueType::Timestamp
        );
        assert_eq!(
            infer_type("\"x\"", Some(&JsonKind::String)),
            ValueType::String
        );
        assert_eq!(infer_type("true", None), ValueType::Bool);
        assert_eq!(infer_type("2026-09-07", None), ValueType::Timestamp);
        assert_eq!(infer_type("worker", None), ValueType::String);
        assert!(!looks_like_timestamp("2026-9-7"));
    }
}
