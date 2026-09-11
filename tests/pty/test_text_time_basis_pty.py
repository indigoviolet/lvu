#!/usr/bin/env python3
"""Declaring a *text* enrichment column as the event-time basis.

The engine picks the shapes (`lvu_live::time::infer_text_time_formats`) and the
query layer measures them (`lvu_query::time_field`); both are covered by unit
tests. What this covers is the part a user touches: that a text column is
offered at all, that the format inferred for it is shown with its match rate
and a sample, and that accepting it remains an explicit reviewed action. Unit
tests own correction and remeasurement semantics.
"""

from __future__ import annotations

import pathlib
import sys
import tempfile
import time

from test_lvu_pty import PtyApp


# Naive local date-times in a text field. Copied into an enrichment column,
# they are the case the TODO row named: text, so an epoch unit cannot describe
# them, and a chrono format is needed to read them.
RECORDS = "".join(
    '{"msg":"event-%02d","ts":"2026-03-04T05:06:%02d"}\n' % (index, index)
    for index in range(12)
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


def click_text(app: PtyApp, text: str) -> None:
    app.drain()
    for y, row in enumerate(app.screen.display):
        if text in row:
            x = row.index(text)
            app.send(f"\x1b[<0;{x + 2};{y + 1}M".encode())
            app.send(f"\x1b[<0;{x + 2};{y + 1}m".encode())
            return
    raise AssertionError(f"missing clickable text {text!r}\n{app.text()}")


def run(binary: pathlib.Path) -> None:
    root = pathlib.Path(tempfile.mkdtemp(prefix="lvu-text-basis-"))
    (root / "events.log").write_text(RECORDS)
    app = launch(binary, root)
    try:
        app.wait_for("event-11", timeout=20)

        # An enrichment step that carries the text field into a column.
        app.send(b"e")
        app.wait_for("[ Add ]", timeout=8)
        app.send(b"\r")
        app.wait_for("Expression", timeout=8)
        app.send(b"\x1b[200~started_at = pl.col(\"ts\")\x1b[201~")
        app.wait_until(lambda text: "started_at" in text, "the step draft", timeout=8)
        app.send(b"\x1b[200~\x1b[201~")  # settle the paste before saving
        app.wait_for("[ Save ]", timeout=8)
        app.send(b"\r")
        app.wait_until(
            lambda text: "New step" not in text, "the step editor closes", timeout=10
        )
        app.send(b"\x1b")
        app.wait_until(lambda text: "Steps" not in text, "enrichment closes", timeout=8)

        # The column is now offered as a basis, read as text.
        app.send(b"t")
        app.wait_for("Time basis", timeout=10)
        app.send(b"\r")
        app.wait_until(
            lambda text: "column: started_at" in text,
            "the text column is offered as a basis",
            timeout=20,
        )
        offered = app.text()
        assert "text ·" in offered, offered

        # Choose it. The confirmation step names the format, the sample it came
        # from and the share it read.
        click_text(app, "column: started_at")
        format_frame = app.wait_until(
            lambda text: "Format" in text and "Accept assumption" in text,
            "the confirmation step shows the inferred format",
            timeout=10,
        )
        # Coverage is part of the same confirmation but can sit below the
        # responsive body fold. Walk focus until the shared viewport reveals
        # it rather than requiring every confirmation row simultaneously.
        for _ in range(12):
            app.drain()
            if "Coverage" in app.text():
                break
            app.send(b"\t")
            time.sleep(0.05)
        confirmation = app.wait_for("Coverage", timeout=10)
        assert "%Y-%m-%dT%H:%M:%S" in format_frame, format_frame
        assert "2026-03-04T05:06:00" in format_frame, format_frame
        assert "Accept assumption" in format_frame, format_frame
        assert "Coverage" in confirmation, confirmation

        app.send(b"\x1b")
        app.wait_until(lambda text: "Time basis" not in text, "time closes", timeout=8)
        app.send(b"q")
        assert app.wait_exit(timeout=15) == 0
        app.assert_restored()
    finally:
        if app.process.poll() is None:
            app.process.kill()
            app.process.wait()
        app.close()


if __name__ == "__main__":
    run(pathlib.Path(sys.argv[1]).resolve())
    print("Text time-basis PTY passed: inferred format, coverage, sample, and explicit review")
