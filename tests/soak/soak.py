#!/usr/bin/env python3
"""Drive the real binary through a long mixed workload and record what it costs.

The matrix proves behaviour; nothing proved that behaviour holds for half an
hour over gigabytes. This runs user-like activity — filters, All events, time
windows, folding, bookmarks, restarts — against a file source, a chatty command
source and a stdin capture, and samples the things that only a long run can
show: resident memory, derived-index and journal growth, the slowest input-loop
iteration, query latency, and shutdown time.

It is not part of the matrix. `--mode short` is a few minutes for a change that
touches these paths; `--mode long` is the real soak.

Disk: the run needs roughly three times the source size — the source, its
journal and the derived index — under `--root`, which defaults to a `soak`
directory on the build volume. Generated input is removed at the end unless
`--keep-inputs` is given, because long mode writes several gigabytes onto a
volume other agents are building on.

Thresholds are deliberate, not decorative. Resident memory that keeps climbing
across cycles is a leak whatever the absolute number. A query the user waits
more than a second for on an idle 16-core box is a defect, not a slow machine.
"""

from __future__ import annotations

import argparse
import json
import os
import pathlib
import re
import shlex
import shutil
import statistics
import subprocess
import sys
import time

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent.parent / "pty"))

from test_lvu_pty import PtyApp, isolated_environment, load_average  # noqa: E402

MODES = {
    # `cycles` is per app instance, so a run performs cycles * (restarts + 1) of
    # them: enough samples in short mode to see a trend, not only two points.
    "short": {"bytes": 64 * 1024 * 1024, "cycles": 3, "restarts": 1},
    "long": {"bytes": 3 * 1024 * 1024 * 1024, "cycles": 10, "restarts": 3},
}
# A user waiting longer than this for a filter on an unloaded 16-core box is
# looking at a defect, not at their machine.
QUERY_P99_BUDGET = 1.0
# Resident memory is expected to move with cache and membership; what must not
# happen is that every cycle leaves more behind than the last.
LEAK_TOLERANCE_MIB = 64.0


def rss_mib(pid: int) -> float:
    try:
        for line in pathlib.Path(f"/proc/{pid}/status").read_text().splitlines():
            if line.startswith("VmRSS:"):
                return int(line.split()[1]) / 1024.0
    except (OSError, IndexError, ValueError):
        pass
    return 0.0


def tree_bytes(path: pathlib.Path) -> int:
    total = 0
    if not path.exists():
        return 0
    for entry in path.rglob("*"):
        try:
            if entry.is_file() and not entry.is_symlink():
                total += entry.stat().st_size
        except OSError:
            continue
    return total


class Metrics:
    def __init__(self) -> None:
        self.samples: list[dict] = []
        self.queries: list[float] = []
        self.shutdowns: list[float] = []
        self.slowest_iteration = 0.0
        self.notes: list[str] = []

    def sample(self, cycle: int, pid: int, capture: pathlib.Path, cache: pathlib.Path) -> None:
        resident = rss_mib(pid)
        if resident == 0.0:
            # The process is gone; a zero would read as a collapse in the trend
            # rather than as the absence of a reading.
            return
        self.samples.append(
            {
                "cycle": cycle,
                "seconds": round(time.monotonic() - START, 1),
                "rss_mib": round(resident, 1),
                "capture_mib": round(tree_bytes(capture) / 1048576.0, 1),
                "derived_mib": round(tree_bytes(cache) / 1048576.0, 1),
                "load": load_average().split()[0],
            }
        )

    def summary(self) -> dict:
        rss = [row["rss_mib"] for row in self.samples]
        return {
            "samples": self.samples,
            "rss_first": rss[0] if rss else 0,
            "rss_last": rss[-1] if rss else 0,
            "rss_max": max(rss) if rss else 0,
            "query_count": len(self.queries),
            "query_p50": round(statistics.median(self.queries), 3) if self.queries else 0,
            "query_p99": round(
                sorted(self.queries)[min(len(self.queries) - 1, int(len(self.queries) * 0.99))],
                3,
            )
            if self.queries
            else 0,
            "query_max": round(max(self.queries), 3) if self.queries else 0,
            "shutdown_max": round(max(self.shutdowns), 3) if self.shutdowns else 0,
            "slowest_input_iteration": self.slowest_iteration,
            "notes": self.notes,
        }


START = time.monotonic()


