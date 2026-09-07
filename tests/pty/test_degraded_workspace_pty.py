#!/usr/bin/env python3
"""A workspace lvu cannot read degrades the session; it does not fail it.

A workspace database written by a newer build stays on disk when that build is
reverted, and this one refuses it. The product's answer is to say so and keep
raw browsing working, which it does — but the dead memory worker then turned a
deliberate `q` into a nonzero exit, so every suite sharing that workspace failed
its shutdown assertion with no visible cause. Degrading and then failing the
exit status is the contradiction under test.
"""

from __future__ import annotations

import pathlib
import sqlite3
import sys
import tempfile

from test_lvu_pty import PtyApp, isolated_environment

# Far enough ahead that no future migration makes this readable by accident.
UNREADABLE_SCHEMA_VERSION = 9_999


def unreadable_workspace(capture: pathlib.Path) -> None:
    workspace = capture / "workspace"
    workspace.mkdir(parents=True, exist_ok=True)
    connection = sqlite3.connect(workspace / "workspace.sqlite3")
    try:
        connection.execute(f"pragma user_version = {UNREADABLE_SCHEMA_VERSION}")
        connection.commit()
    finally:
        connection.close()


def run(binary: pathlib.Path) -> None:
    with tempfile.TemporaryDirectory(prefix="lvu-degraded-workspace-") as directory:
        root = pathlib.Path(directory)
        capture = root / "capture"
        unreadable_workspace(capture)
        source = root / "events.log"
        source.write_text("first degraded event\nsecond degraded event\n")
        app = PtyApp(
            binary,
            ["--file", str(source), "--capture-dir", str(capture)],
            width=120,
            height=30,
            environment=isolated_environment(root),
        )
        try:
            # Raw browsing still works, and the reason is on screen rather than
            # left to the exit status to communicate.
            app.wait_for("second degraded event", timeout=20.0)
            # The status line clips the sentence at the pane edge, so assert the
            # part that is actually legible rather than the full notice.
            app.wait_until(
                lambda text: "memory error: memory unavailable" in text,
                "the unusable workspace is explained on screen",
                timeout=10.0,
            )
            app.send(b"q")
            assert app.wait_exit(timeout=15.0) == 0, (
                "a deliberate quit from an already-degraded session must still "
                "report success"
            )
            app.assert_restored()
        finally:
            if app.process.poll() is None:
                app.process.kill()
                app.process.wait()
            app.close()
    print(
        "Degraded workspace PTY passed: unreadable workspace explained, raw rows "
        "served, clean exit status and terminal restoration"
    )


if __name__ == "__main__":
    run(pathlib.Path(sys.argv[1]).resolve())
