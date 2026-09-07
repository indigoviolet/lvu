#!/usr/bin/env python3
"""Shared Enrichment, Grouping, and External-command controls in an actual PTY."""

import pathlib
import sys

from test_lvu_pty import PtyApp


def run(binary: pathlib.Path) -> None:
    app = PtyApp(
        binary,
        ["--demo"],
        width=100,
        height=30,
        environment={"LVU_NO_DELIGHT": "1", "LVU_THEME": "love-dark"},
    )
    try:
        app.wait_for("DEMO FIXTURE")

        app.send(b"m")
        grouping = app.wait_for("Display-only multiline grouping")
        assert "Applied:" in grouping, grouping
        assert "Enter Apply" in grouping, grouping
        assert "Scroll status" not in grouping, grouping
        app.send(b"q")
        app.wait_for("q^(\\s+|Caused by:)")
        app.send(b"\x1b")

        app.send(b"e")
        enrichment = app.wait_for("[ External command… ]")
        for label in ("[ Add ]", "[ Edit ]", "[ Remove ]"):
            assert label in enrichment, enrichment
        assert "Steps" in enrichment, enrichment
        assert "Expression" not in enrichment, enrichment
        app.resize(38, 18)
        app.send(b"\t\t\t\t")
        narrow = app.wait_for("[ External c")
        assert "Status" not in narrow, narrow
        app.send(b"\r")

        command = app.wait_for("runs only when confirmed")
        for label in ("[ New line (Alt-N) ]", "[ Save ]", "[ Review ]", "[ Remove ]"):
            assert label in command, command
        app.resize(100, 30)
        command = app.wait_for("Applied command step:")
        assert "Status and review · ↑/↓ scroll" not in command, command
        app.send(b"\x1b")
        app.wait_for("External command")
        app.send(b"\x1b")

        app.send(b"q")
        assert app.wait_exit(timeout=5) == 0
        app.drain()
        app.assert_restored()
    finally:
        if app.process.poll() is None:
            app.process.kill()
            app.process.wait(timeout=5)
        app.close()


if __name__ == "__main__":
    run(pathlib.Path(sys.argv[1]).resolve())
    print("Shared Enrichment/External-command PTY passed")
