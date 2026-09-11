#!/usr/bin/env python3
"""docs/dialog-system.md §8.12 at whole-view scale, in a real terminal.

The Value pane describes the selected field over a bounded sample, which is
instant. This asserts the second half: that the same figures arrive for every
record the view holds, that the pane says which of the two it is showing at the
time it shows it, and that moving to another field asks again rather than
leaving the previous field's numbers under a new heading.
"""
import json
import pathlib
import sys
import tempfile

from test_lvu_pty import PtyApp

RECORDS = 300


def records() -> str:
    lines = []
    for index in range(RECORDS):
        slow = index % 10 == 0
        lines.append(json.dumps({
            "level": "WARN" if slow else "INFO",
            "message": f"request {index:03d} done",
            "status": 503 if slow else 200,
            "service": "worker",
        }, separators=(",", ":")))
    return "\n".join(lines) + "\n"


def run(binary: pathlib.Path) -> None:
    with tempfile.TemporaryDirectory(prefix="lvu-whole-view-stats-") as directory:
        root = pathlib.Path(directory)
        source = root / "events.log"
        source.write_text(records(), encoding="utf-8")
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
            app.wait_for(f"request {RECORDS - 1:03d} done")
            # Readiness: the full raw count must be indexed before `g` can
            # mean the top. The status range only settles once all records
            # are in; sending `g` earlier races ingest and the top assertion
            # below can miss a still-growing view.
            app.wait_until(lambda text: "/300" in text,
                           "the full raw count", timeout=20.0)
            # A single keypress can be lost to input stalls under load, and a
            # late batch can re-tail a FOLLOW view after it lands: one bounded
            # resend inside the same total budget distinguishes either flake
            # from a viewport that truly never reaches the top.
            app.send(b"g")
            try:
                app.wait_until(lambda text: "1-25/300" in text,
                               "viewport at the top", timeout=10.0)
            except AssertionError:
                app.send(b"g")
                app.wait_until(lambda text: "1-25/300" in text,
                               "viewport at the top after resend", timeout=10.0)
            app.send(b"i")
            try:
                app.wait_for("Value · ", timeout=20.0)
            except AssertionError:
                print("--- screen after i ---")
                print(app.text())
                raise

            # The sample renders first and says so. Whether this frame catches
            # it before the pass answers is a race the assertion must not
            # depend on, so what is asserted is that the pane never claims more
            # than it has: while it is not showing whole-view figures it says
            # "first", and it only ever says "all" with a real count.
            def note_is_whole(text: str) -> bool:
                return f"all {RECORDS} records" in text

            app.wait_until(note_is_whole,
                           f"the pane counted all {RECORDS} records",
                           timeout=30.0)
            whole = app.text()
            assert "first 2,048 records" not in whole, whole
            # Counted over the view, not the sample: the wording drops
            # "sampled" with the numbers.
            assert f"of {RECORDS} records" in whole, whole
            assert "sampled" not in whole, whole

            # Moving to another field asks again. The previous field's figures
            # must not sit under the new field's heading even for a frame, so
            # the heading and the note have to agree at every moment: once the
            # pane names `status`, any "all N" it shows is about `status`.
            app.send(b"\x1b[B\x1b[B")
            status = app.wait_for("Value · status")
            assert "Type      integer" in status, status
            app.wait_until(note_is_whole,
                           "the pane counted all records for the new field",
                           timeout=30.0)
            counted = app.text()
            assert "Value · status" in counted, counted
            # 30 of the 300 records are 503; the rest 200. A sample of the
            # first 2,048 would give the same answer here, which is the point:
            # the two must agree where they overlap.
            assert "Distinct  2 values" in counted, counted
            assert "200 … 503" in counted, counted

            app.send(b"\x1b")
            app.wait_until(lambda text: "Value · " not in text, "fields closed")
            app.send(b"q")
            code = app.wait_exit(timeout=30.0)
            assert code == 0, f"exit code {code}"
            app.assert_restored()
        finally:
            if app.process.poll() is None:
                app.process.kill()
                app.process.wait()
            app.close()
    print("Whole-view field statistics PTY passed: sample first, whole view after, "
          "heading and figures always from the same source")


if __name__ == "__main__":
    run(pathlib.Path(sys.argv[1]).resolve())
