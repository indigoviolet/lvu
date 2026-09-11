#!/usr/bin/env python3
"""Stopping and restarting a source has a bare key, at 80x24.

docs/dialog-system.md §8.10. These two operations were bound to Alt-S and
Alt-R alone, and an xterm with its default `metaSendsEscape: false` sends
those as the letters `ó` and `ò` — never as a chord — so on that terminal the
only way to stop a source was the command palette. The base screen takes no
text, so every letter there is already a key: stop is `X` and restart is `R`.

`S` is not available for stop, because it opens Storage and the sidebar and
the log share one key namespace on purpose (§8.10). This drives the bare keys
from both panes to show that they mean the same thing in each, which is what
lets the palette print one chord per operation instead of one per focus. The
Alt aliases are exercised by test_source_control_pty.py, which still sends
them.
"""
import pathlib
import sys
import tempfile

from test_lvu_pty import PtyApp
from test_enrichment_chain_pty import stop


def run(binary: pathlib.Path) -> None:
    with tempfile.TemporaryDirectory(prefix="lvu-source-keys-pty-") as directory:
        root = pathlib.Path(directory)
        environment = {
            "XDG_CONFIG_HOME": str(root / "config"),
            "XDG_DATA_HOME": str(root / "data"),
            "XDG_CACHE_HOME": str(root / "cache"),
            "LVU_NO_DELIGHT": "1",
            "NO_COLOR": "",
            "COLORTERM": "truecolor",
        }
        source = root / "sample.log"
        source.write_text("first line\n")
        app = PtyApp(
            binary,
            [str(source), "--capture-dir", str(root / "capture")],
            width=80,
            height=24,
            environment=environment,
        )
        try:
            app.wait_for("first line", timeout=10.0)

            # `X` from the log pane stops the source; nothing appended while it
            # is stopped may reach the view. The 80-column status line clips
            # the per-source notice to `sample.log: capture…`, so arrival of
            # the notice plus the capture actually halting is the assertion;
            # the sidebar then publishes the terminal health (`Stopped:`)
            # instead of freezing at its last `Running:` tick.
            app.send(b"X")
            app.wait_for("sample.log: capture", timeout=8.0)
            app.wait_until(
                lambda text: "Stopped:" in text,
                "the sidebar publishes the stopped health",
                timeout=12.0,
            )
            with source.open("a") as output:
                output.write("while stopped\n")
            app.assert_remains("first line", "while stopped", duration=0.3)

            # `R` restarts it and the line that arrived meanwhile is captured
            # exactly once, from the durable cursor.
            app.send(b"R")
            app.wait_for("while stopped", timeout=8.0)
            app.wait_for("Running: 2 records", timeout=8.0)

            # The same two keys mean the same thing with the sidebar focused:
            # one namespace across both panes (§8.10). A second held line
            # proves this stop halted capture too, not just the first one.
            app.send(b"\t")
            app.wait_until(lambda text: "Sources / views" in text, "sidebar focused")
            app.send(b"X")
            app.wait_for("sample.log: capture", timeout=8.0)
            app.wait_until(
                lambda text: "Stopped:" in text,
                "the sidebar publishes the stopped health",
                timeout=12.0,
            )
            with source.open("a") as output:
                output.write("held back\n")
            app.assert_remains("while stopped", "held back", duration=0.3)
            app.send(b"R")
            app.wait_for("held back", timeout=8.0)
            app.wait_for("Running: 3 records", timeout=8.0)

            # `S` still opens Storage from the sidebar rather than stopping a
            # source: the letter was not taken away from it.
            app.send(b"S")
            storage = app.wait_for("Storage", timeout=8.0)
            assert "capture stopped" not in storage, storage
            app.send(b"\x1b")
            app.wait_until(lambda text: "Reclaimable" not in text, "Storage closed")

            # Leave the source stopped before quitting. The story is about
            # stopping, and a live capture still settling at exit spends the
            # shared eight-second shutdown budget that
            # `test_source_control_pty.py` also ends by stopping to protect.
            app.send(b"X")
            app.wait_for("sample.log: capture", timeout=8.0)
            stop(app)
            journal = next((root / "capture").rglob("*.journal")).read_bytes()
            assert journal.count(b"first line") == 1, "restart repeated a record"
            assert journal.count(b"while stopped") == 1, "restart repeated a record"
            assert journal.count(b"held back") == 1, "restart repeated a record"
        finally:
            # `stop` already closed the pty on the passing path; only a story
            # that died before it needs the kill and the close.
            if app.process.poll() is None:
                app.process.kill()
                app.process.wait()
                app.close()
    print("Source keys PTY passed: bare X stops and R restarts from both panes at 80x24")


if __name__ == "__main__":
    run(pathlib.Path(sys.argv[1]).resolve())
