#!/usr/bin/env python3
"""Run the PTY suites concurrently and report a stable pass/fail matrix.

Each suite isolates its own capture dir and XDG state, so they are independent.
Sequential execution of the whole matrix is the slowest step in an integration,
and it is dominated by a few long suites, so bounded concurrency helps a lot.
Concurrency is capped well below the core count: these are terminal-timing
tests, and oversubscribing the machine makes them flaky rather than fast.

A worker cap alone is not enough when several matrices share one machine. Four
of them on sixteen cores is sixteen concurrent apps, and at that load the suites
start failing on timing rather than on behaviour: lvu takes seconds to notice a
keypress, and assertions drop one at a time in different places each run. So a
worker also waits for the machine before it starts a suite, and the heaviest
suites go first — they are the ones with the least headroom, and starting them
last means they run while everything else is still finishing.
"""

from __future__ import annotations

import argparse
import concurrent.futures
import os
import pathlib
import threading
import subprocess
import sys
import time

REPO = pathlib.Path(__file__).resolve().parent.parent
SKIP = {"test_ssh_pty.py"}
# Measured wall time, longest first; anything unlisted sorts after these. Rough
# figures are enough: the point is ordering, not prediction, and a suite that
# drifts costs a little scheduling accuracy rather than correctness. Starting
# the long ones first keeps them off the end of the queue, where they would run
# with nothing to overlap and stretch the whole matrix.
LONGEST_FIRST = (
    "test_lvu_real_pty.py",
    "test_command_source_orphans_pty.py",
    "test_blank_view_pty.py",
    "test_gzip_pty.py",
    "test_capture_root_pty.py",
    "test_stdin_pty.py",
    "test_settings_pty.py",
    "test_search_race_pty.py",
)
# Length and fragility are different properties. These are the suites that drop
# a different assertion each time when the machine is busy — they drive the app
# through long interactive sequences where every step has its own deadline — so
# only one of them runs at a time. Serialising the merely long ones instead
# costs wall time and buys nothing.
CONTENTION_SENSITIVE = frozenset(
    {
        "test_lvu_real_pty.py",
        "test_dialog_system_pty.py",
        "test_shared_list_dialogs_pty.py",
    }
)
LOAD_POLL_SECONDS = 1.0
CONCURRENT_SENSITIVE = 1
_sensitive_slot = threading.Semaphore(CONCURRENT_SENSITIVE)
_start_gate = threading.Lock()
_last_start = 0.0
# A shared load gate releases every waiting worker at the same moment, which
# recreates the burst it was meant to avoid. Starts are spaced instead.
START_SPACING_SECONDS = 1.5
# Suites whose invocation does not follow the --demo convention.
EXPLICIT_ARGS = {"test_palette_pty.py": ["--real"]}


def load_average() -> float:
    try:
        return float(pathlib.Path("/proc/loadavg").read_text().split()[0])
    except (OSError, IndexError, ValueError):
        return 0.0


def wait_for_headroom(ceiling: float, patience: float) -> float:
    """Hold a worker until the machine has room, and report the load it saw.

    Bounded: after `patience` seconds the suite starts anyway. A matrix that
    refuses to run because a neighbour is busy is worse than one that runs
    slowly, and the load at start is recorded either way so a failure carries
    the condition it happened under.
    """
    global _last_start
    deadline = time.monotonic() + patience
    load = load_average()
    while load > ceiling and time.monotonic() < deadline:
        time.sleep(LOAD_POLL_SECONDS)
        load = load_average()
    with _start_gate:
        wait = _last_start + START_SPACING_SECONDS - time.monotonic()
        if wait > 0:
            time.sleep(wait)
        _last_start = time.monotonic()
    return load


def binary_for(path: pathlib.Path, target: pathlib.Path) -> list[str]:
    if path.name in EXPLICIT_ARGS:
        return [str(target / "lvu-app"), *EXPLICIT_ARGS[path.name]]
    demo = '"--demo"' in path.read_text()
    return [str(target / ("lvu" if demo else "lvu-app"))]


def run_one(
    path: pathlib.Path,
    target: pathlib.Path,
    timeout: int,
    ceiling: float,
    patience: float,
) -> tuple[str, bool, float, str, float]:
    sensitive = path.name in CONTENTION_SENSITIVE
    if sensitive:
        _sensitive_slot.acquire()
    try:
        return _run_one_locked(path, target, timeout, ceiling, patience)
    finally:
        if sensitive:
            _sensitive_slot.release()


