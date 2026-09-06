import json
import subprocess
import sys
from pathlib import Path

import polars as pl
import pytest

from lvu_expr_helper.inspection import InspectionError, _ensure_decode_platform, inspect


def write_part(root: Path, name: str, frame: pl.DataFrame) -> dict:
    path = root / name
    frame.write_parquet(path)
    return {
        "path": name,
        "source_id": "source-a",
        "rows": frame.height,
        "bytes": path.stat().st_size,
        "fields": [{"name": key, "dtype": rust_dtype(dtype)} for key, dtype in frame.schema.items()],
    }


def rust_dtype(dtype: pl.DataType) -> str:
    simple = {
        pl.String: "str", pl.Int64: "i64", pl.UInt64: "u64", pl.Boolean: "bool",
        pl.Float64: "f64", pl.Binary: "binary", pl.Date: "date",
    }
    if dtype in simple:
        return simple[dtype]
    if dtype.base_type() == pl.Datetime:
        suffix = f", {dtype.time_zone}" if dtype.time_zone else ""
        return f"datetime[{dtype.time_unit}{suffix}]"
    if dtype.base_type() == pl.List:
        return f"list[{rust_dtype(dtype.inner)}]"
    return str(dtype)


def sample(source_id: str, dataset: str, *parts: tuple[str, list[int]]) -> dict:
    count = sum(len(offsets) for _, offsets in parts)
    return {
        "source_id": source_id,
        "dataset": dataset,
        "available_rows": count,
        "requested_rows": count,
        "parts": [{"path": path, "row_offsets": offsets} for path, offsets in parts],
    }


def write_manifest(root: Path, body: dict) -> Path:
    path = root / "manifest.json"
    path.write_text(json.dumps(body), encoding="utf-8")
    return path


def v1_manifest(parts: list[dict], samples: list[dict]) -> dict:
    applied_paths = {part["path"] for item in samples if item["dataset"] == "applied_view"
                     for part in item["parts"]}
    return {
        "schema_version": 1,
        "source_parts": [part for part in parts if part["path"] not in applied_paths],
        "filtered_parts": [part for part in parts if part["path"] in applied_paths],
        "inspection_sample": {"sources": samples},
    }


def test_v1_exact_offsets_first_last_and_lossless_values(tmp_path: Path) -> None:
    frame = pl.DataFrame({
        "n": pl.Series([2**63 - 1, None, -(2**63)], dtype=pl.Int64),
        "u": pl.Series([2**64 - 1, 0, 1], dtype=pl.UInt64),
        "when": pl.Series([1, 2, 3], dtype=pl.Datetime("ns", "UTC")),
        "blob": [b"\x00\xff", None, b"z"],
        "float": [float("nan"), 1.0, float("inf")],
        "odd\n\"field": ["雪\n\"\\", "middle", None],
    })
    part = write_part(tmp_path, "part.parquet", frame)
    manifest = write_manifest(tmp_path, v1_manifest([part], [sample("source-a", "applied_view", ("part.parquet", [0, 2]))]))

    output = inspect(manifest)
    assert [row["row_offset"] for row in output["rows"]] == [0, 2]
    assert output["rows"][0]["values"]["n"] == {
        "kind": "integer", "decimal": str(2**63 - 1), "dtype": "i64"
    }
    assert output["rows"][0]["values"]["u"]["decimal"] == str(2**64 - 1)
    assert output["rows"][0]["values"]["when"] == {
        "kind": "datetime", "value": "1", "unit": "ns", "timezone": "UTC"
    }
    assert output["rows"][0]["values"]["blob"] == {"kind": "bytes", "base64": "AP8="}
    assert output["rows"][0]["values"]["float"]["value"] == "nan"
    assert output["rows"][0]["values"]["odd\n\"field"] == "雪\n\"\\"
    assert output["rows"][1]["values"]["n"]["decimal"] == str(-(2**63))


