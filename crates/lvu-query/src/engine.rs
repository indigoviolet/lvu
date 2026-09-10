use std::collections::BTreeMap;

use lvu_core::ExactFieldConstraint;
use polars::prelude::*;
use serde::{Deserialize, Serialize};

use crate::{
    ExpressionKind, SEQUENCE_COLUMN, SOURCE_ID_COLUMN, deserialize_and_validate,
    validate_expression_for_frame,
};

#[derive(Clone, Debug)]
pub struct CompiledDefinition {
    pub source: String,
    kind: ExpressionKind,
    expression: Expr,
    dependencies: Vec<String>,
    compatibility_id: &'static str,
}

impl CompiledDefinition {
    pub(crate) fn compile_native(
        source: String,
        expression: Expr,
        kind: ExpressionKind,
    ) -> Result<Self, crate::ValidationError> {
        crate::validate_expression_candidate(&expression, kind)?;
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

// Names the columns a validated expression reads, for diagnostics only.
// Native Polars still evaluates; this never decides membership. It is complete
// for the row-local allowlist in `validate_expression_structure` (Column,
// Alias, Literal, BinaryExpr, Cast, Ternary, Function): any other `Expr`
// variant is rejected at compile time, so falling through with no names cannot
// hide a dependency of an expression that reaches execution.
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

/// First dependency that is neither failed nor present in this typed batch.
///
/// Uses the real expression dependencies against the actual current batch
/// schema (`frame.column`), never string parsing of the source and never a
/// global schema claim: absence here does not prove no stage produces the
/// column in another batch. A `Null` column still exists, so valid nulls are
/// not missing. A stage that ran and failed is reported via `failed_fields`,
/// not here. Pending command outputs never reach this path: the caller holds
/// the filter back until results exist, and revision staleness is fenced by
/// generation, not inferred here — so an absent name is reported per batch,
/// never as a race.
fn missing_dependency(
    frame: &DataFrame,
    dependencies: &[String],
    failed_fields: &[String],
) -> Option<String> {
    dependencies
        .iter()
        .find(|name| !failed_fields.contains(*name) && frame.column(name.as_str()).is_err())
        .cloned()
}

#[derive(Clone, Debug)]
pub struct EnrichmentStage {
    pub name: String,
    pub definition: CompiledDefinition,
}

#[derive(Clone, Debug)]
pub struct TextSearch {
    expression: Option<Expr>,
    ascii_expression: Option<Expr>,
    text: String,
    dependencies: Vec<String>,
    field: Option<String>,
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
        let ascii_expression = literal_ascii_expression(crate::RAW_COLUMN, &text);
        Ok(Self {
            expression,
            ascii_expression,
            text,
            dependencies: vec![crate::RAW_COLUMN.into()],
            field: Some(crate::RAW_COLUMN.into()),
        })
    }
    /// Search-box syntax. `new` remains the explicit legacy literal API.
    pub fn parse(text: String, compiled: Option<&CompiledDefinition>) -> Result<Self, String> {
        if text.len() > Self::MAX_BYTES {
            return Err("search exceeds 16 KiB".into());
        }
        if Self::is_polars(&text) {
            let compiled = compiled.ok_or("search expression compiler is not configured")?;
            return Ok(Self {
                expression: Some(
                    compiled
                        .expression(ExpressionKind::Filter)
                        .map_err(|e| e.to_string())?,
                ),
                ascii_expression: None,
                text,
                dependencies: compiled.dependencies().to_vec(),
                field: None,
            });
        }
        // JSON quoting addresses arbitrary field names without inventing an escape grammar.
        let quoted_field = if text.starts_with('"') {
            let mut parser = serde_json::Deserializer::from_str(&text).into_iter::<String>();
            match parser.next() {
                Some(Ok(field)) => text[parser.byte_offset()..]
                    .strip_prefix(": ")
                    .map(|value| (field, value)),
                _ => None,
            }
        } else {
            None
        };
        let (field, value) = quoted_field
            .as_ref()
            .map(|(field, value)| (field.as_str(), *value))
            .or_else(|| {
                text.split_once(": ").filter(|(field, _)| {
                    !field.is_empty()
                        && field.len() <= 64
                        && field
                            .chars()
                            .all(|c| c.is_alphanumeric() || "_.-".contains(c))
                })
            })
            .unwrap_or(("raw", text.as_str()));
        if field.len() > 64 {
            return Err("search field name exceeds 64 UTF-8 bytes".into());
        }
        let escaped_literal = value.strip_prefix(r"\/").map(|tail| format!("/{tail}"));
        let value = escaped_literal.as_deref().unwrap_or(value);
        let column = if field == "raw" {
            crate::RAW_COLUMN
        } else {
            field
        };
        let expression = if escaped_literal.is_none() && value.starts_with('/') {
            let (pattern, flags) =
                crate::regex_enrichment::split_pattern(value).map_err(|e| e.to_string())?;
            let pattern = if flags.is_empty() {
                pattern
            } else {
                format!("(?{flags}){pattern}")
            };
            regex::RegexBuilder::new(&pattern)
                .size_limit(1024 * 1024)
                .nest_limit(64)
                .build()
                .map_err(|e| format!("invalid search regex: {e}"))?;
            Some(
                col(column)
                    .cast(DataType::String)
                    .str()
                    .contains(lit(pattern), true),
            )
        } else if text.is_empty() {
            None
        } else {
            Some(
                col(column)
                    .cast(DataType::String)
                    .str()
                    .to_lowercase()
                    .str()
                    .contains_literal(lit(value.to_lowercase())),
            )
        };
        let ascii_expression = if escaped_literal.is_none() && !value.starts_with('/') {
            literal_ascii_expression(column, value)
        } else {
            None
        };
        Ok(Self {
            expression,
            ascii_expression,
            dependencies: vec![column.into()],
            field: Some(column.into()),
            text,
        })
    }
    pub fn is_polars(text: &str) -> bool {
        text.trim_start().starts_with("pl.") || text.trim_start().starts_with("(pl.")
    }
    pub fn requires_projection(&self) -> bool {
        self.dependencies
            .iter()
            .any(|field| field != crate::RAW_COLUMN)
    }
    pub fn dependencies(&self) -> &[String] {
        &self.dependencies
    }
    pub fn text(&self) -> &str {
        &self.text
    }
    /// The predicate this search contributes over `frame`, if any. `None`
    /// means no constraint (empty search). A field the frame does not carry
    /// yields a never-matching predicate, exactly as in batch execution.
    /// Union views evaluate their own text search through this, the same
    /// compiled predicate ordinary views use — never a second matcher.
    pub fn expression(&self, frame: &DataFrame) -> Option<Expr> {
        if self
            .field
            .as_ref()
            .is_some_and(|field| frame.column(field).is_err())
        {
            return Some(lit(false));
        }
        if let (Some(field), Some(expression)) = (&self.field, &self.ascii_expression)
            && frame
                .column(field)
                .ok()
                .and_then(|column| column.str().ok())
                .is_some_and(|column| column.iter().flatten().all(str::is_ascii))
        {
            return Some(expression.clone());
        }
        self.expression.clone()
    }

    /// Evaluate the rare non-ASCII rows with the legacy predicate without
    /// making the dominant ASCII rows pay for a lowercase copy. This is used
    /// only for a standalone text predicate; combined predicates retain the
    /// single-plan fallback.
    fn partitioned_mask(
        &self,
        frame: &DataFrame,
    ) -> Option<Result<BooleanChunked, (&'static str, String)>> {
        let field = self.field.as_ref()?;
        let fast = self.ascii_expression.clone()?;
        let fallback = self.expression.clone()?;
        let column = frame.column(field).ok()?.str().ok()?;
        let ascii_rows = column
            .iter()
            .map(|value| value.is_some_and(str::is_ascii))
            .collect::<Vec<_>>();
        if ascii_rows.iter().all(|ascii| *ascii) || ascii_rows.iter().all(|ascii| !*ascii) {
            return None;
        }
        let ascii_selector = BooleanChunked::from_slice("ascii".into(), &ascii_rows);
        let unicode_rows = ascii_rows.iter().map(|ascii| !ascii).collect::<Vec<_>>();
        let unicode_selector = BooleanChunked::from_slice("unicode".into(), &unicode_rows);
        Some((|| {
            let ascii_frame = frame
                .filter(&ascii_selector)
                .map_err(|error| ("evaluation_error", error.to_string()))?;
            let unicode_frame = frame
                .filter(&unicode_selector)
                .map_err(|error| ("evaluation_error", error.to_string()))?;
            let ascii_matches = predicate_mask_expr(&ascii_frame, fast)?;
            let unicode_matches = predicate_mask_expr(&unicode_frame, fallback)?;
            let mut ascii_index = 0usize;
            let mut unicode_index = 0usize;
            let values = ascii_rows.into_iter().map(|ascii| {
                if ascii {
                    let value = ascii_matches.get(ascii_index);
                    ascii_index += 1;
                    value
                } else {
                    let value = unicode_matches.get(unicode_index);
                    unicode_index += 1;
                    value
                }
            });
            Ok(BooleanChunked::from_iter_options(
                "text search".into(),
                values,
            ))
        })())
    }
}

/// Build the faster regex predicate only where its result is identical to
/// lowercasing both operands: an ASCII literal searched in an ASCII string
/// column. Unicode regex uses simple case folding, while `to_lowercase` can be
/// contextual or expand a scalar, so any non-ASCII batch stays on the legacy
/// expression to preserve membership.
fn literal_ascii_expression(column: &str, value: &str) -> Option<Expr> {
    (!value.is_empty() && value.is_ascii()).then(|| {
        let pattern = format!("(?i-u:{})", regex::escape(value));
        col(column)
            .cast(DataType::String)
            .str()
            .contains(lit(pattern), true)
    })
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
    /// Predicate colour rules, named by the caller. A rule is written in the
    /// search box's own language, so it arrives as a `TextSearch` — the same
    /// type, parsed by the same function, as the filter itself. There is one
    /// predicate evaluator here and rules go through it.
    pub colors: &'a [(String, TextSearch)],
}

pub struct BatchResult {
    pub generation: u64,
    pub definition_generation: u64,
    pub enriched_rows: DataFrame,
    pub matched_ids: Vec<StableRecordId>,
    pub color_matches: BTreeMap<String, Vec<StableRecordId>>,
    /// Diagnostics produced while evaluating colour predicates. Kept
    /// structurally separate because enrichment field names are unrestricted
    /// user data (including numeric names) and cannot identify provenance.
    pub color_diagnostics: Vec<QueryDiagnostic>,
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
    execute_batch_with_predicates(input, query, None, None)
}

/// Execute the ordinary query predicates AND one caller-provided native
/// expression. Union views use this for the canonical exact-key expression
/// over their already concatenated/deduplicated typed frame; evaluation,
/// Boolean validation and stable-ID selection remain in this engine.
pub fn execute_batch_with_native_predicate(
    input: &DataFrame,
    query: BatchQuery<'_>,
    native_predicate: Option<Expr>,
) -> BatchResult {
    execute_batch_with_predicates(input, query, None, native_predicate)
}

/// Executes an exact-field constraint in the same native Polars predicate plan
/// as the existing text and advanced filters. `input` must have been projected
/// with `records_to_batch_with_context_and_exact_field` for this field.
pub fn execute_batch_with_exact_constraint(
    input: &DataFrame,
    query: BatchQuery<'_>,
    exact_constraint: Option<&ExactFieldConstraint>,
) -> BatchResult {
    execute_batch_with_predicates(input, query, exact_constraint, None)
}

fn execute_batch_with_predicates(
    input: &DataFrame,
    query: BatchQuery<'_>,
    exact_constraint: Option<&ExactFieldConstraint>,
    native_predicate: Option<Expr>,
) -> BatchResult {
    let mut frame = input.clone();
    let expected_height = frame.height();
    let protected = protected_snapshot(&frame);
    let mut diagnostics = Vec::new();
    let mut failed_fields: Vec<String> = Vec::new();

    for (index, stage) in query.stages.iter().enumerate() {
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
        if let Some(missing) =
            missing_dependency(&frame, stage.definition.dependencies(), &failed_fields)
        {
            // A forward reference reads a column a later stage has not produced
            // yet in this batch. Name the ordering rather than claiming the
            // column is absent from the whole chain.
            let later = query.stages[index + 1..]
                .iter()
                .any(|later| later.name == missing);
            let message = if later {
                format!(
                    "enrichment {:?} depends on later stage {:?}; stages run in order",
                    stage.name, missing
                )
            } else {
                format!(
                    "enrichment {:?} needs {:?}, which is not available in this batch",
                    stage.name, missing
                )
            };
            diagnostics.push(error(Some(&stage.name), "unknown_field", &message));
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
        if let Err(failure) = validate_expression_for_frame(&frame, expression.clone()) {
            diagnostics.push(error(
                Some(&stage.name),
                "invalid_expression",
                &failure.to_string(),
            ));
            failed_fields.push(stage.name.clone());
            continue;
        }
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

    if let Err(message) = validate_identities(&frame) {
        diagnostics.push(error(None, "invalid_identity", &message));
        return BatchResult {
            generation: query.generation,
            definition_generation: query.definition_generation,
            enriched_rows: frame,
            matched_ids: Vec::new(),
            color_matches: BTreeMap::new(),
            color_diagnostics: Vec::new(),
            diagnostics,
            validity: BatchValidity::InvalidIdentity,
        };
    }
    if let Some(dependency) = query
        .filter
        .into_iter()
        .flat_map(|definition| definition.dependencies())
        .chain(
            query
                .text_search
                .into_iter()
                .flat_map(|search| search.dependencies()),
        )
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
            color_diagnostics: Vec::new(),
            diagnostics,
            validity: BatchValidity::InvalidFilter,
        };
    }
    // A filter naming a column absent from this typed batch is a field-naming
    // error, not a Polars implementation detail. Checked against the actual
    // current batch after the failed-stage guard, so failed, pending (held back
    // by the caller), valid-null (`Null` still has a column) and
    // stale-candidate cases stay distinct. Absence here does not prove no stage
    // produces the column in another batch, so the message stays per batch and
    // never claims a global schema or a race. Literal text search keeps its
    // lenient empty match and is not checked here. Native Polars still
    // evaluates every other predicate.
    if let Some(definition) = query.filter
        && let Some(missing) = missing_dependency(&frame, definition.dependencies(), &failed_fields)
    {
        diagnostics.push(error(
            None,
            "unknown_field",
            &format!("filter needs {missing:?}, which is not available in this batch"),
        ));
        return BatchResult {
            generation: query.generation,
            definition_generation: query.definition_generation,
            enriched_rows: frame,
            matched_ids: Vec::new(),
            color_matches: BTreeMap::new(),
            color_diagnostics: Vec::new(),
            diagnostics,
            validity: BatchValidity::InvalidFilter,
        };
    }
    let mut validity = BatchValidity::Valid;
    let partitioned_search =
        if query.filter.is_none() && exact_constraint.is_none() && native_predicate.is_none() {
            query
                .text_search
                .and_then(|search| search.partitioned_mask(&frame))
        } else {
            None
        };
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
        Ok(advanced) => match (
            query
                .text_search
                .and_then(|search| search.expression(&frame)),
            advanced,
        ) {
            (Some(search), Some(advanced)) => Some(search.and(advanced)),
            (Some(search), None) => Some(search),
            (None, advanced) => advanced,
        },
    };
    let predicate = match exact_constraint {
        None => predicate,
        Some(constraint) => match constraint
            .validate()
            .and_then(|_| constraint.value().exact_token())
        {
            Ok(token)
                if frame
                    .column(&crate::exact_value_column(constraint.field()))
                    .is_ok() =>
            {
                let exact = col(crate::exact_value_column(constraint.field())).eq(lit(token));
                Some(predicate.map_or(exact.clone(), |existing| existing.and(exact)))
            }
            Ok(_) => {
                diagnostics.push(error(
                    Some(constraint.field()),
                    "exact_projection_unavailable",
                    "batch was not projected for the requested exact field",
                ));
                validity = BatchValidity::InvalidFilter;
                None
            }
            Err(failure) => {
                diagnostics.push(error(
                    Some(constraint.field()),
                    "invalid_exact_constraint",
                    &failure.to_string(),
                ));
                validity = BatchValidity::InvalidFilter;
                None
            }
        },
    };
    let predicate = match (predicate, native_predicate) {
        (Some(existing), Some(native)) => Some(existing.and(native)),
        (None, native) => native,
        (existing, None) => existing,
    };
    let matched_ids = match partitioned_search {
        Some(Ok(mask)) => match selected_ids(&frame, Some(&mask)) {
            Ok(ids) => ids,
            Err(message) => {
                diagnostics.push(error(None, "invalid_identity", &message));
                validity = BatchValidity::InvalidIdentity;
                Vec::new()
            }
        },
        Some(Err(failure)) => {
            diagnostics.push(error(None, failure.0, &failure.1));
            validity = BatchValidity::InvalidFilter;
            Vec::new()
        }
        None => match predicate {
            None if validity == BatchValidity::Valid => match selected_ids(&frame, None) {
                Ok(ids) => ids,
                Err(message) => {
                    diagnostics.push(error(None, "invalid_identity", &message));
                    validity = BatchValidity::InvalidIdentity;
                    Vec::new()
                }
            },
            None => Vec::new(),
            Some(expression) => match predicate_mask_expr(&frame, expression).and_then(|mask| {
                selected_ids(&frame, Some(&mask)).map_err(|message| ("invalid_identity", message))
            }) {
                Ok(ids) => ids,
                Err(failure) => {
                    diagnostics.push(error(None, failure.0, &failure.1));
                    validity = BatchValidity::InvalidFilter;
                    Vec::new()
                }
            },
        },
    };
    let mut color_matches = BTreeMap::new();
    let mut color_diagnostics = Vec::new();
    for (name, definition) in query.colors {
        if let Some(dependency) = definition
            .dependencies()
            .iter()
            .find(|field| failed_fields.contains(field))
        {
            color_diagnostics.push(error(
                Some(name),
                "dependency_unavailable",
                &format!("color dependency {dependency:?} failed in this generation"),
            ));
            continue;
        }
        let mask = match definition.partitioned_mask(&frame) {
            Some(mask) => mask,
            None => {
                // An empty predicate matches nothing rather than everything: a
                // rule with no condition is not a rule that paints every row.
                let Some(expression) = definition.expression(&frame) else {
                    continue;
                };
                predicate_mask_expr(&frame, expression)
            }
        };
        match mask.and_then(|mask| {
            selected_ids(&frame, Some(&mask)).map_err(|message| ("invalid_identity", message))
        }) {
            Ok(ids) => {
                color_matches.insert(name.clone(), ids);
            }
            Err(failure) => {
                color_diagnostics.push(error(Some(name), failure.0, &failure.1));
            }
        }
    }
    BatchResult {
        generation: query.generation,
        definition_generation: query.definition_generation,
        enriched_rows: frame,
        matched_ids,
        color_matches,
        color_diagnostics,
        diagnostics,
        validity,
    }
}