def _run_one_locked(
    path: pathlib.Path,
    target: pathlib.Path,
    timeout: int,
    ceiling: float,
    patience: float,
) -> tuple[str, bool, float, str, float]:
    load = wait_for_headroom(ceiling, patience)
    started = time.monotonic()
    command = [
        "mise", "exec", "--", "uv", "run", "--project", "tests/pty", "--locked",
        "python", str(path), *binary_for(path, target),
    ]
    try:
        done = subprocess.run(
            command, cwd=REPO, capture_output=True, text=True, timeout=timeout
        )
        ok, output = done.returncode == 0, done.stdout + done.stderr
    except subprocess.TimeoutExpired:
        ok, output = False, f"timed out after {timeout}s"
    return path.name, ok, time.monotonic() - started, output, load


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--target",
        default=os.environ.get("CARGO_TARGET_DIR", "target") + "/debug",
        help="binary directory; defaults to $CARGO_TARGET_DIR/debug so a worktree "
        "tests its own build instead of the primary checkout's shared target",
    )
    parser.add_argument("--workers", type=int, default=4)
    parser.add_argument(
        "--load-ceiling",
        type=float,
        default=None,
        help="do not start a suite while the 1-minute load exceeds this "
        "(default: the core count)",
    )
    parser.add_argument(
        "--load-patience",
        type=float,
        default=20.0,
        help="seconds a worker waits for headroom before starting anyway; the "
        "wait is per suite, so a large value on a permanently busy machine "
        "costs far more than the contention it avoids",
    )
    parser.add_argument("--timeout", type=int, default=900)
    parser.add_argument("--only", nargs="*", help="suite name substrings")
    arguments = parser.parse_args()

    target = pathlib.Path(arguments.target)
    cores = os.cpu_count() or 4
    # Measured on a 16-core box shared by four agents: gating at half the cores
    # made every worker wait its full patience and stretched the matrix from
    # 180s to 300s without improving the pass rate, because the ambient load of
    # the other agents already exceeded the ceiling. Gating at the core count
    # engages only when the machine is genuinely oversubscribed; a busier host
    # can lower it with --load-ceiling.
    ceiling = (
        arguments.load_ceiling if arguments.load_ceiling is not None else float(cores)
    )
    suites = sorted(
        p for p in (REPO / "tests" / "pty").glob("test_*_pty.py") if p.name not in SKIP
    )
    if arguments.only:
        suites = [p for p in suites if any(token in p.name for token in arguments.only)]
    order = {name: index for index, name in enumerate(LONGEST_FIRST)}
    suites.sort(key=lambda p: (order.get(p.name, len(order)), p.name))

    started = time.monotonic()
    print(
        f"PTY matrix: {len(suites)} suites, {arguments.workers} workers, "
        f"{cores} cores, load ceiling {ceiling:.1f}, load now {load_average():.2f}",
        flush=True,
    )
    failures: list[tuple[str, str]] = []
    timings: list[tuple[str, float, float, bool]] = []
    passed = 0
    with concurrent.futures.ThreadPoolExecutor(max_workers=arguments.workers) as pool:
        futures = [
            pool.submit(
                run_one, p, target, arguments.timeout, ceiling, arguments.load_patience
            )
            for p in suites
        ]
        for future in concurrent.futures.as_completed(futures):
            name, ok, seconds, output, load = future.result()
            timings.append((name, seconds, load, ok))
            if ok:
                passed += 1
            else:
                failures.append((name, output))
                print(f"FAIL {name} ({seconds:.0f}s, load at start {load:.2f})", flush=True)

    elapsed = time.monotonic() - started
    print(f"\nPTY matrix: {passed}/{len(suites)} passed in {elapsed:.0f}s "
          f"({arguments.workers} workers, load ceiling {ceiling:.1f})")
    print("  slowest suites (wall time, load at start):")
    for name, seconds, load, ok in sorted(timings, key=lambda row: -row[1])[:8]:
        print(f"    {seconds:6.1f}s  load {load:5.2f}  {'ok  ' if ok else 'FAIL'} {name}")
    for name, output in failures:
        tail = [line for line in output.splitlines() if "Error" in line or "assert" in line]
        print(f"  {name}: {tail[-1][:140] if tail else 'see log'}")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
