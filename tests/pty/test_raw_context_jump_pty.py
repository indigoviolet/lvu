#!/usr/bin/env python3
"""docs/raw-context-as-jump.md in a real terminal: `o` jumps from a filtered
view to the record in its source's All events view, selected and centred, the
status line says where it came from, and `o` again returns — to the view, the
record, and the dialog it was pressed in (log pane, Fields, Bookmarks) — at
80x24 and 54x16. A source still being indexed shows `locating…` first."""
import pathlib
import sys
import tempfile
import time

from test_enrichment_chain_pty import stop
from test_lvu_pty import FILTER_TITLE, PtyApp


def workspace(root: pathlib.Path) -> dict:
    return {
        "XDG_CONFIG_HOME": str(root / "config"),
        "XDG_DATA_HOME": str(root / "data"),
        "XDG_CACHE_HOME": str(root / "cache"),
        "LVU_NO_DELIGHT": "1",
    }


def filter_to_needle(app: PtyApp) -> None:
    app.send(b"/")
    app.wait_for(FILTER_TITLE)
    time.sleep(0.3)  # fresh dialog takes focus a tick after its frame; first keystroke would be lost
    app.send(b"need")
    app.send(b"\r"); app.wait_for("Applied   need", timeout=15)
    app.send(b"\x1b"); app.wait_until(lambda t: " Search " not in t, "search closed")
    # The filter forked All events into its own view; wait until that view
    # is the selected one before jumping out of it.
    app.wait_until(lambda t: "› need" in t and "need10" in t, "the filtered view is active", timeout=15)
    assert "hide09" not in app.text()


# Six-character records, so the 54x16 log pane (nine cells of event text
# beside the timestamp) still shows which rows are on screen.
def jumped(text: str) -> bool:
    return "hide09" in text and "need10" in text and "hide11" in text


def returned(text: str) -> bool:
    return "need10" in text and "hide09" not in text and "raw of" not in text


def round_trip(binary: pathlib.Path, root: pathlib.Path, source: pathlib.Path, width: int, height: int) -> None:
    app = PtyApp(binary, [str(source), "--capture-dir", str(root / f"capture-{width}")],
                 width=width, height=height, environment=workspace(root))
    try:
        app.wait_for("hide49", timeout=15)
        filter_to_needle(app)

        # Log pane: `o` jumps, the status line says where from, `o` returns.
        app.send(b"o")
        landed = app.wait_until(jumped, "jumped to All events with the anchor centred", timeout=10)
        assert "› All events" in landed, landed
        app.wait_until(lambda t: "o back" in t, "the anchor resolved with its return door", timeout=10)
        # The anchor is the selected row: Details shows it.
        app.send(b"d"); app.wait_for("need10"); app.send(b"d")
        app.send(b"o")
        app.wait_until(returned, "returned to the filtered view", timeout=10)

        # Fields: `o` closes it, jumps, and the return re-opens it.
        app.send(b"i"); app.wait_for("Fields · record")
        app.send(b"o")
        app.wait_until(lambda t: jumped(t) and "Fields · record" not in t, "Fields closed on the jump", timeout=10)
        app.send(b"o")
        app.wait_until(lambda t: "Fields · record" in t and "hide09" not in t, "Fields re-pushed on return", timeout=10)
        app.send(b"\x1b"); app.wait_until(lambda t: "Fields · record" not in t, "Fields closed")

        # Bookmarks: the button closes it, jumps, and the return re-opens it.
        app.send(b"b")
        app.send(b"B"); app.wait_for("Bookmarks · ")
        app.send(b"\t"); app.wait_for("Inspect context")
        app.send(b"\r")
        app.wait_until(lambda t: jumped(t) and "Bookmarks · " not in t, "Bookmarks closed on the jump", timeout=10)
        app.send(b"o")
        app.wait_until(lambda t: "Bookmarks · " in t and "hide09" not in t, "Bookmarks re-pushed on return", timeout=10)
        app.send(b"\x1b"); app.wait_until(lambda t: "Bookmarks · " not in t, "Bookmarks closed")

        # On the raw stream itself `o` has nowhere to go and says so.
        app.send(b"]"); app.wait_until(lambda t: "hide09" in t, "switched to All events")
        app.send(b"o")
        raw_notice = (app.wait_for("this is the raw st") if width >= 80 else
                      app.wait_until(lambda t: "› All events" in t and "need10" in t,
                                     "raw source and selection remain visible"))
        assert "› All events" in raw_notice and "need10" in raw_notice, raw_notice
        stop(app)
    finally:
        if app.process.poll() is None:
            app.send(b"\x03")
            try:
                app.wait_exit(5)
            except Exception:
                app.process.kill(); app.process.wait()
            app.close()


def readiness(binary: pathlib.Path, root: pathlib.Path) -> None:
    # A source big enough that All events is still indexing when the jump
    # lands: the status line says `locating…` until the record resolves.
    big = root / "big.log"
    with big.open("w") as out:
        for n in range(400_000):
            out.write(f"{'needle' if n == 399_990 else 'hidden'} big-{n:06}\n")
    app = PtyApp(binary, [str(big), "--capture-dir", str(root / "capture-big")],
                 width=80, height=24, environment=workspace(root))
    try:
        app.wait_for("big-", timeout=30)
        app.send(b"/")
        app.wait_for(FILTER_TITLE)
        time.sleep(0.3)  # fresh dialog takes focus a tick after its frame; first keystroke would be lost
        app.send(b"needle")
        app.send(b"\r")
        # Accepted status first: the single matching row paints beneath
        # the open dialog, so the row itself is only asserted after the
        # dialog closes (it keeps arriving live while indexing finishes).
        app.wait_until(lambda t: "Applied" in t and "› needle" in t,
                       "needle applied and forked", timeout=15)
        app.send(b"\x1b"); app.wait_until(lambda t: " Search " not in t, "search closed")
        app.wait_until(lambda t: "needle big-399990" in t, "the needle matched", timeout=90)
        app.wait_until(lambda t: "› needle" in t, "the filtered view is active", timeout=30)
        app.send(b"o")
        seen = app.wait_until(lambda t: "locating…" in t or "o back" in t, "jump status", timeout=30)
        app.wait_until(lambda t: "o back" in t and "hidden big-399989" in t, "the record resolved and was centred", timeout=120)
        print("readiness:", "locating… was shown" if "locating…" in seen else "resolved within one frame")
        app.send(b"o")
        app.wait_until(lambda t: "needle big-399990" in t and "raw of" not in t, "returned", timeout=30)
        stop(app)
    finally:
        if app.process.poll() is None:
            app.send(b"\x03")
            try:
                app.wait_exit(5)
            except Exception:
                app.process.kill(); app.process.wait()
            app.close()


def run(binary: pathlib.Path) -> None:
    with tempfile.TemporaryDirectory(prefix="lvu-raw-context-jump-") as directory:
        root = pathlib.Path(directory)
        source = root / "events.log"
        source.write_text("".join(f"{'need' if n == 10 else 'hide'}{n:02}\n" for n in range(50)))
        round_trip(binary, root, source, 80, 24)
        round_trip(binary, root, source, 54, 16)
        readiness(binary, root)
    print("Raw context jump PTY passed: log pane, Fields and Bookmarks entry points with return at 80x24 and 54x16, readiness while indexing")


if __name__ == "__main__":
    run(pathlib.Path(sys.argv[1]).resolve())
