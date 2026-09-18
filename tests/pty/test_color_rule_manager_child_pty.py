#!/usr/bin/env python3
"""Colour rules keeps object management separate from parameter editing.

This is the real-terminal proof for the semantic flow:
view/rule object -> explicit Add/Edit -> child parameters -> Save -> Apply.
It also proves Escape discards local child edits and restores the same selected
manager row, while the manager never exposes parameter controls itself.
"""

import pathlib
import sys
import tempfile

from test_color_rules_pty import settings_at
from test_lvu_pty import PtyApp


def assert_manager(text: str) -> None:
    assert "View ·" in text and "› Colour rules" in text, text
    assert "Predicate" not in text, text
    assert "Column" not in text, text
    assert "Value" not in text, text


def selected_second(text: str) -> bool:
    return any("› 2" in line and "second" in line for line in text.splitlines())


def run(binary: pathlib.Path) -> None:
    with tempfile.TemporaryDirectory(prefix="lvu-color-rule-manager-child-") as directory:
        root = pathlib.Path(directory)
        source = root / "events.log"
        source.write_text("first event\nsecond event\n")
        app = PtyApp(
            binary,
            [str(source), "--capture-dir", str(root / "capture")],
            width=90,
            height=24,
            environment=settings_at(root),
        )
        try:
            app.wait_for("second event")
            app.send(b"c")
            manager = app.wait_for("Colour rules")
            assert_manager(manager)
            assert "[ Add ]" in manager, manager

            # Add chooses an operation in the manager, then opens parameters in
            # a distinct child. Escape discards the child-local value.
            app.send(b"\x1ba")
            child = app.wait_for("New colour rule")
            assert "Predicate" in child and "Colour" in child, child
            app.send(b"discarded")
            app.wait_for("discarded")
            app.send(b"\x1b")
            manager = app.wait_until(
                lambda text: "New colour rule" not in text and "no rules yet" in text,
                "Escape returns to the empty manager",
            )
            assert_manager(manager)
            assert "discarded" not in manager, manager

            # Save adds the object to the manager; it does not apply it yet.
            app.send(b"\x1ba")
            app.wait_for("New colour rule")
            app.send(b"first")
            app.send(b"\x1bs")
            first = app.wait_until(
                lambda text: "New colour rule" not in text
                and "first" in text
                and "edited · Apply" in text,
                "saved first rule returns to manager",
            )
            assert_manager(first)

            app.send(b"\x1ba")
            app.wait_for("New colour rule")
            app.send(b"second")
            app.send(b"\x1bs")
            app.wait_until(
                lambda text: "New colour rule" not in text
                and "1 of 2" in text
                and "second" in text,
                "saved second rule returns to manager",
            )

            # Row selection is still only selection. Edit is a separate verb.
            app.send(b"\x1b[B")
            selected = app.wait_until(selected_second, "second manager row selected")
            assert_manager(selected)
            app.send(b"\x1be")
            editing = app.wait_for("Rule · 2 › Edit")
            assert "Predicate" in editing and "second" in editing, editing
            app.send(b" changed")
            app.wait_for("second changed")
            app.send(b"\x1b")
            restored = app.wait_until(
                lambda text: "Rule · 2 › Edit" not in text and selected_second(text),
                "Escape restores selected rule in manager",
            )
            assert_manager(restored)
            assert "second changed" not in restored, restored

            # Save commits parameters to the manager; Apply remains the
            # manager's explicit submit operation.
            app.send(b"\x1be")
            app.wait_for("Rule · 2 › Edit")
            app.send(b" saved")
            app.send(b"\x1bs")
            saved = app.wait_until(
                lambda text: "Rule · 2 › Edit" not in text
                and selected_second(text)
                and "second saved" in text,
                "saved edit returns to the same manager object",
            )
            assert_manager(saved)
            app.send(b"\x1bp")
            app.wait_until(
                lambda text: "2 rules painting this view" in text,
                "manager Apply submits both saved rules",
                timeout=15,
            )

            # Compact geometry keeps the object-first manager, and opening
            # Edit still replaces parameters with the child rather than
            # fusing them into the list.
            app.resize(54, 16)
            compact = app.wait_until(
                lambda text: "Colour rules" in text and "second saved" in text,
                "compact manager repaint",
            )
            assert_manager(compact)
            app.send(b"\x1be")
            compact_child = app.wait_for("Rule · 2 › Edit")
            assert "Predicate" in compact_child, compact_child
            app.send(b"\x1b")
            app.wait_until(
                lambda text: "Rule · 2 › Edit" not in text and "Colour rules" in text,
                "compact child returns to manager",
            )
            app.send(b"\x1b")
            app.wait_until(lambda text: "Colour rules" not in text, "manager closes")
            app.send(b"q")
            assert app.wait_exit(timeout=12) == 0
            app.assert_restored()
        finally:
            if app.process.poll() is None:
                app.send(b"\x03")
                try:
                    app.wait_exit(5)
                except Exception:
                    app.process.kill()
                    app.process.wait()
            app.close()

    print("Colour rule manager/child PTY passed: object, operation, parameters, Escape, Save, Apply")


if __name__ == "__main__":
    run(pathlib.Path(sys.argv[1]).resolve())
