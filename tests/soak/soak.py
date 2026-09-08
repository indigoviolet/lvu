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
import math
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
    # Sized to settle inside the wait below on this box rather than to be
    # impressive. At the measured capture rate 3 GB needs about an hour, which
    # is longer than any patience the harness has and longer than the volume
    # several agents share can spare; `--bytes` asks for more when there is time
    # for it.
    "long": {"bytes": 800 * 1024 * 1024, "cycles": 10, "restarts": 3},
}
# Capture is fsync-bound, not throughput-bound: this box measures about 1.55 MB/s
# of journal, which is roughly 0.9 MB/s of source, and it moves by an order of
# magnitude with what else is writing to the disk (docs/performance.md). So the
# settle wait is derived from how much was generated at half the measured rate,
# rather than being a flat number that a larger `--bytes` silently outgrows: a
# capture that is merely slow should not read as a capture that has hung.
CAPTURE_FLOOR_BYTES_PER_SECOND = 450_000
MINIMUM_SETTLE_SECONDS = 300.0


def settle_limit(size_bytes: int) -> float:
    return max(MINIMUM_SETTLE_SECONDS, size_bytes / CAPTURE_FLOOR_BYTES_PER_SECOND)
# A user waiting longer than this for a filter on an unloaded 16-core box is
# looking at a defect, not at their machine.
QUERY_P99_BUDGET = 1.0
# `| <first>-<last>/<total> |` in the status line; the first number is what a
# viewport key has to move.
RANGE = re.compile(r"\| (\d+)-\d+/\d+ \|")
# Resident memory is expected to move with cache and membership; what must not
# happen is that every cycle leaves more behind than the last.
LEAK_TOLERANCE_MIB = 64.0
# Capture's wall clock on this host is not a measurement. The same commit took
# 39 s and 221 s to settle the same 64 MB source six hours apart, because the
# build volume is shared and the page cache is not ours; the spread is 5.6x on
# identical code. So wall time is reported for context and never asserted on,
# and what the run concludes from is normalised by the work: CPU seconds, and
# the number of durable commits capture chose to make.
CAPTURE_WALL_NOTE = "wall time is environment-dependent here; it is not a verdict"
# Floors and ceilings, set below and above what this host measures so that
# ordinary variance cannot trip them, while a change that adds a syscall, an
# allocation or a commit per record puts the ratio through them. The matching
# assertions in `crates/lvu-ingest/tests/throughput.rs` are tighter; this run
# measures reader framing plus writer encoding/commit CPU inside the app.
CAPTURE_MIB_PER_CPU_FLOOR = 28.0
CAPTURE_JOURNAL_MIB_PER_WRITER_CPU_FLOOR = 20.0
CAPTURE_COMMITS_PER_JOURNAL_MIB_CEILING = 6.0
CAPTURE_RECORDS_PER_HANDOVER_FLOOR = 64.0


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


def process_cpu_seconds(pid: int) -> float:
    """Total CPU this process and its threads have used, or 0 where unknown."""
    try:
        fields = pathlib.Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
        return (int(fields[11]) + int(fields[12])) / os.sysconf("SC_CLK_TCK")
    except (OSError, IndexError, ValueError):
        return 0.0


