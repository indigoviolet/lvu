#!/usr/bin/env python3
"""ANSI-bearing captured logs are inert text in the log and Details panes."""

import os
import pathlib
import shutil
import sys
import tempfile

from test_lvu_pty import PtyApp


binary = pathlib.Path(sys.argv[1]).resolve()
root = pathlib.Path(tempfile.mkdtemp(prefix="lvu-ansi-presentation-"))
source = root / "ansi.log"
raw = (
    b"\x1b(B\x1b[2m\x1bPprivate\x1b\\"
    b'{"message":"info","literal":"[2m","unicode":"\xe6\x9d\xb1\xe4\xba\xace\xcc\x81"}'
    b"\x1b[0m\n"
)
source.write_bytes(raw)

app = PtyApp(
    binary,
    [str(source), "--capture-dir", str(root / "capture")],
    width=100,
    height=24,
    environment={"LVU_NO_DELIGHT": "1"},
)
try:
    # pyte composes the combining acute into the terminal cell's NFC spelling.
    clean = '{"message":"info","literal":"[2m","unicode":"東京é"}'
    app.wait_for(clean)
    log_screen = app.text()
    assert "[0m" not in log_screen and "(B" not in log_screen and "private" not in log_screen
    assert '"literal":"[2m"' in log_screen, log_screen

    app.send(b"d")
    app.wait_for("Selected event details")
    app.wait_until(lambda text: text.count(clean) >= 2, "clean raw value in Details")
    detail_screen = app.text()
    assert "[0m" not in detail_screen and "(B" not in detail_screen and "private" not in detail_screen
    assert detail_screen.count('"literal":"[2m"') >= 2, detail_screen

    app.send(b"d")
    app.send(b"q")
    assert app.wait_exit(timeout=8) == 0
    app.assert_restored()

    journals = list((root / "capture").rglob("*.journal"))
    assert journals, "capture did not produce a journal"
    assert any(raw.rstrip(b"\n") in journal.read_bytes() for journal in journals), (
        "captured journal did not retain the original ANSI bytes",
        journals,
    )
finally:
    if app.process.poll() is None:
        app.process.kill()
        app.process.wait()
    app.close()
    if os.environ.get("LVU_KEEP_PTY_EVIDENCE"):
        print(f"ANSI presentation proof retained at {root}")
    else:
        shutil.rmtree(root)

print("ANSI presentation PTY passed: clean log and Details text, literal tails and captured bytes retained")
