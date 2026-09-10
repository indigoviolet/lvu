#!/usr/bin/env python3
"""docs/dialog-system.md §8.11–§8.13 in a real terminal: nested JSON collapses
to summaries and opens in place with expansion remembered per path, the
Value pane describes the selected field over a bounded sample, one key
filters to a value, and the step editor's picker offers a nested path."""
import json
import pathlib
import sys
import tempfile

from test_enrichment_chain_pty import stop, open_step_editor, paste, close_editor
from test_lvu_pty import PtyApp


def records() -> str:
    lines = []
    for index in range(40):
        slow = index % 10 == 0
        # Record 5 is the near miss: a value that *contains* the slow
        # record's values without equalling them, so an exact match must
        # leave it out and a substring match would let it in.
        status = 5033 if index == 5 else (503 if slow else 200)
        tag = "slower" if index == 5 else ("slow" if slow else "fast")
        lines.append(json.dumps({
            "level": "WARN" if slow else "INFO",
            "message": f"request {index:02d} done",
            "http": {
                "status": status,
                "path": f"/v1/items/{index}",
                "tags": ["api", tag],
            },
            "service": "worker",
        }, separators=(",", ":")))
    # One record with a byte that is not UTF-8: the screen shows U+FFFD and
    # nothing is re-serialised.
    return "\n".join(lines) + "\n"


