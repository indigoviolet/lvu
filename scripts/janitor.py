#!/usr/bin/env python3
"""Reclaim disposable build and test scratch. Never touches durable data.

Cargo does not garbage-collect superseded artifacts: a changed feature set or
profile gives a crate a new hash and leaves the previous copy in
target/debug/deps. Most of what accumulates there is still live, though, so the
gigabytes come from targets whose worktree is finished and from abandoned test
scratch, not from pruning deps.

Durable by definition and never removed here: previews/, capture directories,
proof archives, source, git objects, any cargo artifact that is the newest for
its stem or that cargo still holds a fingerprint for, anything in a target a
build is writing to right now, and any per-worktree target whose worktree still
exists with work that `main` does not yet contain.

`--stale-flags` adds one thing to that: artifacts a build with different
`RUSTFLAGS` or a different rustc left behind. They are live by the fingerprint
test and dead in fact, and after a flag change they are most of a target.
"""

from __future__ import annotations

import argparse
import fcntl
import json
import os
import signal
import subprocess
import pathlib
import re
import shutil
import sys
import time

STEM = re.compile(r"^(?P<stem>.+?)-(?P<hash>[0-9a-f]{8,})(?P<ext>\.[^.]+|)$")
PROTECTED = ("proof", "previews")
# Directories only this harness creates, so a match is never a real workload.
WORKTREES = pathlib.Path("/home/venky/.paseo/worktrees/2hywlzbe")
BUILD_VOLUME = pathlib.Path("/mnt/HC_Volume_106796581/lvu-build")
TARGET_SUFFIX = "-target"
HARNESS_TEMP = re.compile(r"/tmp/lvu-[a-z0-9_-]*(?:pty|scratch)[a-z0-9_-]*")
UUID = re.compile(r"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$")
# `<source uuid>.<journal uuid>.rows.idx`, the only shape the product writes.
DERIVED_INDEX = re.compile(
    r"^(?P<source>[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})"
    r"\.[^.]+\.rows\.idx$"
)


def live_hashes(target: pathlib.Path) -> set[str]:
    """The artifact hashes cargo still has a fingerprint for.

    `deps/libpolars_core-<hash>.rlib` belongs to `.fingerprint/polars-core-<hash>`,
    and cargo reuses an artifact for as long as that fingerprint says it is
    fresh. Several hashes of one crate are live at once whenever the same crate
    is built under two configurations — a lib and a test unit, a build script
    and its host, a feature set unified differently — so "newest for its stem" is not the
    same question as "still in use", and answering the wrong one deletes a file
    the next build expects to find. That failure surfaces as `error[E0463]: can't
    find crate`, minutes into someone else's build, on a machine where the
    janitor and eleven worktrees share one volume.
    """
    fingerprints = target / "debug" / ".fingerprint"
    if not fingerprints.is_dir():
        return set()
    hashes = set()
    try:
        for entry in fingerprints.iterdir():
            if entry.is_dir() and "-" in entry.name:
                hashes.add(entry.name.rsplit("-", 1)[1])
    except OSError:
        return set()
    return hashes


def build_in_progress(target: pathlib.Path) -> bool:
    """True while a cargo build holds this target's lock.

    Cargo takes it for the whole build, so this is the same question cargo asks
    before writing, not a guess from timestamps. Pruning under a live build is
    how an artifact disappears between the moment cargo decides it is fresh and
    the moment rustc opens it.
    """
    lock = target / "debug" / ".cargo-lock"
    return lock.is_file() and not unlocked(lock)


