#!/usr/bin/env python3
"""Reclaim disposable build and test scratch. Never touches durable data.

Cargo does not garbage-collect superseded artifacts: every rebuild leaves the
previous hash-suffixed copy in target/debug/deps. On this repo a single stale
lvu-app test binary is ~386 MB, so a few days of iteration fills the disk.

Durable by definition and never removed here: previews/, capture directories,
proof archives, source, git objects, and any cargo artifact that is the newest
for its stem.
"""

from __future__ import annotations

import argparse
import fcntl
import os
import pathlib
import re
import shutil
import sys
import time

STEM = re.compile(r"^(?P<stem>.+?)-[0-9a-f]{8,}(?P<ext>\.[^.]+|)$")
PROTECTED = ("proof", "previews")
UUID = re.compile(r"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$")
# `<source uuid>.<journal uuid>.rows.idx`, the only shape the product writes.
DERIVED_INDEX = re.compile(
    r"^(?P<source>[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})"
    r"\.[^.]+\.rows\.idx$"
)


def prune_target(target: pathlib.Path, keep: int, dry: bool) -> int:
    deps = target / "debug" / "deps"
    if not deps.is_dir():
        return 0
    groups: dict[tuple[str, str], list[pathlib.Path]] = {}
    for entry in deps.iterdir():
        if not entry.is_file():
            continue
        match = STEM.match(entry.name)
        if not match:
            continue
        groups.setdefault((match["stem"], match["ext"]), []).append(entry)

    freed = 0
    for (_stem, _ext), files in groups.items():
        if len(files) <= keep:
            continue
        files.sort(key=lambda p: p.stat().st_mtime, reverse=True)
        for stale in files[keep:]:
            size = stale.stat().st_size
            freed += size
            if not dry:
                stale.unlink(missing_ok=True)
    return freed


def sweep_scratch(prefixes: list[str], older_than_hours: float, dry: bool) -> int:
    cutoff = time.time() - older_than_hours * 3600
    freed = 0
    for prefix in prefixes:
        for entry in pathlib.Path("/tmp").glob(f"{prefix}*"):
            if not entry.is_dir() or entry.is_symlink():
                continue
            if any(token in entry.name for token in PROTECTED):
                continue
            if entry.stat().st_mtime > cutoff or entry.stat().st_uid != os.getuid():
                continue
            size = sum(f.stat().st_size for f in entry.rglob("*") if f.is_file())
            freed += size
            if not dry:
                shutil.rmtree(entry, ignore_errors=True)
    return freed


def capture_roots() -> list[pathlib.Path]:
    """Where captures live, so an index can be matched to one."""
    data = os.environ.get("XDG_DATA_HOME") or str(pathlib.Path.home() / ".local/share")
    roots = [pathlib.Path(data) / "lvu"]
    legacy = pathlib.Path(__file__).resolve().parent.parent / ".lvu-captures"
    if legacy.is_dir():
        roots.append(legacy)
    return [root for root in roots if root.is_dir()]


def captured_source_ids(roots: list[pathlib.Path]) -> set[str]:
    ids: set[str] = set()
    for root in roots:
        for entry in root.iterdir():
            if entry.is_dir() and UUID.match(entry.name):
                ids.add(entry.name)
    return ids


def unlocked(path: pathlib.Path) -> bool:
    """True when no process holds this index, using the product's own lock.

    A live provider keeps its index exclusively locked for as long as it is
    open, so this is the same question the product asks before treating an index
    as inactive — not a guess from timestamps.
    """
    try:
        handle = os.open(path, os.O_RDWR)
    except OSError:
        return False
    try:
        fcntl.flock(handle, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except OSError:
        return False
    finally:
        os.close(handle)
    return True


def sweep_derived_indexes(dry: bool) -> tuple[int, int]:
    """Remove derived indexes whose capture no longer exists.

    A derived index is a rebuildable projection of a journal, not captured data:
    the product recreates it on demand. Left behind, they accumulate — a day of
    PTY runs leaked 707 of them into the real cache — and once the directory
    holds more than one bounded reconciliation can account for, the shared
    budget goes unverified for every source that follows.

    Captures, previews and proof archives are never considered: only files whose
    name is the product's own `<source>.<journal>.rows.idx` shape, under the
    cache's `derived` directory, whose source has no capture, and which no
    process currently holds.
    """
    cache = os.environ.get("XDG_CACHE_HOME") or str(pathlib.Path.home() / ".cache")
    derived = pathlib.Path(cache) / "lvu" / "derived"
    if not derived.is_dir():
        return 0, 0
    referenced = captured_source_ids(capture_roots())
    freed = 0
    removed = 0
    for entry in sorted(derived.iterdir()):
        if not entry.is_file() or entry.is_symlink():
            continue
        match = DERIVED_INDEX.match(entry.name)
        if not match or match["source"] in referenced:
            continue
        if not unlocked(entry):
            continue
        freed += entry.stat().st_size
        removed += 1
        if not dry:
            entry.unlink(missing_ok=True)
    return freed, removed


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--keep", type=int, default=1, help="artifacts kept per stem")
    parser.add_argument("--hours", type=float, default=2.0, help="scratch age threshold")
    parser.add_argument("--dry-run", action="store_true")
    arguments = parser.parse_args()

    roots = [pathlib.Path("/tmp"), pathlib.Path("/mnt/HC_Volume_106796581/lvu-build")]
    configured = os.environ.get("CARGO_TARGET_DIR")
    targets: list[pathlib.Path] = []
    for root in roots:
        if not root.is_dir():
            continue
        targets += sorted(root.glob("lvu-*target*"))
        targets += sorted(root.glob("lvu-t-*"))
        targets += sorted(root.glob("*-target"))
        if (root / "target").is_dir():
            targets.append(root / "target")
    if configured:
        candidate = pathlib.Path(configured)
        if candidate.is_dir() and candidate not in targets:
            targets.append(candidate)
    freed = 0
    for target in targets:
        if target.is_dir():
            amount = prune_target(target, arguments.keep, arguments.dry_run)
            if amount:
                print(f"  {amount / 1e9:6.2f} GB  stale artifacts in {target}")
            freed += amount

    scratch = sweep_scratch(
        ["lvu-pty-scratch-", "lvurt-", "lvu-qq-runtime-"], arguments.hours, arguments.dry_run
    )
    if scratch:
        print(f"  {scratch / 1e9:6.2f} GB  abandoned PTY scratch")
    freed += scratch

    indexes, index_count = sweep_derived_indexes(arguments.dry_run)
    if index_count:
        print(
            f"  {indexes / 1e9:6.2f} GB  {index_count} derived indexes with no capture"
        )
    freed += indexes

    verb = "would reclaim" if arguments.dry_run else "reclaimed"
    print(f"janitor {verb} {freed / 1e9:.2f} GB")
    return 0


if __name__ == "__main__":
    sys.exit(main())
