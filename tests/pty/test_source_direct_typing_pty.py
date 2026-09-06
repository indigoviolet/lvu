#!/usr/bin/env python3
"""Exercise literal q during automatic Source completion with real key events."""

from __future__ import annotations

import os
import pathlib
import sys
import tempfile
import time

from test_lvu_pty import PtyApp


def run_case(binary: pathlib.Path, delay: float, burst: bool) -> None:
    fixture = pathlib.Path(tempfile.mkdtemp(prefix="lvu-qq-fixture-"))
    runtime = pathlib.Path(tempfile.mkdtemp(prefix="lvu-qq-runtime-"))
    candidate = fixture / "qaq-sequence.log"
    marker = f"DIRECT-Q-{int(delay * 1000)}-{'burst' if burst else 'paced'}"
    candidate.write_text(marker + "\n")
    prefix = str(fixture / "qaq-seq")
    environment = {
        "LVU_NO_DELIGHT": "1",
        "XDG_CONFIG_HOME": str(runtime / "config"),
        "XDG_DATA_HOME": str(runtime / "data"),
        "XDG_CACHE_HOME": str(runtime / "cache"),
    }
    app = PtyApp(
        binary,
        ["--capture-dir", str(runtime / "capture")],
        width=132,
        height=28,
        cwd=pathlib.Path("/tmp"),
        environment=environment,
    )
    try:
        app.wait_for("FILE PATH", timeout=5.0)
        encoded = prefix.encode()
        if burst:
            written = os.write(app.master, encoded)
            assert written == len(encoded), (written, len(encoded))
        else:
            for byte in encoded:
                written = os.write(app.master, bytes([byte]))
                assert written == 1
                time.sleep(delay)
        screen = app.wait_for("qaq-sequence.log", timeout=5.0)
        assert prefix in screen, screen
        app.send(b"\r")
        app.wait_for(marker, timeout=5.0)
    finally:
        if app.process.poll() is None:
            app.process.kill()
            app.process.wait(timeout=5.0)
        app.close()


def run(binary: pathlib.Path) -> None:
    for milliseconds in (5, 10, 20):
        run_case(binary, milliseconds / 1000, burst=False)
    run_case(binary, 0, burst=True)


if __name__ == "__main__":
    run(pathlib.Path(sys.argv[1]).resolve())
    print("Source direct typing PTY passed: q early/middle/repeated, paced and burst")
