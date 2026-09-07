#!/usr/bin/env python3
"""docs/dialog-system.md §8.10 in a real terminal: a button underlines its
Alt-letter and nothing prints the chord, the base screen shows its two doors
and no footers, and the palette prints the chord that works."""
import pathlib
import sys
import tempfile

from test_enrichment_chain_pty import stop
from test_lvu_pty import PtyApp
from test_shared_palette_colors_pty import find_text


def row_shows_chord(text: str, name: str, chord: str) -> bool:
    """The selected palette row for `name` prints `chord` in its shortcut column."""
    for line in text.splitlines():
        if f"› {name}" in line:
            rest = line.split(name, 1)[1].split("│")[0].split()
            return bool(rest) and rest[0] == chord
    return False


def underlined_letters(app: PtyApp, label: str) -> str:
    column, row = find_text(app, label)
    cells = [app.screen.buffer[row][column + offset] for offset in range(len(label))]
    return "".join(cell.data for cell in cells if cell.underscore)


def run(binary: pathlib.Path) -> None:
    with tempfile.TemporaryDirectory(prefix="lvu-discoverability-") as directory:
        root = pathlib.Path(directory)
        source = root / "events.log"
        source.write_text("".join(f"event {index:02d} ready\n" for index in range(24)))
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
            width=100,
            height=30,
            environment=env,
        )
        try:
            base = app.wait_for("event 23 ready")
            # The base screen: two doors, no footers.
            assert "? help · Ctrl-P commands" in base, base
            assert "↑/↓" not in base, base
            app.send(b"d")
            details = app.wait_for("Selected event details")
            assert "↑/↓" not in details and "scroll" not in details, details
            app.send(b"d")
            app.wait_until(lambda text: "Selected event details" not in text, "details closed")

            # Mnemonics: the letter is underlined, the chord is not printed.
            app.send(b"e")
            enrichment = app.wait_for("[ External command")
            assert "Alt-" not in enrichment, enrichment
            assert underlined_letters(app, "[ Add ]") == "A", app.text()
            assert underlined_letters(app, "[ Edit ]") == "E", app.text()
            assert underlined_letters(app, "[ Remove ]") == "R", app.text()
            assert underlined_letters(app, "[ External command") == "c", app.text()
            app.send(b"\x1b")
            app.wait_until(lambda text: "[ External command" not in text, "enrichment closed")

            # The mnemonic presses the button: Alt-C clones in View.
            app.send(b"v")
            app.wait_for("[ Apply ]")
            assert underlined_letters(app, "[ Clone ]") == "C", app.text()
            app.send(b"\x1bb")
            app.wait_for("New view")
            app.send(b"\x1bc")
            app.wait_for("Copy of")
            app.send(b"\x1b")
            app.wait_until(lambda text: "[ New blank ]" not in text, "view closed")

            # The palette prints the chord that works, in either base pane.
            app.send(b"\x10")
            app.wait_for("Command palette")
            app.send(b"Settings")
            settings_row = app.wait_until(
                lambda text: row_shows_chord(text, "Settings", ","),
                "Settings row shows its comma chord",
            )
            assert "Application" in settings_row, settings_row
            app.send(b"\x1b")
            app.wait_until(lambda text: "Command palette" not in text, "palette closed")
            app.send(b"\t")  # focus the sidebar
            app.send(b"\x10")
            app.wait_for("Command palette")
            app.send(b"Add source")
            app.wait_until(
                lambda text: row_shows_chord(text, "Add source", "n"),
                "Add source shows `n` from the sidebar focus too",
            )
            app.send(b"\x1b")
            app.wait_until(lambda text: "Command palette" not in text, "palette closed")
            app.send(b"/")
            app.wait_for("Type to filter")
            app.send(b"\x1b")
            app.wait_until(lambda text: "Type to filter" not in text, "search closed")

            # Help: conventions once, no dialog internals.
            app.send(b"?")
            help_text = app.wait_for("CONVENTIONS")
            assert "Alt + letter" in help_text, help_text
            for retired in ("Alt-C in Enrichment", "Ctrl-P Fold", "Space pins", "Ctrl-D"):
                assert retired not in help_text, help_text
            app.send(b"\x1b")
            app.wait_until(lambda text: "CONVENTIONS" not in text, "help closed")
            stop(app)
        finally:
            if app.process.poll() is None:
                app.send(b"\x03")
                try:
                    app.wait_exit(5)
                except Exception:
                    app.process.kill()
    print("Discoverability PTY passed: mnemonics, doors, no footers, honest palette chords")


if __name__ == "__main__":
    run(pathlib.Path(sys.argv[1]).resolve())
