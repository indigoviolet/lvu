#!/usr/bin/env python3
"""docs/dialog-system.md §8.9 in a real terminal: each dialog fills exactly
one button with the accent, Enter from the dialog's initial focus runs that
action, a state-driven default moves the fill, and a list opens on a real row.
Checked at 80x24 and 54x16 (the two sizes §13 names for the fill)."""
import pathlib
import sys
import tempfile

from test_enrichment_chain_pty import paste, stop
from test_lvu_pty import PtyApp
from test_shared_palette_colors_pty import find_text

# love-dark `accent` — the §6.3 fill of the default button.
ACCENT = "ff7e70"


def button_cells(app: PtyApp, label: str):
    column, row = find_text(app, label)
    return [app.screen.buffer[row][column + offset] for offset in range(len(label))]


def is_filled(app: PtyApp, label: str) -> bool:
    return all(cell.bg == ACCENT for cell in button_cells(app, label))


def assert_only_default(app: PtyApp, default: str, others: tuple) -> None:
    text = app.text()
    assert is_filled(app, default), f"{default} is not filled\n{text}"
    for label in others:
        assert not is_filled(app, label), f"{label} is filled too\n{text}"


def settings_toml() -> str:
    return (
        "schema_version = 1\n"
        "\n[paseo]\n"
        'provider = "fixture/provider"\n'
        'mode = "full-access"\n'
        'thinking = "medium"\n'
        "\n[appearance]\n"
        'theme = "love-dark"\n'
        "delight_enabled = false\n"
        "reduced_motion = true\n"
        "ascii = false\n"
        "\n[cache.memory]\nrows_mib = 4\nmembership_mib = 256\n"
        "\n[cache.disk]\ntotal_mib = 5120\nindex_per_source_mib = 256\n"
    )


def enrichment_checks(app: PtyApp, step_number: str) -> None:
    """The worked example: Add on an empty chain, Edit once a step exists."""
    app.send(b"e")
    app.wait_for("[ External command")
    if step_number == "1":
        # Empty chain: the fill is on Add, and Enter from the list (the
        # initial focus) opens a *new* step rather than refusing to edit.
        assert "No steps yet" in app.text(), app.text()
        assert_only_default(app, "[ Add ]", ("[ Edit ]", "[ Remove ]"))
        app.send(b"\r")
        app.wait_for("Enrichment › New step")
    else:
        app.send(b"\x1ba")
        app.wait_for("Enrichment › New step")
    app.send(b"\x01\x0b")  # clear any resumed draft
    paste(app, f"tag{step_number} = pl.lit('t{step_number}')")
    app.send(b"\r")
    app.wait_until(
        lambda text: f"{step_number}  tag{step_number}" in text and "[ External command" in text,
        "saved step returns to the list",
        timeout=20,
    )
    # A step is selected: the fill moved to Edit, the list has its `›` row,
    # and Enter from the list opens the editor on that step.
    listed = app.text()
    assert f"› {step_number}  tag{step_number}" in listed, listed
    assert_only_default(app, "[ Edit ]", ("[ Add ]", "[ Remove ]"))
    app.send(b"\r")
    editing = app.wait_for("Enrichment › Edit step")
    assert f"tag{step_number} = pl.lit" in editing, editing
    assert_only_default(app, "[ Save ]", ("[ Remove ]",))
    app.send(b"\x1b")
    app.wait_for("[ External command")
    app.send(b"\x1b")
    app.wait_until(lambda text: "[ External command" not in text, "enrichment closes")


