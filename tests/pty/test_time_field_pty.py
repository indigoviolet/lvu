#!/usr/bin/env python3
"""Declaring an arbitrary timestamp field as the event-time basis.

The engine (`lvu_live::time` recognition, `lvu_query::time_field` validation)
is exercised by unit tests; what this covers is the part a user touches: that
recognized fields are offered, that a reading resting on a guess is shown with
its assumption and its validated coverage and is *not* applied until accepted,
and that the accepted declaration survives a restart. Other Time PTYs own
absolute and rolling membership behavior.
"""

from __future__ import annotations

import pathlib
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


def click_text(app: PtyApp, text: str) -> None:
    app.drain()
    for y, row in enumerate(app.screen.display):
        if text in row:
            x = row.index(text)
            app.send(f"\x1b[<0;{x + 2};{y + 1}M".encode())
            app.send(f"\x1b[<0;{x + 2};{y + 1}m".encode())
            return
    raise AssertionError(f"missing clickable text {text!r}\n{app.text()}")


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

        offered = open_basis_menu(app)
        # Ranked recognized candidates, each stating what accepting it costs.
        assert "epoch_ms · epoch milliseconds" in offered, offered
        assert "ts · " in offered, offered
        assert "needs an assumption" in offered, offered

        # Choose the visible zone-less `ts` candidate directly. Mouse and row
        # geometry are part of this dialog's PTY contract; this avoids making
        # the recognition story depend on terminal escape packet timing.
        click_text(app, "ts ·")
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

        # Apply the accepted basis with the current all-time window. Other Time
        # PTYs exercise absolute/rolling membership; this story owns recognized
        # field review and persistence. The responsive dialog covers base rows,
        # so synchronize on its applied state, then close it before quitting.
        click_text(app, "[ Apply ]")
        # The pre-click frame already says Applied for the old all-time state,
        # so it cannot be the handshake. Let the click cross the event loop,
        # drain its resulting frame, and then close only if the dialog remains.
        time.sleep(0.5)
        app.drain()
        applied = app.text()
        assert "Error" not in applied, applied
        if "Time basis" in applied:
            assert "Time basis   Field ts" in applied, applied
            app.send(b"\x1b")
            app.wait_until(lambda text: "Time basis" not in text, "time dialog closed")

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
