#!/usr/bin/env python3
"""Drive lvu through a terminal that is far away and fragmenting.

A remote terminal is not a fast one with pauses: it delivers input in pieces, so
an escape sequence arrives split across reads, and it applies backpressure, so a
frame the app is writing does not complete before the next redraw is due. Both
are places lvu could go wrong in ways a local PTY never shows — a half-read
`\\x1b[<0;10;5M` mouse report taken as an Escape and a stray `[`, or a resize
landing mid-frame and tearing the synchronized-output block.

This subclass keeps the real binary on a real PTY and puts a relay in front of
it: input is written in small chunks with a per-chunk delay, and output is
drained on a delayed schedule so the app meets a slow reader. Nothing here is
mocked; only the timing is.
"""

from __future__ import annotations

import os
import pathlib
import sys
import time

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent.parent / "pty"))

from test_lvu_pty import PtyApp, isolated_environment  # noqa: E402

# One-way delay; a keypress and the frame it causes each pay it once.
DEFAULT_LATENCY = 0.1
DEFAULT_CHUNK = 64


class SlowTerminalApp(PtyApp):
    """A `PtyApp` whose link to the app has latency and a small MTU."""

    def __init__(self, *args, latency: float = DEFAULT_LATENCY,
                 chunk: int = DEFAULT_CHUNK, **kwargs) -> None:
        self.latency = latency
        self.chunk = chunk
        self._next_read = 0.0
        super().__init__(*args, **kwargs)

    def send(self, data: bytes) -> None:
        for start in range(0, len(data), self.chunk):
            time.sleep(self.latency)
            os.write(self.master, data[start : start + self.chunk])

    def drain(self) -> None:
        # Read no more often than the link allows, so the app writes into a pipe
        # that is not being emptied and has to cope with the backpressure.
        now = time.monotonic()
        if now < self._next_read:
            return
        self._next_read = now + self.latency
        super().drain()


def story(binary: pathlib.Path, root: pathlib.Path, latency: float, chunk: int) -> None:
    source = root / "remote.log"
    source.write_text(
        "".join(f'{{"level":"INFO","seq":{index},"message":"remote row"}}\n'
                for index in range(2000))
        + "SLOW_TERMINAL_TAIL\n"
    )
    app = SlowTerminalApp(
        binary,
        ["--capture-dir", str(root / "capture"), "--file", str(source)],
        width=120,
        height=32,
        environment=isolated_environment(root),
        latency=latency,
        chunk=chunk,
    )
    try:
        app.wait_for("SLOW_TERMINAL_TAIL", timeout=120.0)

        # A mouse report is the longest sequence the app parses and the one that
        # fragments worst; split it deliberately across writes.
        app.send(b"\x1b[<0")
        app.send(b";20;6M")
        app.send(b"\x1b[<0;20;6m")
        app.wait_until(lambda text: "SLOW_TERMINAL_TAIL" in text,
                       "viewport survives a fragmented mouse report", timeout=60.0)
        assert "[<0" not in app.text(), "a split mouse report was echoed as text"

        # A filter typed a fragment at a time, then a resize while the frame it
        # caused is still being written.
        app.send(b"/")
        app.wait_for("Search", timeout=60.0)
        app.send(b"\x1b[200~remote row\x1b[201~")
        app.send(b"\r")
        app.resize(90, 24)
        # The status line clips at the narrow width, so assert on the editor's
        # own applied row and on the match count, both of which fit.
        app.wait_until(lambda text: "Applied   remote row" in text,
                       "filter applied across a resize on a slow link", timeout=120.0)
        app.wait_until(lambda text: "matched 2000" in text,
                       "the narrowed frame still reports the filtered count",
                       timeout=120.0)
        app.resize(120, 32)
        app.send(b"\x1b")
        app.wait_until(lambda text: "Examples:" not in text, "search closed", timeout=60.0)

        # Frames must still arrive whole: the harness only reads at frame
        # boundaries, so a torn frame here means the app emitted one.
        assert app.torn_frames == 0, f"{app.torn_frames} frames were not presentable"

        app.send(b"q")
        assert app.wait_exit(timeout=120.0) == 0
        app.assert_restored()
    finally:
        if app.process.poll() is None:
            app.process.kill()
            app.process.wait()
        app.close()


def main() -> int:
    import argparse
    import tempfile

    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=pathlib.Path)
    parser.add_argument("--latency", type=float, default=DEFAULT_LATENCY)
    parser.add_argument("--chunk", type=int, default=DEFAULT_CHUNK)
    arguments = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="lvu-slow-terminal-") as directory:
        story(arguments.binary.resolve(), pathlib.Path(directory),
              arguments.latency, arguments.chunk)
    print(
        f"slow terminal passed: {arguments.latency * 2 * 1000:.0f}ms round trip, "
        f"{arguments.chunk}-byte chunks, fragmented mouse report, resize mid-frame, "
        "whole frames and clean exit"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
