use std::io::{self, BufRead, Write};

use lvu_query::{
    COMPATIBILITY_ID, ExpressionKind, MAX_EXPRESSION_JSON_BYTES, ValidationError,
    deserialize_and_validate,
};
use polars::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

const MAX_REQUEST_BYTES: usize = 384 * 1024;

#[derive(Deserialize)]
struct Request {
    request_id: String,
    kind: String,
    compatibility_id: String,
    expression_json: String,
    #[serde(default)]
    batch_rows: Option<usize>,
}

#[derive(Serialize)]
struct Response {
    request_id: Option<String>,
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    dtype: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    values: Option<Vec<Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<ErrorBody>,
}

#[derive(Serialize)]
struct ErrorBody {
    code: &'static str,
    message: String,
}

fn fixture() -> PolarsResult<DataFrame> {
    let region = Series::new(
        "region".into(),
        &[Some("us-east"), None, Some("eu-west"), Some("ap-south")],
    );
    let attempt = Series::new("attempt".into(), &[1_i64, 2, 3, 4]);
    let nested = StructChunked::from_series("nested".into(), 4, [&region, &attempt].into_iter())?;
    DataFrame::new(
        4,
        vec![
            Series::new("amount".into(), &[Some(10_i64), None, Some(-3), Some(7)]).into(),
            Series::new("delta".into(), &[1_i64, 2, 4, -2]).into(),
            Series::new("status".into(), &[200_i64, 404, 500, 302]).into(),
            Series::new(
                "message".into(),
                &[
                    Some("ok user=amy"),
                    Some("bad user=bob"),
                    None,
                    Some("redirect"),
                ],
            )
            .into(),
            Series::new("code".into(), &[Some("12"), Some("bad"), None, Some("-7")]).into(),
            Series::new(
                "ts".into(),
                &[
                    Some("2026-01-02T03:04:05.123Z"),
                    Some("bad"),
                    None,
                    Some("2026-09-05T00:00:00.000Z"),
                ],
            )
            .into(),
            Series::new(
                "_lvu_raw".into(),
                &["ok", "error: missing", "ERROR", "redirect"],
            )
            .into(),
            nested.into_series().into(),
        ],
    )
}

fn dtype_name(dtype: &DataType) -> String {
    match dtype {
        DataType::Boolean => "Boolean".into(),
        DataType::Int64 => "Int64".into(),
        DataType::Float64 => "Float64".into(),
        DataType::String => "String".into(),
        DataType::Datetime(unit, zone) => format!("Datetime({unit:?},{zone:?})"),
        other => format!("{other:?}"),
    }
}

fn any_to_json(value: AnyValue<'_>) -> Result<Value, String> {
    Ok(match value {
        AnyValue::Null => Value::Null,
        AnyValue::Boolean(v) => json!(v),
        AnyValue::Int64(v) => json!(v),
        AnyValue::Float64(v) if v.is_finite() => json!(v),
        AnyValue::String(v) => json!(v),
        AnyValue::Datetime(v, _, _) => json!(v),
        other => {
            return Err(format!(
                "fixture output dtype is not supported by proof encoder: {other:?}"
            ));
        }
    })
}

fn execute(request: Request) -> Result<(String, Vec<Value>), ErrorBody> {
    if request.request_id.is_empty() || request.request_id.len() > 256 {
        return Err(ErrorBody {
            code: "invalid_request_id",
            message: "request_id must be a non-empty string of at most 256 bytes".into(),
        });
    }
    if request.compatibility_id != COMPATIBILITY_ID {
        return Err(ErrorBody {
            code: "incompatible_expression",
            message: format!("expected compatibility_id {COMPATIBILITY_ID}"),
        });
    }
    if request.expression_json.len() > MAX_EXPRESSION_JSON_BYTES {
        return Err(ErrorBody {
            code: "expression_json_too_large",
            message: format!("expression JSON exceeds {MAX_EXPRESSION_JSON_BYTES} bytes"),
        });
    }
    if !matches!(request.kind.as_str(), "filter" | "enrichment" | "color") {
        return Err(ErrorBody {
            code: "invalid_kind",
            message: "kind must be filter, enrichment, or color".into(),
        });
    }
    // serde_json's default recursion limit is retained in addition to the
    // explicit expression-tree depth check below.
    let kind = match request.kind.as_str() {
        "filter" => ExpressionKind::Filter,
        "enrichment" => ExpressionKind::Enrichment,
        "color" => ExpressionKind::Color,
        _ => unreachable!(),
    };
    let expression = deserialize_and_validate(&request.expression_json, kind).map_err(|error| {
        let code = match error {
            ValidationError::Deserialize(_) => "incompatible_expression",
            ValidationError::TooLarge => "expression_json_too_large",
            ValidationError::Unsupported(_) => "non_row_local_expression",
        };
        ErrorBody {
            code,
            message: error.to_string(),
        }
    })?;
    let frame = fixture().map_err(|e| ErrorBody {
        code: "evaluation_error",
        message: e.to_string(),
    })?;
    let batch_rows = request.batch_rows.unwrap_or(frame.height());
    if batch_rows == 0 || batch_rows > frame.height() {
        return Err(ErrorBody {
            code: "invalid_batch_rows",
            message: format!("batch_rows must be between 1 and {}", frame.height()),
        });
    }
    let mut result: Option<Series> = None;
    for offset in (0..frame.height()).step_by(batch_rows) {
        let batch = frame.slice(offset as i64, batch_rows.min(frame.height() - offset));
        let output = batch
            .lazy()
            .select([expression.clone().alias("value")])
            .collect()
            .map_err(|e| ErrorBody {
                code: "evaluation_error",
                message: e.to_string(),
            })?;
        let series = output.column("value").map_err(|e| ErrorBody {
            code: "evaluation_error",
            message: e.to_string(),
        })?;
        if let Some(accumulated) = &mut result {
            accumulated
                .append(series.as_materialized_series())
                .map_err(|e| ErrorBody {
                    code: "evaluation_error",
                    message: e.to_string(),
                })?;
        } else {
            result = Some(series.as_materialized_series().clone());
        }
    }
    let series = result.expect("non-empty fixture");
    if series.len() != 4 {
        return Err(ErrorBody {
            code: "row_alignment_error",
            message: format!(
                "row-local expression returned {} rows, expected 4",
                series.len()
            ),
        });
    }
    let values = series
        .iter()
        .map(any_to_json)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|message| ErrorBody {
            code: "encoding_error",
            message,
        })?;
    Ok((dtype_name(series.dtype()), values))
}

