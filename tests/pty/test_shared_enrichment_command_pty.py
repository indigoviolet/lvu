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
        # dialog-system.md §7.1 titles are nouns, and §7.4 replaces the
        # `Applied:` vocabulary with the shared message row.
        grouping = app.wait_for("Multiline grouping")
        assert "Mode Auto" in grouping, grouping
        assert "conservative multiline detection" in grouping, grouping
        assert "Applied:" not in grouping, grouping
        # Grouping must expose Apply as an activatable action. The old "Enter Apply"
        # hint was removed with the universal-shortcut cleanup but never replaced,
        # leaving the dialog with no actions region at all. Tracked in TODO.md.
        assert "[ Apply ]" in grouping, grouping
        assert "Scroll status" not in grouping, grouping
        # Typing replaces Auto with an exact Custom regex draft; the reserved
        # persistence token is never exposed in ordinary UI.
        app.send(b"q")
        app.wait_for("Mode Custom")
        app.send(b"\x1b")
        # Wait for the dialog to actually close: ESC immediately followed by a
        # printable byte is parsed as Alt-<key>, so `e` would land in the field.
        app.wait_until(
            lambda text: "Multiline grouping" not in text,
            "grouping closes before the next shortcut",
        )

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

        # §7.1 moved the confirmation warning out of the title, which could not
        # render at 38 columns anyway. Synchronise on a control unique to the
        # child dialog, then assert the full control set once there is room:
        # External command has not been adopted onto the dialog anatomy yet, so
        # at 38x18 its fixed-height layout still drops the last button.
        app.wait_for("[ Review and run ]")
        app.resize(100, 30)
        command = app.wait_until(
            lambda text: "runs only when you confirm" in text
            and "[ New line ]" in text,
            "the command form at full size",
        )
        # §11 retired the printed Alt-N; the button is the affordance.
        for label in ("[ New line ]", "[ Save ]", "[ Review and run ]", "[ Remove ]"):
            assert label in command, command
        assert "Alt-N" not in command, command
        assert "Applied command step:" in command, command
        assert "External command ·" not in command, command
        assert "Status and review · ↑/↓ scroll" not in command, command
        app.send(b"\x1b")
        # As after the grouping dialog above: the screen sampled immediately
        # after a keypress is still the previous frame, so the old
        # `wait_for("External command")` here matched the dialog that was on
        # its way out and synchronised on nothing. That left the *second* Esc
        # to do the closing, and the Esc after that reached the base, where the
        # dismissal rule makes it quit — which is how this story used to exit,
        # before `q` was ever read. One Esc closes the dialog; wait for it.
        app.wait_until(
            lambda text: "External command" not in text,
            "external command closes before the next shortcut",
        )
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
