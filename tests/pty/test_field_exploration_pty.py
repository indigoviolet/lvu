#!/usr/bin/env python3
"""docs/dialog-system.md §8.11–§8.13 in a real terminal: nested JSON collapses
to summaries and opens in place with expansion remembered per path, the
Value pane describes the selected field over a bounded sample, one key
filters to a value, and the step editor's picker offers a nested path."""
import json
import pathlib
import sys
import tempfile

from test_enrichment_chain_pty import stop
from test_lvu_pty import PtyApp


def records() -> str:
    lines = []
    for index in range(40):
        lines.append(json.dumps({
            "level": "WARN" if index % 10 == 0 else "INFO",
            "message": f"request {index:02d} done",
            "http": {
                "status": 503 if index % 10 == 0 else 200,
                "path": f"/v1/items/{index}",
                "tags": ["api", "slow" if index % 10 == 0 else "fast"],
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
            assert "first 2,048 records" in fields, fields
            # The Value pane describes the selected top-level field over the
            # sample: a string with a handful of distinct values.
            assert "Type      string" in fields, fields
            assert "of 41 sampled records" in fields, fields
            # Move onto http.status: an integer with a range.
            app.send(b"\x1b[B\x1b[B\x1b[B")
            status = app.wait_for("Value · http.status")
            assert "Type      integer" in status, status
            assert "200 … 503" in status, status
            assert "Distinct  2 values" in status, status
            # The buttons underline their mnemonics and Alt-X excludes.
            for label in ("[ Pin ]", "[ Filter ]", "[ Exclude ]", "[ Color ]", "[ Fold ]", "[ Correlate ]"):
                assert label in status, status
            app.send(b"\x1bx")
            app.wait_for("excluding http.status = 503")
            app.send(b"\x1b")
            app.wait_until(lambda text: "Value · " not in text, "fields closed")
            app.wait_until(lambda text: "advanced:on" in text and "/36" in text, "the exclusion applied", timeout=15)

            # The record with the bad byte shows the replacement character,
            # verbatim, and is still explorable — back in the unfiltered view.
            app.send(b"[")
            app.wait_until(lambda text: "/41" in text, "back on the unfiltered view")
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