def unit_settings(target: pathlib.Path) -> dict[str, tuple]:
    """Each artifact hash mapped to the build-wide settings it was compiled under.

    Cargo records the compiler and the flags in every unit's fingerprint. Both
    are properties of the whole build rather than of the unit — no two units of
    one `cargo build` can disagree about them — so a fingerprint carrying a
    different pair was left by a different build, and nothing the current
    configuration compiles will ever read it again. Everything else in the
    fingerprint (features, profile, the dependency hashes) legitimately differs
    between two units that are both live, which is why this asks only about the
    two settings that cannot.
    """
    fingerprints = target / "debug" / ".fingerprint"
    if not fingerprints.is_dir():
        return {}
    settings: dict[str, tuple] = {}
    try:
        entries = list(fingerprints.iterdir())
    except OSError:
        return {}
    for entry in entries:
        if not entry.is_dir() or "-" not in entry.name:
            continue
        try:
            described = next(f for f in entry.iterdir() if f.suffix == ".json")
            data = json.loads(described.read_text())
        except (OSError, StopIteration, ValueError):
            continue
        settings[entry.name.rsplit("-", 1)[1]] = (
            data.get("rustc"),
            tuple(data.get("rustflags") or []),
        )
    return settings


def superseded_hashes(target: pathlib.Path, wanted: tuple[str, ...] | None) -> set[str]:
    """Hashes left behind by a build with different flags or a different rustc.

    The reference is the newest fingerprint in this target, not an average: the
    last build is the one whose artifacts the next build will reuse. When the
    caller knows which flags are canonical — the janitor reads `RUSTFLAGS` from
    the same environment every build here uses — a target whose newest build
    disagrees is left entirely alone. That case is somebody halfway through an
    investigation with `RUSTFLAGS` overridden, and both sets of artifacts are
    about to be wanted; uncertainty means do nothing, as everywhere else here.
    """
    settings = unit_settings(target)
    if not settings:
        return set()
    fingerprints = target / "debug" / ".fingerprint"
    newest_time = 0.0
    current: tuple | None = None
    for entry in fingerprints.iterdir():
        if not entry.is_dir() or "-" not in entry.name:
            continue
        key = settings.get(entry.name.rsplit("-", 1)[1])
        if key is None:
            continue
        try:
            when = entry.stat().st_mtime
        except OSError:
            continue
        if when > newest_time:
            newest_time, current = when, key
    if current is None:
        return set()
    if wanted is not None and current[1] != wanted:
        return set()
    return {digest for digest, key in settings.items() if key != current}


def sweep_superseded(targets: list[pathlib.Path], dry: bool) -> tuple[int, list[str]]:
    """Remove what a settings change orphaned, in deps, build and .fingerprint.

    A flag change gives every crate a new hash and leaves the whole previous
    dependency set on disk — 3 GB per target the day `RUSTFLAGS` gained one
    entry. Cargo never collects it and the fingerprint guard in `prune_target`
    deliberately will not, because from its point of view those artifacts are
    perfectly live; they are simply live for a build nobody will run again.
    """
    wanted = os.environ.get("RUSTFLAGS")
    wanted_flags = tuple(wanted.split()) if wanted is not None else None
    freed = 0
    reasons = []
    for target in targets:
        if not target.is_dir() or build_in_progress(target):
            continue
        dead = superseded_hashes(target, wanted_flags)
        if not dead:
            continue
        amount = 0
        for directory in ("deps", "build", ".fingerprint"):
            root = target / "debug" / directory
            if not root.is_dir():
                continue
            for entry in list(root.iterdir()):
                match = STEM.match(entry.name)
                if not match or match["hash"] not in dead:
                    continue
                if entry.is_dir():
                    amount += sum(
                        f.stat().st_size for f in entry.rglob("*") if f.is_file()
                    )
                    if not dry:
                        shutil.rmtree(entry, ignore_errors=True)
                else:
                    amount += entry.stat().st_size
                    if not dry:
                        entry.unlink(missing_ok=True)
        if amount:
            reasons.append(
                f"{target.name}: {len(dead)} units from an earlier flag set "
                f"({amount / 1e9:.2f} GB)"
            )
            freed += amount
    return freed, reasons