def test_v2_keeps_incompatible_schemas_separate_and_reports_conflict(tmp_path: Path) -> None:
    one = write_part(tmp_path, "one.parquet", pl.DataFrame({"x": [7], "only_one": [True]}))
    two = write_part(tmp_path, "two.parquet", pl.DataFrame({"x": ["seven"], "only_two": [3.5]}))
    two["source_id"] = "source-b"
    schemas = [
        {"schema_id": 7, "fields": one.pop("fields")},
        {"schema_id": 11, "fields": two.pop("fields")},
    ]
    one["schema_id"] = 7
    two["schema_id"] = 11
    manifest = write_manifest(tmp_path, {
        "schema_version": 2, "schemas": schemas, "source_parts": [two], "filtered_parts": [one],
        "inspection_sample": {"sources": [
            sample("source-a", "applied_view", ("one.parquet", [0])),
            sample("source-b", "source_context", ("two.parquet", [0])),
        ]},
    })
    output = inspect(manifest)
    assert [schema["schema_id"] for schema in output["schemas"]] == [11, 7]
    assert output["coverage"]["type_conflicts"] == [{"field": "x", "dtypes": ["i64", "str"]}]
    assert [row["source_id"] for row in output["rows"]] == ["source-a", "source-b"]
    filtered = inspect(manifest, source="source-b", field="x")
    assert filtered["rows"][0]["values"] == {"x": "seven"}
    assert filtered["sources"][0]["dataset"] == "source_context"


def test_field_projection_marks_physically_missing_field_without_reading_it(tmp_path: Path) -> None:
    one = write_part(tmp_path, "one.parquet", pl.DataFrame({"target": [4], "wide": ["x" * 10_000]}))
    two = write_part(tmp_path, "two.parquet", pl.DataFrame({"different": [9]}))
    two["source_id"] = "source-b"
    manifest = write_manifest(tmp_path, v1_manifest(
        [one, two],
        [sample("source-a", "applied_view", ("one.parquet", [0])),
         sample("source-b", "source_context", ("two.parquet", [0]))],
    ))
    output = inspect(manifest, field="target", max_bytes=4096)
    assert output["rows"][0]["values"] == {"target": 4}
    assert output["rows"][1]["values"] == {"target": {"kind": "missing"}}
    assert output["coverage"]["omitted_fields"] == 2
    assert len(json.dumps(output, ensure_ascii=False).encode()) <= 4096


def test_field_projection_includes_type_provenance_and_null_evidence(tmp_path: Path) -> None:
    part = write_part(tmp_path, "part.parquet", pl.DataFrame({
        "target": pl.Series([None, 4], dtype=pl.Int64),
        "_lvu_type_target": ["str", None],
        "unrelated": ["secret", "wide"],
    }))
    manifest = write_manifest(tmp_path, v1_manifest(
        [part], [sample("source-a", "applied_view", ("part.parquet", [0, 1]))]
    ))
    output = inspect(manifest, field="target")
    assert output["rows"][0]["values"] == {"target": None, "_lvu_type_target": "str"}
    assert output["rows"][1]["values"] == {"target": 4, "_lvu_type_target": None}
    assert output["coverage"]["projected_fields"] == ["target", "_lvu_type_target"]


def test_nested_values_are_recursively_lossless(tmp_path: Path) -> None:
    part = write_part(tmp_path, "nested.parquet", pl.DataFrame({
        "numbers": pl.Series([[2**63 - 1, -(2**63)]], dtype=pl.List(pl.Int64)),
        "floats": [[float("nan"), float("-inf")]],
    }))
    manifest = write_manifest(tmp_path, v1_manifest(
        [part], [sample("source-a", "applied_view", ("nested.parquet", [0]))]
    ))
    values = inspect(manifest)["rows"][0]["values"]
    assert values["numbers"][0]["decimal"] == str(2**63 - 1)
    assert values["numbers"][1]["decimal"] == str(-(2**63))
    assert [item["value"] for item in values["floats"]] == ["nan", "-infinity"]