fn next_bounded_line(reader: &mut impl BufRead) -> io::Result<Option<Result<Vec<u8>, ()>>> {
    let mut line = Vec::new();
    let mut oversized = false;
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return if line.is_empty() && !oversized {
                Ok(None)
            } else {
                Ok(Some(if oversized { Err(()) } else { Ok(line) }))
            };
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let consumed = newline.map_or(available.len(), |position| position + 1);
        if !oversized {
            if line.len() + consumed > MAX_REQUEST_BYTES {
                oversized = true;
                line.clear();
            } else {
                line.extend_from_slice(&available[..consumed]);
            }
        }
        reader.consume(consumed);
        if newline.is_some() {
            if line.last() == Some(&b'\n') {
                line.pop();
            }
            return Ok(Some(if oversized { Err(()) } else { Ok(line) }));
        }
    }
}

fn main() {
    let stdin = io::stdin();
    let mut input = stdin.lock();
    let mut stdout = io::BufWriter::new(io::stdout().lock());
    while let Some(line) = next_bounded_line(&mut input).unwrap_or_else(|error| {
        eprintln!("stdin read failed: {error}");
        None
    }) {
        let response = match line {
            Ok(line) => match serde_json::from_slice::<Request>(&line) {
                Ok(request) => {
                    let request_id = request.request_id.clone();
                    match execute(request) {
                        Ok((dtype, values)) => Response {
                            request_id: Some(request_id),
                            ok: true,
                            dtype: Some(dtype),
                            values: Some(values),
                            error: None,
                        },
                        Err(error) => Response {
                            request_id: Some(request_id),
                            ok: false,
                            dtype: None,
                            values: None,
                            error: Some(error),
                        },
                    }
                }
                Err(error) => Response {
                    request_id: None,
                    ok: false,
                    dtype: None,
                    values: None,
                    error: Some(ErrorBody {
                        code: "invalid_request",
                        message: error.to_string(),
                    }),
                },
            },
            Err(()) => Response {
                request_id: None,
                ok: false,
                dtype: None,
                values: None,
                error: Some(ErrorBody {
                    code: "request_too_large",
                    message: format!("request line exceeds {MAX_REQUEST_BYTES} bytes"),
                }),
            },
        };
        serde_json::to_writer(&mut stdout, &response).expect("stdout JSON serialization");
        writeln!(stdout).expect("stdout write");
        stdout.flush().expect("stdout flush");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_tampered_aggregate_before_deserialization() {
        let value = json!({"Agg": {"Sum": {"Column": "amount"}}});
        let expression: Expr = serde_json::from_value(value).unwrap();
        assert!(
            lvu_query::validate_expression(&expression, ExpressionKind::Enrichment, 0).is_err()
        );
    }

    #[test]
    fn bounded_reader_resynchronizes_after_oversized_line() {
        let mut bytes = vec![b'x'; MAX_REQUEST_BYTES + 1];
        bytes.extend_from_slice(b"\nnext\n");
        let mut reader = io::BufReader::new(bytes.as_slice());
        assert_eq!(next_bounded_line(&mut reader).unwrap(), Some(Err(())));
        assert_eq!(
            next_bounded_line(&mut reader).unwrap(),
            Some(Ok(b"next".to_vec()))
        );
    }

    #[test]
    fn fixture_has_stable_row_identity_order() {
        let frame = fixture().unwrap();
        assert_eq!(frame.height(), 4);
        assert_eq!(
            frame.column("status").unwrap().i64().unwrap().get(1),
            Some(404)
        );
    }
}