def prune_target(target: pathlib.Path, keep: int, dry: bool) -> int:
    deps = target / "debug" / "deps"
    if not deps.is_dir() or build_in_progress(target):
        return 0
    live = live_hashes(target)
    groups: dict[tuple[str, str], list[pathlib.Path]] = {}
    for entry in deps.iterdir():
        if not entry.is_file():
            continue
        match = STEM.match(entry.name)
        if not match or match["hash"] in live:
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


def sweep_orphaned_fixtures(minutes: float, dry: bool) -> int:
    """Kill fixture processes that outlived the test run that started them.

    A PTY suite starts real commands — shells that loop, sleeps, follow
    processes — and a suite killed mid-run used to leave them behind. Each one
    is cheap alone; 87 of them are why the matrix went flaky.

    Only processes this user owns, only ones whose command line names a
    directory this harness creates, only ones already reparented to init (so no
    live test still owns them), and only after they are older than the
    threshold. The whole group goes, because a leaked shell has children.
    """
    try:
        uptime = float(pathlib.Path("/proc/uptime").read_text().split()[0])
    except (OSError, IndexError, ValueError):
        return 0
    ticks = os.sysconf("SC_CLK_TCK")
    killed = 0
    for entry in pathlib.Path("/proc").iterdir():
        if not entry.name.isdigit():
            continue
        try:
            if entry.stat().st_uid != os.getuid():
                continue
            command = (entry / "cmdline").read_bytes().replace(b"\0", b" ").decode(
                "utf-8", "replace"
            )
            # `/proc/<pid>` mtime tracks activity, not creation; field 22 of
            # `stat` is the only reading that means "how long has this been
            # running", which is the question being asked.
            fields = (entry / "stat").read_text().rsplit(") ", 1)[-1].split()
            parent = int(fields[1])
            age = uptime - float(fields[19]) / ticks
        except (OSError, IndexError, ValueError, ZeroDivisionError):
            continue
        if parent != 1 or age < minutes * 60 or not HARNESS_TEMP.search(command):
            continue
        killed += 1
        if not dry:
            try:
                os.killpg(os.getpgid(int(entry.name)), signal.SIGKILL)
            except (ProcessLookupError, PermissionError, OSError):
                pass
    return killed


def abandoned_targets(idle_hours: float) -> list[tuple[pathlib.Path, str]]:
    """Per-worktree cargo targets nobody can still need.

    Each is several gigabytes and cargo never removes one, so they outlive the
    work by days. Two conditions are safe to act on and nothing else is:

    - the worktree directory is gone, so nothing can rebuild there; or
    - the worktree exists, its branch is already contained in `main`, *and* the
      target has not been written for `idle_hours`.

    The age condition matters more than it looks. A branch shows as merged the
    moment its agent rebases onto main, before it has committed a line of the
    next assignment, so "merged" alone would delete the target of a worktree
    that is building right now — including this one. Recency is what separates
    finished from between-assignments.

    A worktree with unmerged work is never touched however old, and neither is
    the shared `target` the primary checkout uses, nor a target a build holds
    the lock on right now.
    """
    if not BUILD_VOLUME.is_dir():
        return []
    cutoff = time.time() - idle_hours * 3600
    found = []
    for entry in sorted(BUILD_VOLUME.glob(f"*{TARGET_SUFFIX}")):
        if not entry.is_dir() or entry.is_symlink() or build_in_progress(entry):
            continue
        name = entry.name[: -len(TARGET_SUFFIX)]
        worktree = WORKTREES / name
        if not worktree.is_dir():
            found.append((entry, "worktree is gone"))
            continue
        merged = branch_is_merged(worktree)
        if merged is None:
            continue
        touched = newest_mtime(entry)
        if touched > cutoff:
            continue
        idle = (time.time() - touched) / 3600
        found.append(
            (entry, f"branch {merged} is in main and it has been idle {idle:.1f}h")
        )
    return found