class Metrics:
    def __init__(self) -> None:
        self.samples: list[dict] = []
        self.queries: list[float] = []
        self.query_samples: list[dict] = []
        # CPU seconds the app spent per query, paired with `queries` by index.
        # A slow query that used no CPU is a wait — a poll interval, a round
        # trip, a lock — and a slow query that used a lot is throughput. The
        # first report of this defect guessed the wrong one of those, which
        # sent the investigation looking for a sleep that was not there.
        self.query_cpu: list[float] = []
        self.shutdowns: list[float] = []
        self.slowest_iteration = 0.0
        self.captures: list[dict] = []
        self.notes: list[str] = []

    def sample(
        self,
        instance: int,
        cycle: int,
        pid: int,
        capture: pathlib.Path,
        cache: pathlib.Path,
    ) -> None:
        resident = rss_mib(pid)
        if resident == 0.0:
            # The process is gone; a zero would read as a collapse in the trend
            # rather than as the absence of a reading.
            return
        self.samples.append(
            {
                "instance": instance,
                "cycle": cycle,
                "seconds": round(time.monotonic() - START, 1),
                "rss_mib": round(resident, 1),
                "capture_mib": round(tree_bytes(capture) / 1048576.0, 1),
                "derived_mib": round(tree_bytes(cache) / 1048576.0, 1),
                "load": load_average().split()[0],
            }
        )

    def capture_sources(self, recording: dict) -> None:
        """A source's own account of what capture cost it."""
        if not self.captures:
            return
        sources = recording.get("sources")
        self.captures[-1]["probe_shape_error"] = (
            None if isinstance(sources, list) else "sources must be a list"
        )
        self.captures[-1]["thread_cpu_clock_available"] = recording.get(
            "thread_cpu_clock_available"
        )
        self.captures[-1]["measured_source_id"] = recording.get("measured_source_id")
        self.captures[-1]["sources"] = [
            {
                "source_id": entry.get("source_id"),
                "name": entry.get("name", "?"),
                "journal_bytes": entry.get("journal_bytes"),
                "records": entry.get("records"),
                "commits": entry.get("commits"),
                "handovers": entry.get("handovers"),
                "writer_cpu_s": entry.get("writer_cpu_seconds"),
                "reader_cpu_s": entry.get("reader_cpu_seconds"),
            }
            for entry in (sources if isinstance(sources, list) else [])
            if isinstance(entry, dict)
        ]

    def capture(self, source_bytes: int, wall: float, process_cpu: float) -> None:
        """One app instance's capture of the generated source."""
        self.captures.append(
            {
                "source_bytes": source_bytes,
                # Recorded and reported, but never asserted on: the same commit
                # took 39 s and 221 s for this on the same host six hours apart.
                "wall_s": wall,
                "process_cpu_s": process_cpu,
            }
        )

    def capture_summary(self) -> dict:
        """Capture, normalised by the work rather than by the clock."""
        if not self.captures:
            return {}
        if (not sys.platform.startswith("linux") and
                any(row.get("thread_cpu_clock_available") is not True
                    for row in self.captures)):
            return {
                "instances": len(self.captures),
                "measurement_status": "skipped: thread CPU clock unavailable",
                "measurement_errors": [],
                "wall_seconds_environment_dependent": [
                    round(row["wall_s"], 1) for row in self.captures
                ],
                "per_instance": self.captures,
            }
        errors = []
        selected = []
        for index, row in enumerate(self.captures):
            if row.get("probe_shape_error"):
                errors.append(
                    f"capture instance {index}: {row['probe_shape_error']}"
                )
            if row.get("thread_cpu_clock_available") is not True:
                if sys.platform.startswith("linux"):
                    errors.append(f"capture instance {index}: thread CPU clock unavailable")
                continue
            measured_id = row.get("measured_source_id")
            if not isinstance(measured_id, str) or not measured_id:
                errors.append(f"capture instance {index}: measured source identity missing")
                continue
            matches = [
                source for source in row.get("sources", [])
                if source.get("source_id") == measured_id
            ]
            if len(matches) != 1:
                errors.append(
                    f"capture instance {index}: measured source {measured_id!r} "
                    f"matched {len(matches)} probe entries"
                )
                continue
            selected.append(matches[0])

        fields = {
            "source_bytes": [row.get("source_bytes") for row in self.captures],
            "process_cpu_s": [row.get("process_cpu_s") for row in self.captures],
            "journal_bytes": [entry.get("journal_bytes") for entry in selected],
            "writer_cpu_s": [entry.get("writer_cpu_s") for entry in selected],
            "reader_cpu_s": [entry.get("reader_cpu_s") for entry in selected],
            "commits": [entry.get("commits") for entry in selected],
            "handovers": [entry.get("handovers") for entry in selected],
            "records": [entry.get("records") for entry in selected],
        }
        for name, values in fields.items():
            if len(values) != len(self.captures):
                errors.append(f"capture probe field {name} is incomplete")
                continue
            for index, value in enumerate(values):
                if (isinstance(value, bool) or not isinstance(value, (int, float))
                        or not math.isfinite(value) or value <= 0):
                    errors.append(
                        f"capture instance {index}: {name} must be finite and positive"
                    )

        valid = not errors
        source_mib = sum(fields["source_bytes"]) / 1048576.0 if valid else 0.0
        process_cpu = sum(fields["process_cpu_s"]) if valid else 0.0
        writer_cpu = sum(fields["writer_cpu_s"]) if valid else 0.0
        reader_cpu = sum(fields["reader_cpu_s"]) if valid else 0.0
        journal_mib = sum(fields["journal_bytes"]) / 1048576.0 if valid else 0.0
        commits = sum(fields["commits"]) if valid else 0
        handovers = sum(fields["handovers"]) if valid else 0
        records = sum(fields["records"]) if valid else 0
        return {
            "instances": len(self.captures),
            "measurement_status": "valid" if valid else "invalid",
            "measurement_errors": errors,
            "measured_source_ids": [row.get("measured_source_id") for row in self.captures],
            "source_mib": round(source_mib, 1),
            "mib_per_process_cpu_second": (
                source_mib / process_cpu if process_cpu > 0 else 0
            ),
            "journal_mib_per_writer_cpu_second": (
                journal_mib / writer_cpu if writer_cpu > 0 else 0
            ),
            "mib_per_reader_plus_writer_cpu_second": (
                source_mib / (reader_cpu + writer_cpu)
                if reader_cpu + writer_cpu > 0 else 0
            ),
            "commits_per_journal_mib": (
                commits / journal_mib if journal_mib > 0 else 0
            ),
            "records_per_handover": records / handovers if handovers > 0 else 0,
            # Reported for context and never asserted on; see CAPTURE_WALL_NOTE.
            "wall_seconds_environment_dependent": [round(row["wall_s"], 1) for row in self.captures],
            "per_instance": self.captures,
        }

    def summary(self) -> dict:
        rss = [row["rss_mib"] for row in self.samples]
        instances = sorted({row["instance"] for row in self.samples})
        growth_by_instance = []
        for instance in instances:
            values = [
                row["rss_mib"] for row in self.samples if row["instance"] == instance
            ]
            # Each process gets its own cold sample. Comparing a restarted
            # process's warm cache against its cold start is warm-up, not a
            # retained-across-cycles leak.
            steady = values[1:] or values
            growth_by_instance.append(
                {
                    "instance": instance,
                    "steady_rss_mib": steady,
                    "growth_mib": round(steady[-1] - min(steady), 1) if steady else 0,
                }
            )
        return {
            "capture": self.capture_summary(),
            "samples": self.samples,
            "rss_first": rss[0] if rss else 0,
            "rss_last": rss[-1] if rss else 0,
            "rss_max": max(rss) if rss else 0,
            "rss_growth_by_instance": growth_by_instance,
            "query_count": len(self.queries),
            "query_samples": self.query_samples,
            "query_p50": round(statistics.median(self.queries), 3) if self.queries else 0,
            "query_p99": round(
                sorted(self.queries)[min(len(self.queries) - 1, int(len(self.queries) * 0.99))],
                3,
            )
            if self.queries
            else 0,
            "query_max": round(max(self.queries), 3) if self.queries else 0,
            "query_cpu_total": round(sum(self.query_cpu), 3),
            "query_cpu_per_wall": round(
                sum(self.query_cpu) / sum(self.queries), 2
            )
            if sum(self.queries) > 0
            else 0,
            "shutdown_max": round(max(self.shutdowns), 3) if self.shutdowns else 0,
            "slowest_input_iteration": self.slowest_iteration,
            "notes": self.notes,
        }


