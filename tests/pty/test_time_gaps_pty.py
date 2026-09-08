#!/usr/bin/env python3
"""Dataset-relative time ranges and gap navigation, through the real binary.

The engine and the dialog are covered by unit tests at their own layers; what
this covers is the part a user touches — that the window choices name the clock
and the data apart, that the gap threshold is stated rather than assumed, and
that `{`/`}` move the selection to a quiet period, say which one they found and
change nothing about the view while they do it.

The log is written in three bursts with real pauses, so the quiet periods are
in the capture stream itself rather than simulated.
"""

from __future__ import annotations

import pathlib
import sys
import tempfile
import time

from test_lvu_pty import PtyApp

# Two pauses, each comfortably longer than the one-second threshold the story
# selects and shorter than anything that would make the suite slow.
PAUSE_SECONDS = 1.6
BURSTS = [["a1", "a2", "a3"], ["b1", "b2"], ["c1"]]


def launch(binary: pathlib.Path, root: pathlib.Path) -> PtyApp:
    return PtyApp(
        binary,
        ["--capture-dir", str(root / "capture"), "--file", str(root / "events.log")],
        width=110,
        height=32,
        cwd=root,
        environment={
            "LVU_NO_DELIGHT": "1",
            "XDG_CONFIG_HOME": str(root / "config"),
            "XDG_DATA_HOME": str(root / "data"),
            "XDG_CACHE_HOME": str(root / "cache"),
        },
    )


def open_time(app: PtyApp) -> str:
    app.send(b"t")
    return app.wait_for("Time basis", timeout=8)


def close_time(app: PtyApp) -> None:
    app.send(b"\x1b")
    app.wait_until(lambda text: "Time basis" not in text, "time dialog closed")


def run(binary: pathlib.Path) -> None:
    root = pathlib.Path(tempfile.mkdtemp(prefix="lvu-time-gaps-"))
    log = root / "events.log"
    log.write_text("".join(f"burst {name} record\n" for name in BURSTS[0]))
    app = launch(binary, root)
    try:
        app.wait_for("burst a3 record", timeout=10)
        for burst in BURSTS[1:]:
            time.sleep(PAUSE_SECONDS)
            with log.open("a") as stream:
                stream.write("".join(f"burst {name} record\n" for name in burst))
                stream.flush()
        app.wait_for("burst c1 record", timeout=10)

        # The dialog states the gap threshold instead of leaving it to be
        # discovered, and names the clock-relative and data-relative windows
        # apart so "last 5 minutes" cannot mean two things.
        opened = open_time(app)
        assert "Gap jump" in opened, opened
        assert "Quiet" in opened and "1m" in opened, opened
        app.send(b"\t\r")
        choices = app.wait_until(
            lambda text: "Last 5m by clock" in text,
            "the window choices",
            timeout=5,
        )
        assert "Last 5m of data" in choices, choices
        assert "First" in choices and "last event" in choices, choices

        # A data-relative window fills the fields, so what it means is visible
        # before it is applied rather than hidden behind a label.
        for _ in range(5):
            app.send(b"\x1b[B")
        app.send(b"\r")
        filled = app.wait_until(
            lambda text: "last event" in text and "Window" in text,
            "the data range selected",
            timeout=5,
        )
        assert "Start 20" in filled, filled
        assert "End   20" in filled, filled

        # Lower the threshold to a second: the number in the dialog is what the
        # key acts on.
        app.send(b"\t\r")
        app.wait_until(lambda text: "1s" in text, "threshold choices", timeout=5)
        # The list is 1s, 10s, 30s, 1m, 5m, 15m, 1h and opens on the current
        # value; three steps up reaches the shortest.
        for _ in range(3):
            app.send(b"\x1b[A")
        app.send(b"\r")
        chosen = app.wait_until(
            lambda text: "Gap jump" in text and "1s" in text,
            "the chosen threshold",
            timeout=5,
        )
        assert "Gap jump" in chosen, chosen
        close_time(app)

        # Gap navigation. `}` lands on the record that resumes after the quiet
        # period and the status line says which gap that was.
        app.send(b"g")
        app.send(b"}")
        first = app.wait_until(
            lambda text: "gap 1s" in text and "quiet from" in text,
            "the status line naming the first gap",
            timeout=8,
        )
        assert "HISTORY" in first, "a jump leaves follow mode"

        # Again advances rather than standing still.
        app.send(b"}")
        app.wait_until(
            lambda text: "gap 1s" in text and "quiet from" in text,
            "the second quiet period",
            timeout=8,
        )
        # There is no third, and saying so is not the same as moving silently.
        app.send(b"}")
        app.wait_until(
            lambda text: "no gap longer than 1s after here" in text,
            "an honest answer when there is nothing to find",
            timeout=8,
        )

        # `{` retraces, and none of it changed the view's definition: no filter
        # was applied and every record is still counted.
        app.send(b"{")
        back = app.wait_until(
            lambda text: "gap 1s" in text, "the previous gap", timeout=8
        )
        assert "search:" not in back, back
        assert "/6" in back, back

        # The threshold survives closing and reopening the dialog, and the
        # dialog states the two things a timestamp's meaning depends on that it
        # does not own.
        reopened = open_time(app)
        assert "Gap jump" in reopened and "1s" in reopened, reopened
        assert "Shown in UTC" in reopened, reopened
        assert "order: capture (arrival)" in reopened, reopened
        close_time(app)

        # The status line names the zone without opening anything. The order
        # is not on it: it is the same for every view and not settable, and
        # this line is already competing for characters.
        assert "tz:UTC" in app.text(), app.text()
        assert "order:capture" not in app.text(), app.text()

        # Every row carries its offset and the status line names the zone and
        # the order, so neither `14:30` nor the row order is an inference. The
        # zone itself is a Settings field; its preview, rollback and save are
        # asserted in the component tests, which can drive them precisely.
        final = app.text()
        assert "HISTORY" in final, final
        assert any(
            "burst a1 record" in line and "Z " in line for line in final.splitlines()
        ), final
    finally:
        if app.process.poll() is None:
            app.send(b"q")
            code = app.wait_exit(timeout=8)
            assert code == 0, (code, app.text())
        app.assert_restored()


def main() -> None:
    if len(sys.argv) != 2:
        raise SystemExit("usage: test_time_gaps_pty.py <lvu-app>")
    run(pathlib.Path(sys.argv[1]).resolve())
    print("time gaps pty ok")


if __name__ == "__main__":
    main()