def test_nested_temporal_is_refused_instead_of_losing_nanoseconds(tmp_path: Path) -> None:
    part = write_part(tmp_path, "nested-time.parquet", pl.DataFrame({
        "times": pl.Series([[1, 2]], dtype=pl.List(pl.Datetime("ns", "UTC"))),
    }))
    manifest = write_manifest(tmp_path, v1_manifest(
        [part], [sample("source-a", "applied_view", ("nested-time.parquet", [0]))]
    ))
    with pytest.raises(InspectionError, match="precision loss"):
        inspect(manifest)


def test_compressed_oversized_cell_fails_explicitly(tmp_path: Path) -> None:
    part = write_part(tmp_path, "compressed.parquet", pl.DataFrame({"value": ["z" * 1_000_000]}))
    assert (tmp_path / "compressed.parquet").stat().st_size < 100_000
    manifest = write_manifest(tmp_path, v1_manifest(
        [part], [sample("source-a", "applied_view", ("compressed.parquet", [0]))]
    ))
    with pytest.raises(InspectionError, match="decoded value limit"):
        inspect(manifest)


def test_metadata_and_final_counters_remain_within_tight_cap(tmp_path: Path) -> None:
    frame = pl.DataFrame({f"field_{index:03}_" + "雪" * 8: [index] for index in range(40)})
    part = write_part(tmp_path, "wide.parquet", frame)
    manifest = write_manifest(tmp_path, v1_manifest(
        [part], [sample("source-a", "applied_view", ("wide.parquet", [0]))]
    ))
    output = inspect(manifest, max_bytes=1400)
    encoded = json.dumps(output, ensure_ascii=False, separators=(",", ":")).encode()
    assert len(encoded) <= 1400
    assert output["coverage"]["omitted_schemas"] > 0
    assert output["coverage"]["omitted_projected_fields"] > 0
    assert output["coverage"]["omitted_serialized_bytes"] > 0
    assert output["coverage"]["omitted_rows"] == 1


def test_default_32k_envelope_accounts_for_schema_and_provenance(tmp_path: Path) -> None:
    frame = pl.DataFrame({
        **{f"field_{index:03}": ["v" * 300] for index in range(80)},
        "target": pl.Series([None], dtype=pl.Int64),
        "_lvu_type_target": ["str"],
    })
    part = write_part(tmp_path, "wide.parquet", frame)
    manifest = write_manifest(tmp_path, v1_manifest(
        [part], [sample("source-a", "applied_view", ("wide.parquet", [0]))]
    ))
    output = inspect(manifest, field="target")
    encoded = json.dumps(output, ensure_ascii=False, separators=(",", ":")).encode()
    assert len(encoded) <= 32 * 1024
    assert output["rows"][0]["values"] == {"target": None, "_lvu_type_target": "str"}
    assert output["limits"]["max_output_bytes"] == 32 * 1024


def test_manifest_requested_total_must_match_authoritative_offsets(tmp_path: Path) -> None:
    part = write_part(tmp_path, "part.parquet", pl.DataFrame({"x": [1]}))
    body = v1_manifest([part], [sample("source-a", "applied_view", ("part.parquet", [0]))])
    body["inspection_sample"]["requested_rows"] = 2
    with pytest.raises(InspectionError, match="requested_rows"):
        inspect(write_manifest(tmp_path, body))


def test_whole_deadline_stops_before_launching_another_part(tmp_path: Path) -> None:
    first = write_part(tmp_path, "first.parquet", pl.DataFrame({"x": [1]}))
    second = write_part(tmp_path, "second.parquet", pl.DataFrame({"x": [2]}))
    manifest = write_manifest(tmp_path, v1_manifest(
        [first, second],
        [sample("source-a", "applied_view", ("first.parquet", [0]), ("second.parquet", [0]))],
    ))
    moments = iter([0.0, 1.0, 31.0])
    launches = []

    def worker(part, offsets, selected, timeout):
        launches.append((part.path, timeout))
        return [{"offset": offsets[0], "values": {name: 1 for name in selected}}]

    with pytest.raises(InspectionError, match="before the next part"):
        inspect(manifest, _clock=lambda: next(moments), _worker=worker)
    assert launches == [("first.parquet", 29.0)]


