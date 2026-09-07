#!/usr/bin/env python3
"""Exercise shared line editing and visible source/enrichment controls in a real PTY."""
import pathlib
import os
import sys
import tempfile

from test_lvu_pty import PtyApp


def run(binary, arguments=None, environment=None):
    app = PtyApp(binary, ["--demo"] if arguments is None else arguments, width=100, height=28,
                 environment={"LVU_NO_DELIGHT": "1", **(environment or {})})
    try:
        app.wait_for("DEMO FIXTURE")

        app.send(b"/")
        app.wait_for("Search")
        app.send("界e\u0301tail".encode())
        app.wait_for("界étail")
        right_x = app.screen.cursor.x
        app.send(b"\x1b[D")
        app.wait_until(lambda _text: app.screen.cursor.x < right_x, "Search Left moves caret")
        app.send(b"\x1b[C")
        app.wait_until(lambda _text: app.screen.cursor.x == right_x, "Search Right restores caret")
        app.send(b"\x01q")
        app.wait_for("q界étail")
        start_x = app.screen.cursor.x
        app.send(b"\x05")
        app.wait_until(lambda _text: app.screen.cursor.x > start_x, "Ctrl-E moves to line end")
        app.send(b"\x0b")
        app.assert_remains("q界étail", "THIS STRING CANNOT APPEAR")
        app.send(b"\x1b")
        app.wait_until(lambda text: "Search ─" not in text, "Search closes")

        app.send(b"e")
        enrichment = app.wait_for("External command")
        for label in ("Steps", "Add", "Edit", "Remove", "External command"):
            assert label in enrichment, enrichment
        # Multiline editing lives in the nested step editor, not the step list.
        app.send(b"\x1ba")
        app.wait_for("Enrichment › New step")
        app.send(b"first\x1bnsecond")
        app.wait_for("second")
        lower_y = app.screen.cursor.y
        app.send(b"\x1b[A")
        app.wait_until(lambda _text: app.screen.cursor.y < lower_y, "enrichment Up moves a line")
        app.send(b"\x1b[B")
        app.wait_until(lambda _text: app.screen.cursor.y == lower_y, "enrichment Down restores line")
        end_x = app.screen.cursor.x
        app.send(b"\x01")
        app.wait_until(lambda _text: app.screen.cursor.x < end_x, "multiline Ctrl-A")
        app.send(b"q")
        app.wait_for("qsecond")
        app.send(b"\x0b")
        app.wait_until(lambda text: "second" not in text, "Ctrl-K deletes to newline")
        app.send(b"\x1b")
        app.wait_for("External command")
        app.send(b"\x1bc")
        command = app.wait_for("runs only when confirmed")
        for label in ("New line", "Save", "Review", "Remove"):
            assert label in command, command
        app.send(b"/bin/echo\tfirst\x1bnsecond")
        app.wait_for("2 line(s)")
        app.send(b"\x1b[AZ\x1b[B")
        app.assert_remains("2 line(s)", "THIS STRING CANNOT APPEAR")
        app.send(b"\x1b")
        app.wait_until(lambda text: "runs only when confirmed" not in text,
                       "command dialog closes")
        app.send(b"e")
        app.wait_for("External command")
        app.send(b"\x1b")
        app.wait_until(lambda text: "External command" not in text, "Enrichment closes")

        app.send(b"n")
        source = app.wait_for("Add source")
        # Add source has never had an Open control in any revision; the word used
        # to survive on screen as residue from an earlier frame, which the scrim
        # now repaints. Assert the controls this dialog actually has. `[ Open ]`
        # arrives when Source is adopted onto dialog-system.md §12.7.
        for label in ("Manual", "Discover", "🧠", "File", "Command"):
            assert label in source, (label, source)
        app.send(b"abc\x01q")
        app.wait_for("qabc")
        source_x = app.screen.cursor.x
        app.send(b"\x1b[D")
        app.wait_until(lambda _text: app.screen.cursor.x < source_x, "source Left moves caret")
        app.send(b"\x1b[C")
        app.send(b"\x1bc")
        app.wait_for("COMMAND (sh -c)")
        app.send(b"\x1bf")
        app.wait_for("FILE PATH")
        app.send(b"\t")
        app.assert_remains("Manual", "THIS STRING CANNOT APPEAR")
        app.send(b"\x1b[C")
        app.wait_for("Discover sources")
        app.send(b"\x1b")
        app.wait_until(lambda text: "Add source" not in text, "source dialog closes")

        app.send(b"\x10")
        app.wait_for("Command palette")
        app.send(b"abc\x01q")
        app.wait_for("qabc")
        app.send(b"\x1b")
        app.wait_until(lambda text: "Command palette" not in text, "palette closes")

        app.send(b"q")
        app.process.wait(timeout=5)
        app.drain()
        assert app.process.returncode == 0
        app.assert_restored()
    finally:
        if app.process.poll() is None:
            app.process.kill()
            app.process.wait(timeout=5)
        app.close()


if __name__ == "__main__":
    binary = pathlib.Path(sys.argv[1]).resolve()
    if "--real" in sys.argv[2:]:
        with tempfile.TemporaryDirectory(prefix="lvu-real-editor-") as directory:
            root = pathlib.Path(directory)
            source = root / "editor.log"
            source.write_text("DEMO FIXTURE editor input\n")
            run(binary, [str(source), "--capture-dir", str(root / "capture")], {
                "XDG_CONFIG_HOME": str(root / "config"),
                "XDG_DATA_HOME": str(root / "data"),
                "XDG_CACHE_HOME": str(root / "cache"),
                "MISE_DATA_DIR": os.environ.get("MISE_DATA_DIR", str(pathlib.Path.home() / ".local/share/mise")),
                "MISE_CONFIG_DIR": os.environ.get("MISE_CONFIG_DIR", str(pathlib.Path.home() / ".config/mise")),
                "MISE_CACHE_DIR": os.environ.get("MISE_CACHE_DIR", str(pathlib.Path.home() / ".cache/mise")),
                "UV_CACHE_DIR": os.environ.get("UV_CACHE_DIR", str(pathlib.Path.home() / ".cache/uv")),
            })
    else:
        run(binary)
    print("Editor controls PTY passed")
