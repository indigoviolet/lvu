#!/usr/bin/env python3
"""Rapid draft replacement/clear while native queries publish.

The live debounced search belongs to editable views. All events only ever holds
a draft, so the race is exercised in the view that applying one creates.
"""
import os
import json
import pathlib
import sys
import tempfile
import time
from test_lvu_pty import PtyApp

with tempfile.TemporaryDirectory(prefix="lvu-search-race-") as directory:
    root = pathlib.Path(directory)
    source = root / "events.log"
    source.write_text("\n".join(json.dumps(row) for row in [
        {"message": "alpha one", "level": "ERROR", "status": "503", "field name": "/var/log"},
        {"message": "beta two", "level": "info", "status": "200", "field name": "ready"},
        {"message": "gamma three", "level": "debug", "status": "100"},
    ]) + "\n")
    app = PtyApp(pathlib.Path(sys.argv[1]).resolve(), [str(source)], width=110, height=26,
                 environment={"MISE_DATA_DIR": os.environ.get("MISE_DATA_DIR", str(pathlib.Path.home() / ".local/share/mise")),
                              "UV_CACHE_DIR": os.environ.get("UV_CACHE_DIR", str(pathlib.Path.home() / ".cache/uv")),
                              "XDG_CONFIG_HOME": str(root / "config"),
                              "XDG_DATA_HOME": str(root / "data"),
                              "XDG_CACHE_HOME": str(root / "cache"),
                              "NO_COLOR": "", "COLORTERM": "truecolor"})
    try:
        app.wait_for("gamma three")
        # All events keeps its draft to itself; applying one creates the
        # editable view the rest of this suite races against.
        app.send(b"/")
        app.wait_for("No filter every record is shown")
        app.send(b"gamma")
        time.sleep(0.5)
        assert "Applied" not in app.text(), ("All events applied a draft", app.text())
        app.send(b"\r")
        app.wait_until(lambda screen: "Applied   gamma" in screen and "alpha one" not in screen,
                       "applying on All events created an editable view", timeout=10)
        app.send(b"\x7f" * 5)
        app.wait_until(lambda screen: "1-3/3" in screen and "every record is shown" in screen,
                       "the derived view starts from an empty draft", timeout=8)
        screen_lines = app.text().splitlines()
        help_y = next(y for y, line in enumerate(screen_lines) if "Examples:" in line)
        help_x = screen_lines[help_y].index("Examples:")
        help_cell = app.screen.buffer[help_y][help_x]
        cursor = app.screen.cursor
        input_cell = app.screen.buffer[cursor.y][cursor.x]
        status_y = next(y for y, line in enumerate(screen_lines) if "No filter every record is shown" in line)
        assert not cursor.hidden and cursor.y < status_y < help_y, "input, applied state, then examples"
        assert app.text().count("Search") == 1, "dialog title must not be repeated as an input label"
        assert help_cell.fg == "default", "examples should use readable normal text in Terminal theme"
        assert input_cell.bg != help_cell.bg, "editable input needs its own background"
        assert "Enter apply now" not in app.text() and "Esc close" not in app.text()
        assert "300ms" not in app.text() and "applied:" not in app.text()
        for delay in (0.01, 0.28, 0.30, 0.32, 0.40):
            app.send(b"alpha")
            time.sleep(delay)
            app.send(b"\x7f" * 5)
            # The class S prompt is top-biased (dialog-system.md §5.1), so with a
            # three-record fixture it sits over the record rows. Assert the view
            # state instead of the occluded text: "1-3/3" is the authoritative
            # statement that every record is back in the view.
            app.wait_until(lambda screen: "1-3/3" in screen and "search:on" not in screen
                           and "every record is shown" in screen,
                           "empty draft restores all records", timeout=5)
        app.send(b"beta")
        app.wait_until(lambda screen: "beta two" in screen and "alpha one" not in screen
                       and "1-1/1" in screen,
                       "settled search filters rows", timeout=5)
        app.send(b"\x7f" * 4 + b"\x1b")
        app.wait_until(lambda screen: "alpha one" in screen and "gamma three" in screen
                       and "search:on" not in screen, "clear survives immediate editor close", timeout=5)
        app.send(b"/")
        app.wait_for("Search")
        for query, visible, absent in [
            ("level: error", "alpha one", "beta two"),
            (r'"field name": \/var/log', "alpha one", "beta two"),
            (r'"field name": /^ready$/', "beta two", "alpha one"),
            (r'\/var/log', "alpha one", "beta two"),
            ("/beta|gamma/", "beta two", "alpha one"),
            ("message: /^gamma/", "gamma three", "alpha one"),
            ("pl.col('status') == '503'", "alpha one", "beta two"),
        ]:
            app.send(b"\x1b[200~" + query.encode() + b"\x1b[201~")
            app.wait_until(lambda screen: visible in screen and absent not in screen,
                           "search form " + query, timeout=15)
            app.send(b"\x7f" * len(query))
            # As above: the top-biased prompt covers the record rows in this
            # three-record fixture, so the restored view is read from the count.
            app.wait_until(lambda screen: "1-3/3" in screen
                           and "every record is shown" in screen,
                           "clear " + query, timeout=5)
        app.send(b"alpha")
        # Wait for the message row to name the accepted filter: that is the
        # unambiguous signal that there *is* a last-accepted value to preserve.
        app.wait_until(lambda screen: "Applied   alpha" in screen
                       and "alpha one" in screen and "beta two" not in screen,
                       "last good search", timeout=5)
        # Prepend the invalid fragment (Ctrl-A, then type) instead of clearing
        # first: clearing applies
        # an empty draft, which drops the accepted filter, so the old sequence
        # never actually had a last-accepted value to preserve. It passed only
        # because the narrower pre-spec dialog left a sliver of log visible.
        app.send(b"\x01/[/")
        app.wait_until(lambda text: "Error" in text, "rejected draft", timeout=5)
        assert "alpha one" in app.text() and "beta two" not in app.text()
        wide_cursor_row = app.screen.cursor.y
        app.resize(72, 16)
        app.wait_until(lambda text: app.screen.cursor.y != wide_cursor_row
                       and "Error" in text
                       and "last accepted alpha" in text,
                       "narrow error preserves accepted filter")
        assert "Enter apply now" not in app.text() and "Esc close" not in app.text()
        assert not app.screen.cursor.hidden
        app.resize(110, 26)
        app.wait_until(lambda text: app.screen.cursor.y == wide_cursor_row and "last accepted" in text,
                       "wide editor layout restored")
        # Remove the three prepended characters, then Ctrl-K the rest of the
        # line so the draft is genuinely empty and every record returns.
        app.send(b"\x7f" * 3 + b"\x0b")
        app.wait_until(lambda screen: "1-3/3" in screen
                       and "every record is shown" in screen,
                       "clear invalid draft", timeout=5)
        app.send(b"\x1b")
        assert "base snapshot" not in app.text()
        app.send(b"t")
        app.wait_for("Time window")
        app.wait_for("Recognize timestamp")
        app.send(b"\x1bt")
        # Ask opened for a prepared task names the task and drops the kind
        # selector (dialog-system.md §12.17); the request stays editable.
        app.wait_for("Recognize timestamp")
        app.wait_for("timestamp_utc")
        assert "enrich:on" not in app.text(), "template must not apply automatically"
        app.send(b"\x1b")
        app.wait_until(lambda screen: "Request" not in screen, "timestamp assistant closed")
        app.send(b"q")
        assert app.wait_exit(timeout=8) == 0
        app.assert_restored()
    finally:
        if app.process.poll() is None:
            app.process.kill()
            app.process.wait()
        app.close()
print("Search PTY passed: rapid typing/clear across debounce, native publication, close, restoration")