def test_unsupported_decode_platform_is_actionable(monkeypatch) -> None:
    monkeypatch.setattr(sys, "platform", "darwin")
    with pytest.raises(InspectionError, match="requires Linux.*macOS"):
        _ensure_decode_platform()


def test_deadline_during_output_admission_refuses_partial_success(tmp_path: Path) -> None:
    part = write_part(tmp_path, "part.parquet", pl.DataFrame({"x": [1]}))
    manifest = write_manifest(tmp_path, v1_manifest(
        [part], [sample("source-a", "applied_view", ("part.parquet", [0]))],
    ))
    moments = iter([0.0, 1.0, 2.0, 31.0])

    def worker(part, offsets, selected, timeout):
        return [{"offset": offsets[0], "values": {name: 1 for name in selected}}]

    with pytest.raises(InspectionError, match="during output admission"):
        inspect(manifest, _clock=lambda: next(moments), _worker=worker)


def test_byte_cap_omits_whole_rows_fairly_and_reports_bytes(tmp_path: Path) -> None:
    parts = []
    samples = []
    for source_id in ("source-a", "source-b"):
        part = write_part(tmp_path, f"{source_id}.parquet", pl.DataFrame({"value": [source_id + "雪" * 300] * 2}))
        part["source_id"] = source_id
        parts.append(part)
        samples.append(sample(source_id, "applied_view", (part["path"], [0, 1])))
    manifest = write_manifest(tmp_path, v1_manifest(parts, samples))
    output = inspect(manifest, max_bytes=4096)
    assert [row["source_id"] for row in output["rows"][:2]] == ["source-a", "source-b"]
    assert output["coverage"]["omitted_rows"] > 0
    assert output["coverage"]["omitted_serialized_bytes"] > 0
    encoded = json.dumps(output, ensure_ascii=False, separators=(",", ":")).encode()
    assert len(encoded) <= 4096
    assert all(row["values"]["value"].endswith("雪" * 300) for row in output["rows"])


@pytest.mark.parametrize("mutation", ["traversal", "offset", "schema", "physical_schema"])
def test_rejects_invalid_paths_offsets_and_v2_schema(tmp_path: Path, mutation: str) -> None:
    part = write_part(tmp_path, "part.parquet", pl.DataFrame({"x": [1]}))
    body = v1_manifest([part], [sample("source-a", "applied_view", ("part.parquet", [0]))])
    if mutation == "traversal":
        body["filtered_parts"][0]["path"] = "../outside.parquet"
    elif mutation == "offset":
        body["inspection_sample"]["sources"][0]["parts"][0]["row_offsets"] = [1]
    elif mutation == "schema":
        body["schema_version"] = 2
        body["schemas"] = []
        body["filtered_parts"][0].pop("fields")
        body["filtered_parts"][0]["schema_id"] = 99
    else:
        body["filtered_parts"][0]["fields"][0]["dtype"] = "str"
    with pytest.raises(InspectionError):
        inspect(write_manifest(tmp_path, body))


def test_module_cli_emits_one_json_object(tmp_path: Path) -> None:
    part = write_part(tmp_path, "part.parquet", pl.DataFrame({"x": [1]}))
    manifest = write_manifest(tmp_path, v1_manifest([part], [sample("source-a", "applied_view", ("part.parquet", [0]))]))
    completed = subprocess.run(
        [sys.executable, "-m", "lvu_expr_helper.inspection", str(manifest), "--field", "x"],
        check=True, capture_output=True, text=True,
    )
    assert completed.stderr == ""
    assert json.loads(completed.stdout)["rows"][0]["values"] == {"x": 1}
