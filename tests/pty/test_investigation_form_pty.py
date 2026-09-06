#!/usr/bin/env python3
"""Exercise Investigation form navigation without starting a remote session."""
import os
import pathlib
import sys
import tempfile

from test_lvu_pty import PtyApp


def run(binary: pathlib.Path) -> None:
    with tempfile.TemporaryDirectory(prefix="lvu-investigation-form-pty-") as temporary:
        root = pathlib.Path(temporary)
        environment = {
            "XDG_CONFIG_HOME": str(root / "config"),
            "XDG_CACHE_HOME": str(root / "cache"),
            "XDG_DATA_HOME": str(root / "data"),
            "COLORTERM": "truecolor",
        }
        app = PtyApp(binary, ["--demo"], width=100, height=24, environment=environment)
        try:
            app.wait_for("PRESS ANY KEY")
            app.send(b" ")
            app.wait_for("fixture request 01 completed")
            app.send(b"I")
            opened = app.wait_for("Investigation")
            assert "[ Start ]" in opened and "Question or follow-up" in opened
            app.send("wide 界\rnext e\u0301".encode())
            edited = app.wait_until(
                lambda text: "wide 界" in text and "next é" in text,
                "multiline investigation request",
            )
            assert not app.screen.cursor.hidden
            app.send(b"\t")
            focused = app.wait_for("[ Start ]")
            assert app.screen.cursor.hidden
            assert "wide 界" in focused
            app.resize(46, 12)
            narrow = app.wait_for("[ Start ]")
            assert "State" in narrow and "Question" in narrow
            app.send(b"\x1b")
            app.wait_until(lambda text: "Investigation" not in text, "dialog close")
            app.send(b"q")
            app.process.wait(timeout=5)
            app.drain()
            assert app.process.returncode == 0
            app.assert_restored()
        finally:
            if app.process.poll() is None:
                app.process.kill()
            app.close()


if __name__ == "__main__":
    run(pathlib.Path(sys.argv[1]).resolve())
    print("Investigation form PTY passed")
