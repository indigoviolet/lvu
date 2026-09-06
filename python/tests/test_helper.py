import json
import io
import subprocess
import sys

import pytest
import polars as pl

from lvu_expr_helper.helper import MAX_REQUEST_BYTES, compile_expression, handle


@pytest.mark.parametrize("method, expected", [
    ("to_uppercase", ["HELLO", "STRASSE", "ÉTÉ", "", None]),
    ("to_lowercase", ["hello", "straße", "été", "", None]),
])
def test_case_conversion_preserves_rows_nulls_and_unicode(method, expected) -> None:
    source = f'pl.col("raw").str.{method}()'
    serialized = compile_expression(source, "enrichment")
    expression = pl.Expr.deserialize(io.StringIO(serialized), format="json")
    frame = pl.DataFrame({"raw": ["Hello", "Straße", "ÉTÉ", "", None]})
    assert frame.select(expression).to_series().to_list() == expected
    assert [frame.slice(i, 1).select(expression).item() for i in range(frame.height)] == expected


def test_unsupported_method_diagnostic_does_not_claim_nonlocal_semantics() -> None:
    with pytest.raises(Exception, match="not yet supported by lvu's live expression compiler"):
        compile_expression('pl.col("raw").str.to_titlecase()', "enrichment")


def test_compiles_with_versioned_metadata() -> None:
    response = handle({"schema_version": 1, "request_id": "a", "operation": "compile", "kind": "filter", "expression": '(pl.col("status") >= 400) & pl.col("message").is_not_null()'})
    assert response["ok"] is True
    assert response["request_id"] == "a"
    assert response["schema_version"] == 1
    assert response["python_polars_version"] == "1.44.1"
    assert response["compatibility_id"] == "polars-py-1.44.1-rs-0.55.2-expr-json-v1"
    assert json.loads(response["expression_json"])


@pytest.mark.parametrize("source, fragment", [
    ('pl.col("x").sum()', "sum"),
    ('pl.col("x").sort()', "sort"),
    ('pl.col("x").reverse()', "reverse"),
    ('pl.col("x").shift(1)', "shift"),
    ('pl.col("x").map_elements(lambda x: x)', "map_elements"),
    ('pl.col("x").filter(pl.col("ok"))', "filter"),
    ('pl.col("x").alias("_lvu_raw")', "protected"),
    ('pl.col(["x", "y"])', "multi-output"),
    ('pl.col("^x.*$")', "multi-output"),
    ('__import__("os")', "native Polars"),
])
def test_rejects_non_row_local_or_protected(source: str, fragment: str) -> None:
    with pytest.raises(Exception, match=fragment):
        compile_expression(source, "enrichment")


def test_protected_inputs_are_readable_but_enrichment_cannot_overwrite_them() -> None:
    assert compile_expression('pl.col("_lvu_raw").str.contains("error")', "filter")
    with pytest.raises(Exception, match="protected"):
        compile_expression('pl.col("_lvu_raw").alias("_lvu_raw")', "enrichment")


def test_rejects_neighbor_dependent_fill_strategy() -> None:
    with pytest.raises(Exception, match="neighboring/global"):
        compile_expression('pl.col("amount").fill_null(strategy="forward")', "enrichment")


@pytest.mark.parametrize("text", ["Reverse", "Shift", "Agg", "Sort"])
def test_operation_names_are_legal_literal_and_field_payloads(text: str) -> None:
    assert compile_expression(f'pl.col({text!r})', "enrichment")
    assert compile_expression(f'pl.lit({text!r})', "enrichment")
    assert compile_expression(f'pl.col("message").str.contains({text!r}, literal=True)', "filter")


def test_datetime_inference_is_rejected_but_explicit_format_is_accepted() -> None:
    with pytest.raises(Exception, match="explicit format"):
        compile_expression('pl.col("ts").str.to_datetime(strict=False)', "enrichment")
    assert compile_expression(
        'pl.col("ts").str.to_datetime("%Y-%m-%dT%H:%M:%S%.fZ", strict=False)',
        "enrichment",
    )
    with pytest.raises(Exception, match="explicit format"):
        compile_expression('pl.col("ts").str.strptime(pl.Datetime, strict=False)', "enrichment")
    assert compile_expression(
        'pl.col("ts").str.strptime(pl.Datetime, "%Y-%m-%d", strict=False)',
        "enrichment",
    )
    assert compile_expression(
        'pl.col("ts").str.strptime(pl.Date, format="%Y-%m-%d", strict=False)',
        "enrichment",
    )


def test_bad_field_types_are_structured_and_do_not_poison_next_request() -> None:
    bad = {"schema_version": 1, "request_id": "review", "operation": "compile", "kind": [], "expression": 'pl.col("amount")'}
    response = handle(bad)
    assert response["ok"] is False and response["error"]["code"] == "invalid_kind"
    assert handle({"schema_version": 1, "request_id": "next", "operation": "compile", "kind": "enrichment", "expression": 'pl.col("amount")'})["ok"] is True


def test_jsonl_recovers_after_malformed_and_oversized_input() -> None:
    bad_type = json.dumps({"schema_version": 1, "request_id": "typed", "operation": "compile", "kind": [], "expression": 'pl.col("x")'}).encode() + b"\n"
    valid = json.dumps({"schema_version": 1, "request_id": "after", "operation": "compile", "kind": "enrichment", "expression": 'pl.col("x") + 1'}).encode() + b"\n"
    proc = subprocess.run(
        [sys.executable, "-m", "lvu_expr_helper"],
        input=b"{bad json}\n" + b"x" * (MAX_REQUEST_BYTES + 8) + b"\n" + bad_type + valid,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        timeout=10,
        check=True,
    )
    rows = [json.loads(line) for line in proc.stdout.splitlines()]
    assert [row.get("error", {}).get("code") for row in rows[:2]] == ["invalid_json", "request_too_large"]
    assert rows[2]["error"]["code"] == "invalid_kind"
    assert rows[3]["ok"] is True and rows[3]["request_id"] == "after"
    assert proc.stderr == b""


def test_explicit_timestamp_normalization_and_formatting_compile() -> None:
    assert compile_expression(
        'pl.col("ts").str.to_datetime("%+", strict=False).dt.strftime("%Y-%m-%dT%H:%M:%S%.6fZ")',
        "enrichment",
    )


@pytest.mark.parametrize("source, expected", [
    ('pl.col("raw").str.replace("a", "X")', ["X a", "été", None]),
    ('pl.col("raw").str.replace_all("a", "X")', ["X X", "été", None]),
    ('pl.col("raw").str.replace_all("(?P<letter>a)", "${letter}!")', ["a! a!", "été", None]),
    ('pl.col("raw").str.replace(".", "$", literal=True)', ["a a", "été", None]),
])
def test_replacement_roundtrip_and_partition_semantics(source, expected):
    expression = pl.Expr.deserialize(io.StringIO(compile_expression(source, "enrichment")), format="json")
    frame = pl.DataFrame({"raw": ["a a", "été", None]})
    assert frame.select(expression).to_series().to_list() == expected
    assert [frame.slice(i, 1).select(expression).item() for i in range(frame.height)] == expected
