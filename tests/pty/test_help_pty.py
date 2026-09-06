#!/usr/bin/env python3
"""Exercise help scrolling and source-dialog action discovery in a real PTY."""
import pathlib
import sys

from test_lvu_pty import PtyApp


def run(binary: pathlib.Path) -> None:
    app = PtyApp(
        binary,
        ["--demo"],
        width=72,
        height=16,
        environment={
            "LVU_NO_DELIGHT": "1",
            "NO_COLOR": "",
            "COLORTERM": "truecolor",
        },
    )
    try:
        initial = app.wait_for("fixture request 16 complete")
        assert "6-16/16" in initial

        app.send(b"?")
        help_top = app.wait_for("EVERYWHERE")
        assert "g / G" in help_top and "PgUp/PgDn" not in help_top
        assert b"\x1b[38;" in app.transcript and b"\x1b[1m" in app.transcript, (
            "color and bold semantic styles were not emitted"
        )

        app.send(b"\x1b[B" * 16)  # Repeated Down reaches later help sections.
        app.send(b"\x1b[<65;20;8M")  # Wheel down inside help.
        app.wait_until(
            lambda text: "Alt-R" in text and "EVERYWHERE" not in text,
            "scrolled help content",
        )
        app.send(b"\x1b[B" * 100)
        bottom = app.wait_for("Alt-N")
        assert "MOUSE & SELECTION" not in bottom and "explicit review and apply" not in bottom
        assert "j/k · ↑/↓" not in help_top
        app.send(b"\x1b")
        restored = app.wait_for("fixture request 16 complete")
        assert "6-16/16" in restored, "help navigation scrolled the log behind it"

        app.send(b"n")
        source = app.wait_for("Add source")
        assert "Ctrl-A 🧠" in source
        assert "Ctrl-D Discover" in source and "Enter Open" not in source
        assert "Alt-F file" not in source, "shortcut list leaked into source body"

        app.send(b"\x04")  # Ctrl-D: discovery.
        discovery = app.wait_for("Discover sources")
        assert "↑/↓ active pane" in discovery
        assert "Ctrl-R Refresh" in discovery
        assert "Ctrl-D Manual" in discovery
        assert "SCAN SUMMARY" in discovery or "UPDATING" in discovery
        assert "wheel select" not in discovery

        app.send(b"\x1b")
        app.wait_until(lambda text: "Discover sources" not in text, "discovery closed")
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
    print("Help PTY passed: styled grouped scrolling, background isolation, source and discovery actions")
