#!/usr/bin/env python3
"""Compile once in Python, evaluate independently in Python and the Rust runner."""

from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path

import polars as pl

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "python"))
from lvu_expr_helper.helper import COMPATIBILITY_ID, compile_expression  # noqa: E402


def fixture() -> pl.DataFrame:
    return pl.DataFrame({
        "amount": [10, None, -3, 7], "delta": [1, 2, 4, -2],
        "status": [200, 404, 500, 302],
        "message": ["ok user=amy", "bad user=bob", None, "redirect"],
        "code": ["12", "bad", None, "-7"],
        "ts": ["2026-01-02T03:04:05.123Z", "bad", None, "2026-09-05T00:00:00.000Z"],
        "_lvu_raw": ["ok", "error: missing", "ERROR", "redirect"],
        "nested": [{"region": "us-east", "attempt": 1}, {"region": None, "attempt": 2}, {"region": "eu-west", "attempt": 3}, {"region": "ap-south", "attempt": 4}],
    })


def dtype_name(dtype: pl.DataType) -> str:
    if isinstance(dtype, pl.Datetime):
        unit = {"us": "Microseconds", "ms": "Milliseconds", "ns": "Nanoseconds"}[dtype.time_unit]
        return f"Datetime({unit},{dtype.time_zone!r})".replace("'", "")
    return str(dtype)


def main() -> int:
    cases = json.loads((Path(__file__).parent / "fixtures/cases.json").read_text())
    requests = []
    python_results = {}
    frame = fixture()
    for case in cases:
        serialized = compile_expression(case["expression"], case["kind"])
        expression = pl.Expr.deserialize(__import__("io").StringIO(serialized), format="json")
        result = frame.select(expression.alias("value"))["value"]
        values = result.cast(pl.Int64).to_list() if isinstance(result.dtype, pl.Datetime) else result.to_list()
        python_results[case["id"]] = {"dtype": dtype_name(result.dtype), "values": values}
        request = {"request_id": case["id"], "kind": case["kind"], "compatibility_id": COMPATIBILITY_ID, "expression_json": serialized}
        requests.append(request)
        requests.append({**request, "request_id": f"partition-{case['id']}", "batch_rows": 1})
    runner = Path(sys.argv[1] if len(sys.argv) > 1 else Path(__file__).parent / "target/debug/lvu-polars-interop")
    proc = subprocess.Popen([runner], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    rust_pid = proc.pid
    stdout, stderr = proc.communicate("".join(json.dumps(r) + "\n" for r in requests), timeout=20)
    if proc.returncode != 0:
        raise RuntimeError(f"Rust runner exited {proc.returncode}: {stderr}")
    rust_results = {row["request_id"]: row for row in map(json.loads, stdout.splitlines())}
    for case in cases:
        expected = {"dtype": case["dtype"], "values": case["values"]}
        py = python_results[case["id"]]
        rust = rust_results[case["id"]]
        partitioned = rust_results[f"partition-{case['id']}"]
        rust_value = {"dtype": rust["dtype"], "values": rust["values"]}
        partitioned_value = {"dtype": partitioned["dtype"], "values": partitioned["values"]}
        if not rust["ok"] or not partitioned["ok"] or py != expected or rust_value != expected or partitioned_value != expected:
            raise AssertionError(f"{case['id']}: expected={expected!r} python={py!r} rust={rust!r}")
        print(f"PASS {case['id']}: {case['dtype']} {case['values']}")
    negative = [
        {"request_id": "wrong-version", "kind": "enrichment", "compatibility_id": "wrong", "expression_json": requests[0]["expression_json"]},
        {"request_id": "tampered-aggregate", "kind": "enrichment", "compatibility_id": COMPATIBILITY_ID, "expression_json": json.dumps({"Agg": {"Sum": {"Column": "amount"}}})},
        {"request_id": "tampered-reverse", "kind": "enrichment", "compatibility_id": COMPATIBILITY_ID, "expression_json": json.dumps({"Function":{"input":[{"Column":"amount"}],"function":"Reverse"}})},
        {"request_id": "tampered-shift", "kind": "enrichment", "compatibility_id": COMPATIBILITY_ID, "expression_json": json.dumps({"Function":{"input":[{"Column":"amount"},{"Literal":{"Dyn":{"Int":1}}}],"function":"Shift"}})},
        {"request_id": "tampered-fill", "kind": "enrichment", "compatibility_id": COMPATIBILITY_ID, "expression_json": json.dumps({"Function":{"input":[{"Column":"amount"}],"function":{"FillNullWithStrategy":{"Forward":None}}}})},
        {"request_id": "inferred-datetime", "kind": "enrichment", "compatibility_id": COMPATIBILITY_ID, "expression_json": json.dumps({"Function":{"input":[{"Column":"ts"},{"Literal":{"Scalar":{"String":"raise"}}}],"function":{"StringExpr":{"Strptime":[{"Literal":{"Datetime":["Microseconds",None]}},{"format":None,"strict":False,"exact":True,"cache":True}]}}}})},
        {"request_id": "oversized", "kind": "enrichment", "compatibility_id": COMPATIBILITY_ID, "expression_json": " " * (256 * 1024 + 1)},
    ]
    failed = subprocess.run([runner], input="not-json\n" + "".join(json.dumps(r) + "\n" for r in negative), text=True, capture_output=True, timeout=20, check=True)
    errors = [row["error"]["code"] for row in map(json.loads, failed.stdout.splitlines())]
    assert errors == ["invalid_request", "incompatible_expression", "non_row_local_expression", "non_row_local_expression", "non_row_local_expression", "non_row_local_expression", "non_row_local_expression", "expression_json_too_large"], errors
    print(f"PASS Rust diagnostics: {errors}")
    oversized_envelope = "x" * (384 * 1024 + 1) + "\n" + json.dumps(requests[0]) + "\n"
    bounded = subprocess.run([runner], input=oversized_envelope, text=True, capture_output=True, timeout=20, check=True)
    bounded_rows = list(map(json.loads, bounded.stdout.splitlines()))
    assert bounded_rows[0]["error"]["code"] == "request_too_large" and bounded_rows[1]["ok"] is True
    print("PASS bounded Rust envelope and resynchronization")
    deep_expr = '{"Alias":[' * 140 + '{"Column":"amount"}' + ',"x"]}' * 140
    nested_request = {"request_id": "nested", "kind": "enrichment", "compatibility_id": COMPATIBILITY_ID, "expression_json": deep_expr}
    nested = json.dumps(nested_request) + "\n" + json.dumps(requests[0]) + "\n"
    recursive = subprocess.run([runner], input=nested, text=True, capture_output=True, timeout=20, check=True)
    recursive_rows = list(map(json.loads, recursive.stdout.splitlines()))
    assert recursive_rows[0]["error"]["code"] == "incompatible_expression" and "recursion limit" in recursive_rows[0]["error"]["message"] and recursive_rows[1]["ok"] is True
    print("PASS bounded Rust deserialization depth and resynchronization")
    print(f"PASS independent native execution: runner={runner} rust_pid={rust_pid} whole_and_partitioned_rows={len(cases) * frame.height * 2}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
