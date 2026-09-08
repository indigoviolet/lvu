use lvu_core::{
    ChunkPosition, ExactFieldConstraint, ExactFieldError, ExactScalar, RawRecord, StreamKind,
};
use polars::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const RAW_ALIAS: &str = "raw";
pub const RAW_COLUMN: &str = "_lvu_raw";
pub const RAW_BYTES_COLUMN: &str = "_lvu_raw_bytes";
pub const SOURCE_ID_COLUMN: &str = "_lvu_source_id";
pub const SEQUENCE_COLUMN: &str = "_lvu_sequence";
pub const EXACT_VALUE_COLUMN_PREFIX: &str = "_lvu_exact_correlation_value:";

pub fn exact_value_column(field: &str) -> String {
    format!("{EXACT_VALUE_COLUMN_PREFIX}{field}")
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ParseStatus {
    Json,
    Logfmt,
    Unstructured,
    MalformedJson,
    UnsupportedLogfmt,
    ProjectionLimited,
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AdapterDiagnostic {
    pub row: usize,
    pub field: Option<String>,
    pub code: String,
    pub message: String,
}
pub struct RecordBatch {
    pub frame: DataFrame,
    pub diagnostics: Vec<AdapterDiagnostic>,
    pub parse_status: Vec<ParseStatus>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FieldType {
    Unknown,
    Bool,
    Int64,
    Float64,
    String,
}
#[derive(Clone, Debug)]
pub struct SchemaContext {
    fields: BTreeMap<String, FieldType>,
    pub max_fields: usize,
    pub max_diagnostics: usize,
    pub max_parse_bytes: usize,
}
impl Default for SchemaContext {
    fn default() -> Self {
        Self {
            fields: BTreeMap::new(),
            max_fields: 256,
            max_diagnostics: 4096,
            max_parse_bytes: 1024 * 1024,
        }
    }
}
impl SchemaContext {
    pub fn fields(&self) -> &BTreeMap<String, FieldType> {
        &self.fields
    }
}

#[derive(Clone, Debug)]
enum Value {
    Null,
    Bool(bool),
    Int(i64),
    UInt(u64),
    Float(f64),
    String(String),
    Object(String),
    Array(String),
}
impl Value {
    fn kind(&self) -> Option<FieldType> {
        match self {
            Self::Null => None,
            Self::Bool(_) => Some(FieldType::Bool),
            Self::Int(_) => Some(FieldType::Int64),
            Self::UInt(_) => Some(FieldType::Float64),
            Self::Float(_) => Some(FieldType::Float64),
            Self::String(_) | Self::Object(_) | Self::Array(_) => Some(FieldType::String),
        }
    }
    fn exact_scalar(&self) -> Result<ExactScalar, ExactValueError> {
        match self {
            Self::Null => Ok(ExactScalar::Null),
            Self::Bool(value) => Ok(ExactScalar::Bool(*value)),
            Self::Int(value) => Ok(ExactScalar::SignedInteger(*value)),
            Self::UInt(value) => Ok(ExactScalar::UnsignedInteger(*value)),
            Self::Float(value) => ExactScalar::finite_float(*value).map_err(Into::into),
            Self::String(value) => ExactScalar::string(value.clone()).map_err(Into::into),
            Self::Object(_) | Self::Array(_) => Err(ExactValueError::UnsupportedType),
        }
    }
    fn type_name(&self) -> &'static str {
        match self {
            Self::Null => "null",
            Self::Bool(_) => "bool",
            Self::Int(_) => "int64",
            Self::UInt(_) => "uint64",
            Self::Float(_) => "float64",
            Self::String(_) => "string",
            Self::Object(_) => "object",
            Self::Array(_) => "array",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ExactValueError {
    #[error(transparent)]
    InvalidConstraint(#[from] ExactFieldError),
    #[error("field is missing")]
    Missing,
    #[error("object and array values are not supported for exact correlation")]
    UnsupportedType,
    #[error("record has no structured fields")]
    Unstructured,
    #[error("record exceeds bounded structured projection; raw bytes retained")]
    ProjectionLimited,
    #[error("invalid UTF-8 cannot be used for exact field correlation")]
    InvalidUtf8,
    #[error("field is outside the bounded canonical structured projection")]
    ProjectionUnavailable,
}

/// Resolves a field from this record's raw structured JSON/logfmt projection.
/// Native enrichment outputs are not present here and must be rejected by the
/// caller rather than resolved from a same-named raw field.
pub fn resolve_exact_field(
    record: &RawRecord,
    field: &str,
) -> Result<ExactScalar, ExactValueError> {
    ExactFieldConstraint::new(field, ExactScalar::Null)?;
    if record.bytes.len() > SchemaContext::default().max_parse_bytes {
        return Err(ExactValueError::ProjectionLimited);
    }
    let text = std::str::from_utf8(&record.bytes).map_err(|_| ExactValueError::InvalidUtf8)?;
    let (_, fields, warning) = parse_fields(text, SchemaContext::default().max_fields, Some(field));
    if warning.is_some_and(|(code, _)| code == "exact_field_not_projected") {
        return Err(ExactValueError::ProjectionUnavailable);
    }
    fields
        .get(field)
        .ok_or(ExactValueError::Missing)?
        .exact_scalar()
}

/// Field names this record's bounded structured projection would produce, in
/// canonical order. Correlation offers these as the per-source choices; they
/// are observed names, not a mapping.
pub fn structured_field_names(record: &RawRecord, maximum: usize) -> Vec<String> {
    let context = SchemaContext::default();
    if record.bytes.len() > context.max_parse_bytes {
        return Vec::new();
    }
    let Ok(text) = std::str::from_utf8(&record.bytes) else {
        return Vec::new();
    };
    let (_, fields, _) = parse_fields(text, context.max_fields, None);
    fields
        .into_keys()
        .filter(|key| ExactFieldConstraint::new(key.clone(), ExactScalar::Null).is_ok())
        .take(maximum)
        .collect()
}

pub fn records_to_batch(records: &[RawRecord]) -> PolarsResult<RecordBatch> {
    records_to_batch_with_context(records, &mut SchemaContext::default())
}
pub fn records_to_batch_with_context(
    records: &[RawRecord],
    schema: &mut SchemaContext,
) -> PolarsResult<RecordBatch> {
    records_to_batch_with_context_and_exact_field(records, schema, None)
}

/// One field's values, and nothing else.
///
/// The canonical projection builds a column for every field it finds plus ten
/// of metadata and three copies of the record's text, which is what a filter
/// needs and what a statistics pass over a single field does not. This parses
/// the same way — so JSON and logfmt records are read exactly as the canonical
/// projection reads them, and a figure counted here is a figure a filter would
/// agree with — and then materialises the one column asked for.
///
/// The field is a *top-level* key. A nested value lives as JSON text inside its
/// top-level column, so the caller asks for that column and addresses the value
/// inside it with a JSON path, exactly as the canonical projection leaves it.
///
/// `schema` accumulates across batches as it does for the canonical projection,
/// so a field that is an integer in one batch and a string in a later one is
/// widened the same way rather than being re-decided per batch.
pub fn records_to_field_column(
    records: &[RawRecord],
    schema: &mut SchemaContext,
    field: &str,
) -> PolarsResult<DataFrame> {
    // `raw` is not a parsed field: the canonical projection synthesises it from
    // the record's own text, and a caller addressing a JSON path from the record
    // root asks for it by that name. Serving it here keeps the two projections
    // answering the same question.
    if field == RAW_ALIAS || field == RAW_COLUMN {
        let text = records
            .iter()
            .map(|record| String::from_utf8_lossy(&record.bytes).into_owned())
            .collect::<Vec<_>>();
        return DataFrame::new(records.len(), vec![Series::new(field.into(), text).into()]);
    }
    let mut values: Vec<Option<Value>> = Vec::with_capacity(records.len());
    for record in records {
        if record.bytes.len() > schema.max_parse_bytes {
            values.push(None);
            continue;
        }
        let text = String::from_utf8_lossy(&record.bytes);
        let value = parse_one_field(&text, schema.max_fields, field);
        if let Some(value) = &value {
            let kind = value.kind().unwrap_or(FieldType::Unknown);
            schema
                .fields
                .entry(field.to_owned())
                .and_modify(|existing| *existing = merge_type(*existing, kind))
                .or_insert(kind);
        }
        values.push(value);
    }
    let kind = schema
        .fields
        .get(field)
        .copied()
        .unwrap_or(FieldType::Unknown);
    let column = field_column(field, &values, kind);
    DataFrame::new(records.len(), vec![column])
}

/// One field's value from one record, converting only that field.
///
/// The canonical parse converts every key it finds, which for a nested object
/// or array means serialising it back to text — work this pass then throws
/// away. The record still has to be parsed to find the key, but only the key
/// asked for is converted. The recognition rules are the canonical ones, so a
/// value found here is the value a filter would see.
fn parse_one_field(text: &str, max_fields: usize, field: &str) -> Option<Value> {
    let trimmed = text.trim();
    if trimmed.starts_with('{') {
        return match serde_json::from_str::<serde_json::Value>(trimmed) {
            Ok(serde_json::Value::Object(object)) => object
                .into_iter()
                .take(max_fields)
                .find(|(key, _)| key == field)
                .map(|(_, value)| json_value(value)),
            _ => None,
        };
    }
    // Not JSON: the canonical parse recognises logfmt and unstructured records
    // the same way, and those carry no nested values to skip converting, so
    // there is nothing to gain from a second implementation of it here.
    let (_, mut fields, _) = parse_fields(text, max_fields, None);
    fields.remove(field)
}

/// One column, typed the way the canonical projection types it.
fn field_column(name: &str, values: &[Option<Value>], kind: FieldType) -> Column {
    match kind {
        FieldType::Unknown => Series::full_null(name.into(), values.len(), &DataType::Null).into(),
        FieldType::Bool => Series::new(
            name.into(),
            values
                .iter()
                .map(|value| match value {
                    Some(Value::Bool(inner)) => Some(*inner),
                    _ => None,
                })
                .collect::<Vec<_>>(),
        )
        .into(),
        FieldType::Int64 => Series::new(
            name.into(),
            values
                .iter()
                .map(|value| match value {
                    Some(Value::Int(inner)) => Some(*inner),
                    _ => None,
                })
                .collect::<Vec<_>>(),
        )
        .into(),
        FieldType::Float64 => Series::new(
            name.into(),
            values
                .iter()
                .map(|value| match value {
                    Some(Value::Int(inner)) => Some(*inner as f64),
                    Some(Value::UInt(inner)) => Some(*inner as f64),
                    Some(Value::Float(inner)) => Some(*inner),
                    _ => None,
                })
                .collect::<Vec<_>>(),
        )
        .into(),
        FieldType::String => Series::new(
            name.into(),
            values
                .iter()
                .map(|value| match value {
                    Some(Value::String(inner) | Value::Object(inner) | Value::Array(inner)) => {
                        Some(inner.clone())
                    }
                    _ => None,
                })
                .collect::<Vec<Option<String>>>(),
        )
        .into(),
    }
}

pub fn records_to_batch_with_context_and_exact_field(
    records: &[RawRecord],
    schema: &mut SchemaContext,
    exact_field: Option<&str>,
) -> PolarsResult<RecordBatch> {
    if let Some(field) = exact_field {
        ExactFieldConstraint::new(field, ExactScalar::Null)
            .map_err(|failure| PolarsError::ComputeError(failure.to_string().into()))?;
    }
    let mut rows = Vec::with_capacity(records.len());
    let mut exact_utf8 = Vec::with_capacity(records.len());
    let mut statuses = Vec::with_capacity(records.len());
    let mut diagnostics = Vec::new();
    for (row, record) in records.iter().enumerate() {
        exact_utf8.push(std::str::from_utf8(&record.bytes).is_ok());
        if record.bytes.len() > schema.max_parse_bytes {
            diagnostic(
                &mut diagnostics,
                schema.max_diagnostics,
                row,
                None,
                "projection_limited",
                "record exceeds bounded structured projection; raw bytes retained",
            );
            rows.push(BTreeMap::new());
            statuses.push(ParseStatus::ProjectionLimited);
            continue;
        }
        let text = String::from_utf8_lossy(&record.bytes);
        let (status, fields, warning) = parse_fields(&text, schema.max_fields, exact_field);
        if exact_field.is_some()
            && warning.is_some_and(|(code, _)| code == "exact_field_not_projected")
        {
            return Err(PolarsError::ComputeError(
                "requested exact field is outside the bounded canonical projection".into(),
            ));
        }
        if let Some((code, message)) = warning {
            diagnostic(
                &mut diagnostics,
                schema.max_diagnostics,
                row,
                None,
                code,
                message,
            );
        }
        for (key, value) in &fields {
            if key.starts_with("_lvu_") || key == RAW_ALIAS {
                diagnostic(
                    &mut diagnostics,
                    schema.max_diagnostics,
                    row,
                    Some(key),
                    "reserved_field",
                    "reserved input field was not projected",
                );
                continue;
            }
            if !schema.fields.contains_key(key) && schema.fields.len() >= schema.max_fields {
                diagnostic(
                    &mut diagnostics,
                    schema.max_diagnostics,
                    row,
                    Some(key),
                    "field_limit",
                    "schema field limit reached",
                );
                continue;
            }
            let kind = value.kind().unwrap_or(FieldType::Unknown);
            if let Some(existing) = schema.fields.get(key)
                && *existing != FieldType::Unknown
                && kind != FieldType::Unknown
                && *existing != kind
                && !matches!(
                    (*existing, kind),
                    (FieldType::Int64, FieldType::Float64) | (FieldType::Float64, FieldType::Int64)
                )
            {
                diagnostic(
                    &mut diagnostics,
                    schema.max_diagnostics,
                    row,
                    Some(key),
                    "type_conflict",
                    "value is null in the canonical typed projection; original type remains in provenance",
                );
            }
            schema
                .fields
                .entry(key.clone())
                .and_modify(|existing| *existing = merge_type(*existing, kind))
                .or_insert(kind);
        }
        rows.push(fields);
        statuses.push(status);
    }
    if let Some(field) = exact_field
        && rows.iter().any(|row| row.contains_key(field))
        && !schema.fields.contains_key(field)
    {
        return Err(PolarsError::ComputeError(
            "requested exact field was not admitted to the canonical schema".into(),
        ));
    }
    let source_ids: Vec<String> = records
        .iter()
        .map(|r| r.record_id.source_id.0.to_string())
        .collect();
    let sequences: Vec<u64> = records.iter().map(|r| r.record_id.sequence).collect();
    let raw: Vec<String> = records
        .iter()
        .map(|r| String::from_utf8_lossy(&r.bytes).into_owned())
        .collect();
    let mut columns: Vec<Column> = vec![
        Series::new(SOURCE_ID_COLUMN.into(), source_ids).into(),
        Series::new(SEQUENCE_COLUMN.into(), sequences).into(),
        Series::new(
            RAW_BYTES_COLUMN.into(),
            records
                .iter()
                .map(|r| r.bytes.as_slice())
                .collect::<Vec<_>>(),
        )
        .into(),
        Series::new(RAW_COLUMN.into(), raw.clone()).into(),
        Series::new(RAW_ALIAS.into(), raw).into(),
        Series::new(
            "_lvu_captured_at_unix_nanos".into(),
            records
                .iter()
                .map(|r| r.captured_at_unix_nanos)
                .collect::<Vec<_>>(),
        )
        .into(),
        Series::new(
            "_lvu_stream".into(),
            records
                .iter()
                .map(|r| stream_name(r.stream))
                .collect::<Vec<_>>(),
        )
        .into(),
        Series::new(
            "_lvu_acquisition_id".into(),
            records
                .iter()
                .map(|r| r.acquisition_id.to_string())
                .collect::<Vec<_>>(),
        )
        .into(),
        Series::new(
            "_lvu_chunk".into(),
            records
                .iter()
                .map(|r| chunk_name(r.chunk))
                .collect::<Vec<_>>(),
        )
        .into(),
        Series::new(
            "_lvu_delimiter".into(),
            records
                .iter()
                .map(|r| r.delimiter.as_slice())
                .collect::<Vec<_>>(),
        )
        .into(),
        Series::new(
            "_lvu_parse_status".into(),
            statuses.iter().map(status_name).collect::<Vec<_>>(),
        )
        .into(),
    ];
    if let Some(field) = exact_field {
        let values = rows
            .iter()
            .zip(exact_utf8)
            .map(|(row, valid_utf8)| {
                valid_utf8
                    .then(|| row.get(field))
                    .flatten()
                    .and_then(|value| value.exact_scalar().ok())
                    .and_then(|value| value.exact_token().ok())
            })
            .collect::<Vec<_>>();
        columns.push(Series::new(exact_value_column(field).into(), values).into());
    }
    for (key, kind) in schema.fields.clone() {
        let values: Vec<Option<&Value>> = rows.iter().map(|row| row.get(&key)).collect();
        let column: Column = match kind {
            FieldType::Unknown => {
                Series::full_null(key.clone().into(), rows.len(), &DataType::Null).into()
            }
            FieldType::Bool => Series::new(
                key.clone().into(),
                values
                    .iter()
                    .map(|v| match v {
                        Some(Value::Bool(x)) => Some(*x),
                        _ => None,
                    })
                    .collect::<Vec<_>>(),
            )
            .into(),
            FieldType::Int64 => Series::new(
                key.clone().into(),
                values
                    .iter()
                    .map(|v| match v {
                        Some(Value::Int(x)) => Some(*x),
                        _ => None,
                    })
                    .collect::<Vec<_>>(),
            )
            .into(),
            FieldType::Float64 => Series::new(
                key.clone().into(),
                values
                    .iter()
                    .map(|v| match v {
                        Some(Value::Int(x)) => Some(*x as f64),
                        Some(Value::UInt(x)) => Some(*x as f64),
                        Some(Value::Float(x)) => Some(*x),
                        _ => None,
                    })
                    .collect::<Vec<_>>(),
            )
            .into(),
            FieldType::String => {
                let owned: Vec<Option<String>> = values
                    .iter()
                    .map(|v| match v {
                        Some(Value::String(value) | Value::Object(value) | Value::Array(value)) => {
                            Some(value.clone())
                        }
                        _ => None,
                    })
                    .collect();
                Series::new(key.clone().into(), owned).into()
            }
        };
        columns.push(column);
        columns.push(
            Series::new(
                format!("_lvu_type_{key}").into(),
                values
                    .iter()
                    .map(|v| v.map(|v| v.type_name()))
                    .collect::<Vec<_>>(),
            )
            .into(),
        );
        for (row, value) in values.iter().enumerate() {
            if value.is_none() {
                diagnostic(
                    &mut diagnostics,
                    schema.max_diagnostics,
                    row,
                    Some(&key),
                    "missing",
                    "field is absent in this record",
                );
            }
        }
    }
    Ok(RecordBatch {
        frame: DataFrame::new(records.len(), columns)?,
        diagnostics,
        parse_status: statuses,
    })
}

fn merge_type(a: FieldType, b: FieldType) -> FieldType {
    if a == FieldType::Unknown {
        b
    } else if b == FieldType::Unknown || a == b {
        a
    } else if matches!(
        (a, b),
        (FieldType::Int64, FieldType::Float64) | (FieldType::Float64, FieldType::Int64)
    ) {
        FieldType::Float64
    } else {
        a
    }
}
fn parse_fields(
    text: &str,
    max_fields: usize,
    exact_field: Option<&str>,
) -> (
    ParseStatus,
    BTreeMap<String, Value>,
    Option<(&'static str, &'static str)>,
) {
    let trimmed = text.trim();
    if trimmed.starts_with('{') {
        return match serde_json::from_str::<serde_json::Value>(trimmed) {
            Ok(serde_json::Value::Object(object)) => {
                let mut fields = BTreeMap::new();
                let mut projected = 0;
                let mut exact_field_not_projected = false;
                for (key, value) in object {
                    if projected < max_fields {
                        projected += 1;
                        fields.insert(key, json_value(value));
                    } else if exact_field == Some(key.as_str()) {
                        exact_field_not_projected = true;
                        fields.insert(key, json_value(value));
                    }
                }
                let warning = exact_field_not_projected.then_some((
                    "exact_field_not_projected",
                    "requested exact field is outside the bounded canonical projection",
                ));
                (ParseStatus::Json, fields, warning)
            }
            _ => (
                ParseStatus::MalformedJson,
                BTreeMap::new(),
                Some((
                    "malformed_json",
                    "JSON-looking record is malformed; raw bytes retained",
                )),
            ),
        };
    }
    match parse_logfmt(trimmed, max_fields) {
        Ok((fields, false)) if fields.is_empty() => (ParseStatus::Unstructured, fields, None),
        Ok((fields, false)) => (ParseStatus::Logfmt, fields, None),
        Ok((fields, true)) if exact_field.is_some() => (
            ParseStatus::Logfmt,
            fields,
            Some((
                "exact_field_not_projected",
                "requested exact field is unavailable because logfmt exceeds the canonical field limit",
            )),
        ),
        Ok((_fields, true)) => (
            ParseStatus::UnsupportedLogfmt,
            BTreeMap::new(),
            Some((
                "unsupported_logfmt",
                "quoted logfmt is malformed or exceeds field bounds; no misleading fields projected",
            )),
        ),
        Err(()) => (
            ParseStatus::UnsupportedLogfmt,
            BTreeMap::new(),
            Some((
                "unsupported_logfmt",
                "quoted logfmt is malformed or exceeds field bounds; no misleading fields projected",
            )),
        ),
    }
}
fn json_value(value: serde_json::Value) -> Value {
    match value {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::Bool(v) => Value::Bool(v),
        serde_json::Value::Number(v) if v.is_i64() => Value::Int(v.as_i64().unwrap()),
        serde_json::Value::Number(v) if v.is_u64() => Value::UInt(v.as_u64().unwrap()),
        serde_json::Value::Number(v) => Value::Float(v.as_f64().unwrap_or(f64::NAN)),
        serde_json::Value::String(v) => Value::String(v),
        serde_json::Value::Object(value) => {
            Value::Object(serde_json::Value::Object(value).to_string())
        }
        serde_json::Value::Array(value) => {
            Value::Array(serde_json::Value::Array(value).to_string())
        }
    }
}
fn parse_logfmt(text: &str, max_fields: usize) -> Result<(BTreeMap<String, Value>, bool), ()> {
    let bytes = text.as_bytes();
    let mut i = 0;
    let mut fields = BTreeMap::new();
    while i < bytes.len() {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i == bytes.len() {
            break;
        }
        let start = i;
        while i < bytes.len() && !bytes[i].is_ascii_whitespace() && bytes[i] != b'=' {
            i += 1;
        }
        if i == start || i == bytes.len() || bytes[i] != b'=' {
            while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            continue;
        }
        let key = &text[start..i];
        i += 1;
        let mut value = String::new();
        if i < bytes.len() && bytes[i] == b'"' {
            i += 1;
            let value_start = i;
            let mut closed = false;
            while i < bytes.len() {
                match bytes[i] {
                    b'"' => {
                        value = unescape_quoted(&text[value_start..i])?;
                        i += 1;
                        closed = true;
                        break;
                    }
                    b'\\' if i + 1 < bytes.len() => {
                        i += 2;
                    }
                    _ => i += 1,
                }
            }
            if !closed || (i < bytes.len() && !bytes[i].is_ascii_whitespace()) {
                return Err(());
            }
        } else {
            let vstart = i;
            while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            value = text[vstart..i].to_owned();
        }
        // Preserve the ordinary parser's original admission boundary: the cap
        // counts distinct stored keys, and is checked before key validation.
        // Repeated keys below that cap remain legal and the last value wins.
        if fields.len() >= max_fields {
            return Ok((fields, true));
        }
        if key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
        {
            fields.insert(key.into(), Value::String(value));
        }
    }
    Ok((fields, false))
}

fn unescape_quoted(value: &str) -> Result<String, ()> {
    let mut output = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(character) = chars.next() {
        if character != '\\' {
            output.push(character);
            continue;
        }
        match chars.next() {
            Some('"') => output.push('"'),
            Some('\\') => output.push('\\'),
            _ => return Err(()),
        }
    }
    Ok(output)
}
fn diagnostic(
    out: &mut Vec<AdapterDiagnostic>,
    limit: usize,
    row: usize,
    field: Option<&str>,
    code: &str,
    message: &str,
) {
    if out.len() < limit {
        out.push(AdapterDiagnostic {
            row,
            field: field.map(str::to_owned),
            code: code.into(),
            message: message.into(),
        });
    }
}
fn stream_name(v: StreamKind) -> &'static str {
    match v {
        StreamKind::Stdout => "stdout",
        StreamKind::Stderr => "stderr",
        StreamKind::File => "file",
        StreamKind::Http => "http",
        StreamKind::Stdin => "stdin",
    }
}
fn chunk_name(v: ChunkPosition) -> &'static str {
    match v {
        ChunkPosition::Complete => "complete",
        ChunkPosition::Start => "start",
        ChunkPosition::Continue => "continue",
        ChunkPosition::End => "end",
    }
}
fn status_name(v: &ParseStatus) -> &'static str {
    match v {
        ParseStatus::Json => "json",
        ParseStatus::Logfmt => "logfmt",
        ParseStatus::Unstructured => "unstructured",
        ParseStatus::MalformedJson => "malformed_json",
        ParseStatus::UnsupportedLogfmt => "unsupported_logfmt",
        ParseStatus::ProjectionLimited => "projection_limited",
    }
}
