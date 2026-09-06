"""Bounded, read-only inspection of an immutable lvu Parquet snapshot.

Each selected part is decoded in an isolated process with a fixed address-space
allowance above the loaded runtime.  Cells, nesting, worker output, and the final
JSON envelope have independent hard limits.  Exceeding any limit refuses the
inspection; the row-count bound alone is never presented as a memory bound.
Bounded decoding currently requires Linux ``/proc/self/statm`` and ``RLIMIT_AS``;
macOS decoding has not been validated and is refused explicitly.
"""

from __future__ import annotations

import argparse
import base64
import json
import math
import os
import re
import resource
import subprocess
import sys
import time
from collections import defaultdict, deque
from dataclasses import dataclass
from decimal import Decimal
from pathlib import Path
from typing import Any

import polars as pl

OUTPUT_LIMIT = 32 * 1024
MANIFEST_LIMIT = 8 * 1024 * 1024
PART_LIMIT = 512 * 1024 * 1024
TOTAL_PART_BYTES_LIMIT = 512 * 1024 * 1024
MAX_PARTS = 4096
MAX_SCHEMAS = 1024
MAX_FIELDS = 4096
MAX_PATH_BYTES = 4096
MAX_ROWS = 512
MAX_ROWS_PER_SOURCE = 128
MAX_CELL_BYTES = 256 * 1024
MAX_WORKER_RESULT_BYTES = 8 * 1024 * 1024
MAX_NESTING = 16
MAX_NESTED_ITEMS = 4096
WORKER_EXTRA_ADDRESS_SPACE = 1024 * 1024 * 1024
INSPECTION_DEADLINE_SECONDS = 30.0
JS_EXACT_INTEGER = (1 << 53) - 1


class InspectionError(Exception):
    pass


@dataclass(frozen=True)
class Part:
    path: str
    resolved: Path
    source_id: str
    rows: int
    schema_id: str | int
    fields: tuple[tuple[str, str], ...]
    kind: str


def _integer(value: Any, label: str, maximum: int) -> int:
    if isinstance(value, bool) or not isinstance(value, int) or value < 0 or value > maximum:
        raise InspectionError(f"invalid {label}")
    return value


def _text(value: Any, label: str, maximum: int = 4096) -> str:
    if not isinstance(value, str) or len(value.encode("utf-8")) > maximum:
        raise InspectionError(f"invalid {label}")
    return value


def _fields(value: Any, label: str) -> tuple[tuple[str, str], ...]:
    if not isinstance(value, list) or len(value) > MAX_FIELDS:
        raise InspectionError(f"invalid {label}")
    result = []
    seen = set()
    for item in value:
        if not isinstance(item, dict):
            raise InspectionError(f"invalid {label}")
        name = _text(item.get("name"), f"{label} field name")
        dtype = _text(item.get("dtype"), f"{label} dtype")
        if name in seen:
            raise InspectionError(f"duplicate field {name!r} in {label}")
        seen.add(name)
        result.append((name, dtype))
    return tuple(result)


def _inside(root: Path, relative: str) -> Path:
    if not relative or len(relative.encode("utf-8")) > MAX_PATH_BYTES:
        raise InspectionError("invalid part path")
    path = Path(relative)
    if path.is_absolute():
        raise InspectionError("part path must be relative to the snapshot")
    try:
        resolved = (root / path).resolve(strict=True)
        resolved.relative_to(root)
    except (OSError, ValueError):
        raise InspectionError(f"part path escapes or is missing: {relative!r}") from None
    if not resolved.is_file():
        raise InspectionError(f"part is not a regular file: {relative!r}")
    size = resolved.stat().st_size
    if size > PART_LIMIT:
        raise InspectionError(f"part exceeds {PART_LIMIT} bytes: {relative!r}")
    return resolved


def _load_manifest(path: Path) -> tuple[dict[str, Any], Path]:
    if not path.is_absolute():
        raise InspectionError("manifest path must be absolute")
    try:
        resolved = path.resolve(strict=True)
    except OSError as error:
        raise InspectionError(f"manifest is unavailable: {error}") from None
    if not resolved.is_file() or resolved.stat().st_size > MANIFEST_LIMIT:
        raise InspectionError("manifest is not a bounded regular file")
    try:
        value = json.loads(resolved.read_bytes())
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as error:
        raise InspectionError(f"invalid manifest JSON: {error}") from None
    if not isinstance(value, dict) or value.get("schema_version") not in (1, 2):
        raise InspectionError("unsupported manifest schema_version")
    return value, resolved.parent.resolve(strict=True)


