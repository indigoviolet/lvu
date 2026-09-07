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
        # §8.10: the docked pane carries no key footer; arrows are a convention.
        assert "↑/↓" not in details and "scroll" not in details, details
        app.send(b"d")
        app.wait_until(lambda text: "Selected event details" not in text, "Details closed")

        app.send(b"i")
        fields = app.wait_for("Color rows by field")
        # §11 retired the key footer: the affordances are buttons now, and the
        # dialog still lists no Enter/Esc reminders.
        assert "[ Pin ]" in fields, fields
        assert "Pinned fields become log columns" in fields, fields
        assert "Enter" not in fields and "Esc" not in fields
        # The demo binary renders only the `terminal` theme, whose sole RGB value
        # is the delight heart that this suite disables — so a truecolor check
        # here can never pass. Assert that the dialog emits explicit semantic
        # colour roles rather than terminal defaults; truecolor itself is covered
        # by the love-theme suites (shared_dialog_controls, shared_palette_colors).
        assert b"\x1b[38;5;" in app.transcript, "semantic colour roles were not emitted"
        app.resize(54, 12)
        app.wait_for("Fields · record")
        app.send(b"\x1b")

        app.resize(96, 22)
        app.wait_for("fixture request 16 complete")
        app.send(b"o")
        context = app.wait_for("raw, unfiltered")
        # §12.12 puts the anchor and the span it is showing on one header
        # line, so the span moves as you scroll — the anchor must not.
        def anchor_of(text):
            line = next(line for line in text.splitlines() if "Anchor:" in line)
            return line.split("Anchor: ", 1)[1].split(" ·", 1)[0]

        anchor_id = anchor_of(context)
        app.send(b"\x1b[B" * 8)
        assert anchor_of(app.text()) == anchor_id
        app.resize(58, 12)
        # §11 retired the key list; `g` stays the accelerator for the button.
        app.wait_for("[ Back to anchor ]")
        # Resize first and let the frame settle, then dismiss: an Escape sent
        # into a resize can be held by the escape-sequence parser, and a marker
        # sampled on the blank frame a resize produces reads as "closed" while
        # the modal is still up and still eating the next key. Both waits below
        # pair a positive marker with the negative one for that reason.
        app.resize(96, 22)
        app.wait_until(lambda text: "Raw context" in text and "fixture request 16 complete" in text,
                       "raw context redrawn at full size")
        app.send(b"\x1b")
        app.wait_until(lambda text: "Raw context" not in text and "fixture request 16 complete" in text,
                       "raw context closed and the workspace repainted")

        app.send(b"?")
        app.wait_for("EVERYWHERE")
        app.send(b"\x1b[B" * 100)
        app.wait_for("Alt-R")
        app.send(b"?")
        app.wait_for("fixture request 16 complete")

        app.send(b"S")
        storage = app.wait_for("Storage")
        # §3 replaces the key-reminder footer with the action row.
        assert "[ Refresh ]" in storage and "[ Preview cleanup ]" in storage
        assert "r refresh" not in storage, storage
        app.resize(58, 12)
        app.wait_for("Storage")
        app.send(b"\x1b")
        app.wait_until(lambda text: "Storage" not in text, "Storage closed")

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