fn predicate_mask_expr(
    frame: &DataFrame,
    expression: Expr,
) -> Result<BooleanChunked, (&'static str, String)> {
    validate_expression_for_frame(frame, expression.clone())
        .map_err(|e| ("invalid_expression", e.to_string()))?;
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

type IdentityColumns<'a> = (&'a StringChunked, &'a UInt64Chunked);

fn identity_columns(frame: &DataFrame) -> Result<IdentityColumns<'_>, String> {
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
    Ok((sources, sequences))
}

/// Every row carries a non-null identity and no identity repeats.
///
/// This is a per-batch backstop against an expression that shifts values under
/// stable-looking identities, so it has to see every row; what it must not do is
/// pay for a `String` and an ordered-set insertion per row to say so. Journal
/// pages arrive in strictly ascending identity order, and an ascending sequence
/// is already proof of uniqueness, so the ordinary case is one comparison per
/// row. Anything not in that order falls back to the complete set check rather
/// than assuming.
fn validate_identities(frame: &DataFrame) -> Result<(), String> {
    let (sources, sequences) = identity_columns(frame)?;
    let height = frame.height();
    let mut previous: Option<(&str, u64)> = None;
    let mut ascending = true;
    for index in 0..height {
        let current = row_identity(sources, sequences, index)?;
        if previous.is_some_and(|previous| previous >= current) {
            ascending = false;
            break;
        }
        previous = Some(current);
    }
    if ascending {
        return Ok(());
    }
    let mut unique = std::collections::HashSet::with_capacity(height);
    for index in 0..height {
        let current = row_identity(sources, sequences, index)?;
        if !unique.insert(current) {
            return Err(format!("row {index} has duplicate stable identity"));
        }
    }
    Ok(())
}

