#!/usr/bin/env python3
"""docs/dialog-system.md §8.14 in the real application: a command step sits
between expression steps, a later expression and a filter read its output
(null and matching nothing until the run, real values after), the Enrichment
list names what is unrun, and a recipe replays the command with its program,
saying so at apply time when the program is not on this machine."""

from __future__ import annotations

import json
import os
import pathlib
import re
import sys
import tempfile
import time

from test_command_enrichment_pty import paste, stop
from test_lvu_pty import PtyApp

CTRL_S = b"\x13"
CTRL_R = b"\x12"
ALT_A = b"\x1ba"
ALT_C = b"\x1bc"
ALT_E = b"\x1be"
ALT_N = b"\x1bn"
ALT_S = b"\x1bs"

NATIVE = r'/"request":"(?P<req>r-\d+)"/'
HOT = "hot = pl.col('command.tier') == 'hot'"

HELPER = r'''#!/usr/bin/env python3
import json
import sys

batch = None
for line in sys.stdin:
    item = json.loads(line)
    if item["type"] == "batch_begin":
        batch = item
    elif item["type"] == "event":
        status = item["fields"].get("status")
        hot = status == 503
        print(json.dumps({
            "type": "event",
            "session": batch["session"],
            "revision": batch["revision"],
            "event_id": item["event_id"],
            "fields": {"tier": "hot" if hot else "cold", "score": 7 if hot else 2},
        }), flush=True)
    elif item["type"] == "batch_end":
        print(json.dumps({
            "type": "batch_complete",
            "session": batch["session"],
            "revision": batch["revision"],
        }), flush=True)
'''


def flat(text: str) -> str:
    """The screen with box borders and line breaks collapsed, so a sentence
    the notes pane wrapped (where depends on the width of what sits above
    it, such as the interpreter path) reads as one line again."""
    return re.sub(r"[│\s]+", " ", text)


def close_dialogs(app: PtyApp) -> None:
    """One Escape closes the frontmost dialog; wait for the dialog frame to
    go, not for the footer, which a long status diagnostic can push off."""
    app.send(b"\x1b")
    app.wait_until(
        lambda text: "┌ Enrichment" not in text
        and "Enrichment ›" not in text
        and "Advanced filter" not in text
        and "Saved recipes" not in text,
        "dialog closed",
    )


def details(app: PtyApp, labels: tuple[str, ...]) -> str:
    if "Selected event details" not in app.text():
        app.send(b"d")
    seen = app.wait_for("Selected event details", timeout=10)
    app.send(b"\x1b[B" * 16)
    for _ in range(32):
        if all(label in seen for label in labels):
            app.send(b"d")
            app.wait_until(lambda text: "Selected event details" not in text, "details closed")
            return seen
        app.send(b"\x1b[B")
        time.sleep(0.1)
        app.drain()
        seen += "\n" + app.text()
    raise AssertionError("Details did not show " + ", ".join(labels) + ":\n" + seen)


