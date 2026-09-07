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
import os
import pathlib
import re
import shutil
import sys
import time

STEM = re.compile(r"^(?P<stem>.+?)-[0-9a-f]{8,}(?P<ext>\.[^.]+|)$")
PROTECTED = ("proof", "previews")


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

    verb = "would reclaim" if arguments.dry_run else "reclaimed"
    print(f"janitor {verb} {freed / 1e9:.2f} GB")
    return 0


if __name__ == "__main__":
    sys.exit(main())
