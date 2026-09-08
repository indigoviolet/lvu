#!/usr/bin/env python3
"""The View summary (`V`) in a real terminal: one read-only stack of everything
applied to the current view, each row worded as its owning dialog words it,
`—` where nothing is applied, and Enter on a row opening that dialog.

The story applies a search, pins a column and turns folding on, then reads the
stack at 80x24 and again at 54x16, where the eleven rows scroll inside the
compact dialog and the selection stays on screen.
"""
import json
import os
import pathlib
import re
import sys
import tempfile

from test_enrichment_chain_pty import stop
from test_lvu_pty import PtyApp


def source_text() -> str:
    lines = [
        json.dumps({"level": "INFO", "module": "auth", "msg": f"login ok {index:02d}"})
        for index in range(12)
    ]
    lines += [
        json.dumps({"level": "WARN", "module": "net", "msg": "login retry after timeout"})
        for _ in range(8)
    ]
    return "\n".join(lines) + "\n"


def open_palette(app, query, expected):
    app.send(b"\x10")
    app.wait_for("Command palette")
    app.send(query.encode())
    app.wait_for(expected)
    app.send(b"\r")
    app.wait_until(lambda text: "Command palette" not in text, "palette dismissed")


def summary_row(text: str, label: str) -> str:
    """The `Label · value` cells of one row, up to the dialog's right border.

    The dialog does not cover the whole screen, so a screen line carries log
    text on both sides of it; the row is what sits between the `›` gutter and
    the next border glyph.
    """
    for line in text.splitlines():
        match = re.search(rf"{re.escape(label)}\s+[·.] [^│]*", line)
        if match:
            return match.group(0).rstrip()
    raise AssertionError(f"no {label!r} row\n{text}")


def run(binary):
    home = pathlib.Path.home()
    tooling = {}
    for key, xdg, fallback, suffix in [
        ("MISE_DATA_DIR", "XDG_DATA_HOME", home / ".local/share", "mise"),
        ("MISE_CONFIG_DIR", "XDG_CONFIG_HOME", home / ".config", "mise"),
        ("MISE_CACHE_DIR", "XDG_CACHE_HOME", home / ".cache", "mise"),
        ("UV_CACHE_DIR", "XDG_CACHE_HOME", home / ".cache", "uv"),
    ]:
        tooling[key] = os.environ.get(key, str(pathlib.Path(os.environ.get(xdg, fallback)) / suffix))
    with tempfile.TemporaryDirectory(prefix="lvu-view-summary-pty-") as directory:
        root = pathlib.Path(directory)
        source = root / "events.log"
        source.write_text(source_text())
        environment = {**tooling, "XDG_CONFIG_HOME": str(root / "config"),
                       "XDG_DATA_HOME": str(root / "data"), "XDG_CACHE_HOME": str(root / "cache")}
        arguments = [str(source), "--capture-dir", str(root / "capture")]

        app = PtyApp(binary, arguments, width=80, height=24, environment=environment)
        try:
            # The JSON lines run past 80 columns; the module key is on screen.
            app.wait_for('"module": "net"')

            # A canonical view with nothing applied: every operation row is a
            # dash and the view row says the definition is fixed.
            app.send(b"V")
            summary = app.wait_for("View summary · ")
            assert summary_row(summary, "View") == "View       · All events of events.log · fixed", summary
            assert summary_row(summary, "Search").endswith("—"), summary
            assert summary_row(summary, "Fold").endswith("—"), summary
            assert "no operation applied" in summary, summary
            assert "[ Open ]" in summary, summary
            for banned in ("Enter", "Esc", "↑/↓"):
                assert banned not in summary, summary
            app.send(b"\x1b")
            app.wait_until(lambda text: "View summary" not in text, "summary closed")

            # Apply three operations the ordinary way. The search forks a
            # derived view off the fixed canonical one.
            app.send(b"/")
            app.wait_for("Search")
            app.send(b"login retry")
            app.send(b"\r")
            app.wait_for("Applied   login retry", timeout=20)
            app.send(b"\x1b")
            # ESC immediately followed by a printable byte parses as Alt-<key>;
            # wait for the dialog to close before the next shortcut.
            app.wait_until(lambda text: "Examples:" not in text, "search closed")
            # The status line runs past 80 columns; the forked view is named
            # after the literal and selected in the sidebar.
            app.wait_until(lambda text: "› login retry" in text, "the derived view is selected", timeout=20)

            app.send(b"i")
            app.wait_for("Value · ")
            app.send(b"\x1b[B")  # module: rows follow the record's key order
            app.wait_for("Value · module")
            app.send(b"\r")
            app.send(b"\x1b")
            # The pinned field becomes a log column: it appears in the header
            # row between the built-in level column and the event text.
            app.wait_until(lambda text: "Value · " not in text and "level  module" in text,
                           "the module column is pinned")

            open_palette(app, "fold repeated", "Fold repeated events")
            # Eight identical retries fold into one run, counted under it.
            app.wait_until(lambda text: "×8 events" in text, "folding is on", timeout=20)

            # --- 80x24: the stack reads back what was applied ----------------
            app.send(b"V")
            summary = app.wait_for("View summary · login retry")
            assert summary_row(summary, "View").startswith("View       · derived from events.log"), summary
            assert summary_row(summary, "Sources") == "Sources    · events.log", summary
            assert summary_row(summary, "Time").endswith("—"), summary
            assert summary_row(summary, "Search") == "Search     · login retry", summary
            # 60 columns: the row truncates with an ellipsis, never mid-glyph.
            fold = summary_row(summary, "Fold")
            assert fold.startswith("Fold       · by Message pattern · adjacent · min 3") and fold.endswith("…"), summary
            assert summary_row(summary, "Columns") == "Columns    · pinned: module", summary
            assert summary_row(summary, "Readiness") == "Readiness  · ready", summary
            assert "3 of 8 operations applied" in summary, summary
            assert "1 of 11" in summary, summary

            # Enter on the Search row opens Search with the literal applied.
            app.send(b"\x1b[B" * 4)
            app.wait_for("5 of 11")
            app.send(b"\r")
            opened = app.wait_for("Applied   login retry")
            assert "View summary" not in opened, opened
            app.send(b"\x1b")
            app.wait_until(lambda text: "Applied   login retry" not in text, "search closed")

            # --- 54x16: the list scrolls and the selection stays on screen ---
            app.resize(54, 16)
            # The header row clips at 54 columns; that clipped header is the
            # sign the app has redrawn at the new size.
            app.wait_for("level  module  e│")
            app.send(b"V")
            compact = app.wait_for("View summary")
            assert "1 of 11" in compact, compact
            assert "[ Open ]" in compact, compact
            app.send(b"\x1b[B" * 10)
            bottom = app.wait_for("11 of 11")
            assert "Readiness" in bottom and "ready" in bottom, bottom
            assert "[ Open ]" in bottom, bottom
            # Enter on Readiness of a ready view opens the view's own dialog.
            app.send(b"\r")
            app.wait_for("View · login retry")
            app.send(b"\x1b")
            app.wait_until(lambda text: "View · " not in text, "view closed")
            stop(app)
        finally:
            if app.process.poll() is None:
                app.send(b"\x03")
                try:
                    app.wait_exit(5)
                except Exception:
                    app.process.kill()
                    app.process.wait()
                app.close()
    print("View summary PTY passed: dashes before anything is applied, the applied stack at 80x24, Enter opening the owner, and a scrolling list at 54x16")


if __name__ == "__main__":
    run(pathlib.Path(sys.argv[1]).resolve())
