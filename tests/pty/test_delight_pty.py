#!/usr/bin/env python3
"""Actual terminal checks for startup dismissal and nonblocking delight."""
import pathlib
import sys
from test_lvu_pty import PtyApp


def story(binary, environment, dismiss):
    app = PtyApp(binary, ["--demo"], environment=environment)
    try:
        if "LVU_NO_DELIGHT" not in environment:
            app.wait_for("ESC TO ENTER")
            app.assert_remains("ESC TO ENTER", "DEMO FIXTURE", duration=0.8)
            app.send(b"\x1b")
            app.wait_for("DEMO FIXTURE")
            if dismiss:
                app.send(b"/")
                app.wait_for("Search")
                assert "ESC TO ENTER" not in app.text()
                app.send(b"\x1b")
        app.wait_for("DEMO FIXTURE")
        if "LVU_NO_DELIGHT" not in environment:
            app.wait_for("idle")
        app.send(b"a")
        app.wait_for("late fixture arrival")
        app.resize(18, 4)
        app.wait_for("small")
        app.resize(88, 24)
        app.wait_for("DEMO FIXTURE")
        app.send(b"q")
        app.process.wait(timeout=5)
        app.drain()
        assert app.process.returncode == 0
        app.assert_restored()
        if "LVU_NO_DELIGHT" in environment:
            assert b"ESC TO ENTER" not in app.transcript
    finally:
        if app.process.poll() is None:
            app.process.kill()
            app.process.wait(timeout=5)
        app.close()


if __name__ == "__main__":
    binary = pathlib.Path(sys.argv[1]).resolve()
    story(binary, {}, True)
    story(binary, {"LVU_REDUCED_MOTION": "1", "LVU_ASCII": "1"}, False)
    story(binary, {"LVU_NO_DELIGHT": "1"}, False)
    print("Delight PTY passed: persistent title, Escape entry, idle, arrivals, resize, reduced motion, disabled, restoration")