def file_source_records(text: str, counts: re.Pattern[str]) -> int:
    """The record count of the file source only.

    The command source is deliberately endless, so its count never settles; the
    left pane lists each source's count under its name, and only the file one
    answers "has the file been read yet".
    """
    lines = text.splitlines()
    for index, line in enumerate(lines):
        if "soak.log" not in line or index + 1 >= len(lines):
            continue
        found = counts.search(lines[index + 1])
        if found:
            return int(found.group(1))
    return 0


def wait_for_settled_capture(app: PtyApp, quiet_for: float = 3.0, limit: float = 900.0) -> float:
    """Block until the file source stops adding records, and say how long it took.

    The command source never settles by design, so this watches the record count
    reported for the file source and treats it as settled once it stops moving.
    """
    # The pane is narrow enough to clip the word "records" off the end.
    counts = re.compile(r"Running: (\d+)")
    deadline = time.monotonic() + limit
    started = time.monotonic()
    last = None
    unchanged_since = time.monotonic()
    while time.monotonic() < deadline:
        app.drain()
        current = file_source_records(app.text(), counts)
        if current != last:
            last = current
            unchanged_since = time.monotonic()
        elif time.monotonic() - unchanged_since >= quiet_for and current > 0:
            return time.monotonic() - started
        time.sleep(0.25)
    raise AssertionError("the file source never stopped growing")


def apply_search(app: PtyApp, literal: str, metrics: Metrics) -> None:
    """Time a filter from keypress to the status line reporting it settled."""
    app.send(b"/")
    # The same budget as the other waits here: on a loaded box the app can take
    # seconds to draw a surface, and a short wait measures the machine.
    app.wait_for("Search", timeout=60.0)
    app.send(b"\x01\x0b")
    app.send(b"\x1b[200~" + literal.encode() + b"\x1b[201~")
    started = time.monotonic()
    app.send(b"\r")
    app.wait_until(
        lambda text: f'search:"{literal}"' in text
        and ("query ready" in text or "No matches" in text),
        f"filter {literal!r} settled",
        timeout=120.0,
    )
    metrics.queries.append(time.monotonic() - started)
    app.send(b"\x1b")
    app.wait_until(lambda text: "Examples:" not in text, "search closed", timeout=60.0)


def open_and_close(app: PtyApp, key: bytes, marker: str) -> None:
    app.send(key)
    try:
        app.wait_for(marker, timeout=60.0)
    except AssertionError:
        # The surface never opened, so there is nothing to close and an Escape
        # here would quit the app instead.
        return
    app.send(b"\x1b")
    app.wait_until(lambda text: marker not in text, f"{marker} closed", timeout=60.0)
    # The viewport has to be listening again before the next key is sent.
    app.wait_until(lambda text: "? help" in text, "workspace has focus", timeout=60.0)


def exercise(app: PtyApp, cycle: int, metrics: Metrics) -> None:
    """One pass of the things a person does to a log all afternoon."""
    for literal in ("SOAK_MARKER", "ERROR", "req-00042", "region", "malformed"):
        apply_search(app, literal, metrics)
    # Clear back to everything so the next cycle starts from a full view.
    apply_search(app, "seq", metrics)
    for key in (b"g", b"G", b"j", b"k"):
        app.send(key)
        time.sleep(0.05)
        app.drain()
    # Escape closes one layer and, at the top, closes the app: every surface is
    # opened and then closed against a marker rather than by pressing Escape a
    # few times and hoping.
    # Dialog titles, not words that also appear in panes and footers: an
    # ambiguous marker reads as "closed" while the surface is still up, and the
    # next keystroke goes into it instead of the viewport.
    open_and_close(app, b"d", "Details")
    open_and_close(app, b"i", "Fields ")
    app.send(b"b")  # bookmark the selected row; no surface opens
    time.sleep(0.1)
    open_and_close(app, b"B", "Bookmarks / notes")


def read_probe_lines(app: PtyApp, metrics: Metrics) -> None:
    tail = bytes(app.transcript[-16000:]).decode("utf-8", "replace")
    loop = re.findall(r"lvu-app input loop: \d+ iterations, slowest ([0-9.]+)s", tail)
    if loop:
        metrics.slowest_iteration = max(metrics.slowest_iteration, float(loop[-1]))
    shutdown = re.findall(r"lvu-app shutdown ([0-9.]+)s total", tail)
    if shutdown:
        metrics.shutdowns.append(float(shutdown[-1]))


