import json
import io
import subprocess
import sys
from datetime import datetime

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


def test_row_local_methods_are_not_duplicated_in_a_python_allowlist() -> None:
    expression = pl.Expr.deserialize(
        io.StringIO(compile_expression('pl.col("raw").str.to_titlecase()', "enrichment")),
        format="json",
    )
    assert pl.DataFrame({"raw": ["hello WORLD", None]}).select(expression).to_series().to_list() == [
        "Hello World",
        None,
    ]


def test_compiles_with_versioned_metadata() -> None:
    response = handle({"schema_version": 1, "request_id": "a", "operation": "compile", "kind": "filter", "expression": '(pl.col("status") >= 400) & pl.col("message").is_not_null()'})
    assert response["ok"] is True
    assert response["request_id"] == "a"
    assert response["schema_version"] == 1
    assert response["python_polars_version"] == "1.44.1"
    assert response["compatibility_id"] == "polars-py-1.44.1-rs-0.55.2-expr-json-v1"
    assert json.loads(response["expression_json"])


@pytest.mark.parametrize("source, fragment", [
    ('pl.col("x").map_elements(lambda x: x)', "map_elements"),
    ('pl.col("x").alias("_lvu_raw")', "protected"),
    ('pl.col(["x", "y"])', "multi-output"),
    ('pl.col("^x.*$")', "multi-output"),
    ('__import__("os")', "native Polars"),
])
def test_rejects_non_row_local_or_protected(source: str, fragment: str) -> None:
    with pytest.raises(Exception, match=fragment):
        compile_expression(source, "enrichment")


@pytest.mark.parametrize("source", [
    'pl.col("x").sum()',
    'pl.col("x").sort()',
    'pl.col("x").reverse()',
    'pl.col("x").shift(1)',
    'pl.col("x").cum_sum()',
    'pl.col("x").fill_null(strategy="forward")',
    'pl.col("x").mean()',
])
def test_structural_expressions_serialize_for_authoritative_native_rejection(source: str) -> None:
    assert compile_expression(source, "enrichment")


def test_protected_inputs_are_readable_but_enrichment_cannot_overwrite_them() -> None:
    assert compile_expression('pl.col("_lvu_raw").str.contains("error")', "filter")
    with pytest.raises(Exception, match="protected"):
        compile_expression('pl.col("_lvu_raw").alias("_lvu_raw")', "enrichment")


def test_helper_rejects_io_eager_execution_and_callback_entry_points() -> None:
    for source in [
        'pl.read_csv("/tmp/input")',
        'pl.scan_parquet("/tmp/input")',
        'pl.collect_all([pl.col("x")])',
        'pl.DataFrame({"x": [1]})',
        'pl.Series("x", [1])',
        'pl.LazyFrame({"x": [1]})',
        'pl.Config.set_tbl_rows(1)',
        'pl.io.read_csv("/tmp/input")',
        'pl.plugins.register_plugin_function(plugin_path="/tmp/plugin", function_name="f")',
        'pl.from_epoch([0, 1], time_unit="ms")',
        'pl.from_epoch(column=[0, 1], time_unit="ms")',
        'pl.struct("x", eager=1 == 1)',
        'pl.coalesce("x", eager=pl.col("flag"))',
        'pl.col("x").map_batches(pl.col("x"))',
        'pl.col("x").meta.serialize("/tmp/output")',
        'pl.col("x").meta.tree_format()',
        'pl.col("x").meta.show_graph()',
        'pl.col("x").deserialize("/tmp/input")',
        'pl.col("x").from_json("{}")',
        'pl.col("x").register_plugin(lib="/tmp/plugin")',
        'pl.register_plugin_function(plugin_path="/tmp/plugin", function_name="f")',
    ]:
        with pytest.raises(Exception, match="unavailable|not a pinned|not a documented"):
            compile_expression(source, "enrichment")


@pytest.mark.parametrize(
    "source",
    [
        'pl.col("x").rolling_map(pl.col("x").sum)',
        'pl.col("x").rolling_map(function=pl.col("x").sum)',
    ],
)
def test_callable_attribute_arguments_cannot_escape_the_construction_policy(source):
    with pytest.raises(Exception, match="callback"):
        compile_expression(source, "enrichment")


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


@pytest.mark.parametrize("dtype", ['pl.Datetime', 'pl.Datetime("ms", "UTC")', 'pl.Datetime(time_unit="ms", time_zone="UTC")'])
def test_datetime_dtype_name_and_constructor_compile(dtype):
    source = f'pl.col("raw").cast({dtype})'
    expression = pl.Expr.deserialize(io.StringIO(compile_expression(source, "enrichment")), format="json")
    result = pl.DataFrame({"raw": [0, 1000, None]}).select(expression)
    assert result.height == 3
    assert result.to_series().null_count() == 1
    assert result.dtypes[0].base_type() == pl.Datetime


def test_explicit_datetime_constructor_normalizes_epoch_milliseconds():
    source = 'pl.col("raw").cast(pl.Datetime(time_unit="ms", time_zone="UTC")).dt.strftime("%Y-%m-%dT%H:%M:%S%.6fZ")'
    expression = pl.Expr.deserialize(io.StringIO(compile_expression(source, "enrichment")), format="json")
    assert pl.DataFrame({"raw": [0, 1000, None]}).select(expression).to_series().to_list() == [
        "1970-01-01T00:00:00.000000Z", "1970-01-01T00:00:01.000000Z", None]


@pytest.mark.parametrize(
    "source, expected",
    [
        ('pl.col("raw").str.strip_chars()', ["hello", "éclair", "", None]),
        ('pl.col("raw").str.slice(1, 3)', ["hel", "cla", "", None]),
        ('pl.col("raw").str.starts_with(" h")', [True, False, False, None]),
        ('pl.col("raw").str.ends_with(" ")', [True, True, False, None]),
        ('pl.col("raw").str.len_chars()', [7, 7, 0, None]),
        ('pl.col("raw").str.split("l").list.get(-1, null_on_oob=True)', ["o ", "air ", "", None]),
    ],
)
def test_broad_string_and_list_operations_preserve_partition_values(source, expected):
    expression = pl.Expr.deserialize(io.StringIO(compile_expression(source, "enrichment")), format="json")
    frame = pl.DataFrame({"raw": [" hello ", "éclair ", "", None]})
    whole = frame.select(expression).to_series()
    assert whole.to_list() == expected
    assert [frame.slice(i, 1).select(expression).item() for i in range(frame.height)] == expected
    assert all(frame.slice(i, 1).select(expression).dtypes[0] == whole.dtype for i in range(frame.height))


def test_from_epoch_and_coalesce_are_constructible_and_partition_equivalent():
    source = 'pl.coalesce(pl.from_epoch(pl.col("epoch"), time_unit="ms"), pl.lit(None, dtype=pl.Datetime("ms")))'
    expression = pl.Expr.deserialize(io.StringIO(compile_expression(source, "enrichment")), format="json")
    frame = pl.DataFrame({"epoch": [0, None, 1000]})
    whole = frame.select(expression).to_series()
    assert whole.to_list() == [
        datetime(1970, 1, 1),
        None,
        datetime(1970, 1, 1, 0, 0, 1),
    ]
    assert [frame.slice(i, 1).select(expression).item() for i in range(frame.height)] == whole.to_list()
    assert whole.dtype == pl.Datetime("ms")