def run(binary: pathlib.Path) -> None:
    with tempfile.TemporaryDirectory(prefix="lvu-field-exploration-") as directory:
        root = pathlib.Path(directory)
        source = root / "events.log"
        source.write_bytes(records().encode() + b'{"level":"INFO","message":"bad \xff byte","service":"worker"}\n')
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
            app.wait_for("request 39 done")

            # Details: the JSON record is a tree; `http` is collapsed to its
            # summary until Enter opens it, and Left climbs back out.
            app.send(b"g")
            app.send(b"d")
            details = app.wait_for("Selected event details")
            assert "{3 keys}" in details, details
            assert "status: 503" not in details, details
            # Toggling the pane focuses it; the cursor starts on the first row.
            app.send(b"\x1b[B\x1b[B")  # cursor to `http`
            app.send(b"\r")
            app.wait_for("status: 503")
            # The pane is short here; the cursor scrolls the tree into view.
            app.send(b"\x1b[B\x1b[B\x1b[B")
            app.wait_for("tags: [2]")
            app.send(b"\r")  # open `tags`
            app.send(b"\x1b[B")  # the cursor pulls the first item into view
            app.wait_for('[0]: "api"')
            app.send(b"\x1b[D")  # Left on a leaf climbs to `tags`
            app.send(b"\x1b[D")  # Left on `tags` closes it
            app.wait_until(lambda text: "[0]:" not in text, "tags closed")
            app.send(b"d")
            app.wait_until(lambda text: "Selected event details" not in text, "details closed")

            # Fields: the same tree with the same memory — `http` is still open.
            app.send(b"i")
            fields = app.wait_for("Value · ")
            assert "▾ " in fields or "v " in fields, fields
            assert "status" in fields and "[2]" in fields, fields
            # The pane says what its figures rest on, but not which of the two
            # it will be by the time this reads the screen: over 41 records the
            # whole-view pass answers almost at once and the caption becomes
            # "all 41 records". Either is correct; a caption naming neither is
            # not. `test_whole_view_stats_pty.py` covers the transition itself.
            assert "first 2,048 records" in fields or "all 41 records" in fields, fields
            # The Value pane describes the selected top-level field over the
            # sample: a string with a handful of distinct values.
            assert "Type      string" in fields, fields
            assert (
                "of 41 sampled records" in fields or "of 41 records" in fields
            ), fields
            # Move onto http.status: an integer with a range.
            app.send(b"\x1b[B\x1b[B\x1b[B")
            status = app.wait_for("Value · http.status")
            assert "Type      integer" in status, status
            assert "200 … 5033" in status, status
            assert "Distinct  3 values" in status, status
            for label in ("[ Pin ]", "[ Filter ]", "[ Exclude ]", "[ Color ]", "[ Fold ]", "[ Correlate ]"):
                assert label in status, status
            # §8.12: a nested number compares as a number. Filtering to 503
            # keeps the four 503 records and not the 5033 one.
            app.send(b"\x1bf")
            app.wait_for("filtering to http.status = 503")
            app.send(b"\x1b")
            app.wait_until(lambda text: "Value · " not in text, "fields closed")
            app.wait_until(lambda text: "advanced:on" in text and "matched 4/41" in text,
                           "exactly the 503 records", timeout=15)
            app.send(b"[")
            app.wait_until(lambda text: "/41" in text, "back on the unfiltered view")

            # A nested string compares whole: "slow" is not "slower".
            app.send(b"g")
            app.send(b"i")
            app.wait_for("Value · ")
            app.send(b"\x1b[B\x1b[B\x1b[B\x1b[B\x1b[B")  # onto tags (http is open)
            app.wait_for("Value · http.tags")
            app.send(b"\x1b[C")  # open the array
            app.send(b"\x1b[B\x1b[B")  # onto tags[1]
            app.wait_for("Value · http.tags[1]")
            app.send(b"\x1bf")
            app.wait_for('filtering to http.tags[1] = "slow"')
            app.send(b"\x1b")
            app.wait_until(lambda text: "Value · " not in text, "fields closed")
            app.wait_until(lambda text: "advanced:on" in text and "matched 4/41" in text,
                           "exactly the slow records", timeout=15)
            app.send(b"[")
            app.wait_until(lambda text: "/41" in text, "back on the unfiltered view")

            # Exclude drops the 503s and, being null there, the plain record.
            app.send(b"g")
            app.send(b"i")
            app.wait_for("Value · ")
            app.send(b"\x1b[B\x1b[B\x1b[B")
            app.wait_for("Value · http.status")
            app.send(b"\x1bx")
            app.wait_for("excluding http.status = 503")
            app.send(b"\x1b")
            app.wait_until(lambda text: "Value · " not in text, "fields closed")
            app.wait_until(lambda text: "advanced:on" in text and "matched 36/41" in text, "the exclusion applied", timeout=15)

            # §8.9: Fold from Fields follows the state it acts on. Having
            # folded from here, the same key in the same place unfolds; the
            # button says which it will do, so the user is never left hunting
            # the Folding dialog for the way back.
            app.send(b"[")
            app.wait_until(lambda text: "/41" in text, "back on the unfiltered view")
            open_step_editor(app)
            paste(app, "level = pl.col('level')")
            app.send(b"\r")
            app.wait_until(lambda text: "Applied" in text, "level enrichment applied", timeout=20)
            close_editor(app)
            app.send(b"g")
            app.send(b"i")
            app.wait_for("Value · ")
            offered = app.text()
            assert "[ Fold ]" in offered, offered
            assert "[ Unfold ]" not in offered, offered
            app.send(b"\x1bd")
            folded = app.wait_until(lambda text: "[ Unfold ]" in text,
                                    "the button follows the fold it just made", timeout=15)
            assert "grouping runs on " in folded, folded
            app.send(b"\x1bd")
            app.wait_until(lambda text: "[ Fold ]" in text and "[ Unfold ]" not in text,
                           "the same key turns folding back off", timeout=15)
            app.wait_until(lambda text: "grouping off" in text, "and says so", timeout=10)
            app.send(b"\x1b")
            app.wait_until(lambda text: "Value · " not in text, "fields closed")

            # The record with the bad byte shows the replacement character,
            # verbatim, and is still explorable — still in the unfiltered view,
            # which the fold check above already returned to.
            app.send(b"G")
            app.send(b"i")
            lossy = app.wait_for("Value · ")
            assert "�" in lossy, lossy
            app.send(b"\x1b")
            app.wait_until(lambda text: "Value · " not in text, "fields closed")

            # The step editor's picker offers the nested path.
            app.send(b"e")
            app.wait_for("[ External command")
            app.send(b"\x1ba")
            app.wait_for("Enrichment › New step")
            app.send(b"\x00")  # Ctrl-Space
            picker = app.wait_for("Complete field")
            assert "http.status" in picker and "nested" in picker, picker
            app.send(b"\x1b")
            app.wait_until(lambda text: "Complete field" not in text, "picker closed")
            app.send(b"\x1b")
            app.wait_for("[ External command")
            app.send(b"\x1b")
            app.wait_until(lambda text: "[ External command" not in text, "enrichment closed")
            stop(app)
        finally:
            if app.process.poll() is None:
                app.send(b"\x03")
                try:
                    app.wait_exit(5)
                except Exception:
                    app.process.kill()
    print("Field exploration PTY passed: tree, memory, value pane, exclude, lossy bytes, path picker")


if __name__ == "__main__":
    run(pathlib.Path(sys.argv[1]).resolve())