def run(binary: pathlib.Path, root: pathlib.Path, mode: dict, metrics: Metrics) -> None:
    source = root / "soak.log"
    if not source.exists():
        subprocess.run(
            [sys.executable, str(pathlib.Path(__file__).with_name("generate.py")),
             str(source), "--bytes", str(mode["bytes"])],
            check=True,
        )
    capture = root / "capture"
    environment = {**isolated_environment(root), "LVU_SHUTDOWN_TIMING": "1"}
    cache = pathlib.Path(environment["XDG_CACHE_HOME"]) / "lvu"
    chatty = (
        "i=0; while :; do "
        "printf '%s\\n' \"{\\\"level\\\":\\\"INFO\\\",\\\"service\\\":\\\"chatty\\\",\\\"seq\\\":$i}\"; "
        "i=$((i+1)); sleep 0.05; done"
    )

    cycles_per_restart = mode["cycles"]
    cycle = 0
    for restart in range(mode["restarts"] + 1):
        app = PtyApp(
            binary,
            ["--capture-dir", str(capture), "--file", str(source), "--command", chatty],
            width=160,
            height=44,
            environment=environment,
        )
        try:
            app.wait_until(
                lambda text: "seq" in text or "handled request" in text,
                "first rows after start",
                # Capture of the generated source is measured at roughly
                # 10 MB/s, so even short mode reads for minutes, and this runs
                # on a machine other work shares.
                timeout=900.0,
            )
            # Query latency is about queries. Measuring while the file is still
            # being read measures ingest, and the two have different budgets, so
            # wait for the file source to stop growing before timing anything.
            wait_for_settled_capture(app)
            metrics.sample(cycle, app.process.pid, capture, cache)
            for _ in range(cycles_per_restart):
                cycle += 1
                exercise(app, cycle, metrics)
                metrics.sample(cycle, app.process.pid, capture, cache)
                print(f"  cycle {cycle}: {json.dumps(metrics.samples[-1])}", flush=True)
            started = time.monotonic()
            app.send(b"q")
            code = app.wait_exit(timeout=120.0)
            if code != 0:
                metrics.notes.append(f"restart {restart}: exit code {code}")
            metrics.shutdowns.append(time.monotonic() - started)
            read_probe_lines(app, metrics)
            app.assert_restored()
        finally:
            if app.process.poll() is None:
                app.process.kill()
                app.process.wait()
            app.close()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=pathlib.Path)
    parser.add_argument("--mode", choices=sorted(MODES), default="short")
    parser.add_argument("--root", type=pathlib.Path, default=None)
    parser.add_argument("--report", type=pathlib.Path, default=None)
    parser.add_argument(
        "--keep-inputs",
        action="store_true",
        help="leave the generated source and captures behind for inspection",
    )
    arguments = parser.parse_args()

    mode = MODES[arguments.mode]
    root = arguments.root or pathlib.Path(
        os.environ.get("LVU_SOAK_ROOT", "/mnt/HC_Volume_106796581/lvu-build/soak")
    )
    # Start from nothing. A root left behind by an interrupted run carries a
    # half-written capture and a restored filter, and the first thing the soak
    # would measure is that instead of the workload it just generated.
    if root.exists() and not arguments.keep_inputs:
        shutil.rmtree(root, ignore_errors=True)
    root.mkdir(parents=True, exist_ok=True)
    metrics = Metrics()
    print(f"soak {arguments.mode}: root={root} load={load_average()}", flush=True)
    print("  (query timings start once the file source has settled)", flush=True)
    run(arguments.binary.resolve(), root, mode, metrics)

    if not arguments.keep_inputs:
        # Long mode writes a multi-GB source and captures a copy of it into the
        # journal, on a volume several agents share. Generated input is cheap to
        # recreate and expensive to leave lying around.
        shutil.rmtree(root, ignore_errors=True)

    summary = metrics.summary()
    print(json.dumps(summary, indent=2), flush=True)
    if arguments.report:
        arguments.report.write_text(json.dumps(summary, indent=2))

    failures = []
    growth = summary["rss_last"] - summary["rss_first"]
    if growth > LEAK_TOLERANCE_MIB:
        failures.append(
            f"resident memory grew {growth:.1f} MiB across the run "
            f"({summary['rss_first']:.1f} -> {summary['rss_last']:.1f})"
        )
    if summary["query_p99"] > QUERY_P99_BUDGET:
        failures.append(
            f"query p99 {summary['query_p99']:.3f}s exceeds {QUERY_P99_BUDGET:.1f}s"
        )
    failures.extend(summary["notes"])
    for failure in failures:
        print(f"SOAK FAILURE: {failure}", flush=True)
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
