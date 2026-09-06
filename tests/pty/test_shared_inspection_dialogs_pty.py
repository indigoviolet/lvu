#!/usr/bin/env python3
"""Read-only dialogs retain anchors, overflow, and semantic truecolor actions."""
import pathlib
import sys

from test_lvu_pty import PtyApp


def run(binary: pathlib.Path) -> None:
    app = PtyApp(
        binary,
        ["--demo"],
        width=96,
        height=22,
        environment={"LVU_NO_DELIGHT": "1", "NO_COLOR": "", "COLORTERM": "truecolor"},
    )
    try:
        app.wait_for("fixture request 16 complete")

        app.send(b"d")
        details = app.wait_for("Selected event details")
        assert "stable display id:" in details and "raw:" in details
        assert "↑/↓ scroll" in details
        app.send(b"d")
        app.wait_until(lambda text: "Selected event details" not in text, "Details closed")

        app.send(b"i")
        fields = app.wait_for("Color rows by this field")
        assert "Space pin" in fields and "Enter" not in fields and "Esc" not in fields
        assert b"\x1b[38;2;" in app.transcript, "truecolor semantic roles were not emitted"
        app.resize(54, 12)
        app.wait_for("Event fields")
        app.send(b"\x1b")

        app.resize(96, 22)
        app.wait_for("fixture request 16 complete")
        app.send(b"o")
        context = app.wait_for("raw, unfiltered")
        anchor_line = next(line for line in context.splitlines() if "Anchor:" in line)
        app.send(b"\x1b[B" * 8)
        assert next(line for line in app.text().splitlines() if "Anchor:" in line) == anchor_line
        app.resize(58, 12)
        app.wait_for("g anchor")
        app.send(b"\x1b")

        app.send(b"?")
        app.wait_for("EVERYWHERE")
        app.send(b"\x1b[B" * 100)
        app.wait_for("Alt-N")
        app.send(b"?")
        app.wait_for("fixture request 16 complete")

        app.send(b"S")
        storage = app.wait_for("Storage usage")
        assert "r refresh" in storage and "c preview/confirm cleanup" in storage
        app.resize(58, 12)
        app.wait_for("Storage usage")
        app.send(b"\x1b")
        app.wait_until(lambda text: "Storage usage" not in text, "Storage closed")

        app.send(b"q")
        assert app.wait_exit(timeout=5) == 0
        app.assert_restored()
    finally:
        if app.process.poll() is None:
            app.process.kill()
            app.process.wait(timeout=5)
        app.close()


if __name__ == "__main__":
    run(pathlib.Path(sys.argv[1]).resolve())
    print("Shared inspection dialogs PTY passed: styles, narrow actions, fixed raw anchor, help overflow")