def capture_verdict(capture: dict) -> list[str]:
    """Return acceptance failures for one capture summary.

    Invalid measurements stop here instead of letting derived zeroes silently
    pass truthiness-guarded thresholds. Unsupported platforms are explicit
    skips; Linux is required to produce the complete thread-clock recording.
    """
    if capture.get("measurement_status", "").startswith("skipped:"):
        return []
    failures = list(capture.get("measurement_errors") or [])
    if failures:
        return failures
    required = [
        "records_per_handover",
        "mib_per_reader_plus_writer_cpu_second",
        "journal_mib_per_writer_cpu_second",
        "commits_per_journal_mib",
    ]
    missing = [name for name in required if name not in capture]
    if missing:
        return [f"capture summary missing {', '.join(missing)}"]
    handover = capture["records_per_handover"]
    if handover < CAPTURE_RECORDS_PER_HANDOVER_FLOOR:
        failures.append(
            f"capture handed over {handover:.1f} records at a time, below "
            f"{CAPTURE_RECORDS_PER_HANDOVER_FLOOR:.0f}"
        )
    rate = capture["mib_per_reader_plus_writer_cpu_second"]
    if rate < CAPTURE_MIB_PER_CPU_FLOOR:
        failures.append(
            f"capture fell to {rate:.1f} MiB per reader+writer CPU-second, "
            f"below {CAPTURE_MIB_PER_CPU_FLOOR:.1f}"
        )
    writer = capture["journal_mib_per_writer_cpu_second"]
    if writer < CAPTURE_JOURNAL_MIB_PER_WRITER_CPU_FLOOR:
        failures.append(
            f"the writer fell to {writer:.1f} journal MiB per CPU-second, "
            f"below {CAPTURE_JOURNAL_MIB_PER_WRITER_CPU_FLOOR:.1f}"
        )
    commits = capture["commits_per_journal_mib"]
    if commits > CAPTURE_COMMITS_PER_JOURNAL_MIB_CEILING:
        failures.append(
            f"capture committed {commits:.2f} times per journal MiB, "
            f"above {CAPTURE_COMMITS_PER_JOURNAL_MIB_CEILING:.1f}"
        )
    return failures


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