def newest_mtime(target: pathlib.Path) -> float:
    """When this target was last written, from the artifacts cargo touches."""
    newest = 0.0
    for relative in ("debug", "."):
        directory = target / relative
        if not directory.is_dir():
            continue
        try:
            for entry in directory.iterdir():
                newest = max(newest, entry.stat().st_mtime)
        except OSError:
            continue
    return newest


def branch_is_merged(worktree: pathlib.Path) -> str | None:
    """The worktree's branch name if `main` already contains its tip.

    Anything unexpected — a detached head, no `main`, git unavailable — returns
    `None`, so uncertainty always means "leave it alone".
    """
    try:
        branch = subprocess.run(
            ["git", "-C", str(worktree), "rev-parse", "--abbrev-ref", "HEAD"],
            capture_output=True, text=True, timeout=20, check=True,
        ).stdout.strip()
        if not branch or branch == "HEAD" or branch == "main":
            return None
        contained = subprocess.run(
            ["git", "-C", str(worktree), "branch", "--contains", branch, "--format=%(refname:short)"],
            capture_output=True, text=True, timeout=20, check=True,
        ).stdout.split()
    except (OSError, subprocess.SubprocessError):
        return None
    return branch if "main" in contained else None


def sweep_abandoned_targets(dry: bool, idle_hours: float) -> tuple[int, list[str]]:
    freed = 0
    reasons = []
    for target, reason in abandoned_targets(idle_hours):
        size = sum(f.stat().st_size for f in target.rglob("*") if f.is_file())
        freed += size
        reasons.append(f"{target.name}: {reason} ({size / 1e9:.2f} GB)")
        if not dry:
            shutil.rmtree(target, ignore_errors=True)
    return freed, reasons


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
    parser.add_argument(
        "--orphan-minutes",
        type=float,
        default=10.0,
        help="age before a reparented fixture process is swept",
    )
    parser.add_argument(
        "--target-idle-hours",
        type=float,
        default=6.0,
        help="how long a merged worktree's target must be untouched before it "
        "is reclaimed; a worktree that is gone is reclaimed regardless",
    )
    parser.add_argument(
        "--stale-flags",
        "--stale-profiles",
        dest="stale_flags",
        action="store_true",
        help="also remove artifacts left by a build with different RUSTFLAGS or "
        "a different rustc; a profile or flag change leaves a whole second "
        "dependency set that nothing will read again",
    )
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
    # The globs above overlap: `lvu-w13-…-target` matches two of them, and a
    # sweep that reports per target would count it twice.
    targets = list(dict.fromkeys(targets))
    freed = 0
    for target in targets:
        if target.is_dir():
            amount = prune_target(target, arguments.keep, arguments.dry_run)
            if amount:
                print(f"  {amount / 1e9:6.2f} GB  stale artifacts in {target}")
            freed += amount

    if arguments.stale_flags:
        superseded, notes = sweep_superseded(targets, arguments.dry_run)
        for note in notes:
            print(f"    {note}")
        if superseded:
            print(f"  {superseded / 1e9:6.2f} GB  artifacts from an earlier flag set")
        freed += superseded

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

    abandoned, reasons = sweep_abandoned_targets(
        arguments.dry_run, arguments.target_idle_hours
    )
    for reason in reasons:
        print(f"    {reason}")
    if abandoned:
        print(f"  {abandoned / 1e9:6.2f} GB  cargo targets with no live worktree")
    freed += abandoned

    orphans = sweep_orphaned_fixtures(arguments.orphan_minutes, arguments.dry_run)
    if orphans:
        verb = "would kill" if arguments.dry_run else "killed"
        print(f"  {orphans:6d}     orphaned fixture processes ({verb})")

    verb = "would reclaim" if arguments.dry_run else "reclaimed"
    print(f"janitor {verb} {freed / 1e9:.2f} GB")
    return 0


if __name__ == "__main__":
    sys.exit(main())