def _parse_parts(manifest: dict[str, Any], root: Path) -> dict[str, Part]:
    version = manifest["schema_version"]
    schemas: dict[str | int, tuple[tuple[str, str], ...]] = {}
    if version == 2:
        raw_schemas = manifest.get("schemas")
        if not isinstance(raw_schemas, list) or len(raw_schemas) > MAX_SCHEMAS:
            raise InspectionError("invalid schemas")
        for raw in raw_schemas:
            if not isinstance(raw, dict):
                raise InspectionError("invalid schema")
            schema_id = raw.get("schema_id")
            if isinstance(schema_id, bool) or not isinstance(schema_id, (str, int)):
                raise InspectionError("invalid schema_id")
            if isinstance(schema_id, str):
                schema_id = _text(schema_id, "schema_id")
            else:
                schema_id = _integer(schema_id, "schema_id", (1 << 32) - 1)
            if schema_id in schemas:
                raise InspectionError("duplicate schema_id")
            schemas[schema_id] = _fields(raw.get("fields"), f"schema {schema_id}")

    raw_parts = []
    for key in ("source_parts", "filtered_parts"):
        values = manifest.get(key)
        if not isinstance(values, list):
            raise InspectionError(f"invalid {key}")
        raw_parts.extend((key, value) for value in values)
    if len(raw_parts) > MAX_PARTS:
        raise InspectionError("too many parts")
    parts: dict[str, Part] = {}
    total_bytes = 0
    for kind, raw in raw_parts:
        if not isinstance(raw, dict):
            raise InspectionError("invalid part")
        relative = _text(raw.get("path"), "part path", MAX_PATH_BYTES)
        if relative in parts:
            raise InspectionError(f"duplicate part path: {relative!r}")
        source_id = _text(raw.get("source_id"), "source_id")
        rows = _integer(raw.get("rows"), "part rows", 1 << 50)
        resolved = _inside(root, relative)
        total_bytes += resolved.stat().st_size
        if total_bytes > TOTAL_PART_BYTES_LIMIT:
            raise InspectionError("declared snapshot parts exceed inspection byte budget")
        if version == 1:
            schema_id = f"part:{relative}"
            fields = _fields(raw.get("fields"), f"part {relative}")
        else:
            schema_id = raw.get("schema_id")
            if isinstance(schema_id, bool) or not isinstance(schema_id, (str, int)):
                raise InspectionError("invalid part schema_id")
            if schema_id not in schemas:
                raise InspectionError(f"unknown schema_id {schema_id!r}")
            fields = schemas[schema_id]
        parts[relative] = Part(relative, resolved, source_id, rows, schema_id, fields, kind)
    return parts


