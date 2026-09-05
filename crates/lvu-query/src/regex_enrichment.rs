//! Native compilation of the leading-slash named-capture shorthand.

use crate::{
    CompiledDefinition, CompilerHost, EnrichmentStage, ExpressionKind, HostError, RAW_COLUMN,
};
use polars::prelude::{col, lit};
use regex::RegexBuilder;
use std::sync::atomic::AtomicBool;
use thiserror::Error;

pub const MAX_REGEX_ENRICHMENT_BYTES: usize = 16 * 1024;
pub const MAX_REGEX_ENRICHMENT_OUTPUTS: usize = 32;
pub const MAX_ENRICHMENT_NAME_BYTES: usize = 64;
const MAX_COMPILED_REGEX_BYTES: usize = 1024 * 1024;
const MAX_REGEX_NESTING: u32 = 64;
pub const MAX_ENRICHMENT_DEFINITIONS: usize = 32;
pub const MAX_ENRICHMENT_OUTPUTS: usize = 64;
pub const MAX_ENRICHMENT_STAGE_ID_BYTES: usize = 128;
pub const MAX_ENRICHMENT_SOURCE_BYTES: usize = 16 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EnrichmentStageId(pub String);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EnrichmentDefinition {
    pub id: EnrichmentStageId,
    pub source: String,
}

/// Lossless migration for the formerly singular persisted enrichment field.
pub fn legacy_enrichment_definition(source: impl Into<String>) -> EnrichmentDefinition {
    EnrichmentDefinition {
        id: EnrichmentStageId("legacy-enrichment".into()),
        source: source.into(),
    }
}

#[derive(Clone, Debug)]
pub struct CompiledEnrichment {
    pub definition: EnrichmentDefinition,
    pub outputs: Vec<EnrichmentOutput>,
    stages: Vec<EnrichmentStage>,
}

impl CompiledEnrichment {
    pub fn stages(&self) -> &[EnrichmentStage] {
        &self.stages
    }
    pub fn into_stages(self) -> Vec<EnrichmentStage> {
        self.stages
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EnrichmentOutput {
    pub name: String,
    pub capture_index: Option<usize>,
    /// Equivalent Python Polars source for display/recipe interoperability.
    /// Execution uses the cached native expression, not Python.
    pub expression_source: String,
}

#[derive(Clone, Debug)]
pub struct RegexEnrichmentPlan {
    original: String,
    pattern: String,
    flags: String,
    outputs: Vec<EnrichmentOutput>,
    stages: Vec<EnrichmentStage>,
}

impl RegexEnrichmentPlan {
    pub fn original(&self) -> &str {
        &self.original
    }
    pub fn pattern(&self) -> &str {
        &self.pattern
    }
    pub fn flags(&self) -> &str {
        &self.flags
    }
    pub fn outputs(&self) -> &[EnrichmentOutput] {
        &self.outputs
    }
    pub fn stages(&self) -> &[EnrichmentStage] {
        &self.stages
    }
    pub fn into_stages(self) -> Vec<EnrichmentStage> {
        self.stages
    }
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum RegexEnrichmentError {
    #[error("regex enrichment exceeds {MAX_REGEX_ENRICHMENT_BYTES} UTF-8 bytes")]
    TooLarge,
    #[error(
        "regex enrichment must end with an unescaped '/' followed only by optional i, m, or s flags"
    )]
    MissingDelimiter,
    #[error("invalid regex enrichment flag {0:?}; supported flags are i, m, and s")]
    InvalidFlag(char),
    #[error("duplicate regex enrichment flag {0:?}")]
    DuplicateFlag(char),
    #[error("invalid regex enrichment pattern: {0}")]
    InvalidPattern(String),
    #[error("regex enrichment requires at least one named capture such as (?P<request_id>...)")]
    NoNamedCaptures,
    #[error("regex enrichment has more than {MAX_REGEX_ENRICHMENT_OUTPUTS} named captures")]
    TooManyOutputs,
    #[error(
        "invalid or protected regex enrichment output name {0:?} (maximum {MAX_ENRICHMENT_NAME_BYTES} UTF-8 bytes)"
    )]
    InvalidName(String),
    #[error("duplicate regex enrichment output name {0:?}")]
    DuplicateName(String),
    #[error("native regex enrichment expression is unsupported: {0}")]
    Native(String),
}

