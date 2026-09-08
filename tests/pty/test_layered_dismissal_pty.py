#!/usr/bin/env python3
"""Exercise q ownership across editable, selection, modal, and workspace layers."""
import pathlib
import sys
import tempfile

from test_lvu_pty import PtyApp


def drag_select(app, start, end):
    x, y = start
    ex, ey = end
    app.send(f"\x1b[<0;{x + 1};{y + 1}M".encode())
    app.send(f"\x1b[<32;{ex + 1};{ey + 1}M".encode())
    app.send(f"\x1b[<0;{ex + 1};{ey + 1}m".encode())


root = pathlib.Path(tempfile.mkdtemp(prefix="lvu-layered-dismissal-proof-"))
real = "--real" in sys.argv[2:]
source = root / "events.log"
source.write_text('{"service":"api","message":"dismissal fixture"}\n')
arguments = [str(source), "--capture-dir", str(root / "capture")] if real else ["--demo"]
app = PtyApp(pathlib.Path(sys.argv[1]).resolve(), arguments, width=100, height=28,
             environment={"NO_COLOR": "", "COLORTERM": "truecolor",
                          "XDG_CONFIG_HOME": str(root / "config"),
                          "XDG_DATA_HOME": str(root / "data"),
                          "XDG_CACHE_HOME": str(root / "cache")})
try:
    if real:
        app.wait_for("dismissal fixture")
    else:
        app.wait_for("PRESS ANY KEY")
        app.send(b" ")
        app.wait_for("DEMO FIXTURE")

    app.send(b"/")
    app.wait_for("┌ Search ")
    app.send(b"q")
    app.wait_for("q")
    assert app.process.poll() is None, "q in an editor must remain literal"
    # Do not let the debounce timing of this literal-input check filter out the
    # fixture before the subsequent Fields/Details checks.
    app.send(b"\x01\x0b")
    app.send(b"\x1b")
    app.wait_until(lambda text: "┌ Search " not in text, "editor dismissed")

    app.send(b"i")
    app.wait_for("Fields · record")
    field_y, field_row = next(
        # §12.11 splits the field into a name column and a value column.
        (y, row) for y, row in enumerate(app.screen.display) if "service" in row and "api" in row
    )
    field_x = field_row.index("service")
    drag_select(app, (field_x, field_y), (field_x + 5, field_y))

    app.send(b"q")
    app.assert_remains("Fields · record", "layered-dismissal-impossible-marker")
    app.send(b"q")
    app.wait_until(lambda text: "Fields · record" not in text, "field picker dismissed")
    assert app.process.poll() is None, "modal dismissal must not quit"

    for dismiss in (b"q", b"\x1b"):
        app.send(b"p")
        app.wait_for("Advanced filter")
        app.send(b"\t")
        app.wait_for("Complete field")
        app.send(dismiss)
        app.wait_until(lambda text: "Complete field" not in text, "completion dismissed")
        app.wait_for("Advanced filter")
        app.send(b"\x1b")
        app.wait_until(lambda text: "Advanced filter" not in text, "advanced editor dismissed")

        app.send(b"t")
        app.wait_for("Time window")
        app.send(b"\r")
        app.wait_for("Recognized")
        app.send(dismiss)
        app.wait_until(lambda text: "Recognized" not in text, "time dropdown dismissed")
        app.wait_for("Time window")
        app.send(dismiss)
        app.wait_until(lambda text: "Time window" not in text, "time dialog dismissed")

        app.send(b"i")
        app.wait_for("Fields · record")
        # `o` is a jump (raw-context-as-jump.md); the demo fixture's view
        # has no All events view to jump to, so the shell says so and only
        # re-pushes Fields, which the dismissal then closes.
        app.send(b"o")
        app.wait_for("no All events view")
        app.wait_for("Fields · record")
        app.send(dismiss)
        app.wait_until(lambda text: "Fields · record" not in text, "fields dismissed")

        app.send(b"d")
        app.wait_for("Selected event details")
        app.send(dismiss)
        app.wait_until(lambda text: "Selected event details" not in text, "Details dismissed")
        assert app.process.poll() is None, "Details dismissal must leave workspace running"

    app.send(b"\x1b")
    assert app.wait_exit(timeout=8) == 0, "workspace Escape must quit"
    app.assert_restored()
finally:
    (root / "terminal.ansi").write_bytes(app.transcript)
    (root / "screen.txt").write_text(app.text())
    if app.process.poll() is None:
        app.process.kill()
        app.process.wait()
    app.close()

print(f"Layered dismissal PTY passed: editor, selection, completion, dropdown, Context, Details, workspace; proof={root}")