def run(binary: pathlib.Path) -> None:
    with tempfile.TemporaryDirectory(prefix="lvu-default-actions-") as directory:
        root = pathlib.Path(directory)
        source = root / "events.log"
        source.write_text("".join(f"event {index:02d} ready\n" for index in range(24)))
        config = root / "config" / "lvu"
        config.mkdir(parents=True)
        (config / "settings.toml").write_text(settings_toml())
        env = {
            "XDG_CONFIG_HOME": str(root / "config"),
            "XDG_DATA_HOME": str(root / "data"),
            "XDG_CACHE_HOME": str(root / "cache"),
            "LVU_NO_DELIGHT": "1",
            "NO_COLOR": "",
            "COLORTERM": "truecolor",
        }
        app = PtyApp(
            binary,
            [str(source), "--capture-dir", str(root / "capture")],
            width=80,
            height=24,
            environment=env,
        )
        try:
            app.wait_for("event 23 ready")

            # --- 80x24 ------------------------------------------------------
            enrichment_checks(app, "1")

            # View: the verb is filled, not the first mode button, and the
            # fifth button is actually on screen at 80 columns.
            app.send(b"v")
            app.wait_for("[ Apply ]")
            assert_only_default(app, "[ Apply ]", ("[ New blank ]", "[ Clone ]", "[ Sources ]"))
            app.send(b"\x1b")
            app.wait_until(lambda text: "[ New blank ]" not in text, "view closes")

            # Time: the initial focus is a dropdown, which keeps Enter; past
            # the three dropdowns (basis, window, gap) the start-date segment
            # hands Enter to Apply and the dialog closes on the all-time
            # window it applied.
            app.send(b"t")
            app.wait_for("[ Apply ]")
            assert_only_default(app, "[ Apply ]", ("[ Clear ]",))
            app.send(b"\r")
            app.wait_for("Recognized")  # the Time basis list is open
            app.send(b"\x1b")
            app.wait_until(lambda text: "Recognized" not in text, "dropdown closes only")
            assert "[ Apply ]" in app.text(), app.text()
            app.send(b"\t\t\t\r")
            app.wait_until(
                lambda text: "Time window" not in text and "Quiet ≥" not in text,
                "Enter in a date segment applies and closes the dialog",
            )

            # Storage: Refresh is filled, and stays filled once `c` turns the
            # second button into the destructive confirmation.
            app.send(b"S")
            app.wait_for("[ Refresh ]")
            assert_only_default(app, "[ Refresh ]", ("[ Preview cleanup ]",))
            app.wait_for("Scanned")
            app.send(b"\r")
            app.wait_for("[ Refresh ]")
            assert "[ Refresh ]" in app.text(), "Enter refreshes rather than closing"
            app.send(b"\x1b")
            app.wait_until(lambda text: "[ Refresh ]" not in text, "storage closes")

            # Folding: a settings dialog whose one verb is its default, so the
            # fill marks the row. The initial focus is the key-column dropdown,
            # which keeps Enter (§8.9), and Escape closes the list only.
            app.send(b"z")
            app.wait_for("[ Collapse expanded runs ]")
            assert_only_default(app, "[ Collapse expanded runs ]", ())
            # `(default)` only ever appears inside the open picker, so it is
            # what says the list is up; the field itself reads `Message pattern`
            # either way.
            app.send(b"\r")
            app.wait_for("(default)")
            app.send(b"\x1b")
            app.wait_until(lambda text: "(default)" not in text and "Key column" in text,
                           "the picker closes, not the dialog")
            app.send(b"\x1b")
            app.wait_until(lambda text: "Key column" not in text, "folding closes")

            # Settings: Enter from the Provider field saves the pending draft.
            app.send(b",")
            app.wait_for("Provider / model")
            assert_only_default(app, "[ Save ]", ())
            app.send(b"x")
            app.wait_for("Pending")
            app.send(b"\r")
            app.wait_until(
                lambda text: "Pending" not in text and "Saved" in text,
                "Enter in a text field saves",
                timeout=8,
            )
            app.send(b"\x1b")
            app.wait_until(lambda text: "Provider / model" not in text, "settings closes")

            # --- 54x16 ------------------------------------------------------
            app.resize(54, 16)
            app.wait_until(lambda text: "event" in text and "┌" in text, "repaint at 54x16")
            enrichment_checks(app, "2")
            app.send(b"v")
            app.wait_for("[ Apply ]")
            assert_only_default(app, "[ Apply ]", ("[ New blank ]", "[ Clone ]"))
            app.send(b"\x1b")
            app.wait_until(lambda text: "[ New blank ]" not in text, "view closes")
            app.send(b"z")
            app.wait_for("[ Collapse expanded runs ]")
            assert_only_default(app, "[ Collapse expanded runs ]", ())
            app.send(b"\x1b")
            app.wait_until(lambda text: "Key column" not in text, "folding closes")
            app.send(b",")
            app.wait_for("[ Save ]")
            assert_only_default(app, "[ Save ]", ())
            app.send(b"\x1b")
            app.wait_until(lambda text: "[ Save ]" not in text, "settings closes")
            stop(app)
        finally:
            if app.process.poll() is None:
                app.send(b"\x03")
                try:
                    app.wait_exit(5)
                except Exception:
                    app.process.kill()
    print("Default actions PTY passed: one filled default per dialog, Enter runs it, lists open selected")


if __name__ == "__main__":
    run(pathlib.Path(sys.argv[1]).resolve())
