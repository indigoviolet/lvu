#!/usr/bin/env python3
"""Extracted UTC time drives actual view membership, anchors and restart state."""
import os
import pathlib
import sys
import tempfile

from test_lvu_pty import PtyApp
from test_enrichment_chain_pty import paste, close_editor, stop


def run(binary):
    with tempfile.TemporaryDirectory(prefix="lvu-extracted-time-") as directory:
        root = pathlib.Path(directory)
        source = root / "events.log"
        source.write_text(
            "stamp<2026-09-05T12:30:46Z> boundary-row\n"
            "stamp<bad> malformed-row\n"
            "timestamp=2026-09-05T12:30:45Z missing-derived-row\n"
            "stamp<2026-09-05T12:30:45Z> timestamp=2020-01-01T00:00:00Z inside-row\n"
        )
        environment = {
            "MISE_DATA_DIR": os.environ.get("MISE_DATA_DIR", str(pathlib.Path.home() / ".local/share/mise")),
            "UV_CACHE_DIR": os.environ.get("UV_CACHE_DIR", str(pathlib.Path.home() / ".cache/uv")),
            "XDG_CONFIG_HOME": str(root / "config"),
            "XDG_DATA_HOME": str(root / "data"),
            "XDG_CACHE_HOME": str(root / "cache"),
        }
        arguments = [str(source)]
        for restart in (False, True):
            app = PtyApp(binary, arguments, width=130, height=30, environment=environment)
            try:
                app.wait_for("inside-row")
                if not restart:
                    app.send(b"e")
                    app.wait_for("Native enrichment")
                    paste(app, r"/stamp<(?P<timestamp_utc>[^>]+)>/")
                    app.send(b"\r")
                    app.wait_for("enrich:on")
                    close_editor(app)
                    app.send(b"t")
                    app.wait_for("Time window")
                    app.send(b"\x1bu")
                    app.wait_for("Extracted timestamp_utc")
                    paste(app, "2026-09-05T12:30:45Z")
                    app.send(b"\t")
                    paste(app, "2026-09-05T12:30:46Z")
                    app.send(b"\r")
                    app.wait_for("extracted-time:absolute")
                    app.send(b"\x1b")
                app.wait_until(lambda text: "inside-row" in text and "boundary-row" not in text
                               and "malformed-row" not in text and "missing-derived-row" not in text,
                               "extracted membership restored" if restart else "extracted membership applied")
                if not restart:
                    with source.open("a") as output:
                        output.write("stamp<2026-09-05T12:30:45.500000Z> late-row\n")
                app.wait_for("late-row")
                app.send(b"t")
                app.wait_for("Extracted timestamp_utc")
                app.send(b"\x1ba")
                app.wait_for("2026-09-05T12:30:15.500000000Z")
                app.send(b"\x1b")
                app.wait_until(lambda text: "Time window" not in text, "anchor dialog closed")
                if restart:
                    app.send(b"t")
                    app.wait_for("Time window")
                    app.send(b"\x1bc")
                    app.wait_until(lambda text: "extracted-time:absolute" not in text,
                                   "time cleared")
                    app.send(b"\x1b")
                    app.wait_for("malformed-row")
                    app.wait_for("missing-derived-row")
                app.wait_until(lambda text: "Time window" not in text, "time dialog closed")
                stop(app)
            finally:
                if app.process.poll() is None:
                    app.process.kill()
                    app.process.wait()
                    app.close()
    print("Extracted time PTY passed: explicit basis, exact membership, arrivals, anchor, restart, clear, terminal restoration")


if __name__ == "__main__":
    run(pathlib.Path(sys.argv[1]).resolve())