def run(binary: pathlib.Path) -> None:
    with tempfile.TemporaryDirectory(prefix="lvu-command-chain-pty-") as temporary:
        root = pathlib.Path(temporary)
        source = root / "events.jsonl"
        helper = root / "tier.py"
        helper.write_text(HELPER)
        helper.chmod(0o700)
        rows = [
            {"request": "r-1", "status": 503},
            {"request": "r-2", "status": 200},
            {"request": "r-3", "status": 200},
        ]
        source.write_text("".join(json.dumps(row, separators=(",", ":")) + "\n" for row in rows))
        environment = {
            "MISE_DATA_DIR": os.environ.get("MISE_DATA_DIR", str(pathlib.Path.home() / ".local/share/mise")),
            "UV_CACHE_DIR": os.environ.get("UV_CACHE_DIR", str(pathlib.Path.home() / ".cache/uv")),
            "XDG_CONFIG_HOME": str(root / "config"),
            "XDG_DATA_HOME": str(root / "data"),
            "XDG_CACHE_HOME": str(root / "cache"),
            "NO_COLOR": "",
            "COLORTERM": "truecolor",
            "LVU_NO_DELIGHT": "1",
        }
        app = PtyApp(
            binary,
            [str(source), "--capture-dir", str(root / "capture")],
            width=150,
            height=40,
            environment=environment,
        )
        try:
            app.wait_for('"request":"r-3"', timeout=8)

            # Step 1: a native extraction.
            app.send(b"e")
            app.wait_for("┌ Enrichment ")
            app.send(ALT_A)
            app.wait_for("Enrichment › New step")
            paste(app, NATIVE)
            app.send(b"\r")
            app.wait_until(lambda text: "1 steps active" in text, "native step saved", timeout=15)

            # Step 2: the command, inserted after the selected step. Its name
            # is the output prefix, shown before the program.
            app.send(ALT_C)
            form = app.wait_for("Enrichment › External command")
            assert "Name command " in flat(form), form
            assert "will be inserted as step 2 of 2" in flat(form), form
            paste(app, sys.executable)
            app.send(b"\t")
            paste(app, str(helper))
            app.send(CTRL_S)
            app.wait_until(lambda text: "Saved definition" in text and "Unrun" in text,
                           "command step saved without running", timeout=12)
            app.wait_until(
                lambda text: "step 2 of 2; later steps may read command.<field>" in flat(text),
                "the saved step says where it sits and who may read it",
            )
            close_dialogs(app)

            # Step 3: an expression that reads the command's output. The chain
            # is accepted while the command is unrun; the list says which
            # step is stale and the value reads as null.
            app.send(b"e")
            listing = app.wait_for("⚙ command · ")
            assert "1 unrun: command" in listing, listing
            app.send(ALT_A)
            app.wait_for("Enrichment › New step")
            paste(app, HOT)
            app.send(b"\r")
            listing = app.wait_until(lambda text: "3 steps active" in text, "downstream step accepted", timeout=15)
            assert listing.index('/"request"') < listing.index("⚙ command") < listing.index("hot = "), listing
            close_dialogs(app)
            app.send(b"g")
            pending = details(app, ("command.status", "derived.hot"))
            assert "Pending" in pending, pending
            assert "derived.hot: null" in pending, pending

            # A filter over the command's output is valid before the run and
            # waits rather than hiding the rows the command needs; the status
            # line says so.
            app.send(b"p")
            app.wait_for("Advanced filter")
            paste(app, "pl.col('command.score') > 5")
            app.send(b"\r")
            app.wait_until(lambda text: "advanced:on" in text and "matched 3" in text
                           and "filter waits for a command step that has not run" in text,
                           "filter over an unrun command waits", timeout=15)
            close_dialogs(app)

            # Run the command from its row: Edit on the command row opens it.
            app.send(b"e")
            app.wait_for("⚙ command · ")
            app.send(b"\x1b[A")  # Up from step 3 to the command row
            app.send(ALT_E)
            app.wait_for("Enrichment › External command")
            app.send(CTRL_R)
            review = app.wait_for("Run review", timeout=12)
            assert "fixed snapshot: 3 records" in review, review
            app.send(b"\r")
            app.wait_for("Published 3 durable command results", timeout=20)
            close_dialogs(app)

            # The results flow downstream: the filter narrows to the hot row
            # and the expression reads the tier.
            app.wait_until(lambda text: "matched 1" in text and '"request":"r-1"' in text,
                           "filter re-ran over the published results", timeout=20)
            app.send(b"g")
            ready = details(app, ("command.status", "command.tier", "derived.hot"))
            assert "Ready" in ready and "command.tier: hot" in ready, ready
            assert "derived.hot: true" in ready, ready
            app.send(b"e")
            listing = app.wait_for("⚙ command · ")
            assert "results published" in listing and "unrun" not in listing, listing
            close_dialogs(app)

            # A recipe carries the command step. Applying it here keeps the
            # step (same definition, so the results stay); a recipe that names
            # a program this machine lacks applies unrun and says so.
            app.send(b"r")
            app.wait_for("Saved recipes")
            app.send(ALT_S)
            app.wait_for("Save revision")
            app.send(b"Chain\r")
            app.wait_for("1 saved recipes")
            app.send(b"\r")
            app.wait_until(lambda text: "Saved recipes" not in text, "recipe applied")
            app.wait_until(lambda text: "matched 1" in text, "the applied recipe keeps the chain", timeout=15)
            app.send(b"e")
            listing = app.wait_for("⚙ command · ")
            assert "3 steps active" in listing and "results published" in listing, listing
            # Add a command step whose program does not exist, then save and
            # apply that as a recipe.
            app.send(b"\x1b[A\x1b[A")  # select step 1
            app.send(ALT_C)
            form = app.wait_for("Enrichment › External command")
            assert "Name command2" in flat(form), form
            paste(app, "/nonexistent/lvu-enricher")
            app.send(CTRL_S)
            app.wait_until(lambda text: "Saved definition" in text, "second command saved", timeout=12)
            close_dialogs(app)
            app.send(b"r")
            app.wait_for("Saved recipes")
            app.send(ALT_S)
            app.wait_for("Save revision")
            app.send(b"Missing\r")
            app.wait_for("2 saved recipes")
            # Enter applies the selected recipe; make sure that is this one.
            def missing_selected(text: str) -> bool:
                return any("›" in line and "Missing" in line for line in text.splitlines())
            if not missing_selected(app.text()):
                app.send(b"\x1b[B")
                try:
                    app.wait_until(missing_selected, "Missing selected", timeout=3)
                except AssertionError:
                    app.send(b"\x1b[A\x1b[A")
                    app.wait_until(missing_selected, "Missing selected", timeout=3)
            app.send(b"\r")
            app.wait_until(lambda text: "Saved recipes" not in text, "second recipe applied")
            notice = app.wait_for("not on this machine", timeout=15)
            assert "command2 needs /nonexistent/lvu-enricher" in notice, notice
            assert "saved unrun" in notice, notice
            app.send(b"e")
            listing = app.wait_for("⚙ command2 · unrun · /nonexistent/lvu-enricher")
            assert "1 unrun: command2" in listing, listing
            close_dialogs(app)
            stop(app)
        finally:
            if app.process.poll() is None:
                app.process.kill()
                app.process.wait(timeout=5)
                app.close()
    print("Command chain PTY passed: interleaved step, downstream expression and filter wait then read results, list names stale steps, recipe replay and missing program")


if __name__ == "__main__":
    run(pathlib.Path(sys.argv[1]).resolve())
