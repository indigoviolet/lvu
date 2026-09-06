#!/usr/bin/env python3
"""Save/update, inspect immutable history, apply old revision without moving current."""
import pathlib
import sys
import tempfile
import tomllib
from test_lvu_pty import PtyApp
from test_enrichment_chain_pty import stop, paste


def search(app, text):
    app.send(b"/"); app.wait_for("Search")
    app.send(b"\x7f" * 32); app.send(text.encode()); app.wait_for("Applied  " + text)
    app.send(b"\x1b"); app.wait_until(lambda t: " Search " not in t, "search closed")


def run(binary):
    with tempfile.TemporaryDirectory(prefix="lvu-recipe-history-") as directory:
        root = pathlib.Path(directory)
        source = root / "events.log"; source.write_text("keep message\nignore message\n")
        env = {"XDG_CONFIG_HOME": str(root / "config"), "XDG_DATA_HOME": str(root / "data"), "XDG_CACHE_HOME": str(root / "cache")}
        for iteration in (0, 1):
            app = PtyApp(binary, [str(source), "--capture-dir", str(root / "capture")], width=120, height=28, environment=env)
            try:
                app.wait_for("message")
                if iteration == 0:
                    search(app, "keep")
                    app.send(b"r"); app.wait_for("Named recipes")
                    app.send(b"\x1bs"); app.wait_for("Mode: Save"); app.send(b"Versioned\r")
                    app.wait_for("1 saved recipes"); app.send(b"\x1b")
                    app.wait_until(lambda t: "Named recipes" not in t, "recipes closed")
                    search(app, "ignore")
                    app.send(b"r"); app.wait_for("1 saved recipes")
                    app.send(b"\x1bu"); app.wait_for("NEW revision"); app.send(b"\r")
                    app.wait_for("1 saved recipes")
                else:
                    app.send(b"r"); app.wait_for("1 saved recipes")
                app.send(b"\x1bh"); app.wait_for("2 revisions")
                app.send(b"\x1b[B"); app.wait_for('search="keep"')
                if iteration == 0:
                    output = root / "old.toml"
                    app.send(b"\x1be"); app.wait_for("Mode: Export")
                    paste(app, str(output)); app.send(b"\r"); app.wait_for("Status: exported")
                    assert tomllib.loads(output.read_text())["view"]["search"] == "keep"
                    app.send(b"\x1bh"); app.wait_for("2 revisions"); app.send(b"\x1b[B")
                app.send(b"\r"); app.wait_until(lambda t: "Named recipes" not in t, "old revision applied")
                app.wait_until(lambda t: "keep message" in t and "ignore message" not in t, "old filter applied")
                app.send(b"r"); app.wait_for("1 saved recipes"); app.wait_for('search="ignore"')
                app.send(b"\r"); app.wait_until(lambda t: "Named recipes" not in t, "current recipe applied")
                app.wait_until(lambda t: "ignore message" in t and "keep message" not in t, "current pointer unchanged")
                stop(app)
            finally:
                if app.process.poll() is None:
                    app.send(b"\x03")
                    try: app.wait_exit(5)
                    except Exception: app.process.kill(); app.process.wait()
                    app.close()
    print("Recipe history PTY passed: explicit update, immutable old revision/export/apply, current pointer and restart, restoration")


if __name__ == "__main__": run(pathlib.Path(sys.argv[1]).resolve())
