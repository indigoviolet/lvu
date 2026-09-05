use std::collections::BTreeMap;

use polars::prelude::*;
use serde::{Deserialize, Serialize};

use crate::{ExpressionKind, SEQUENCE_COLUMN, SOURCE_ID_COLUMN, deserialize_and_validate};

#[derive(Clone, Debug)]
pub struct CompiledDefinition {
    pub source: String,
    kind: ExpressionKind,
    expression: Expr,
    dependencies: Vec<String>,
    compatibility_id: &'static str,
}

impl CompiledDefinition {
    pub fn compile(
        source: String,
        expression_json: &str,
        kind: ExpressionKind,
    ) -> Result<Self, crate::ValidationError> {
        let expression = deserialize_and_validate(expression_json, kind)?;
        let mut dependencies = Vec::new();
        collect_dependencies(&expression, &mut dependencies);
        dependencies.sort();
        dependencies.dedup();
        Ok(Self {
            source,
            kind,
            expression,
            dependencies,
            compatibility_id: crate::COMPATIBILITY_ID,
        })
    }
    pub fn expression(&self, kind: ExpressionKind) -> Result<Expr, crate::ValidationError> {
        if self.kind != kind {
            return Err(crate::ValidationError::Unsupported(format!(
                "compiled {:?} expression cannot be used as {:?}",
                self.kind, kind
            )));
        }
        Ok(self.expression.clone())
    }
    pub fn kind(&self) -> ExpressionKind {
        self.kind
    }
    pub fn dependencies(&self) -> &[String] {
        &self.dependencies
    }
    pub fn compatibility_id(&self) -> &str {
        self.compatibility_id
    }
}

fn collect_dependencies(expression: &Expr, output: &mut Vec<String>) {
    match expression {
        Expr::Column(name) => output.push(name.to_string()),
        Expr::Alias(child, _) | Expr::Cast { expr: child, .. } => {
            collect_dependencies(child, output)
        }
        Expr::BinaryExpr { left, right, .. } => {
            collect_dependencies(left, output);
            collect_dependencies(right, output);
        }
        Expr::Ternary {
            predicate,
            truthy,
            falsy,
        } => {
            collect_dependencies(predicate, output);
            collect_dependencies(truthy, output);
            collect_dependencies(falsy, output);
        }
        Expr::Function { input, .. } => input
            .iter()
            .for_each(|child| collect_dependencies(child, output)),
        _ => {}
    }
}

#[derive(Clone, Debug)]
pub struct EnrichmentStage {
    pub name: String,
    pub definition: CompiledDefinition,
}

