#!/usr/bin/env python3
"""Drag/copy visible dialog text, dismiss, then copy the log behind it."""
import os
import base64
import pathlib
import re
import sys
import tempfile
from test_lvu_pty import PtyApp


def drag_copy(app, start, end):
    before = len(app.transcript)
    x, y = start
    ex, ey = end
    app.send(f"\x1b[<0;{x+1};{y+1}M".encode())
    app.send(f"\x1b[<32;{ex+1};{ey+1}M".encode())
    app.send(f"\x1b[<0;{ex+1};{ey+1}m".encode())
    app.send(b"\x03")
    app.wait_until(lambda _: b"\x1b]52;c;" in app.transcript[before:], "clipboard escape")
    match = re.search(rb"\x1b\]52;c;([A-Za-z0-9+/=]*)(?:\x07|\x1b\\)", bytes(app.transcript[before:]))
    assert match is not None, bytes(app.transcript[before:])
    assert app.process.poll() is None, "copy must not quit"
    return base64.b64decode(match.group(1)).decode()


def copy_text(app, text):
    app.wait_for(text)
    y, row = next((y, row) for y, row in enumerate(app.screen.display) if text in row)
    x = row.index(text)
    assert drag_copy(app, (x, y), (x + len(text) - 1, y)) == text


def copy_across_dialog(app):
    rows = app.screen.display
    top, title = next((y, row) for y, row in enumerate(rows) if "┌ Enrichment " in row)
    left, right = title.index("┌"), title.rindex("┐")
    bottom = next(y for y in range(top + 1, len(rows)) if rows[y][left] == "└")
    expected = "\n".join(row[left + 1:right].rstrip() for row in rows[top + 1:bottom])
    for start, end in [
        ((left + 1, top + 1), (app.screen.columns - 1, app.screen.lines - 1)),
        ((right - 1, bottom - 1), (0, 0)),
    ]:
        copied = drag_copy(app, start, end)
        assert copied == expected, (copied, expected)
        assert "lvu live sources" not in copied
        assert "FOLLOW" not in copied
        assert "┌ Enrichment " not in copied, "dialog border is outside selection"


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
        app.wait_for("┌ Enrichment ")
        copy_text(app, "No accepted outputs yet.")
        copy_across_dialog(app)
        # Escape closes the dialog even after selection/copy.
        app.send(b"\x1b")
        app.wait_until(lambda text: "┌ Enrichment " not in text, "dialog dismissed")
        copy_text(app, "COPY_BACKGROUND_MARKER")
        app.send(b"e")
        app.wait_for("┌ Enrichment ")
        expression = "changed = pl.col('raw').str.replace('BACKGROUND', 'REPLACED', literal=True)"
        app.send(b"\x1b[200~" + expression.encode() + b"\x1b[201~")
        app.send(b"\r")
        app.wait_for("COPY_REPLACED_MARKER", timeout=20)
        assert "COPY_BACKGROUND_MARKER" in app.text(), "raw input remains unchanged"
        app.send(b"\x1b")
        app.wait_until(lambda text: "┌ Enrichment " not in text, "editor closed")
        app.send(b"q")
        assert app.wait_exit(timeout=8) == 0
        app.assert_restored()
    finally:
        if app.process.poll() is None:
            app.process.kill()
            app.process.wait()
        app.close()
print("Selection PTY passed: dialog copy, dismissal, background copy, replacement and terminal restoration")