def _sample_plan(manifest: dict[str, Any], parts: dict[str, Part], source: str | None) -> list[dict[str, Any]]:
    sample = manifest.get("inspection_sample")
    if not isinstance(sample, dict) or not isinstance(sample.get("sources"), list):
        raise InspectionError("invalid inspection_sample")
    declared_maximum = _integer(sample.get("maximum_rows", MAX_ROWS), "maximum_rows", MAX_ROWS)
    declared_per_source = _integer(
        sample.get("maximum_rows_per_source", MAX_ROWS_PER_SOURCE),
        "maximum_rows_per_source", MAX_ROWS_PER_SOURCE,
    )
    if declared_maximum > MAX_ROWS or declared_per_source > MAX_ROWS_PER_SOURCE:
        raise InspectionError("inspection_sample declares unsupported row limits")
    if len(sample["sources"]) > MAX_ROWS:
        raise InspectionError("too many sampled sources")
    plan = []
    total = 0
    seen_sources = set()
    for raw_source in sample["sources"]:
        if not isinstance(raw_source, dict):
            raise InspectionError("invalid sampled source")
        source_id = _text(raw_source.get("source_id"), "sample source_id")
        if source_id in seen_sources:
            raise InspectionError("duplicate sampled source")
        seen_sources.add(source_id)
        if source is not None and source_id != source:
            continue
        dataset = _text(raw_source.get("dataset"), "sample dataset")
        if dataset not in ("applied_view", "source_context"):
            raise InspectionError("invalid sample dataset")
        raw_locations = raw_source.get("parts")
        available_rows = _integer(raw_source.get("available_rows"), "available_rows", 1 << 50)
        declared_requested = _integer(raw_source.get("requested_rows"), "requested_rows", MAX_ROWS_PER_SOURCE)
        if not isinstance(raw_locations, list) or len(raw_locations) > MAX_PARTS:
            raise InspectionError("invalid sampled parts")
        locations = []
        source_total = 0
        for raw in raw_locations:
            if not isinstance(raw, dict):
                raise InspectionError("invalid sampled part")
            relative = _text(raw.get("path"), "sample part path", MAX_PATH_BYTES)
            part = parts.get(relative)
            if part is None or part.source_id != source_id:
                raise InspectionError("sample references an undeclared or wrong-source part")
            expected_kind = "filtered_parts" if dataset == "applied_view" else "source_parts"
            if part.kind != expected_kind:
                raise InspectionError("sample dataset does not match its part kind")
            offsets = raw.get("row_offsets")
            if not isinstance(offsets, list) or len(offsets) > MAX_ROWS_PER_SOURCE:
                raise InspectionError("invalid row_offsets")
            parsed = [_integer(offset, "row offset", part.rows - 1) for offset in offsets]
            if len(set(parsed)) != len(parsed):
                raise InspectionError("duplicate row offset")
            source_total += len(parsed)
            locations.append((part, parsed))
        if source_total > declared_per_source:
            raise InspectionError("sample exceeds per-source row limit")
        if source_total != declared_requested or source_total > available_rows:
            raise InspectionError("sample row coverage is inconsistent")
        total += source_total
        if total > declared_maximum:
            raise InspectionError("sample exceeds total row limit")
        plan.append({"source_id": source_id, "dataset": dataset, "available_rows": available_rows,
                     "locations": locations})
    if source is not None and not plan:
        raise InspectionError(f"source is not present in inspection_sample: {source!r}")
    unfiltered_total = sum(
        _integer(item.get("requested_rows"), "requested_rows", MAX_ROWS_PER_SOURCE)
        for item in sample["sources"] if isinstance(item, dict)
    )
    declared_total = _integer(
        sample.get("requested_rows", unfiltered_total), "inspection requested_rows", MAX_ROWS
    )
    if declared_total != unfiltered_total:
        raise InspectionError("inspection_sample requested_rows does not match its sources")
    return plan


def _dtype_details(dtype: str) -> tuple[str | None, str | None]:
    lower = dtype.lower()
    if lower.startswith("datetime[") or dtype.startswith("Datetime("):
        units = re.search(r"(?:time_unit=)?['\"]?([num]s)['\"]?", dtype)
        if units is None:
            units = re.search(r"(Nano|Micro|Milli)seconds", dtype)
        zone = re.search(r"(?:time_zone=)?(?:Some\()?['\"]?([^,'\"\])]+/[^,'\"\])]+|UTC)['\"]?", dtype)
        unit = units.group(1) if units else None
        unit = {"Nano": "ns", "Micro": "us", "Milli": "ms"}.get(unit, unit)
        return unit, zone.group(1) if zone else None
    if lower == "date":
        return "d", None
    if lower == "time":
        return "ns", None
    if lower.startswith("duration[") or dtype.startswith("Duration("):
        units = re.search(r"['\"]?([num]s)['\"]?", dtype)
        return units.group(1) if units else None, None
    return None, None


def _dtype_kind(dtype: str) -> str:
    kind = re.split(r"[\[(]", dtype, maxsplit=1)[0]
    aliases = {
        "str": "String", "i64": "Int64", "u64": "UInt64", "bool": "Boolean",
        "date": "Date", "time": "Time", "datetime": "Datetime", "duration": "Duration",
        "list": "List", "struct": "Struct", "binary": "Binary", "f64": "Float64",
        "f32": "Float32", "null": "Null", "decimal": "Decimal", "Utf8": "String",
    }
    if re.fullmatch(r"i(8|16|32|64|128)", kind):
        return f"Int{kind[1:]}"
    if re.fullmatch(r"u(8|16|32|64|128)", kind):
        return f"UInt{kind[1:]}"
    return aliases.get(kind, kind)


