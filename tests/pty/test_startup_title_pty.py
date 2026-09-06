#!/usr/bin/env python3
"""Persistent title is modal only when no source was supplied at launch."""

from __future__ import annotations

import pathlib
import sys
import tempfile

from test_lvu_pty import PtyApp


def close(app: PtyApp) -> None:
    if app.process.poll() is None:
        app.process.kill()
        app.process.wait(timeout=5)
    app.close()


def run(binary: pathlib.Path) -> None:
    with tempfile.TemporaryDirectory(prefix="lvu-title-pty-") as directory:
        root = pathlib.Path(directory)
        environment = {
            "NO_COLOR": "",
            "COLORTERM": "truecolor",
            "XDG_CONFIG_HOME": str(root / "config"),
            "XDG_CACHE_HOME": str(root / "cache"),
            "XDG_DATA_HOME": str(root / "data"),
            "LVU_REDUCED_MOTION": "1",
        }
        app = PtyApp(binary, [], width=124, height=42, cwd=root, environment=environment)
        try:
            app.wait_for("LOVE YOU LOG TIME")
            app.assert_remains("PRESS ANY KEY", "Add source", duration=1.0)
            app.send(b"\x1b[200~hidden-paste\x1b[201~")
            app.assert_remains("PRESS ANY KEY", "hidden-paste", duration=0.2)
            app.send(b"x")
            app.wait_until(lambda text: "PRESS ANY KEY" not in text, "Any key enters the app")
            assert "hidden-paste" not in app.text()
            assert "x" not in app.text().split("Kind:", 1)[-1].split("Tab", 1)[0], "dismissal key leaked into source draft"
            app.send(b"\x03")
            assert app.wait_exit(timeout=8) == 0
            app.assert_restored()
        finally:
            close(app)

        empty = root / "empty.log"
        empty.touch()
        for args in [[str(empty)], ["-c", "sleep 30"]]:
            app = PtyApp(binary, [*args, "--capture-dir", str(root / "captures")],
                         cwd=root, environment=environment)
            try:
                app.wait_for("Ctrl-P", timeout=8)
                assert b"PRESS ANY KEY" not in app.transcript, "CLI source showed startup title"
                app.send(b"\x03")
                assert app.wait_exit(timeout=8) == 0
                app.assert_restored()
            finally:
                close(app)

        app = PtyApp(binary, [], cwd=root, environment=environment)
        try:
            app.wait_for("PRESS ANY KEY")
            app.send(b"\x03")
            assert app.wait_exit(timeout=8) == 0
            app.assert_restored()
        finally:
            close(app)


if __name__ == "__main__":
    run(pathlib.Path(sys.argv[1]).resolve())
    print("Title PTY passed: persistent modal, ignored paste, consumed key, empty/slow CLI bypass, Ctrl-C and restoration")
