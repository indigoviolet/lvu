#!/usr/bin/env python3
"""Declaring an arbitrary timestamp field as the event-time basis.

The engine (`lvu_live::time` recognition, `lvu_query::time_field` validation)
is exercised by unit tests; what this covers is the part a user touches: that
recognized fields are offered, that a reading resting on a guess is shown with
its assumption and its validated coverage and is *not* applied until accepted,
that accepting it actually changes which records the window matches, and that
the declaration survives a restart.
"""

from __future__ import annotations

import pathlib
import re
import sys
import tempfile
import time

from test_lvu_pty import PtyApp


# Naive local date-times: readable only under a declared timezone assumption,
# and far from the capture time, so the two bases cannot be confused.
RECORDS = "".join(
    '{"msg":"event-%02d","ts":"2026-03-04T05:06:%02d","epoch_ms":%d}\n'
    % (index, index, 1_772_600_000_000 + index * 1000)
    for index in range(20)
)


def launch(binary: pathlib.Path, root: pathlib.Path) -> PtyApp:
    return PtyApp(
        binary,
        ["--capture-dir", str(root / "capture"), "--file", str(root / "events.log")],
        width=100,
        height=30,
        cwd=root,
        environment={
            "LVU_NO_DELIGHT": "1",
            "XDG_CONFIG_HOME": str(root / "config"),
            "XDG_DATA_HOME": str(root / "data"),
            "XDG_CACHE_HOME": str(root / "cache"),
        },
    )


def open_basis_menu(app: PtyApp) -> str:
    app.send(b"t")
    app.wait_for("Time basis", timeout=8)
    app.send(b"\r")
    return app.wait_until(
        lambda text: "epoch milliseconds" in text,
        "the recognized candidates to be offered",
        timeout=8,
    )