def _encode(value: Any, dtype: str, depth: int = 0, budget: list[int] | None = None) -> Any:
    if depth > MAX_NESTING:
        raise InspectionError("nested cell exceeds depth limit")
    if budget is None:
        budget = [MAX_NESTED_ITEMS]
    budget[0] -= 1
    if budget[0] < 0:
        raise InspectionError("nested cell exceeds item limit")
    if value is None:
        return None
    unit, timezone = _dtype_details(dtype)
    if unit is not None:
        result = {"kind": dtype.split("(", 1)[0].lower(), "value": str(int(value)), "unit": unit}
        if timezone is not None:
            result["timezone"] = timezone
        return result
    if isinstance(value, bool):
        return value
    if isinstance(value, int):
        if abs(value) > JS_EXACT_INTEGER:
            return {"kind": "integer", "decimal": str(value), "dtype": dtype}
        return value
    if isinstance(value, float) and not math.isfinite(value):
        label = "nan" if math.isnan(value) else ("infinity" if value > 0 else "-infinity")
        return {"kind": "nonfinite", "value": label, "dtype": dtype}
    if isinstance(value, float):
        return value
    if isinstance(value, bytes):
        return {"kind": "bytes", "base64": base64.b64encode(value).decode("ascii")}
    if isinstance(value, Decimal):
        return {"kind": "decimal", "value": str(value), "dtype": dtype}
    if isinstance(value, (list, tuple)):
        return [_encode(item, "nested", depth + 1, budget) for item in value]
    if isinstance(value, dict):
        return {str(key): _encode(item, "nested", depth + 1, budget) for key, item in value.items()}
    if value.__class__.__module__ == "datetime":
        raise InspectionError("nested temporal cell cannot be represented without precision loss")
    if not isinstance(value, str):
        raise InspectionError(f"unsupported nested/scalar value type: {type(value).__name__}")
    return value


def _physical_temporal(dtype: pl.DataType) -> tuple[str | None, str | None, str | None]:
    if dtype == pl.Date:
        return "date", "d", None
    if dtype == pl.Time:
        return "time", "ns", None
    if dtype.base_type() == pl.Datetime:
        return "datetime", dtype.time_unit, dtype.time_zone
    if dtype.base_type() == pl.Duration:
        return "duration", dtype.time_unit, None
    return None, None, None


def _ensure_decode_platform() -> None:
    if not sys.platform.startswith("linux") or not Path("/proc/self/statm").is_file() \
            or not hasattr(resource, "RLIMIT_AS"):
        raise InspectionError(
            "bounded Parquet decoding requires Linux /proc/self/statm and RLIMIT_AS; "
            "macOS and other platforms are not yet validated"
        )


