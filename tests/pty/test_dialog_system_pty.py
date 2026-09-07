#!/usr/bin/env python3
"""The dialog system in a real terminal: scrim, input tone, content-driven
height and hitboxes that match what is drawn (docs/dialog-system.md)."""
import pathlib
import sys
import tempfile

from test_enrichment_chain_pty import stop
from test_lvu_pty import PtyApp
from test_shared_palette_colors_pty import cell_colors, click, contrast, find_text

# love-dark, after the §6.1 role change.
DIALOG_BG = "261b24"
INPUT_BG = "46303f"
MUTED = "ae919a"


def dialog_bounds(app: PtyApp, title: str) -> tuple[int, int, int, int]:
    """Left, top, right and bottom of the dialog whose top border shows `title`."""
    lines = app.text().splitlines()
    top = next(row for row, line in enumerate(lines) if title in line and "┌" in line)
    left = lines[top].index("┌", max(0, lines[top].index(title) - 40))
    right = lines[top].index("┐", left)
    bottom = next(
        row
        for row in range(top + 1, len(lines))
        if len(lines[row]) > left and lines[row][left] == "└"
    )
    return left, top, right, bottom


def assert_backdrop_is_scrimmed(app: PtyApp, bounds: tuple[int, int, int, int]) -> None:
    """§6.2: nothing outside the dialog keeps an active foreground."""
    left, top, right, bottom = bounds
    coloured = []
    for row, line in enumerate(app.text().splitlines()):
        for column, character in enumerate(line):
            if left <= column <= right and top <= row <= bottom:
                continue
            if character == " ":
                continue
            cell = app.screen.buffer[row][column]
            if cell.fg not in ("default", MUTED):
                coloured.append((column, row, character, cell.fg))
    assert not coloured, f"backdrop keeps active colour at {coloured[:6]}\n{app.text()}"


def run(binary: pathlib.Path) -> None:
    with tempfile.TemporaryDirectory(prefix="lvu-dialog-system-") as directory:
        root = pathlib.Path(directory)
        source = root / "events.log"
        source.write_text("".join(f"event {index:02d} ready\n" for index in range(40)))
        # The theme is a saved setting, not an environment variable; the colour
        # assertions below are written against love-dark.
        settings = root / "config" / "lvu"
        settings.mkdir(parents=True)
        (settings / "settings.toml").write_text(
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
            "\n[storage]\nreserve_mib = 256\n"
            "\n[storage.retention]\n"
            "enabled = false\n"
            "maximum_total_capture_mib = 0\n"
            "maximum_age_days = 0\n"
        )
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
            width=140,
            height=40,
            environment=env,
        )
        try:
            app.wait_for("event 39 ready")

            # --- wide: a one-field prompt stays a prompt -----------------
            app.send(b"/")
            app.wait_for("Search")
            bounds = dialog_bounds(app, "Search")
            left, top, right, bottom = bounds
            width = right - left + 1
            height = bottom - top + 1
            assert width == 72, f"§5.3 class S is 72 columns at 140 wide, got {width}"
            assert height <= 12, f"§5.1 caps class S at 12 rows, got {height}"
            assert height <= 9, (
                "§5.2 height follows content: a one-field prompt must not fill "
                f"its class maximum, got {height}\n{app.text()}"
            )

            # §6.1: the field is a distinct tone, not the dialog background.
            _, field_bg = cell_colors(app, "Type to filter…")
            assert field_bg == INPUT_BG, (INPUT_BG, field_bg)
            assert contrast(field_bg, DIALOG_BG) >= 1.35, contrast(field_bg, DIALOG_BG)

            # §6.2: the workspace behind it is inactive.
            assert_backdrop_is_scrimmed(app, bounds)

            # §7.4: one message row, one vocabulary, no `Label:` stutter.
            screen = app.text()
            assert "No filter every record is shown" in screen, screen
            assert "Applied  No filter applied." not in screen, screen
            app.send(b"\x1b")
            app.wait_until(lambda text: "Type to filter…" not in text, "search closes")

            # --- wide: an inspection surface takes the room ---------------
            app.send(b"S")
            app.wait_for("Storage")
            _, storage_top, _, storage_bottom = dialog_bounds(app, "Storage")
            assert storage_bottom - storage_top + 1 > height, (
                "an inspection surface must claim more rows than a prompt"
            )
            app.send(b"\x1b")
            app.wait_until(lambda text: "Storage" not in text, "storage closes")

            # --- wide: mouse hitboxes match the drawn buttons -------------
            app.send(b"v")
            app.wait_for("[ Clone ]")
            column, row = find_text(app, "[ Clone ]")
            click(app, column + 2, row)
            app.wait_until(
                lambda text: "[ New blank ]" in text,
                "clicking the drawn button reaches the control under it",
            )
            app.send(b"\x1b")
            app.wait_until(lambda text: "[ New blank ]" not in text, "view closes")

            # --- narrow: degrade, do not clip or overprint ----------------
            app.resize(54, 16)
            app.wait_until(
                lambda text: "event" in text and "┌" in text,
                "the workspace repaints at 54x16",
            )
            app.send(b"v")
            view = app.wait_for("[ New blank ]")

            # §5.5: the sidebar is not competing with the dialog.
            assert "Sources / views" not in view, view

            # The regression this dialog is named for: at 54x16 the button row
            # used to be painted into the wrapped help text.
            lines = view.splitlines()
            button_row = next(
                row for row, line in enumerate(lines) if "[ New blank ]" in line
            )
            assert "keep the capture" not in lines[button_row], lines[button_row]
            assert "renaming" not in lines[button_row], lines[button_row]

            left, top, right, bottom = dialog_bounds(app, "View")
            assert right - left + 1 == 52, "§5.1 compact class M is the full frame"
            assert bottom <= 15, f"dialog escapes a 16-row terminal: {bottom}"

            # Selection and hitboxes still agree with what is drawn.
            column, row = find_text(app, "[ Apply ]")
            assert left < column < right and top < row < bottom, (
                "a control must be inside the surface that owns it"
            )
            app.send(b"\x1b")
            app.wait_until(lambda text: "[ New blank ]" not in text, "view closes")

            # --- narrow: every Settings field stays reachable -------------
            # §12.14: at 54x16 the form used to hide nine of its ten fields
            # behind [ More ]. Now the body scrolls to follow focus, so every
            # field can be reached by Tab alone.
            app.send(b",")
            settings_screen = app.wait_for("Provider / model")
            assert "Theme" in settings_screen, settings_screen
            # Provider -> Mode -> Thinking -> Theme -> the three toggles ->
            # the four cache limits: ten Tabs reaches the last field.
            app.send(b"\t" * 10)
            reached = app.wait_for("Per source")
            assert "Cache limits (MiB)" in reached, reached
            assert "[ More ]" in reached or "Provider / model" in reached, reached
            app.send(b"\x1b")
            app.wait_until(
                lambda text: "Provider / model" not in text,
                "settings closes before the quit key",
            )
            stop(app)
        finally:
            if app.process.poll() is None:
                app.send(b"\x03")
                try:
                    app.wait_exit(5)
                except Exception:
                    app.process.kill()
    print("Dialog system PTY passed: scrim, input tone, class rects, hitboxes")


if __name__ == "__main__":
    run(pathlib.Path(sys.argv[1]).resolve())
