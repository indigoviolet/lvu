#!/usr/bin/env python3
"""The twin of `test_merged_views_pty.py`, for the bases that interleave.

That story pins the capture case: the sources are concatenated in the order
the user arranged them in the View dialog, because capture time for files read
together is an accident of ingest scheduling and the reorder control is a
deliberate choice (docs/merged-view-ordering.md I1). This one pins the other
half. Choosing a time basis is how a user asks for time order, and then the
sources interleave by it whatever order they were listed in — and the Time
dialog's order row says which of the two is in force.
"""
import pathlib
import sys
import tempfile
from test_lvu_pty import PtyApp


def click(app, text):
    app.drain()
    for y, row in enumerate(app.screen.display):
        if text in row:
            x = row.index(text)
            app.send(f"\x1b[<0;{x + 2};{y + 1}M".encode())
            app.send(f"\x1b[<0;{x + 2};{y + 1}m".encode())
            return
    raise AssertionError(f"missing clickable {text!r}\n{app.text()}")


def run(binary):
    with tempfile.TemporaryDirectory(prefix="lvu-merged-event-") as directory:
        root = pathlib.Path(directory)
        alpha, beta = root / "alpha.log", root / "beta.log"
        # Event times interleave; the files do not. Whichever way the sources
        # are listed, one order is by file and the other is by time, so the two
        # can never be confused for each other.
        alpha.write_text(
            "".join(
                '{"ts":"2026-03-04T05:06:%02dZ","svc":"alpha","n":%d}\n' % (index * 2, index)
                for index in range(3)
            )
        )
        beta.write_text(
            "".join(
                '{"ts":"2026-03-04T05:06:%02dZ","svc":"beta","n":%d}\n' % (index * 2 + 1, index)
                for index in range(3)
            )
        )
        env = {
            "LVU_NO_DELIGHT": "1",
            "XDG_CONFIG_HOME": str(root / "config"),
            "XDG_DATA_HOME": str(root / "data"),
            "XDG_CACHE_HOME": str(root / "cache"),
        }
        args = [str(alpha), str(beta), "--capture-dir", str(root / "capture")]
        app = PtyApp(binary, args, width=140, height=30, cwd=root, environment=env)
        try:
            app.wait_for('"svc":"alpha"')
            # An applied filter is what builds the membership the order lives
            # in; every record carries `ts`, so nothing is filtered away.
            app.send(b"/")
            app.wait_for("Search")
            app.send(b"ts\r")
            app.wait_for("Applied   ts")
            app.send(b"\x1b")
            app.wait_until(lambda text: " Search " not in text, "search closed")

            # Merge beta in, listed *after* alpha, so source order and time
            # order disagree in every position.
            app.send(b"v")
            app.wait_for("View · ")
            app.send(b"\x1bm")
            app.wait_for("Apply membership")
            app.send(b"\x1b[B ")
            app.send(b"\r")
            app.wait_until(
                # `beta` alone would match the sidebar's file name; the record
                # is what says the source is in the view.
                lambda text: "Apply membership" not in text
                and '"svc":"alpha"' in text
                and '"svc":"beta"' in text,
                "merged filtered rows",
            )

            # Capture: the user's source order, and the row says so.
            app.send(b"t")
            app.wait_for("Time basis", timeout=10)
            capture = app.wait_until(
                lambda text: "order: capture" in text, "the capture order row", timeout=10
            )
            assert "source order" in capture, capture
            assert "merged" not in capture.split("order: capture")[1][:40], capture

            # Choose the recognized basis and apply it. The order row describes
            # the rows on screen, so until the new basis is applied it still
            # says capture — correctly.
            app.send(b"\r")
            app.wait_until(lambda text: "Recognized" in text, "the basis dropdown", timeout=10)
            app.send(b"\x1b[B\r")
            app.wait_until(
                lambda text: "Recognized" in text and "Time basis" in text,
                "the basis field shows the choice",
                timeout=10,
            )
            click(app, "[ Apply ]")
            app.wait_until(lambda text: "Time basis" not in text, "time applied", timeout=15)

            app.send(b"t")
            interleaved = app.wait_until(
                lambda text: "order: recognized" in text,
                "the order row follows the applied basis",
                timeout=15,
            )
            assert "merged" in interleaved, interleaved
            app.send(b"\x1b")
            app.wait_until(lambda text: "Time basis" not in text, "time closed", timeout=8)

            rows = [
                line for line in app.text().splitlines() if '"svc"' in line and '"ts"' in line
            ]
            services = [
                "alpha" if '"alpha"' in line else "beta" for line in rows
            ]
            assert services[:4] == ["alpha", "beta", "alpha", "beta"], (
                "the sources interleave by event time, not by the order they were listed",
                services,
                app.text(),
            )
            app.send(b"q")
            assert app.wait_exit(timeout=15) == 0
            app.assert_restored()
        finally:
            if app.process.poll() is None:
                app.process.kill()
                app.process.wait()
            app.close()


if __name__ == "__main__":
    run(pathlib.Path(sys.argv[1]).resolve())
    print("Merged event-time PTY passed: the basis decides whether sources interleave")
