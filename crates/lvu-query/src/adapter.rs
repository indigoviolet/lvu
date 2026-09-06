use lvu_core::{ChunkPosition, RawRecord, StreamKind};
use polars::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const RAW_ALIAS: &str = "raw";
pub const RAW_COLUMN: &str = "_lvu_raw";
pub const RAW_BYTES_COLUMN: &str = "_lvu_raw_bytes";
pub const SOURCE_ID_COLUMN: &str = "_lvu_source_id";
pub const SEQUENCE_COLUMN: &str = "_lvu_sequence";

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

pub fn records_to_batch(records: &[RawRecord]) -> PolarsResult<RecordBatch> {
    records_to_batch_with_context(records, &mut SchemaContext::default())
}
pub fn records_to_batch_with_context(
    records: &[RawRecord],
    schema: &mut SchemaContext,
) -> PolarsResult<RecordBatch> {
    let mut rows = Vec::with_capacity(records.len());
    let mut statuses = Vec::with_capacity(records.len());
    let mut diagnostics = Vec::new();
    for (row, record) in records.iter().enumerate() {
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
        let (status, fields, warning) = parse_fields(&text, schema.max_fields);
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
                for (key, value) in object.into_iter().take(max_fields) {
                    fields.insert(key, json_value(value));
                }
                (ParseStatus::Json, fields, None)
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
        Ok(fields) if fields.is_empty() => (ParseStatus::Unstructured, fields, None),
        Ok(fields) => (ParseStatus::Logfmt, fields, None),
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
fn parse_logfmt(text: &str, max_fields: usize) -> Result<BTreeMap<String, Value>, ()> {
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
        if fields.len() >= max_fields {
            return Err(());
        }
        if key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
        {
            fields.insert(key.into(), Value::String(value));
        }
    }
    Ok(fields)
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
