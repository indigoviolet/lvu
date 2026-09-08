#!/usr/bin/env python3
"""Refuse a matrix that would run yesterday's binaries.

The PTY matrix executes `$CARGO_TARGET_DIR/debug/lvu` and `lvu-app`; nothing
between the build and the run checks that those two files were produced from the
sources on disk. When they were not, every suite still runs and the failures
look like product bugs: they are deterministic, they point at real assertions,
and they are wrong.

Two ways that happens here, and this catches both because it asks the same
question of both:

* the build did not happen, or did not rebuild what changed;
* the build and the matrix disagreed about `CARGO_TARGET_DIR` — one shell
  exported the worktree's target and the other took the default — so the matrix
  is reading the *other* target's older binaries. That is the shape the primary
  checkout hit after the Polars `extract_jsonpath` feature change.

The comparison is mtime against the newest input: every `crates/**` source and
manifest, plus the workspace `Cargo.lock`, because a feature change lives in a
manifest and a dependency change lives in the lock. Equal mtimes pass: a build
that finishes in the same second as the checkout that fed it is fresh, and
filesystem timestamp resolution is not evidence of staleness.
"""

from __future__ import annotations

import argparse
import os
import pathlib
import sys

REPO = pathlib.Path(__file__).resolve().parent.parent
BINARIES = ("lvu", "lvu-app")
# Extensions that end up compiled in. A `.md` under `crates/` does not.
SOURCE_SUFFIXES = {".rs", ".toml"}
# Printed when a binary is stale; more than this is a list, not a reason.
MAX_NAMED = 5


def inputs() -> list[pathlib.Path]:
    """Every file whose change should invalidate a binary."""
    found = [
        path
        for path in (REPO / "crates").rglob("*")
        if path.is_file() and path.suffix in SOURCE_SUFFIXES
    ]
    for name in ("Cargo.lock", "Cargo.toml"):
        candidate = REPO / name
        if candidate.is_file():
            found.append(candidate)
    return found


def newest(paths: list[pathlib.Path]) -> tuple[float, pathlib.Path | None]:
    best: tuple[float, pathlib.Path | None] = (0.0, None)
    for path in paths:
        try:
            stamp = path.stat().st_mtime
        except OSError:
            continue
        if stamp > best[0]:
            best = (stamp, path)
    return best


def relative(path: pathlib.Path) -> str:
    try:
        return str(path.relative_to(REPO))
    except ValueError:
        return str(path)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--target",
        default=os.environ.get("CARGO_TARGET_DIR", "target") + "/debug",
        help="binary directory; defaults to $CARGO_TARGET_DIR/debug, which is "
        "what the matrix itself resolves, so a target-directory mismatch "
        "between the build and the run is caught rather than hidden",
    )
    arguments = parser.parse_args()
    target = pathlib.Path(arguments.target)

    stamped = [(path, path.stat().st_mtime) for path in inputs()]
    source_stamp, source_path = newest([path for path, _ in stamped])
    stale: list[tuple[str, str]] = []
    missing: list[str] = []
    oldest_binary = source_stamp
    for name in BINARIES:
        binary = target / name
        if not binary.is_file():
            missing.append(name)
            continue
        stamp = binary.stat().st_mtime
        oldest_binary = min(oldest_binary, stamp)
        if stamp < source_stamp:
            stale.append((name, f"{source_stamp - stamp:.0f}s older"))

    if not stale and not missing:
        return 0

    print(f"Refusing the matrix: {target} does not match the sources.", file=sys.stderr)
    for name in missing:
        print(f"  {name}: not built", file=sys.stderr)
    for name, age in stale:
        print(f"  {name}: {age} than the newest input", file=sys.stderr)
    # Which inputs, not just how many: the point of refusing is to say what to
    # rebuild, and the list is bounded because a long one is not a reason.
    newer = sorted(
        (path for path, stamp in stamped if stamp > oldest_binary),
        key=lambda path: path.stat().st_mtime,
        reverse=True,
    )
    if source_path is not None:
        print(f"  newest input: {relative(source_path)}", file=sys.stderr)
    for path in newer[1:MAX_NAMED]:
        print(f"                {relative(path)}", file=sys.stderr)
    if len(newer) > MAX_NAMED:
        print(f"                … and {len(newer) - MAX_NAMED} more", file=sys.stderr)
    print(
        "  Build into this target, or export the CARGO_TARGET_DIR the build used.",
        file=sys.stderr,
    )
    return 1


if __name__ == "__main__":
    sys.exit(main())