fn row_identity<'a>(
    sources: &'a StringChunked,
    sequences: &UInt64Chunked,
    index: usize,
) -> Result<(&'a str, u64), String> {
    let source_id = sources
        .get(index)
        .ok_or_else(|| format!("row {index} has null source identity"))?;
    let sequence = sequences
        .get(index)
        .ok_or_else(|| format!("row {index} has null sequence identity"))?;
    Ok((source_id, sequence))
}

/// Identities for the rows a mask selected, or for every row when there is none.
///
/// Owned identities are built only for rows a caller is actually going to keep,
/// because a selective filter over a large page would otherwise allocate one
/// string per scanned record and immediately drop nearly all of them.
fn selected_ids(
    frame: &DataFrame,
    mask: Option<&BooleanChunked>,
) -> Result<Vec<StableRecordId>, String> {
    let (sources, sequences) = identity_columns(frame)?;
    let mut ids = Vec::with_capacity(if mask.is_some() { 0 } else { frame.height() });
    for index in 0..frame.height() {
        if let Some(mask) = mask
            && mask.get(index) != Some(true)
        {
            continue;
        }
        let (source_id, sequence) = row_identity(sources, sequences, index)?;
        ids.push(StableRecordId {
            source_id: source_id.into(),
            sequence,
        });
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