#[derive(Clone, Debug)]
pub struct TextSearch {
    expression: Option<Expr>,
    text: String,
}
#[derive(Clone, Debug, thiserror::Error, Eq, PartialEq)]
#[error("text search exceeds {maximum} UTF-8 bytes")]
pub struct TextSearchError {
    pub maximum: usize,
}
impl TextSearch {
    pub const MAX_BYTES: usize = 16 * 1024;
    pub fn new(text: impl Into<String>) -> Result<Self, TextSearchError> {
        let text = text.into();
        if text.len() > Self::MAX_BYTES {
            return Err(TextSearchError {
                maximum: Self::MAX_BYTES,
            });
        }
        let expression = if text.is_empty() {
            None
        } else {
            Some(
                col(crate::RAW_COLUMN)
                    .str()
                    .to_lowercase()
                    .str()
                    .contains_literal(lit(text.to_lowercase())),
            )
        };
        Ok(Self { expression, text })
    }
    pub fn text(&self) -> &str {
        &self.text
    }
    fn expression(&self) -> Option<Expr> {
        self.expression.clone()
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DerivedState {
    Pending,
    Ready,
    Unmatched,
    Error,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct QueryDiagnostic {
    pub field: Option<String>,
    pub state: DerivedState,
    pub code: String,
    pub message: String,
}

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct StableRecordId {
    pub source_id: String,
    pub sequence: u64,
}

pub struct BatchQuery<'a> {
    pub generation: u64,
    pub definition_generation: u64,
    pub stages: &'a [EnrichmentStage],
    pub filter: Option<&'a CompiledDefinition>,
    pub text_search: Option<&'a TextSearch>,
    pub colors: &'a [(String, CompiledDefinition)],
}

pub struct BatchResult {
    pub generation: u64,
    pub definition_generation: u64,
    pub enriched_rows: DataFrame,
    pub matched_ids: Vec<StableRecordId>,
    pub color_matches: BTreeMap<String, Vec<StableRecordId>>,
    pub diagnostics: Vec<QueryDiagnostic>,
    pub validity: BatchValidity,
}

/// Extracts a bounded caller-selected display projection aligned by the
/// protected stable identity columns. Null values remain null.
pub fn scalar_projection(
    frame: &DataFrame,
    name: &str,
    maximum_bytes: usize,
) -> Result<Vec<(StableRecordId, Option<String>)>, String> {
    let (Ok(sources), Ok(sequences), Ok(values)) = (
        frame.column(SOURCE_ID_COLUMN),
        frame.column(SEQUENCE_COLUMN),
        frame.column(name),
    ) else {
        return Err(format!("projection column {name:?} is unavailable"));
    };
    let mut projected = Vec::with_capacity(frame.height());
    for index in 0..frame.height() {
        let source_id = sources
            .get(index)
            .map_err(|error| error.to_string())?
            .get_str()
            .ok_or_else(|| "source identity is not a string".to_owned())?
            .to_owned();
        let sequence = sequences
            .get(index)
            .map_err(|error| error.to_string())?
            .try_extract::<u64>()
            .map_err(|error| error.to_string())?;
        let value = {
            let mut text = match values.get(index).map_err(|error| error.to_string())? {
                AnyValue::Null => {
                    projected.push((
                        StableRecordId {
                            source_id,
                            sequence,
                        },
                        None,
                    ));
                    continue;
                }
                AnyValue::String(value) => value.to_owned(),
                AnyValue::StringOwned(value) => value.as_str().to_owned(),
                AnyValue::Boolean(value) => value.to_string(),
                AnyValue::UInt8(value) => value.to_string(),
                AnyValue::UInt16(value) => value.to_string(),
                AnyValue::UInt32(value) => value.to_string(),
                AnyValue::UInt64(value) => value.to_string(),
                AnyValue::UInt128(value) => value.to_string(),
                AnyValue::Int8(value) => value.to_string(),
                AnyValue::Int16(value) => value.to_string(),
                AnyValue::Int32(value) => value.to_string(),
                AnyValue::Int64(value) => value.to_string(),
                AnyValue::Int128(value) => value.to_string(),
                AnyValue::Float16(value) => value.to_string(),
                AnyValue::Float32(value) => value.to_string(),
                AnyValue::Float64(value) => value.to_string(),
                unsupported => {
                    return Err(format!(
                        "unsupported enrichment output type {:?}",
                        unsupported.dtype()
                    ));
                }
            };
            if text.len() > maximum_bytes {
                let mut end = maximum_bytes;
                while !text.is_char_boundary(end) {
                    end -= 1;
                }
                text.truncate(end);
            }
            Some(text)
        };
        projected.push((
            StableRecordId {
                source_id,
                sequence,
            },
            value,
        ));
    }
    Ok(projected)
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BatchValidity {
    Valid,
    InvalidIdentity,
    InvalidFilter,
}

pub fn execute_batch(input: &DataFrame, query: BatchQuery<'_>) -> BatchResult {
    let mut frame = input.clone();
    let expected_height = frame.height();
    let protected = protected_snapshot(&frame);
    let mut diagnostics = Vec::new();
    let mut failed_fields: Vec<String> = Vec::new();

    for stage in query.stages {
        if stage.name.starts_with("_lvu_") || stage.name == "raw" {
            diagnostics.push(error(
                Some(&stage.name),
                "protected_column",
                "enrichment output name is protected",
            ));
            failed_fields.push(stage.name.clone());
            continue;
        }
        if let Some(dependency) = stage
            .definition
            .dependencies()
            .iter()
            .find(|name| failed_fields.contains(name))
        {
            diagnostics.push(error(
                Some(&stage.name),
                "dependency_unavailable",
                &format!("dependency {dependency:?} failed in this generation"),
            ));
            failed_fields.push(stage.name.clone());
            continue;
        }
        let expression = match stage.definition.expression(ExpressionKind::Enrichment) {
            Ok(expression) => expression.alias(&stage.name),
            Err(failure) => {
                diagnostics.push(error(
                    Some(&stage.name),
                    "invalid_expression",
                    &failure.to_string(),
                ));
                failed_fields.push(stage.name.clone());
                continue;
            }
        };
        match frame.clone().lazy().with_columns([expression]).collect() {
            Ok(candidate)
                if candidate.height() == expected_height
                    && protected_unchanged(&candidate, &protected) =>
            {
                frame = candidate;
                diagnostics.push(QueryDiagnostic {
                    field: Some(stage.name.clone()),
                    state: DerivedState::Ready,
                    code: "ready".into(),
                    message: "stage applied".into(),
                });
            }
            Ok(_) => {
                diagnostics.push(error(
                    Some(&stage.name),
                    "row_alignment",
                    "stage changed row cardinality or protected metadata",
                ));
                failed_fields.push(stage.name.clone());
            }
            Err(failure) => {
                diagnostics.push(error(
                    Some(&stage.name),
                    "evaluation_error",
                    &failure.to_string(),
                ));
                failed_fields.push(stage.name.clone());
            }
        }
    }

    let all_ids = match ids(&frame) {
        Ok(ids) => ids,
        Err(message) => {
            diagnostics.push(error(None, "invalid_identity", &message));
            return BatchResult {
                generation: query.generation,
                definition_generation: query.definition_generation,
                enriched_rows: frame,
                matched_ids: Vec::new(),
                color_matches: BTreeMap::new(),
                diagnostics,
                validity: BatchValidity::InvalidIdentity,
            };
        }
    };
    if let Some(definition) = query.filter
        && let Some(dependency) = definition
            .dependencies()
            .iter()
            .find(|name| failed_fields.contains(name))
    {
        diagnostics.push(error(
            None,
            "dependency_unavailable",
            &format!("filter dependency {dependency:?} failed in this generation"),
        ));
        return BatchResult {
            generation: query.generation,
            definition_generation: query.definition_generation,
            enriched_rows: frame,
            matched_ids: Vec::new(),
            color_matches: BTreeMap::new(),
            diagnostics,
            validity: BatchValidity::InvalidFilter,
        };
    }
    let mut validity = BatchValidity::Valid;
    let advanced = query
        .filter
        .map(|definition| definition.expression(ExpressionKind::Filter))
        .transpose();
    let predicate = match advanced {
        Err(failure) => {
            diagnostics.push(error(None, "invalid_expression", &failure.to_string()));
            validity = BatchValidity::InvalidFilter;
            None
        }
        Ok(advanced) => match (query.text_search.and_then(TextSearch::expression), advanced) {
            (Some(search), Some(advanced)) => Some(search.and(advanced)),
            (Some(search), None) => Some(search),
            (None, advanced) => advanced,
        },
    };
    let matched_ids = match predicate {
        None if validity == BatchValidity::Valid => all_ids.clone(),
        None => Vec::new(),
        Some(expression) => match predicate_mask_expr(&frame, expression) {
            Ok(mask) => all_ids
                .iter()
                .zip(mask.iter())
                .filter(|(_, value)| *value == Some(true))
                .map(|(id, _)| id.clone())
                .collect(),
            Err(failure) => {
                diagnostics.push(error(None, failure.0, &failure.1));
                validity = BatchValidity::InvalidFilter;
                Vec::new()
            }
        },
    };
    let mut color_matches = BTreeMap::new();
    for (name, definition) in query.colors {
        if let Some(dependency) = definition
            .dependencies()
            .iter()
            .find(|field| failed_fields.contains(field))
        {
            diagnostics.push(error(
                Some(name),
                "dependency_unavailable",
                &format!("color dependency {dependency:?} failed in this generation"),
            ));
            continue;
        }
        match predicate_mask(&frame, definition, ExpressionKind::Color) {
            Ok(mask) => {
                color_matches.insert(
                    name.clone(),
                    all_ids
                        .iter()
                        .zip(mask.iter())
                        .filter(|(_, value)| *value == Some(true))
                        .map(|(id, _)| id.clone())
                        .collect(),
                );
            }
            Err(failure) => diagnostics.push(error(Some(name), failure.0, &failure.1)),
        }
    }
    BatchResult {
        generation: query.generation,
        definition_generation: query.definition_generation,
        enriched_rows: frame,
        matched_ids,
        color_matches,
        diagnostics,
        validity,
    }
}

fn predicate_mask(
    frame: &DataFrame,
    definition: &CompiledDefinition,
    kind: ExpressionKind,
) -> Result<BooleanChunked, (&'static str, String)> {
    let expression = definition
        .expression(kind)
        .map_err(|e| ("invalid_expression", e.to_string()))?;
    predicate_mask_expr(frame, expression)
}
fn predicate_mask_expr(
    frame: &DataFrame,
    expression: Expr,
) -> Result<BooleanChunked, (&'static str, String)> {
    let output = frame
        .clone()
        .lazy()
        .with_columns([expression.alias("_lvu_internal_predicate")])
        .collect()
        .map_err(|e| ("evaluation_error", e.to_string()))?;
    let column = output
        .column("_lvu_internal_predicate")
        .map_err(|e| ("evaluation_error", e.to_string()))?;
    if column.dtype() != &DataType::Boolean {
        return Err((
            "predicate_not_boolean",
            format!("predicate returned {:?}, expected Boolean", column.dtype()),
        ));
    }
    column
        .bool()
        .cloned()
        .map_err(|e| ("predicate_not_boolean", e.to_string()))
}

fn ids(frame: &DataFrame) -> Result<Vec<StableRecordId>, String> {
    let sources = frame
        .column(SOURCE_ID_COLUMN)
        .map_err(|e| e.to_string())?
        .str()
        .map_err(|e| format!("source identity must be String: {e}"))?;
    let sequences = frame
        .column(SEQUENCE_COLUMN)
        .map_err(|e| e.to_string())?
        .u64()
        .map_err(|e| format!("sequence identity must be UInt64: {e}"))?;
    let mut ids = Vec::with_capacity(frame.height());
    let mut unique = std::collections::BTreeSet::new();
    for index in 0..frame.height() {
        let source_id = sources
            .get(index)
            .ok_or_else(|| format!("row {index} has null source identity"))?;
        let sequence = sequences
            .get(index)
            .ok_or_else(|| format!("row {index} has null sequence identity"))?;
        let id = StableRecordId {
            source_id: source_id.into(),
            sequence,
        };
        if !unique.insert(id.clone()) {
            return Err(format!("row {index} has duplicate stable identity"));
        }
        ids.push(id);
    }
    Ok(ids)
}

fn protected_snapshot(frame: &DataFrame) -> Vec<(String, Series)> {
    frame
        .get_column_names()
        .iter()
        .filter(|name| name.starts_with("_lvu_") || name.as_str() == "raw")
        .filter_map(|name| {
            frame
                .column(name)
                .ok()
                .map(|column| (name.to_string(), column.as_materialized_series().clone()))
        })
        .collect()
}
fn protected_unchanged(frame: &DataFrame, before: &[(String, Series)]) -> bool {
    before.iter().all(|(name, series)| {
        frame
            .column(name)
            .is_ok_and(|column| column.as_materialized_series().equals_missing(series))
    })
}
fn error(field: Option<&str>, code: &str, message: &str) -> QueryDiagnostic {
    QueryDiagnostic {
        field: field.map(str::to_owned),
        state: DerivedState::Error,
        code: code.into(),
        message: message.chars().take(512).collect(),
    }
}