def _bounded_worker(request: dict[str, Any]) -> dict[str, Any]:
    _ensure_decode_platform()
    page_size = os.sysconf("SC_PAGE_SIZE")
    with open("/proc/self/statm", encoding="ascii") as handle:
        current_address_space = int(handle.read().split()[0]) * page_size
    hard = current_address_space + WORKER_EXTRA_ADDRESS_SPACE
    resource.setrlimit(resource.RLIMIT_AS, (hard, hard))
    path = Path(request["path"])
    offsets = request["offsets"]
    declared_fields = request["fields"]
    selected = request["selected"]
    parquet_schema = pl.scan_parquet(path).collect_schema()
    missing = [name for name, _ in declared_fields if name not in parquet_schema]
    if missing:
        raise InspectionError(f"Parquet schema is missing declared fields: {missing!r}")
    for name, declared in declared_fields:
        actual = parquet_schema[name]
        if _dtype_kind(declared) != _dtype_kind(str(actual)):
            raise InspectionError(f"field {name!r} declares {declared}, found {actual}")
        if _dtype_kind(declared) == "List" and declared.startswith("list["):
            declared_inner = declared[5:-1]
            if _dtype_kind(declared_inner) != _dtype_kind(str(actual.inner)):
                raise InspectionError(f"list field {name!r} has a mismatched inner dtype")
        expected_unit, expected_zone = _dtype_details(declared)
        _, actual_unit, actual_zone = _physical_temporal(actual)
        if expected_unit is not None and (expected_unit, expected_zone) != (actual_unit, actual_zone):
            raise InspectionError(f"temporal field {name!r} does not match its declaration")
    expressions = []
    for name in selected:
        kind, _, _ = _physical_temporal(parquet_schema[name])
        expressions.append(pl.col(name).cast(pl.Int64).alias(name) if kind else pl.col(name))
    frame = (
        pl.scan_parquet(path)
        .with_row_index("__lvu_inspection_offset")
        .filter(pl.col("__lvu_inspection_offset").is_in(offsets))
        .select([pl.col("__lvu_inspection_offset"), *expressions])
        .collect(engine="streaming")
    )
    by_offset = {row["__lvu_inspection_offset"]: row for row in frame.iter_rows(named=True)}
    encoded = []
    declared = dict(declared_fields)
    for offset in offsets:
        if offset not in by_offset:
            raise InspectionError(f"Parquet row {offset} is unavailable")
        values = {}
        for name in selected:
            physical = parquet_schema[name]
            kind, unit, timezone = _physical_temporal(physical)
            if kind:
                value = by_offset[offset][name]
                values[name] = None if value is None else {
                    "kind": kind, "value": str(int(value)), "unit": unit,
                    **({"timezone": timezone} if timezone is not None else {}),
                }
            else:
                values[name] = _encode(by_offset[offset][name], declared[name])
            if len(_json_bytes(values[name])) > MAX_CELL_BYTES:
                raise InspectionError(f"cell {name!r} exceeds decoded value limit")
        encoded.append({"offset": offset, "values": values})
    if len(_json_bytes(encoded)) > MAX_WORKER_RESULT_BYTES:
        raise InspectionError("sampled part exceeds decoded result limit")
    return {"rows": encoded}


def _run_bounded_worker(part: Part, offsets: list[int], selected: list[str],
                        timeout: float) -> list[dict[str, Any]]:
    _ensure_decode_platform()
    request = {"path": str(part.resolved), "offsets": offsets,
               "fields": list(part.fields), "selected": selected}
    environment = os.environ.copy()
    environment["POLARS_MAX_THREADS"] = "2"
    environment["MALLOC_CONF"] = "background_thread:false"
    try:
        completed = subprocess.run(
            [sys.executable, "-m", "lvu_expr_helper.inspection", "--_bounded-worker"],
            input=_json_bytes(request), capture_output=True, timeout=timeout, env=environment,
        )
    except subprocess.TimeoutExpired:
        raise InspectionError("whole-inspection deadline expired during bounded Parquet decode") from None
    if completed.returncode != 0:
        message = completed.stderr.decode("utf-8", "replace").strip()
        if not message:
            message = f"worker exited {completed.returncode}, possibly at its memory limit"
        raise InspectionError(f"bounded Parquet decode failed for {part.path!r}: {message}")
    if len(completed.stdout) > MAX_WORKER_RESULT_BYTES:
        raise InspectionError(f"bounded Parquet decode exceeded its result limit for {part.path!r}")
    try:
        return json.loads(completed.stdout)["rows"]
    except (UnicodeDecodeError, json.JSONDecodeError, KeyError, TypeError):
        raise InspectionError(f"bounded Parquet decode returned an invalid result for {part.path!r}") from None


def _read_rows(plan: list[dict[str, Any]], field: str | None, deadline: float,
               clock: Any, worker: Any) -> tuple[list[dict[str, Any]], int]:
    queues: dict[str, deque[dict[str, Any]]] = defaultdict(deque)
    read_rows = 0
    for source in plan:
        for part, offsets in source["locations"]:
            if not offsets:
                continue
            names = [name for name, _ in part.fields]
            selected = names if field is None else [name for name in (field, f"_lvu_type_{field}") if name in names]
            remaining = deadline - clock()
            if remaining <= 0:
                raise InspectionError("whole-inspection deadline expired before the next part decode")
            decoded = worker(part, offsets, selected, remaining)
            for item in decoded:
                offset = item["offset"]
                values = item["values"]
                if field is not None and field not in names:
                    values[field] = {"kind": "missing"}
                queues[source["source_id"]].append({
                    "source_id": source["source_id"], "dataset": source["dataset"],
                    "path": part.path, "schema_id": part.schema_id,
                    "row_offset": offset, "values": values,
                })
                read_rows += 1
    rows = []
    while any(queues.values()):
        for source in plan:
            queue = queues[source["source_id"]]
            if queue:
                rows.append(queue.popleft())
    return rows, read_rows