def terminated_lines(path: pathlib.Path) -> int:
    """Count deterministic fixture lines without retaining the fixture."""
    count = 0
    with path.open("rb") as source:
        while chunk := source.read(1024 * 1024):
            count += chunk.count(b"\n")
    return count


def wait_for_settled_capture(
    app: PtyApp, expected_lines: int, limit: float = MINIMUM_SETTLE_SECONDS
) -> float:
    """Block until capture accounts for every terminated fixture line.

    The command source never settles by design, so this watches the record count
    reported for the file source. A quiet-period heuristic is insufficient on a
    shared volume: an I/O pause can leave a partially captured file unchanged
    for seconds. Record count may exceed line count while a partial line is in
    flight, so completion is `>=`, never exact equality.
    """
    # The pane is narrow enough to clip the word "records" off the end.
    counts = re.compile(r"Running: (\d+)")
    deadline = time.monotonic() + limit
    started = time.monotonic()
    while time.monotonic() < deadline:
        app.drain()
        current = file_source_records(app.text(), counts)
        if current >= expected_lines:
            return time.monotonic() - started
        time.sleep(0.25)
    raise AssertionError(
        f"the file source never stopped growing within {limit / 60:.0f} minutes; "
        f"capture is fsync-bound and its rate moves with what else is writing to "
        f"the disk, so this is a capture that is too slow rather than one that is stuck"
    )


def app_records(app: PtyApp) -> int:
    return file_source_records(app.text(), re.compile(r"Running: (\d+)"))


def wait_for_scanned_view(app: PtyApp, limit: float = 900.0) -> None:
    scanned = re.compile(r"scanned (\d+)")
    deadline = time.monotonic() + limit
    while time.monotonic() < deadline:
        app.drain()
        text = app.text()
        captured = file_source_records(text, re.compile(r"Running: (\d+)"))
        found = scanned.search(text)
        if captured and found and int(found.group(1)) >= captured:
            return
        if "raw view" in text and captured:
            # No query is applied, so there is nothing to scan and the rows on
            # screen are already the whole source.
            return
        time.sleep(0.25)
    raise AssertionError(f"the view never scanned the captured records\n{app.text()}")


