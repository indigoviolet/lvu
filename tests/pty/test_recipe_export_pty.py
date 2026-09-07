#!/usr/bin/env python3
"""Export a reviewed TOML revision and import it in another workspace."""
import pathlib
import sys
import tempfile
import tomllib
from test_lvu_pty import PtyApp
from test_enrichment_chain_pty import stop, paste


def run(binary):
    with tempfile.TemporaryDirectory(prefix="lvu-recipe-export-") as directory:
        root = pathlib.Path(directory)
        source = root / "events.log"
        source.write_text("keep message\nignore message\n")
        output = root / "portable café.toml"
        for number in (1, 2):
            workspace = root / str(number)
            env = {"XDG_CONFIG_HOME": str(workspace / "config"), "XDG_DATA_HOME": str(workspace / "data"), "XDG_CACHE_HOME": str(workspace / "cache")}
            app = PtyApp(binary, [str(source), "--capture-dir", str(workspace / "capture")], width=120, height=28, environment=env)
            try:
                app.wait_for("ignore message")
                if number == 1:
                    app.send(b"/"); app.wait_for("Search"); app.send(b"keep"); app.wait_for("Applied   keep")
                    app.send(b"\x1b"); app.wait_until(lambda t: " Search " not in t, "search closed")
                    app.send(b"r"); app.wait_for("Saved recipes")
                    app.send(b"\x1bs"); app.wait_for("Save revision"); app.send(b"Portable\r")
                    app.wait_for("1 saved recipes")
                    app.send(b"\x1be"); app.wait_for("Export revision")
                    paste(app, str(output)); app.send(b"\r"); app.wait_until(lambda t: "Applied" in t and "exported" in t, "export reported")
                    first = output.read_bytes()
                    document = tomllib.loads(first.decode())
                    assert document["view"]["search"] == "keep"
                    app.send(b"\r"); app.wait_for("export recipe:")
                    assert output.read_bytes() == first, "repeat export must not replace the file"
                    app.send(b"\x1b"); app.wait_until(lambda t: "Saved recipes" not in t, "recipe dialog closed")
                else:
                    app.send(b"r"); app.wait_for("Saved recipes")
                    app.send(b"\x1bi"); app.wait_for("Review import")
                    paste(app, str(output)); app.send(b"\r"); app.wait_for("1 saved recipes")
                    app.send(b"\r"); app.wait_until(lambda t: "Saved recipes" not in t, "imported recipe applied")
                    app.wait_until(lambda t: "keep message" in t and "ignore message" not in t, "imported filter narrowed rows")
                    app.send(b"/"); app.wait_for("Applied   keep")
                    app.send(b"\x1b"); app.wait_until(lambda t: " Search " not in t, "search closed")
                stop(app)
            finally:
                if app.process.poll() is None:
                    app.send(b"\x03")
                    try: app.wait_exit(5)
                    except Exception: app.process.kill(); app.process.wait()
                    app.close()
    print("Recipe export PTY passed: reviewed TOML, Unicode path, no overwrite, independent workspace import/apply, restoration")


if __name__ == "__main__": run(pathlib.Path(sys.argv[1]).resolve())