#[derive(Debug, Error)]
pub enum EnrichmentCompileError {
    #[error(
        "invalid enrichment stage identity (must be nonempty and at most {MAX_ENRICHMENT_STAGE_ID_BYTES} UTF-8 bytes)"
    )]
    InvalidStageId,
    #[error("enrichment definition exceeds {MAX_ENRICHMENT_SOURCE_BYTES} UTF-8 bytes")]
    SourceTooLarge,
    #[error(transparent)]
    Regex(#[from] RegexEnrichmentError),
    #[error("use: name = Polars expression, or /pattern/ with named captures")]
    InvalidAssignment,
    #[error(
        "invalid or protected enrichment output name {0:?} (maximum {MAX_ENRICHMENT_NAME_BYTES} UTF-8 bytes)"
    )]
    InvalidOutputName(String),
    #[error("enrichment compiler is not configured")]
    CompilerUnavailable,
    #[error("enrichment chain has more than {MAX_ENRICHMENT_DEFINITIONS} definitions")]
    TooManyDefinitions,
    #[error("enrichment chain has more than {MAX_ENRICHMENT_OUTPUTS} output fields")]
    TooManyChainOutputs,
    #[error("duplicate enrichment stage identity {0:?}")]
    DuplicateStageId(String),
    #[error("duplicate enrichment output field {0:?}")]
    DuplicateOutput(String),
    #[error(transparent)]
    Host(#[from] HostError),
}

/// Compiles an ordered candidate chain atomically. Returned stages preserve
/// definition order and capture order, so later expressions may depend on prior
/// outputs. No caller should publish a partial result from an error.
pub fn compile_enrichment_chain(
    definitions: &[EnrichmentDefinition],
    mut host: Option<&mut CompilerHost>,
    cancel: &AtomicBool,
) -> Result<Vec<CompiledEnrichment>, EnrichmentCompileError> {
    if definitions.len() > MAX_ENRICHMENT_DEFINITIONS {
        return Err(EnrichmentCompileError::TooManyDefinitions);
    }
    let mut ids = std::collections::HashSet::with_capacity(definitions.len());
    let mut outputs = std::collections::HashSet::new();
    let mut output_count = 0usize;
    let mut compiled = Vec::with_capacity(definitions.len());
    for definition in definitions {
        if !ids.insert(definition.id.0.clone()) {
            return Err(EnrichmentCompileError::DuplicateStageId(
                definition.id.0.clone(),
            ));
        }
        let item = compile_enrichment_definition(definition.clone(), host.as_deref_mut(), cancel)?;
        output_count = output_count.saturating_add(item.outputs.len());
        if output_count > MAX_ENRICHMENT_OUTPUTS {
            return Err(EnrichmentCompileError::TooManyChainOutputs);
        }
        for output in &item.outputs {
            if !outputs.insert(output.name.clone()) {
                return Err(EnrichmentCompileError::DuplicateOutput(output.name.clone()));
            }
        }
        compiled.push(item);
    }
    Ok(compiled)
}

/// Compiles one ordered definition. Slash shorthand never starts Python; the
/// existing assignment form delegates to the configured compiler host.
pub fn compile_enrichment_definition(
    definition: EnrichmentDefinition,
    host: Option<&mut CompilerHost>,
    cancel: &AtomicBool,
) -> Result<CompiledEnrichment, EnrichmentCompileError> {
    if definition.id.0.trim().is_empty()
        || definition.id.0.len() > MAX_ENRICHMENT_STAGE_ID_BYTES
        || definition.id.0.chars().any(char::is_control)
    {
        return Err(EnrichmentCompileError::InvalidStageId);
    }
    if definition.source.len() > MAX_ENRICHMENT_SOURCE_BYTES {
        return Err(EnrichmentCompileError::SourceTooLarge);
    }
    if let Some(plan) = parse_regex_enrichment(&definition.source)? {
        let outputs = plan.outputs;
        return Ok(CompiledEnrichment {
            definition,
            outputs,
            stages: plan.stages,
        });
    }
    let Some((name, expression)) = definition.source.split_once('=') else {
        return Err(EnrichmentCompileError::InvalidAssignment);
    };
    let name = name.trim();
    validate_name(name).map_err(|_| EnrichmentCompileError::InvalidOutputName(name.into()))?;
    let name = name.to_owned();
    let expression = expression.trim().to_owned();
    let host = host.ok_or(EnrichmentCompileError::CompilerUnavailable)?;
    let compiled = host.compile(&expression, ExpressionKind::Enrichment, cancel)?;
    Ok(CompiledEnrichment {
        definition,
        outputs: vec![EnrichmentOutput {
            name: name.clone(),
            capture_index: None,
            expression_source: expression,
        }],
        stages: vec![EnrichmentStage {
            name,
            definition: compiled,
        }],
    })
}

