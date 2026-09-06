#!/usr/bin/env python3
"""Actual terminal checks for startup dismissal and nonblocking delight."""
import pathlib
import sys
from test_lvu_pty import PtyApp


def story(binary, environment, dismiss):
    app = PtyApp(binary, ["--demo"], environment={"NO_COLOR": "", "COLORTERM": "truecolor", **environment})
    try:
        if "LVU_NO_DELIGHT" not in environment:
            app.wait_for("PRESS ANY KEY")
            app.assert_remains("PRESS ANY KEY", "DEMO FIXTURE", duration=0.8)
            app.send(b"x")
            app.wait_for("DEMO FIXTURE")
            if dismiss:
                app.send(b"/")
                app.wait_for("Search")
                assert "PRESS ANY KEY" not in app.text()
                app.send(b"\x1b")
                app.wait_until(lambda text: "Search" not in text, "search closes before resting-heart capture")
        app.wait_for("DEMO FIXTURE")
        if "LVU_NO_DELIGHT" not in environment:
            assert "idle" not in app.text().splitlines()[-1]
        def corner():
            return tuple((app.screen.buffer[y][x].data, app.screen.buffer[y][x].fg, app.screen.buffer[y][x].bg)
                         for y in range(app.screen.lines - 4, app.screen.lines - 1) for x in range(22))
        quiet = corner()
        if not any(key in environment for key in ("LVU_NO_DELIGHT", "LVU_ASCII")):
            assert all(app.screen.buffer[app.screen.lines - 4][x].data == " "
                       for x in range(0, 8)), "transparent heart margin became a foreground stripe"
        app.send(b"a")
        app.wait_for("late fixture arrival")
        if not any(key in environment for key in ("LVU_NO_DELIGHT", "LVU_REDUCED_MOTION", "LVU_ASCII")):
            app.wait_until(lambda _: corner() != quiet, "corner pulse follows live work", timeout=2)
            app.assert_remains("DEMO FIXTURE", "agent working", duration=1.3)
            assert corner() == quiet, "idle corner did not return to resting sprite"

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
            assert b"PRESS ANY KEY" not in app.transcript
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
    print("Delight PTY passed: persistent title, any-key entry, quiet indicator, arrivals, resize, reduced motion, disabled, restoration")
