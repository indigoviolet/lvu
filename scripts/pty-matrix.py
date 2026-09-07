#!/usr/bin/env python3
"""Run the PTY suites concurrently and report a stable pass/fail matrix.

Each suite isolates its own capture dir and XDG state, so they are independent.
Sequential execution of the whole matrix is the slowest step in an integration,
and it is dominated by a few long suites, so bounded concurrency helps a lot.
Concurrency is capped well below the core count: these are terminal-timing
tests, and oversubscribing the machine makes them flaky rather than fast.
"""

from __future__ import annotations

import argparse
import concurrent.futures
import pathlib
import subprocess
import sys
import time

REPO = pathlib.Path(__file__).resolve().parent.parent
SKIP = {"test_ssh_pty.py"}
# Suites whose invocation does not follow the --demo convention.
EXPLICIT_ARGS = {"test_palette_pty.py": ["--real"]}


def binary_for(path: pathlib.Path, target: pathlib.Path) -> list[str]:
    if path.name in EXPLICIT_ARGS:
        return [str(target / "lvu-app"), *EXPLICIT_ARGS[path.name]]
    demo = '"--demo"' in path.read_text()
    return [str(target / ("lvu" if demo else "lvu-app"))]


def run_one(path: pathlib.Path, target: pathlib.Path, timeout: int) -> tuple[str, bool, float, str]:
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
    return path.name, ok, time.monotonic() - started, output


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", default="/mnt/HC_Volume_106796581/lvu-build/target/debug")
    parser.add_argument("--workers", type=int, default=4)
    parser.add_argument("--timeout", type=int, default=900)
    parser.add_argument("--only", nargs="*", help="suite name substrings")
    arguments = parser.parse_args()

    target = pathlib.Path(arguments.target)
    suites = sorted(
        p for p in (REPO / "tests" / "pty").glob("test_*_pty.py") if p.name not in SKIP
    )
    if arguments.only:
        suites = [p for p in suites if any(token in p.name for token in arguments.only)]

    started = time.monotonic()
    failures: list[tuple[str, str]] = []
    passed = 0
    with concurrent.futures.ThreadPoolExecutor(max_workers=arguments.workers) as pool:
        futures = [pool.submit(run_one, p, target, arguments.timeout) for p in suites]
        for future in concurrent.futures.as_completed(futures):
            name, ok, seconds, output = future.result()
            if ok:
                passed += 1
            else:
                failures.append((name, output))
                print(f"FAIL {name} ({seconds:.0f}s)", flush=True)

    elapsed = time.monotonic() - started
    print(f"\nPTY matrix: {passed}/{len(suites)} passed in {elapsed:.0f}s "
          f"({arguments.workers} workers)")
    for name, output in failures:
        tail = [line for line in output.splitlines() if "Error" in line or "assert" in line]
        print(f"  {name}: {tail[-1][:140] if tail else 'see log'}")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