def apply_search(
    app: PtyApp, literal: str, instance: int, cycle: int, metrics: Metrics
) -> None:
    """Time a filter from keypress to the status line reporting it settled."""
    app.send(b"/")
    # The same budget as the other waits here: on a loaded box the app can take
    # seconds to draw a surface, and a short wait measures the machine.
    app.wait_for("Search", timeout=60.0)
    app.send(b"\x01\x0b")
    app.send(b"\x1b[200~" + literal.encode() + b"\x1b[201~")
    started = time.monotonic()
    cpu_before = process_cpu_seconds(app.process.pid)
    app.send(b"\r")
    app.wait_until(
        lambda text: f'search:"{literal}"' in text
        and ("query ready" in text or "No matches" in text),
        f"filter {literal!r} settled",
        # Filtering is measured at roughly 60,000 records a second — see the
        # TODO row this harness produced — so the budget has to be a function of
        # the source, not a constant. Ten seconds per 100k records, floored.
        timeout=max(120.0, app_records(app) / 10_000.0),
    )
    wall = time.monotonic() - started
    cpu = max(0.0, process_cpu_seconds(app.process.pid) - cpu_before)
    metrics.queries.append(wall)
    metrics.query_cpu.append(cpu)
    metrics.query_samples.append(
        {
            "instance": instance,
            "cycle": cycle,
            "literal": literal,
            "wall_s": wall,
            "process_cpu_s": cpu,
        }
    )
    app.send(b"\x1b")
    app.wait_until(lambda text: "Examples:" not in text, "search closed", timeout=60.0)


def open_and_close(app: PtyApp, key: bytes, marker: str) -> None:
    """Open a surface, close it, and prove the viewport is listening again.

    Silence is the failure mode: a surface that stays open holds focus and the
    keys meant for the viewport are dropped without a word. So this insists the
    surface appeared, insists it went away, and then insists the viewport
    responds, rather than sending the next key and hoping.
    """
    app.send(key)
    app.wait_for(marker, timeout=60.0)
    app.send(b"\x1b")
    app.wait_until(lambda text: marker not in text, f"{marker} closed", timeout=60.0)
    assert_viewport_has_focus(app)


def assert_viewport_has_focus(app: PtyApp) -> None:
    """The viewport is listening iff a viewport key moves the viewport.

    There is no focus indicator to read and the status line says the same thing
    either way, so the observable has to be behavioural: `g` goes to the top and
    `G` to the end, and the range in the status line follows.
    """
    app.send(b"g")
    app.wait_until(
        lambda text: (found := RANGE.search(text)) is not None and found.group(1) == "1",
        "viewport responds to Top",
        timeout=30.0,
    )
    app.send(b"G")
    app.wait_until(
        lambda text: (found := RANGE.search(text)) is not None and found.group(1) != "1",
        "viewport responds to End",
        timeout=30.0,
    )


def exercise(app: PtyApp, instance: int, cycle: int, metrics: Metrics) -> None:
    """One pass of the things a person does to a log all afternoon."""
    for literal in ("SOAK_MARKER", "ERROR", "req-00042", "region", "malformed"):
        apply_search(app, literal, instance, cycle, metrics)
    # Clear back to everything so the next cycle starts from a full view.
    apply_search(app, "seq", instance, cycle, metrics)
    for key in (b"g", b"G", b"j", b"k"):
        app.send(key)
        time.sleep(0.05)
        app.drain()
    # Escape closes one layer and, at the top, closes the app: every surface is
    # opened and then closed against a marker rather than by pressing Escape a
    # few times and hoping.
    # A surface is identified by the title in its frame and nothing else
    # (dialog-system.md 8.10). Two ways to get that wrong have cost a run each.
    #
    # A marker that never appears made the old `open_and_close` take its
    # "nothing opened" path and leave the surface up: with a layer open the
    # viewport does not have focus, `/` silently does nothing, and the cycle
    # after it failed somewhere else entirely.
    #
    # And a marker that includes what the surface is *showing* is not the
    # surface's identity. `Fields . record 12345` is the title only while a
    # record is anchored; over a view with none it is just `Fields`, and a run
    # waited sixty seconds for a suffix that was never coming while the dialog
    # sat open in front of it. So these match the framed title — the corner
    # glyph pins it to a frame rather than to the same words in a pane, a
    # footer, Help or the palette — and stop before anything the content
    # decides.
    open_and_close(app, b"d", "┌ Selected event details")
    open_and_close(app, b"i", "┌ Fields")
    app.send(b"b")  # bookmark the selected row; no surface opens
    time.sleep(0.1)
    open_and_close(app, b"B", "┌ Bookmarks · ")


