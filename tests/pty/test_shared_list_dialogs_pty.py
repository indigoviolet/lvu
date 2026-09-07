#!/usr/bin/env python3
"""Shared Recipes, Views and Bookmarks forms stay usable in a real terminal."""
import pathlib
import sys
import tempfile

from test_enrichment_chain_pty import paste, stop
from test_lvu_pty import PtyApp


def run(binary):
    with tempfile.TemporaryDirectory(prefix="lvu-shared-list-dialogs-") as directory:
        root = pathlib.Path(directory)
        source = root / "events.log"
        source.write_text("first event\nsecond event\n")
        env = {
            "XDG_CONFIG_HOME": str(root / "config"),
            "XDG_DATA_HOME": str(root / "data"),
            "XDG_CACHE_HOME": str(root / "cache"),
        }
        app = PtyApp(binary, [str(source), "--capture-dir", str(root / "capture")],
                     width=94, height=22, environment=env)
        try:
            app.wait_for("second event")

            app.send(b"r")
            app.wait_for("Saved recipes")
            app.wait_for("Apply restores a recipe")
            app.send(b"\x1bs")
            app.wait_for("Save revision")
            paste(app, "q recipe")
            app.wait_for("q recipe")
            app.send(b"\x1b")
            # ESC immediately followed by a printable byte parses as Alt-<key>;
            # wait for the dialog to close before the next shortcut.
            app.wait_until(
                lambda text: "Saved recipes" not in text, "recipes closes"
            )

            app.send(b"v")
            app.wait_for("View · ")
            app.wait_for("New blank")
            app.send(b"\x1bm")
            app.wait_for("Apply membership")
            app.send(b"\x1b")
            app.wait_until(lambda text: "Apply membership" not in text, "view closes")

            app.send(b"g")
            app.send(b"b")
            app.wait_for("bookmarked")
            app.send(b"B")
            app.wait_for("Edit note")
            app.send(b"\x1be")
            app.wait_for("Save note")
            paste(app, "q note")
            app.wait_for("q note")
            app.send(b"\r")
            app.wait_for("note updated")
            app.send(b"\x1b")
            app.wait_until(lambda text: "Save note" not in text, "bookmarks closes")
            stop(app)
        finally:
            if app.process.poll() is None:
                app.send(b"\x03")
                try:
                    app.wait_exit(5)
                except Exception:
                    app.process.kill()
                    app.process.wait()
                app.close()
    print("Shared list dialog PTY passed: visible actions, literal q inputs, and terminal restoration")


if __name__ == "__main__":
    run(pathlib.Path(sys.argv[1]).resolve())
