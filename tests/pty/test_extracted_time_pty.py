#!/usr/bin/env python3
"""Extracted UTC time drives actual view membership, anchors and restart state."""
import os
import pathlib
import sys
import tempfile

from test_lvu_pty import PtyApp
from test_enrichment_chain_pty import paste, close_editor, stop


def edit_segment(app, label, value):
    app.send(b"\x7f" * 32)
    paste(app, value)
    app.wait_until(lambda text: any(value in line for line in text.splitlines()),
                   "segmented " + label)
    assert not app.screen.cursor.hidden, "focused segment cursor must remain visible"
    app.send(b"\t")


def edit_custom_zone(app, label, value):
    app.send(b"\r" + b"\x1b[B" * 16 + b"\r")
    edit_segment(app, label, value)
    app.send(b"\t")


def run(binary):
    with tempfile.TemporaryDirectory(prefix="lvu-extracted-time-") as directory:
        root = pathlib.Path(directory)
        source = root / "events.log"
        source.write_text(
            "stamp<2026-09-05T12:30:46Z> boundary-row\n"
            "stamp<bad> malformed-row\n"
            "timestamp=2026-09-05T12:30:45Z missing-derived-row\n"
            "stamp<2026-09-05T12:30:45Z> timestamp=2020-01-01T00:00:00Z inside-row\n"
        )
        environment = {
            "MISE_DATA_DIR": os.environ.get("MISE_DATA_DIR", str(pathlib.Path.home() / ".local/share/mise")),
            "UV_CACHE_DIR": os.environ.get("UV_CACHE_DIR", str(pathlib.Path.home() / ".cache/uv")),
            "NO_COLOR": "", "COLORTERM": "truecolor",
            "XDG_CONFIG_HOME": str(root / "config"),
            "XDG_DATA_HOME": str(root / "data"),
            "XDG_CACHE_HOME": str(root / "cache"),
        }
        arguments = [str(source)]
        for restart in (False, True):
            app = PtyApp(binary, arguments, width=130, height=30, environment=environment)
            try:
                app.wait_for("inside-row")
                if not restart:
                    app.send(b"e")
                    app.wait_for("Enrichment")
                    # The step draft lives in the nested step editor since the
                    # two-layer rework; the list layer has no editable field.
                    app.send(b"\x1ba")
                    app.wait_for("Enrichment › New step")
                    paste(app, r"/stamp<(?P<timestamp_utc>[^>]+)>/")
                    app.send(b"\r")
                    app.wait_for("enrich:on")
                    close_editor(app)
                    app.send(b"t")
                    app.wait_for("Time window")
                    # Choose the basis and absolute window through the visible dropdowns.
                    app.send(b"\r\x1b[B\x1b[B\r")
                    app.wait_for("Extracted")
                    # Tab past Window, then past the Gap-jump threshold, to
                    # reach the Start date segment.
                    app.send(b"\t\r\x1b[B\r\t\t")
                    app.wait_for("Absolute")
                    edit_segment(app, "Start date", "2026-09-05")
                    edit_segment(app, "Start time", "14:30:45.000000000")
                    edit_custom_zone(app, "Start timezone", "+02:00")
                    edit_segment(app, "End date", "2026-09-05")
                    edit_segment(app, "End time", "14:30:46.000000000")
                    edit_custom_zone(app, "End timezone", "+02:00")
                    app.send(b"\x1b[Z" * 2)
                    app.resize(46, 12)
                    app.wait_until(lambda text: "+02:00" in text
                                   and not app.screen.cursor.hidden and app.screen.cursor.x < 46,
                                   "narrow segmented input remains visible")
                    previous_x = app.screen.cursor.x
                    app.send(b"\x1b[D")
                    app.wait_until(lambda text: app.screen.cursor.x == previous_x - 1,
                                   "left arrow moves the segmented caret")
                    app.send(b"\x1b[C\t\t")
                    app.resize(130, 30)
                    # §7.4: the boxed `Applied:` status became the message row.
                    app.wait_until(lambda text: "+02:00" in text
                                   and "Applied" in text and "Recognize timestamp" in text,
                                   "wide Time form restored")
                    assert "Enter" not in app.text() and "Tab" not in app.text() and "Esc" not in app.text()
                    assert "extracted-time:absolute" not in app.text(), "editing must not apply"
                    app.send(b"\r")
                    app.wait_for("extracted-time:absolute")
                    app.wait_until(lambda text: "Time window" not in text, "explicit Apply closes form")
                app.wait_until(lambda text: "inside-row" in text and "boundary-row" not in text
                               and "malformed-row" not in text and "missing-derived-row" not in text,
                               "extracted membership restored" if restart else "extracted membership applied")
                if not restart:
                    with source.open("a") as output:
                        output.write("stamp<2026-09-05T12:30:45.500000Z> late-row\n")
                app.wait_for("late-row")
                app.send(b"t")
                app.wait_for("Extracted")
                app.send(b"\t\r")
                # The accepted Absolute choice is index 1 on the first run; the
                # unsubmitted Around selection is preserved on restart. The two
                # ± widths are the last two rows whatever else the list offers,
                # so walking up reaches the ± 30s one without counting the
                # data-relative rows in between.
                if not restart:
                    app.send(b"\x1b[A" * 3)
                app.send(b"\r")
                app.wait_for("around selected")
                app.wait_for("12:30:15.500000000")
                app.send(b"\x1b")
                app.wait_until(lambda text: "Time window" not in text, "anchor dialog closed")
                if restart:
                    app.send(b"t")
                    app.wait_for("Time window")
                    # Basis, Window, Gap, the four date/clock segments, two zone
                    # menus, Apply, then Clear.
                    app.send(b"\t" * 10 + b"\r")
                    app.wait_until(lambda text: "extracted-time:absolute" not in text,
                                   "time cleared")
                    app.wait_until(lambda text: "Time window" not in text, "Clear closes form")
                    app.wait_for("malformed-row")
                    app.wait_for("missing-derived-row")
                app.wait_until(lambda text: "Time window" not in text, "time dialog closed")
                stop(app)
            finally:
                if app.process.poll() is None:
                    app.process.kill()
                    app.process.wait()
                    app.close()
    print("Extracted time PTY passed: explicit basis, exact membership, arrivals, anchor, restart, clear, terminal restoration")


if __name__ == "__main__":
    run(pathlib.Path(sys.argv[1]).resolve())