def read_probe_lines(app: PtyApp, metrics: Metrics) -> None:
    tail = bytes(app.transcript[-16000:]).decode("utf-8", "replace")
    loop = re.findall(r"lvu input loop: \d+ iterations, slowest ([0-9.]+)s", tail)
    if loop:
        metrics.slowest_iteration = max(metrics.slowest_iteration, float(loop[-1]))
    shutdown = re.findall(r"lvu shutdown ([0-9.]+)s total", tail)
    if shutdown:
        metrics.shutdowns.append(float(shutdown[-1]))


def read_capture_cost(path: pathlib.Path, metrics: Metrics) -> None:
    """What the app recorded about its own capture, from disk.

    Read from a file rather than scraped out of the terminal transcript: a
    number a harness depends on should not be one escape sequence away from
    going missing.
    """
    try:
        recorded = json.loads(path.read_text())
    except (OSError, ValueError):
        return
    if isinstance(recorded, dict):
        metrics.capture_sources(recorded)


def run(binary: pathlib.Path, root: pathlib.Path, mode: dict, metrics: Metrics) -> None:
    source = root / "soak.log"
    if not source.exists():
        subprocess.run(
            [sys.executable, str(pathlib.Path(__file__).with_name("generate.py")),
             str(source), "--bytes", str(mode["bytes"])],
            check=True,
        )
    expected_lines = terminated_lines(source)
    capture = root / "capture"
    capture_cost = root / "capture-cost.json"
    capture_cost.unlink(missing_ok=True)
    environment = {
        **isolated_environment(root),
        "LVU_SHUTDOWN_TIMING": "1",
        "LVU_CAPTURE_COST_PATH": str(capture_cost),
        "LVU_CAPTURE_COST_SOURCE_PATH": str(source.resolve()),
    }
    cache = pathlib.Path(environment["XDG_CACHE_HOME"]) / "lvu"
    chatty = (
        "i=0; while :; do "
        "printf '%s\\n' \"{\\\"level\\\":\\\"INFO\\\",\\\"service\\\":\\\"chatty\\\",\\\"seq\\\":$i}\"; "
        "i=$((i+1)); sleep 0.05; done"
    )

    cycles_per_restart = mode["cycles"]
    cycle = 0
    for restart in range(mode["restarts"] + 1):
        capture_started = time.monotonic()
        app = PtyApp(
            binary,
            ["--capture-dir", str(capture), "--file", str(source), "--command", chatty],
            width=160,
            height=44,
            environment=environment,
        )
        try:
            app.wait_until(
                lambda text: (
                    "seq" in text
                    or "handled request" in text
                    or "query ready:" in text
                    or "raw view" in text
                ),
                "first rows after start",
                # Capture of the generated source is measured at roughly
                # 10 MB/s, so even short mode reads for minutes, and this runs
                # on a machine other work shares.
                timeout=900.0,
            )
            # Query latency is about queries. Measuring while the file is still
            # being read measures ingest, and the two have different budgets, so
            # wait for the file source to stop growing before timing anything.
            wait_for_settled_capture(
                app, expected_lines, limit=settle_limit(mode["bytes"])
            )
            # A restarted app restores its view before the query worker has
            # rescanned the journal, and a filter applied in that window settles
            # instantly against nothing: "matched 0 / scanned 0". Wait for the
            # scan to reach the records that are already captured.
            wait_for_scanned_view(app)
            if restart == 0:
                # Restarts reopen an already durable journal; counting the
                # input again against their near-zero capture work would make
                # the CPU-normalised rate meaningless.
                metrics.capture(
                    source.stat().st_size,
                    time.monotonic() - capture_started,
                    process_cpu_seconds(app.process.pid),
                )
            metrics.sample(restart, cycle, app.process.pid, capture, cache)
            for _ in range(cycles_per_restart):
                cycle += 1
                exercise(app, restart, cycle, metrics)
                metrics.sample(restart, cycle, app.process.pid, capture, cache)
                print(
                    f"  restart {restart} cycle {cycle}: "
                    f"{json.dumps(metrics.samples[-1])}",
                    flush=True,
                )
            started = time.monotonic()
            app.send(b"q")
            code = app.wait_exit(timeout=120.0)
            if code != 0:
                visible = " | ".join(
                    line.strip() for line in app.text().splitlines() if line.strip()
                )[-1000:]
                metrics.notes.append(
                    f"restart {restart}: exit code {code}; final screen: {visible}"
                )
            metrics.shutdowns.append(time.monotonic() - started)
            read_probe_lines(app, metrics)
            if restart == 0:
                read_capture_cost(capture_cost, metrics)
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
    parser.add_argument("--bytes", type=int, default=None,
                        help="override the mode's generated source size")
    parser.add_argument("--cycles", type=int, default=None,
                        help="override the mode's cycles per app instance")
    parser.add_argument("--restarts", type=int, default=None,
                        help="override the mode's restart count")
    parser.add_argument("--root", type=pathlib.Path, default=None)
    parser.add_argument("--report", type=pathlib.Path, default=None)
    parser.add_argument(
        "--keep-inputs",
        action="store_true",
        help="leave the generated source and captures behind for inspection",
    )
    arguments = parser.parse_args()

    mode = dict(MODES[arguments.mode])
    if arguments.cycles is not None:
        mode["cycles"] = arguments.cycles
    if arguments.restarts is not None:
        mode["restarts"] = arguments.restarts
    if arguments.bytes is not None:
        mode["bytes"] = arguments.bytes
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
    limit = settle_limit(mode["bytes"])
    print(f"soak {arguments.mode}: root={root} load={load_average()}", flush=True)
    print(
        f"  generating {mode['bytes'] / 1048576:.0f} MiB; capture should settle "
        f"within {limit / 60:.0f} minutes at {CAPTURE_FLOOR_BYTES_PER_SECOND / 1e6:.2f} MB/s",
        flush=True,
    )
    print("  (query timings start once the file source has settled)", flush=True)
    # A cycle that fails part way through has still measured everything up to
    # that point, and those samples are the reason to run this at all. Losing
    # them to the traceback meant a run that stopped in cycle two reported no
    # memory trend and no query latency, so the failure hid the measurements
    # instead of adding to them. The failure is still a failure below.
    try:
        run(arguments.binary.resolve(), root, mode, metrics)
    except (AssertionError, OSError) as failure:
        metrics.notes.append(f"run stopped early: {failure}")

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
    # Warm-up is not growth: the first sample is an empty cache and no
    # membership. What a leak looks like is the steady state climbing, so the
    # comparison starts after the first cycle.
    for instance in summary["rss_growth_by_instance"]:
        growth = instance["growth_mib"]
        if growth > LEAK_TOLERANCE_MIB:
            failures.append(
                f"resident memory grew {growth:.1f} MiB after warm-up in app "
                f"instance {instance['instance']} "
                f"(steady-state series {instance['steady_rss_mib']})"
            )
    if summary["query_p99"] > QUERY_P99_BUDGET:
        failures.append(
            f"query p99 {summary['query_p99']:.3f}s exceeds {QUERY_P99_BUDGET:.1f}s"
        )
    capture = summary.get("capture") or {}
    if capture:
        print(f"  capture: {CAPTURE_WALL_NOTE}", flush=True)
        failures.extend(capture_verdict(capture))
    elif metrics.samples:
        # The recording is the only way this run learns what capture cost, so
        # its absence is a failure rather than a silently missing number.
        failures.append(
            "no capture-cost recording was found; is LVU_SHUTDOWN_TIMING set "
            "and the binary current?"
        )
    failures.extend(summary["notes"])
    for failure in failures:
        print(f"SOAK FAILURE: {failure}", flush=True)
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
