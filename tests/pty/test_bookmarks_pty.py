#!/usr/bin/env python3
"""Bookmarks/notes survive restart and inspect filtered-out records."""
import pathlib
import sys
import tempfile
from test_lvu_pty import PtyApp
from test_enrichment_chain_pty import stop, paste


def run(binary):
    with tempfile.TemporaryDirectory(prefix="lvu-bookmarks-pty-") as directory:
        root = pathlib.Path(directory)
        source = root / "events.log"
        source.write_text("first event\nsecond event\n")
        env = {"XDG_CONFIG_HOME": str(root / "config"), "XDG_DATA_HOME": str(root / "data"), "XDG_CACHE_HOME": str(root / "cache")}
        for attempt in (0, 1, 2):
            app = PtyApp(binary, [str(source), "--capture-dir", str(root / "capture")], width=110, height=24, environment=env)
            try:
                app.wait_for("second event")
                if attempt == 0:
                    app.send(b"g"); app.send(b"b"); app.wait_for("bookmarked")
                    app.send(b"B"); app.wait_for("1 of 128"); app.wait_for("#0")
                    app.send(b"\x1be"); app.wait_until(lambda t: "Note for #0" in t and "1024 bytes" in t, "note editor")
                    paste(app, "Café failure to inspect"); app.send(b"\r")
                    app.wait_for("note updated")
                    app.send(b"\x1b"); app.wait_until(lambda t: "Bookmarks · " not in t, "bookmarks closed")
                    app.send(b"/"); app.wait_for("Search"); app.send(b"second"); app.wait_for("Applied   second")
                    app.send(b"\x1b"); app.wait_until(lambda t: "Bookmarks / notes" not in t, "bookmarks closed")
                    # All events holds a draft until it is applied, and
                    # applying it creates the view the filter belongs to.
                    app.send(b"/"); app.wait_for("Search"); app.send(b"second"); app.send(b"\r")
                    app.wait_for("Applied   second")
                    app.send(b"\x1b"); app.wait_until(lambda t: " Search " not in t, "search closed")
                if attempt < 2:
                    # The bookmark belongs to the source, so it is listed in
                    # the filtered view as well as in All events.
                    app.send(b"B"); app.wait_for("Café failure to inspect")
                    app.send(b"\r"); app.wait_for("Raw context"); app.wait_for("first event")
                    app.send(b"\x1b"); app.wait_for("Bookmarks · ")
                    # Enter on the list jumps to the record; raw context is its
                    # own control.
                    app.send(b"\t"); app.wait_for("Raw context")
                    app.send(b"\r"); app.wait_for("first event")
                    app.send(b"\x1b"); app.wait_for("Bookmarks / notes")
                    if attempt == 1:
                        app.send(b"\x1bd"); app.wait_for("0 of 128")
                    app.send(b"\x1b"); app.wait_until(lambda t: "Bookmarks · " not in t, "bookmarks closed after context")
                        app.send(b"\x1bd"); app.wait_for("0 / 128 bookmarks")
                        app.send(b"\x1b"); app.wait_until(lambda t: "Bookmarks / notes" not in t, "bookmarks closed after delete")
                        app.send(b"B"); app.wait_for("0 / 128 bookmarks")
                    app.send(b"\x1b"); app.wait_until(lambda t: "Bookmarks / notes" not in t, "bookmarks closed after context")
                    if attempt == 1:
                        # A restart opens on the source's unfiltered view; the
                        # filter lives in the view applying it created.
                        assert "first event" in app.text(), app.text()
                        app.send(b"]")
                        app.wait_until(lambda t: "› second" in t,
                                       "switched to the view the filter created", timeout=8)
                    app.wait_for("second event")
                    assert "first event" not in app.text(), app.text()
                else:
                    app.send(b"B"); app.wait_for("0 of 128")
                    app.send(b"\x1b"); app.wait_until(lambda t: "Bookmarks · " not in t, "empty bookmarks closed")
                stop(app)
            finally:
                if app.process.poll() is None:
                    app.send(b"\x03")
                    try: app.wait_exit(5)
                    except Exception: app.process.kill(); app.process.wait()
                    app.close()
    print("Bookmarks PTY passed: note editing, restart, hidden-record context, filter preservation, removal persistence, restoration")


if __name__ == "__main__": run(pathlib.Path(sys.argv[1]).resolve())