/// Parses a leading-slash enrichment. `Ok(None)` preserves the existing
/// `name = Polars expression` syntax for its Python compiler path.
pub fn parse_regex_enrichment(
    source: &str,
) -> Result<Option<RegexEnrichmentPlan>, RegexEnrichmentError> {
    if !source.starts_with('/') {
        return Ok(None);
    }
    if source.len() > MAX_REGEX_ENRICHMENT_BYTES {
        return Err(RegexEnrichmentError::TooLarge);
    }
    let (pattern, flags) = split_pattern(source)?;
    let effective = if flags.is_empty() {
        pattern.clone()
    } else {
        format!("(?{flags}:{pattern})")
    };
    let regex = RegexBuilder::new(&effective)
        .size_limit(MAX_COMPILED_REGEX_BYTES)
        .nest_limit(MAX_REGEX_NESTING)
        .build()
        .map_err(|error| RegexEnrichmentError::InvalidPattern(error.to_string()))?;
    let mut outputs = Vec::new();
    let mut stages = Vec::new();
    for (capture_index, capture_name) in regex.capture_names().enumerate() {
        let Some(name) = capture_name else { continue };
        validate_name(name)?;
        if outputs
            .iter()
            .any(|output: &EnrichmentOutput| output.name == name)
        {
            return Err(RegexEnrichmentError::DuplicateName(name.into()));
        }
        if outputs.len() == MAX_REGEX_ENRICHMENT_OUTPUTS {
            return Err(RegexEnrichmentError::TooManyOutputs);
        }
        let expression_source = format!(
            "pl.col(\"raw\").str.extract({}, {capture_index})",
            python_string(&effective)
        );
        let expression = col(RAW_COLUMN)
            .str()
            .extract(lit(effective.clone()), capture_index);
        let definition = CompiledDefinition::compile_native(
            expression_source.clone(),
            expression,
            ExpressionKind::Enrichment,
        )
        .map_err(|error| RegexEnrichmentError::Native(error.to_string()))?;
        outputs.push(EnrichmentOutput {
            name: name.into(),
            capture_index: Some(capture_index),
            expression_source,
        });
        stages.push(EnrichmentStage {
            name: name.into(),
            definition,
        });
    }
    if outputs.is_empty() {
        return Err(RegexEnrichmentError::NoNamedCaptures);
    }
    Ok(Some(RegexEnrichmentPlan {
        original: source.into(),
        pattern: effective,
        flags,
        outputs,
        stages,
    }))
}

fn split_pattern(source: &str) -> Result<(String, String), RegexEnrichmentError> {
    let mut pattern = String::new();
    let mut chars = source[1..].chars().peekable();
    let mut closed = false;
    let mut suffix = String::new();
    while let Some(character) = chars.next() {
        match character {
            '\\' if chars.peek() == Some(&'/') => {
                chars.next();
                pattern.push('/');
            }
            '/' => {
                closed = true;
                suffix.extend(chars);
                break;
            }
            other => pattern.push(other),
        }
    }
    if !closed {
        return Err(RegexEnrichmentError::MissingDelimiter);
    }
    let mut flags = String::new();
    for flag in suffix.chars() {
        if !matches!(flag, 'i' | 'm' | 's') {
            return Err(RegexEnrichmentError::InvalidFlag(flag));
        }
        if flags.contains(flag) {
            return Err(RegexEnrichmentError::DuplicateFlag(flag));
        }
        flags.push(flag);
    }
    Ok((pattern, flags))
}

fn validate_name(name: &str) -> Result<(), RegexEnrichmentError> {
    if name.is_empty()
        || name.len() > MAX_ENRICHMENT_NAME_BYTES
        || name == "raw"
        || name == RAW_COLUMN
        || name.starts_with("_lvu_")
        || !name
            .chars()
            .all(|character| character == '_' || character.is_alphanumeric())
    {
        return Err(RegexEnrichmentError::InvalidName(name.into()));
    }
    Ok(())
}

fn python_string(value: &str) -> String {
    format!("{value:?}")
}
