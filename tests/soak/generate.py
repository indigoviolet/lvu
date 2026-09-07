#!/usr/bin/env python3
"""Deterministic mixed workload for the soak.

Real logs are not uniform, and a soak over uniform lines measures nothing that
production would hit: it never rebuilds a schema, never widens a column, never
makes a filter selective in one region and not another. So the file mixes JSON
and plain records, varies its field set part way through, includes a wide
Unicode record and a malformed one, and carries markers a driver can search for
at known positions.

Deterministic from a seed so a failure is reproducible and two runs can be
compared byte for byte.
"""

from __future__ import annotations

import argparse
import json
import pathlib
import random
import sys

SERVICES = ("api", "worker", "ingest", "scheduler")
LEVELS = ("INFO", "INFO", "INFO", "WARN", "ERROR", "DEBUG")


def record(index: int, rng: random.Random) -> str:
    service = SERVICES[index % len(SERVICES)]
    level = rng.choice(LEVELS)
    # A marker every 100_000 records so a driver can search for something it
    # knows is rare without scanning the file itself.
    marker = " SOAK_MARKER" if index % 100_000 == 0 else ""
    if index % 1_000 == 999:
        return "malformed { not json and not logfmt " + str(index) + marker
    if index % 997 == 0:
        return f'{{"level":"{level}","service":"{service}","wide":"界界é 東京","seq":{index}}}'
    if index % 3 == 0:
        payload = {
            "level": level,
            "service": service,
            "seq": index,
            "request_id": f"req-{index % 5000:05d}",
            "duration_ms": rng.randint(1, 4000),
            "message": f"handled request{marker}",
        }
        # The schema widens half way through, which is what makes a snapshot
        # carry two schemas and a filter meet both.
        if index > 400_000:
            payload["region"] = rng.choice(("eu-west", "us-east"))
        return json.dumps(payload)
    return (
        f"{level} service={service} seq={index} "
        f"request_id=req-{index % 5000:05d} "
        f"duration_ms={rng.randint(1, 4000)} message=handled request{marker}"
    )


def write(path: pathlib.Path, target_bytes: int, seed: int) -> tuple[int, int]:
    rng = random.Random(seed)
    written = 0
    index = 0
    with path.open("w", encoding="utf-8") as out:
        while written < target_bytes:
            line = record(index, rng)
            out.write(line + "\n")
            written += len(line.encode()) + 1
            index += 1
    return index, written


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("path", type=pathlib.Path)
    parser.add_argument("--bytes", type=int, default=64 * 1024 * 1024)
    parser.add_argument("--seed", type=int, default=20260907)
    arguments = parser.parse_args()
    records, written = write(arguments.path, arguments.bytes, arguments.seed)
    print(f"{records} records, {written} bytes -> {arguments.path}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
