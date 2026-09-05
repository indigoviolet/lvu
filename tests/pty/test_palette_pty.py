#!/usr/bin/env python3
"""Drive the real terminal overlay, including editor input isolation."""
import pathlib
import sys
import tempfile

from test_lvu_pty import PtyApp


def run(binary, real=False):
    temporary = tempfile.TemporaryDirectory(prefix="lvu-palette-pty-")
    root = pathlib.Path(temporary.name)
    source = root / "events.log"
    source.write_text("palette-first-record\n")
    arguments = ["--file", str(source), "--capture-dir", str(root / "capture")] if real else ["--demo"]
    app = PtyApp(binary, arguments, width=88, height=24)
    try:
        app.wait_for("palette-first-record" if real else "DEMO FIXTURE")
        app.send(b"\x10")
        app.wait_for("Command palette")
        if real:
            with source.open("a") as output:
                output.write("palette-late-record\n")
            app.send(b"\x1b")
            app.wait_for("palette-late-record")
            app.send(b"\x10")
            app.wait_for("Command palette")
        app.send(b"literal\t")
        app.wait_for("Literal filter")
        app.send(b"\r")
        app.wait_for("Search")
        app.send(b"unfinished-draft")
        app.wait_for("unfinished-draft")
        app.send(b"\x10")
        app.wait_for("Command palette")
        app.send(b"\x1b[200~confirm derived-data cleanup\x1b[201~")
        app.wait_for("Confirm derived-data cleanup")
        app.send(b"\r")
        app.assert_remains("Command palette", "THIS STRING CANNOT APPEAR")
        app.send(b"\x1b")
        screen = app.wait_until(lambda text: "Command palette" not in text and "unfinished-draft" in text,
                                "unfinished editor restored after disabled palette command")
        assert "confirm derived-data cleanup" not in screen
        app.send(b"\x1b")
        app.wait_until(lambda text: "Search ─" not in text, "search editor closed")
        app.send(b"\x10")
        app.wait_for("Command palette")
        app.send(b"quit\t")
        app.wait_for("Quit")
        app.send(b"\r")
        app.process.wait(timeout=5)
        app.drain()
        assert app.process.returncode == 0
        app.assert_restored()
    finally:
        if app.process.poll() is None:
            app.process.kill()
            app.process.wait(timeout=5)
        app.close()
        temporary.cleanup()


if __name__ == "__main__":
    run(pathlib.Path(sys.argv[1]).resolve(), "--real" in sys.argv[2:])
    print("Palette PTY passed: toggle/search/autocomplete/execute/disabled/paste/editor restoration/quit")