def run(binary: pathlib.Path) -> None:
    root = pathlib.Path(tempfile.mkdtemp(prefix="lvu-time-field-"))
    (root / "events.log").write_text(RECORDS)

    app = launch(binary, root)
    try:
        app.wait_for("event-19", timeout=10)

        # The window seeded at open covers the capture time, so under the
        # default basis every record matches. This is the control.
        app.send(b"t")
        app.wait_for("Time basis", timeout=8)
        # Readiness: the seeded bounds must be painted and settled before the
        # dialog is driven. Record them now: choosing Absolute clears the
        # bound/zone drafts (current behavior seeds nothing for it), so the
        # seeded capture-time bounds are typed back explicitly below.
        ready = app.wait_until(
            lambda text: "Applied" in text and "Updating" not in text,
            "seeded dialog ready",
            timeout=8,
        )
        start_match = re.search(r"Start\s+(\S+)\s+(\S+)", ready)
        end_match = re.search(r"End\s+(\S+)\s+(\S+)", ready)
        assert start_match and end_match, f"seeded bounds readable:\n{ready}"
        start_bound = f"{start_match.group(1)}T{start_match.group(2).split('.')[0]}Z"
        end_bound = f"{end_match.group(1)}T{end_match.group(2).split('.')[0]}Z"
        app.send(b"\t\r")  # Window dropdown.
        app.wait_until(lambda text: "Absolute" in text, "window choices", timeout=5)
        # Verified navigation: Enter must commit Absolute rather than
        # re-commit All time, so the highlight has to arrive first. Never
        # commit blind: without the highlight Enter would keep All time and
        # every later step would fail far from the cause.
        highlight_deadline = time.time() + 4.0
        while "> Absolute" not in app.text():
            if time.time() > highlight_deadline:
                break
            app.send(b"\x1b[B")
            time.sleep(0.2)
        assert "> Absolute" in app.text(), (
            "window highlight never reached Absolute; not committing:\n" + app.text()
        )
        app.send(b"\r")
        app.wait_until(
            lambda text: any(
                "Window" in line and "Absolute" in line and "▾" in line
                for line in text.splitlines()
            )
            and "Last 5m by clock" not in text,
            "absolute window",
            timeout=5,
        )
        app.send(b"\x1b")
        app.wait_until(lambda text: "Time basis" not in text, "time dialog closed")

        offered = open_basis_menu(app)
        # Ranked recognized candidates, each stating what accepting it costs.
        assert "epoch_ms · epoch milliseconds" in offered, offered
        assert "ts · " in offered, offered
        assert "needs an assumption" in offered, offered

        # Walk to the zone-less `ts` candidate and choose it. Verify the
        # landing instead of counting steps: the candidate count varies, so a
        # blind count can overshoot (or wrap) and commit the wrong row, and a
        # dead highlight must fail here rather than cascade.
        walk_deadline = time.time() + 4.0
        for _ in range(8):
            if "> ts" in app.text():
                break
            if time.time() > walk_deadline:
                break
            app.send(b"\x1b[B")
            time.sleep(0.2)
        assert "> ts" in app.text(), (
            "candidate highlight never reached ts; not committing:\n" + app.text()
        )
        app.send(b"\r")
        pending = app.wait_until(
            lambda text: "Accept assumption" in text,
            "the confirmation step for a reading that rests on a guess",
            timeout=5,
        )
        assert "Field: ts" in pending, pending
        # The assumption itself, from TimeFieldSelection::assumptions().
        assert "Assumes: value has no timezone; assumed UTC" in pending, pending
        # Validated coverage, before acceptance.
        assert "Coverage: 100% of 20 sampled records" in pending, pending
        # Nothing is applied yet: the basis field still names the old basis.
        assert "Time basis   Capture" in pending, pending
        assert "not applied until you accept" in pending, pending

        app.send(b"\r")  # Accept assumption.
        accepted = app.wait_until(
            lambda text: "Time basis   Field ts" in text,
            "the accepted field to become the basis",
            timeout=5,
        )
        assert "Accept assumption" not in accepted, accepted

        # Choosing Absolute left empty bound/zone drafts, which Apply rejects,
        # so type the seeded capture-time bounds back explicitly. A whole-bound
        # paste replaces the focused Start or End row outright, zones included
        # (Z is a valid UTC zone); after Accept the focus is on Basis, which is
        # not an End row, so the first paste lands on Start.
        def paste(text: str) -> None:
            app.send(("\x1b[200~" + text + "\x1b[201~").encode())

        def dialog_line(text: str, label: str) -> str:
            for line in text.splitlines():
                if re.search(rf"\b{label}\b", line):
                    return line
            return ""

        start_clock = start_bound.split("T")[1].rstrip("Z")
        end_clock = end_bound.split("T")[1].rstrip("Z")
        paste(start_bound)
        app.wait_until(
            lambda text: start_clock in dialog_line(text, "Start"),
            "start bound typed",
            timeout=5,
        )
        # Walk Tab stops until the caret reaches the End row, then paste once:
        # pasting earlier would rewrite Start and reset focus onto it.
        end_rows = [
            index
            for index, row in enumerate(app.screen.display)
            if re.search(r"\bEnd\b", row)
        ]
        assert end_rows, f"End row visible:\n{app.text()}"
        end_row = end_rows[0]
        for _ in range(8):
            app.drain()
            if app.screen.cursor.y == end_row:
                break
            app.send(b"\t")
            time.sleep(0.2)
        paste(end_bound)
        app.wait_until(
            lambda text: end_clock in dialog_line(text, "End"),
            "end bound typed",
            timeout=5,
        )

        # Applying it must actually change which records the window matches:
        # the same absolute window that held every record under capture time
        # holds none under a March event time. Enter on the End segment runs
        # the default Apply; the larger dialog can hide the base rows, so wait
        # for the applied status, close, and assert on the base screen.
        app.send(b"\r")
        applied = app.wait_until(
            lambda text: "Time basis" not in text
            or (
                "Applied" in text
                and "all times" not in text
                and "Error" not in text
            ),
            "the typed window applies",
            timeout=8,
        )
        if "Time basis" in applied:
            app.send(b"\x1b")
            app.wait_until(lambda text: "Time basis" not in text, "time dialog closed")
        app.wait_until(
            lambda text: "event-19" not in text,
            "the chosen field to drive the window",
            timeout=8,
        )

        # The declaration survives a restart, token and all.
        app.send(b"q")
        assert app.wait_exit(timeout=8) == 0
    finally:
        if app.process.poll() is None:
            app.process.kill()
            app.process.wait(timeout=5)
        app.close()

    restarted = launch(binary, root)
    try:
        restarted.wait_for("events.log", timeout=10)
        # Restoration lands asynchronously, so ask the dialog again until it
        # answers rather than racing the first frame after startup.
        restored = ""
        for _ in range(20):
            restarted.send(b"t")
            restored = restarted.wait_until(
                lambda text: "Time basis" in text,
                "the time dialog after restart",
                timeout=8,
            )
            if "Field ts" in restored:
                break
            restarted.send(b"\x1b")
            restarted.wait_until(
                lambda text: "Time basis" not in text, "time dialog closed", timeout=5
            )
            time.sleep(0.5)
        assert "Time basis   Field ts" in restored, restored
    finally:
        if restarted.process.poll() is None:
            restarted.process.kill()
            restarted.process.wait(timeout=5)
        restarted.close()


if __name__ == "__main__":
    run(pathlib.Path(sys.argv[1]).resolve())
    print("Time field PTY passed: recognized candidates, held assumption, applied basis, restart")
