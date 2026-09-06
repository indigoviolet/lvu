use polars::prelude::*;
use polars_plan::plans::ArenaExprIter;
use polars_plan::prelude::{
    AExpr, Arena, ExprToIRContext, IR, Node, is_elementwise_rec, to_expr_ir,
};
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
    validate_expression_candidate(&expression, kind)?;
    Ok(expression)
}

pub(crate) fn validate_expression_candidate(
    expression: &Expr,
    kind: ExpressionKind,
) -> Result<(), ValidationError> {
    validate_expression_structure(expression, kind, 0)?;
    let mut arena = Arena::new();
    let schema = Schema::default();
    let mut context = ExprToIRContext::new_no_verification(&mut arena, &schema);
    let lowered = to_expr_ir(expression.clone(), &mut context).map_err(|error| {
        ValidationError::Unsupported(format!(
            "expression cannot be converted for locality validation: {error}"
        ))
    })?;
    validate_lowered_root(lowered.node(), &arena).map_err(|error| {
        ValidationError::Unsupported(format!(
            "expression cannot be proven row-local before publication: {error}"
        ))
    })
}

fn validate_expression_structure(
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
            validate_expression_structure(input, kind, next)
        }
        Expr::Column(_) => Ok(()),
        Expr::Literal(LiteralValue::Dyn(_) | LiteralValue::Scalar(_)) => Ok(()),
        Expr::BinaryExpr { left, right, .. } => {
            validate_expression_structure(left, kind, next)?;
            validate_expression_structure(right, kind, next)
        }
        Expr::Cast { expr, .. } => validate_expression_structure(expr, kind, next),
        Expr::Ternary {
            predicate,
            truthy,
            falsy,
        } => {
            validate_expression_structure(predicate, kind, next)?;
            validate_expression_structure(truthy, kind, next)?;
            validate_expression_structure(falsy, kind, next)
        }
        Expr::Function { input, function } => {
            validate_function(function)?;
            input
                .iter()
                .try_for_each(|child| validate_expression_structure(child, kind, next))
        }
        _ => Err(ValidationError::Unsupported(
            "expression node is not in lvu's row-local allowlist".into(),
        )),
    }
}

fn validate_function(function: &FunctionExpr) -> Result<(), ValidationError> {
    match function {
        FunctionExpr::StringExpr(StringFunction::Strptime(_, options)) if options.format.is_some() => Ok(()),
        FunctionExpr::StringExpr(StringFunction::Strptime(_, _)) => Err(ValidationError::Unsupported(
            "string-to-date/time parsing requires an explicit format; inference varies by batch".into(),
        )),
        _ => Ok(()),
    }
}

/// Validate the expression after Polars has resolved it against the real batch
/// schema.  Elementwise means both row-separable and length-preserving in the
/// pinned Polars plan contract; either property alone is insufficient for lvu.
pub fn validate_expression_for_frame(
    frame: &DataFrame,
    expression: Expr,
) -> Result<(), ValidationError> {
    let plan = frame
        .clone()
        .lazy()
        .select([expression])
        .to_alp()
        .map_err(|error| {
            ValidationError::Unsupported(format!(
                "expression cannot be lowered for this schema: {error}"
            ))
        })?;
    let IR::Select { expr: exprs, .. } = plan.lp_arena.get(plan.lp_top) else {
        return Err(ValidationError::Unsupported(
            "expression lowering did not produce a projection".into(),
        ));
    };
    for expression in exprs {
        validate_lowered_root(expression.node(), &plan.expr_arena)?;
    }
    Ok(())
}

fn validate_lowered_root(root: Node, arena: &Arena<AExpr>) -> Result<(), ValidationError> {
    if !is_elementwise_rec(root, arena) {
        return Err(ValidationError::Unsupported(
            "expression is not row-separable and length-preserving".into(),
        ));
    }
    for (_, node) in arena.iter(root) {
        match node {
            AExpr::Function {
                function, options, ..
            } => {
                if !function.function_options().is_elementwise() || !options.is_elementwise() {
                    return Err(ValidationError::Unsupported(
                        "expression contains a non-elementwise function".into(),
                    ));
                }
            }
            AExpr::AnonymousFunction { .. } => {
                return Err(ValidationError::Unsupported(
                    "anonymous functions and plugins are unavailable".into(),
                ));
            }
            _ => {}
        }
    }
    Ok(())
}
