#!/usr/bin/env python3
"""Two-layer enrichment dialog: step list, nested step editor, per-layer Escape."""

import os
import pathlib
import sys
import tempfile

from test_lvu_pty import PtyApp
from test_enrichment_chain_pty import close_details, paste

PATTERN = r"/svc=(?P<service>\w+) code=(?P<code>\d+)/"
UPPER = "upper_service = pl.col('service').str.to_uppercase()"


def open_list(app):
    app.send(b"e")
    text = app.wait_for("External command")
    assert "Expression" not in text, text
    # The list layer carries no editor, no key-reminder footer and no boxes.
    for banned in ("Enter", "Tab", "Esc", "PgUp", "↑/↓ scroll", "Status"):
        assert banned not in text, (banned, text)
    return text


def close_list(app):
    app.send(b"\x1b")
    app.wait_until(lambda text: "External command" not in text, "step list dismissed")


def add_step(app, expression, expected, timeout=20):
    """Alt-A enters layer two; a saved step returns to the layer-one list."""
    app.send(b"\x1ba")
    app.wait_for("Enrichment › New step")
    app.send(b"\x01\x0b")  # Ctrl-A then Ctrl-K clears any resumed draft
    paste(app, expression)
    app.send(b"\r")
    app.wait_until(lambda text: expected in text and "External command" in text,
                   f"saved {expected}", timeout=timeout)


def run(binary):
    home = pathlib.Path.home()
    tooling = {}
    for key, xdg, fallback, suffix in [
        ("MISE_DATA_DIR", "XDG_DATA_HOME", home / ".local/share", "mise"),
        ("MISE_CONFIG_DIR", "XDG_CONFIG_HOME", home / ".config", "mise"),
        ("MISE_CACHE_DIR", "XDG_CACHE_HOME", home / ".cache", "mise"),
        ("UV_CACHE_DIR", "XDG_CACHE_HOME", home / ".cache", "uv"),
    ]:
        tooling[key] = os.environ.get(key, str(pathlib.Path(os.environ.get(xdg, fallback)) / suffix))
    with tempfile.TemporaryDirectory(prefix="lvu-layers-pty-") as directory:
        root = pathlib.Path(directory)
        source = root / "events.log"
        source.write_text("svc=api code=200\nsvc=db code=500\n")
        environment = {**tooling, "XDG_CONFIG_HOME": str(root / "config"),
                       "XDG_DATA_HOME": str(root / "data"), "XDG_CACHE_HOME": str(root / "cache")}
        arguments = [str(source), "--capture-dir", str(root / "capture")]
        app = PtyApp(binary, arguments, width=120, height=34, environment=environment)
        try:
            app.wait_for("svc=db code=500")

            # Layer one lists steps only; Add opens the nested step editor.
            open_list(app)
            add_step(app, PATTERN, "1  /svc=")
            close_list(app)
            app.send(b"d")
            app.wait_for("service: db")
            app.wait_for("code: 500")
            close_details(app)

            # Editing and cancelling leaves the accepted chain intact.
            open_list(app)
            app.send(b"\r")
            app.wait_for("Enrichment › Edit step")
            paste(app, " BROKEN")
            app.wait_for("BROKEN")
            app.send(b"\x1b")
            listed = app.wait_for("External command")
            assert "1  /svc=" in listed, listed
            assert "BROKEN" not in listed, listed
            close_list(app)
            app.send(b"d")
            app.wait_for("service: db")
            close_details(app)

            # A later step may use a field an earlier step produced.
            open_list(app)
            add_step(app, UPPER, "2  upper_service")

            # A failing expression keeps its own layer and every earlier step.
            app.send(b"\x1ba")
            app.wait_for("Enrichment › New step")
            paste(app, "bad = pl.col(")
            app.send(b"\r")
            failed = app.wait_for("compiler rejected expression", timeout=20)
            assert "Expression" in failed, failed
            app.send(b"\x1b")
            listed = app.wait_for("External command")
            assert "1  /svc=" in listed and "2  upper_service" in listed, listed

            # One Escape per layer: step editor, then list, then the workspace.
            # Reopening Add resumes the unfinished draft the rejection left.
            app.send(b"\x1ba")
            resumed = app.wait_for("Enrichment › New step")
            assert "bad = pl.col(" in resumed, resumed
            app.send(b"\x1b")
            app.wait_for("External command")
            close_list(app)
            assert app.process.poll() is None, "closing the dialog must not quit"
            app.send(b"d")
            app.wait_for("upper_service: DB")
            close_details(app)

            # The whole flow stays usable on a narrow terminal.
            app.resize(54, 16)
            open_list(app)
            add_step(app, "tag = pl.lit('narrow')", "3  tag = pl.lit")
            app.send(b"\r")
            narrow = app.wait_for("Enrichment › Edit step")
            assert "Input record" in narrow, narrow
            assert "Accepted output" in narrow, narrow
            assert "[ Save ]" in narrow and "[ Remove ]" in narrow, narrow
            assert "Cancel" not in narrow, narrow
            app.send(b"\x1b")
            app.wait_for("External command")
            close_list(app)
            app.resize(120, 34)
            app.send(b"d")
            app.wait_for("upper_service: DB")
            close_details(app)

            app.send(b"q")
            code = app.wait_exit(timeout=10)
            assert code == 0, (code, app.text(), bytes(app.transcript[-8000:]))
            app.assert_restored()
        finally:
            (root / "terminal.ansi").write_bytes(app.transcript)
            (root / "screen.txt").write_text(app.text())
            if app.process.poll() is None:
                app.process.kill()
                app.process.wait(timeout=5)
            app.close()


if __name__ == "__main__":
    run(pathlib.Path(sys.argv[1]).resolve())
    print("Enrichment layers PTY passed: list-only layer one, nested step editor, "
          "cancelled edit, rejected draft, per-layer Escape and narrow layout")