def _json_bytes(value: Any) -> bytes:
    return json.dumps(value, ensure_ascii=False, separators=(",", ":"), allow_nan=False).encode("utf-8")


def inspect(manifest_path: Path, source: str | None = None, field: str | None = None,
            max_bytes: int = OUTPUT_LIMIT, *, _clock: Any = time.monotonic,
            _worker: Any = _run_bounded_worker) -> dict[str, Any]:
    deadline = _clock() + INSPECTION_DEADLINE_SECONDS
    if max_bytes < 1024 or max_bytes > OUTPUT_LIMIT:
        raise InspectionError(f"max-bytes must be between 1024 and {OUTPUT_LIMIT}")
    if source is not None:
        source = _text(source, "source filter")
    if field is not None:
        field = _text(field, "field filter")
    manifest, root = _load_manifest(manifest_path)
    parts = _parse_parts(manifest, root)
    plan = _sample_plan(manifest, parts, source)
    rows, read_rows = _read_rows(plan, field, deadline, _clock, _worker)
    if _clock() >= deadline:
        raise InspectionError("whole-inspection deadline expired before output admission")

    used_schema_ids = {part.schema_id for item in plan for part, _ in item["locations"]}
    schema_parts = {part.schema_id: part for part in parts.values() if part.schema_id in used_schema_ids}
    schemas = []
    omitted_fields = 0
    dtypes: dict[str, set[str]] = defaultdict(set)
    for schema_id, part in schema_parts.items():
        provenance = f"_lvu_type_{field}" if field else None
        visible = [(name, dtype) for name, dtype in part.fields
                   if field is None or name in (field, provenance)]
        omitted = len(part.fields) - len(visible)
        omitted_fields += omitted
        for name, dtype in visible:
            dtypes[name].add(dtype)
        schemas.append({"schema_id": schema_id,
                        "fields": [{"name": name, "dtype": dtype} for name, dtype in visible],
                        "projected_fields": [name for name, _ in visible],
                        "omitted_field_count": omitted})
    sources = []
    for item in plan:
        locations = [{"path": part.path, "part_kind": part.kind, "schema_id": part.schema_id,
                      "requested_offsets": offsets} for part, offsets in item["locations"]]
        sources.append({"source_id": item["source_id"], "dataset": item["dataset"],
                        "available_rows": item["available_rows"],
                        "requested_rows": sum(len(offsets) for _, offsets in item["locations"]),
                        "read_rows": sum(len(offsets) for _, offsets in item["locations"]),
                        "admitted_rows": 0, "parts": locations})
    conflicts = [{"field": name, "dtypes": sorted(types)}
                 for name, types in sorted(dtypes.items()) if len(types) > 1]
    projected = [field, f"_lvu_type_{field}"] if field else sorted(dtypes)
    projected = [name for name in projected if name is not None and name in dtypes]
    schema_bytes = [len(_json_bytes(item)) for item in schemas]
    source_bytes = [len(_json_bytes(item)) for item in sources]
    row_budget_bytes = [len(_json_bytes({**item, "schema_available": False})) for item in rows]
    conflict_bytes = [len(_json_bytes(item)) for item in conflicts]
    projected_bytes = [len(_json_bytes(item)) for item in projected]
    result = {
        "version": 1, "manifest": str(manifest_path.resolve()),
        "limits": {"max_output_bytes": max_bytes, "max_cell_bytes": MAX_CELL_BYTES,
                   "max_nested_items": MAX_NESTED_ITEMS, "max_nesting": MAX_NESTING,
                   "worker_extra_address_space_bytes": WORKER_EXTRA_ADDRESS_SPACE,
                   "whole_inspection_seconds": INSPECTION_DEADLINE_SECONDS},
        "filters": {"source": source, "field": field}, "schemas": [], "sources": [],
        "rows": [], "coverage": {"requested_rows": sum(item["requested_rows"] for item in sources),
            "available_rows": sum(item["available_rows"] for item in sources),
            "read_rows": read_rows, "admitted_rows": 0, "omitted_rows": read_rows,
            "omitted_serialized_bytes": sum(schema_bytes + source_bytes + row_budget_bytes + conflict_bytes + projected_bytes),
            "omitted_schemas": len(schemas), "omitted_sources": len(sources),
            "omitted_type_conflicts": len(conflicts), "omitted_projected_fields": len(projected),
            "projected_fields": [],
            "omitted_fields": omitted_fields,
            "type_conflicts": []},
    }
    if len(_json_bytes(result)) > max_bytes:
        raise InspectionError("max-bytes is too small for inspection metadata")

    def admit(collection: list[Any], target: dict[str, Any], key: str,
              omitted_key: str, sizes: list[int]) -> None:
        for item, size in zip(collection, sizes, strict=True):
            if _clock() >= deadline:
                raise InspectionError("whole-inspection deadline expired during output admission")
            target[key].append(item)
            result["coverage"][omitted_key] -= 1
            result["coverage"]["omitted_serialized_bytes"] -= size
            if len(_json_bytes(result)) > max_bytes:
                target[key].pop()
                result["coverage"][omitted_key] += 1
                result["coverage"]["omitted_serialized_bytes"] += size

    admit(schemas, result, "schemas", "omitted_schemas", schema_bytes)
    admit(sources, result, "sources", "omitted_sources", source_bytes)
    admit(conflicts, result["coverage"], "type_conflicts", "omitted_type_conflicts", conflict_bytes)
    admit(projected, result["coverage"], "projected_fields", "omitted_projected_fields", projected_bytes)
    available_schema_ids = {item["schema_id"] for item in result["schemas"]}
    annotated_rows = [{**row, "schema_available": row["schema_id"] in available_schema_ids}
                      for row in rows]
    row_bytes = [len(_json_bytes(row)) for row in annotated_rows]
    result["coverage"]["omitted_serialized_bytes"] -= sum(row_budget_bytes) - sum(row_bytes)
    source_by_id = {item["source_id"]: item for item in result["sources"]}
    for candidate, size in zip(annotated_rows, row_bytes, strict=True):
        if _clock() >= deadline:
            raise InspectionError("whole-inspection deadline expired during output admission")
        result["rows"].append(candidate)
        result["coverage"]["admitted_rows"] += 1
        result["coverage"]["omitted_rows"] = read_rows - result["coverage"]["admitted_rows"]
        result["coverage"]["omitted_serialized_bytes"] -= size
        source_item = source_by_id.get(candidate["source_id"])
        if source_item is not None:
            source_item["admitted_rows"] += 1
        if len(_json_bytes(result)) > max_bytes:
            result["rows"].pop()
            result["coverage"]["admitted_rows"] -= 1
            result["coverage"]["omitted_rows"] = read_rows - result["coverage"]["admitted_rows"]
            result["coverage"]["omitted_serialized_bytes"] += size
            if source_item is not None:
                source_item["admitted_rows"] -= 1
    if _clock() >= deadline:
        raise InspectionError("whole-inspection deadline expired before returning output")
    return result


def main(argv: list[str] | None = None) -> int:
    if argv == ["--_bounded-worker"] or (argv is None and sys.argv[1:] == ["--_bounded-worker"]):
        try:
            payload = sys.stdin.buffer.read(MANIFEST_LIMIT + 1)
            if len(payload) > MANIFEST_LIMIT:
                raise InspectionError("bounded worker request is too large")
            request = json.loads(payload)
            sys.stdout.buffer.write(_json_bytes(_bounded_worker(request)))
            return 0
        except (InspectionError, MemoryError, OSError, pl.exceptions.PolarsError) as error:
            print(str(error), file=sys.stderr)
            return 2
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("manifest", type=Path)
    parser.add_argument("--source")
    parser.add_argument("--field")
    parser.add_argument("--max-bytes", type=int, default=OUTPUT_LIMIT)
    args = parser.parse_args(argv)
    try:
        output = inspect(args.manifest, args.source, args.field, args.max_bytes)
    except (InspectionError, OSError, pl.exceptions.PolarsError) as error:
        print(json.dumps({"version": 1, "error": str(error)}, separators=(",", ":")), file=sys.stderr)
        return 2
    sys.stdout.buffer.write(_json_bytes(output) + b"\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
