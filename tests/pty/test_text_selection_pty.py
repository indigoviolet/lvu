#!/usr/bin/env python3
"""Drag/copy visible dialog text, dismiss, then copy the log behind it."""
import os
import base64
import pathlib
import re
import sys
import tempfile
from test_lvu_pty import PtyApp


def copy_text(app, text):
    app.wait_for(text)
    rows = app.screen.display
    y, row = next((y, row) for y, row in enumerate(rows) if text in row)
    x = row.index(text)
    before = len(app.transcript)
    app.send(f"\x1b[<0;{x+1};{y+1}M".encode())
    app.send(f"\x1b[<32;{x+len(text)};{y+1}M".encode())
    app.send(f"\x1b[<0;{x+len(text)};{y+1}m".encode())
    app.send(b"\x03")
    app.wait_until(lambda _: b"\x1b]52;c;" in app.transcript[before:], "clipboard escape")
    match = re.search(rb"\x1b\]52;c;([A-Za-z0-9+/=]*)(?:\x07|\x1b\\)", bytes(app.transcript[before:]))
    assert match is not None, bytes(app.transcript[before:])
    assert base64.b64decode(match.group(1)).decode() == text
    assert app.process.poll() is None, "copy must not quit"


tooling = {}
for key, xdg, fallback, suffix in [
    ("MISE_DATA_DIR", "XDG_DATA_HOME", pathlib.Path.home() / ".local/share", "mise"),
    ("MISE_CONFIG_DIR", "XDG_CONFIG_HOME", pathlib.Path.home() / ".config", "mise"),
    ("MISE_CACHE_DIR", "XDG_CACHE_HOME", pathlib.Path.home() / ".cache", "mise"),
    ("UV_CACHE_DIR", "XDG_CACHE_HOME", pathlib.Path.home() / ".cache", "uv"),
]:
    tooling[key] = os.environ.get(key, str(pathlib.Path(os.environ.get(xdg, fallback)) / suffix))

with tempfile.TemporaryDirectory(prefix="lvu-copy-pty-") as directory:
    root = pathlib.Path(directory)
    source = root / "sample.log"
    source.write_text('COPY_BACKGROUND_MARKER\n')
    app = PtyApp(pathlib.Path(sys.argv[1]).resolve(), [str(source)], width=140, height=34,
        environment={**tooling, "XDG_CONFIG_HOME": str(root / "config"), "XDG_DATA_HOME": str(root / "data"),
                     "XDG_CACHE_HOME": str(root / "cache")})
    try:
        app.wait_for("COPY_BACKGROUND_MARKER")
        app.send(b"e")
        app.wait_for("Native enrichment")
        copy_text(app, "No accepted outputs yet.")
        # Escape closes the dialog even after selection/copy.
        app.send(b"\x1b")
        app.wait_until(lambda text: "Native enrichment" not in text, "dialog dismissed")
        copy_text(app, "COPY_BACKGROUND_MARKER")
        app.send(b"e")
        app.wait_for("Native enrichment")
        expression = "changed = pl.col('raw').str.replace('BACKGROUND', 'REPLACED', literal=True)"
        app.send(b"\x1b[200~" + expression.encode() + b"\x1b[201~")
        app.send(b"\r")
        app.wait_for("COPY_REPLACED_MARKER", timeout=20)
        assert "COPY_BACKGROUND_MARKER" in app.text(), "raw input remains unchanged"
        app.send(b"\x1b")
        app.wait_until(lambda text: "Native enrichment" not in text, "editor closed")
        app.send(b"q")
        assert app.wait_exit(timeout=8) == 0
        app.assert_restored()
    finally:
        if app.process.poll() is None:
            app.process.kill()
            app.process.wait()
        app.close()
print("Selection PTY passed: dialog copy, dismissal, background copy, replacement and terminal restoration")
