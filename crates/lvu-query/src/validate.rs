use polars::prelude::*;
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const COMPATIBILITY_ID: &str = "polars-py-1.44.1-rs-0.55.2-expr-json-v1";
pub const PYTHON_POLARS_VERSION: &str = "1.44.1";
pub const MAX_EXPRESSION_JSON_BYTES: usize = 256 * 1024;
pub const MAX_EXPR_DEPTH: usize = 64;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExpressionKind {
    Filter,
    Enrichment,
    Color,
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum ValidationError {
    #[error("expression JSON exceeds {MAX_EXPRESSION_JSON_BYTES} bytes")]
    TooLarge,
    #[error("Polars Expr deserialization failed: {0}")]
    Deserialize(String),
    #[error("{0}")]
    Unsupported(String),
}

pub fn deserialize_and_validate(
    expression_json: &str,
    kind: ExpressionKind,
) -> Result<Expr, ValidationError> {
    if expression_json.len() > MAX_EXPRESSION_JSON_BYTES {
        return Err(ValidationError::TooLarge);
    }
    let expression: Expr = serde_json::from_str(expression_json)
        .map_err(|error| ValidationError::Deserialize(error.to_string()))?;
    validate_expression(&expression, kind, 0)?;
    Ok(expression)
}

pub fn validate_expression(
    expression: &Expr,
    kind: ExpressionKind,
    depth: usize,
) -> Result<(), ValidationError> {
    if depth > MAX_EXPR_DEPTH {
        return Err(ValidationError::Unsupported(format!(
            "expression exceeds maximum depth {MAX_EXPR_DEPTH}"
        )));
    }
    let next = depth + 1;
    match expression {
        Expr::Alias(input, name) => {
            if kind == ExpressionKind::Enrichment && name.as_str().starts_with("_lvu_") {
                return Err(ValidationError::Unsupported(format!(
                    "enrichment cannot write protected column {name:?}"
                )));
            }
            validate_expression(input, kind, next)
        }
        Expr::Column(_) => Ok(()),
        Expr::Literal(LiteralValue::Dyn(_) | LiteralValue::Scalar(_)) => Ok(()),
        Expr::BinaryExpr { left, right, .. } => {
            validate_expression(left, kind, next)?;
            validate_expression(right, kind, next)
        }
        Expr::Cast { expr, .. } => validate_expression(expr, kind, next),
        Expr::Ternary {
            predicate,
            truthy,
            falsy,
        } => {
            validate_expression(predicate, kind, next)?;
            validate_expression(truthy, kind, next)?;
            validate_expression(falsy, kind, next)
        }
        Expr::Function { input, function } => {
            validate_function(function)?;
            input
                .iter()
                .try_for_each(|child| validate_expression(child, kind, next))
        }
        _ => Err(ValidationError::Unsupported(
            "expression node is not in lvu's row-local allowlist".into(),
        )),
    }
}

fn validate_function(function: &FunctionExpr) -> Result<(), ValidationError> {
    match function {
        FunctionExpr::FillNull | FunctionExpr::Negate => Ok(()),
        FunctionExpr::Boolean(
            BooleanFunction::IsNull | BooleanFunction::IsNotNull | BooleanFunction::Not,
        ) => Ok(()),
        FunctionExpr::StringExpr(StringFunction::Extract(_) | StringFunction::Contains { .. }) => Ok(()),
        FunctionExpr::StringExpr(StringFunction::Strptime(_, options)) if options.format.is_some() => Ok(()),
        FunctionExpr::StringExpr(StringFunction::Strptime(_, _)) => Err(ValidationError::Unsupported(
            "string-to-date/time parsing requires an explicit format; inference varies by batch".into(),
        )),
        FunctionExpr::StructExpr(StructFunction::FieldByName(_)) => Ok(()),
        other => Err(ValidationError::Unsupported(format!(
            "function {other:?} is not in lvu's row-local allowlist"
        ))),
    }
}
