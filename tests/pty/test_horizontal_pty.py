#!/usr/bin/env python3
"""Actual event-cell horizontal navigation with fixed metadata and reset."""
import pathlib
import sys
import tempfile
from test_lvu_pty import PtyApp

with tempfile.TemporaryDirectory(prefix="lvu-horizontal-") as directory:
    root = pathlib.Path(directory)
    source = root / "wide.log"
    source.write_text("START_MARKER " + "界" * 40 + " END_MARKER\n")
    app = PtyApp(pathlib.Path(sys.argv[1]).resolve(), [str(source)], width=100, height=24,
                 environment={"XDG_CONFIG_HOME": str(root / "config"),
                              "XDG_DATA_HOME": str(root / "data"),
                              "XDG_CACHE_HOME": str(root / "cache")})
    try:
        app.wait_for("START_MARKER")
        assert "END_MARKER" not in app.text()
        app.send(b"\x1b[C" * 10)
        app.wait_for("END_MARKER")
        assert "x=80" in app.text()
        app.send(b"0")
        app.wait_for("START_MARKER")
        assert "END_MARKER" not in app.text()
        app.send(b"q")
        assert app.wait_exit(timeout=8) == 0
        app.assert_restored()
    finally:
        if app.process.poll() is None:
            app.process.kill()
            app.process.wait()
        app.close()
print("Horizontal PTY passed: wide text, right scroll, reset, terminal restoration")
