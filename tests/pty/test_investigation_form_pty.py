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
            # §12.18 names the field `Question`; §3 replaced the boxed sections.
            assert "[ Start ]" in opened and "Question" in opened
            app.send("wide 界\rnext e\u0301".encode())
            edited = app.wait_until(
                lambda text: "wide 界" in text and "next é" in text,
                "multiline investigation request",
            )
            assert not app.screen.cursor.hidden
            app.send(b"\t")
            # [ Start ] is on screen before and after the Tab, so waiting for it
            # can return the pre-Tab frame. Wait for the transition itself: the
            # caret must disappear once focus leaves the prompt.
            focused = app.wait_until(
                lambda _text: app.screen.cursor.hidden,
                "caret hidden once focus leaves the prompt",
            )
            assert "[ Start ]" in focused, focused
            assert "wide 界" in focused
            app.resize(46, 12)
            # `[ Start ]` is on screen before and after the resize, so waiting
            # for it alone can return a half-repainted frame carrying stale
            # cells from the wide layout. Wait for every row the assertion
            # below needs to be present in the same frame.
            narrow = app.wait_until(
                lambda text: "[ Start ]" in text and "Question" in text and "Ready" in text,
                "the dialog redrawn at the narrow size",
            )
            # §7.4 replaced the `State` box with the shared message row.
            assert "Ready" in narrow and "Question" in narrow
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
