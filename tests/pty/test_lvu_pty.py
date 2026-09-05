#!/usr/bin/env python3
"""Autonomous real-PTY workflows for the standalone lvu binary."""

from __future__ import annotations

import argparse
import codecs
import fcntl
import os
import pathlib
import pty
import signal
import struct
import subprocess
import sys
import termios
import time

import pyte


class PtyApp:
    def __init__(self, binary: pathlib.Path, arguments: list[str], width: int = 88, height: int = 24) -> None:
        self.master, self.slave = pty.openpty()
        self.screen = pyte.Screen(width, height)
        self.stream = pyte.Stream(self.screen)
        self.decoder = codecs.getincrementaldecoder("utf-8")("replace")
        self.transcript = bytearray()
        self.cursor_response_sent = False
        self._set_size(width, height)
        self.before = termios.tcgetattr(self.slave)

        def child_setup() -> None:
            os.setsid()
            fcntl.ioctl(0, termios.TIOCSCTTY, 0)

        self.process = subprocess.Popen(
            [str(binary), *arguments],
            stdin=self.slave,
            stdout=self.slave,
            stderr=self.slave,
            close_fds=True,
            preexec_fn=child_setup,
        )
        os.set_blocking(self.master, False)

    def _set_size(self, width: int, height: int) -> None:
        fcntl.ioctl(self.slave, termios.TIOCSWINSZ, struct.pack("HHHH", height, width, 0, 0))

    def send(self, data: bytes) -> None:
        os.write(self.master, data)

    def resize(self, width: int, height: int) -> None:
        self.screen.resize(lines=height, columns=width)
        self._set_size(width, height)
        os.kill(self.process.pid, signal.SIGWINCH)

    def drain(self) -> None:
        while True:
            try:
                data = os.read(self.master, 65536)
            except (BlockingIOError, OSError):
                return
            if not data:
                return
            self.transcript.extend(data)
            if not self.cursor_response_sent and b"\x1b[6n" in self.transcript:
                os.write(self.master, b"\x1b[1;1R")
                self.cursor_response_sent = True
            self.stream.feed(self.decoder.decode(data))

    def text(self) -> str:
        return "\n".join(self.screen.display)

    def wait_for(self, expected: str, timeout: float = 3.0) -> str:
        return self.wait_until(lambda text: expected in text, f"screen containing {expected!r}", timeout)

    def wait_until(self, predicate, description: str, timeout: float = 3.0) -> str:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            self.drain()
            text = self.text()
            if predicate(text):
                return text
            if self.process.poll() is not None:
                break
            time.sleep(0.01)
        raise AssertionError(
            f"timed out waiting for {description}; exit={self.process.poll()}\n"
            f"--- pyte screen ---\n{self.text()}\n"
            f"--- transcript tail ---\n{bytes(self.transcript[-4000:])!r}"
        )

    def wait_exit(self, timeout: float = 3.0) -> int:
        deadline = time.monotonic() + timeout
        while self.process.poll() is None and time.monotonic() < deadline:
            self.drain()
            time.sleep(0.01)
        return_code = self.process.wait(timeout=max(0.1, deadline - time.monotonic()))
        self.drain()
        return return_code

    def assert_remains(self, expected: str, forbidden: str, duration: float = 0.15) -> None:
        deadline = time.monotonic() + duration
        while time.monotonic() < deadline:
            self.drain()
            text = self.text()
            assert expected in text, f"expected state disappeared: {expected!r}"
            assert forbidden not in text, f"unexpected state appeared: {forbidden!r}"
            if self.process.poll() is not None:
                raise AssertionError(f"process exited while observing stable state: {self.process.returncode}")
            time.sleep(0.01)

    def assert_restored(self) -> None:
        assert termios.tcgetattr(self.slave) == self.before, "child did not restore PTY termios"
        transcript = bytes(self.transcript)
        assert b"\x1b[?1049l" in transcript, "alternate screen was not left"
        assert b"\x1b[?2004l" in transcript, "bracketed paste was not disabled"
        assert b"\x1b[?1000l" in transcript, "mouse capture was not disabled"

    def close(self) -> None:
        os.close(self.master)
        os.close(self.slave)


def run_story(binary: pathlib.Path) -> None:
    app = PtyApp(binary, ["--demo"])
    try:
        initial = app.wait_until(
            lambda text: "fixture request 01 completed" in text
            and "fixture request 16 completed" in text,
            "initial full viewport with first and last fixture rows",
        )
        assert "FOLLOW" in initial and "1-16/16" in initial
        assert "Unicode 東京 café" in initial

        # Exact 1-based SGR coordinates: row 3 is the table header and row 4
        # is the first data row. The header must not change selection.
        app.send(b"d")
        app.wait_for("stable display id: api:16")
        app.send(b"\x1b[<0;24;3M")
        app.assert_remains("stable display id: api:16", "stable display id: api:6")
        app.send(b"\x1b[<0;24;4M")
        selected = app.wait_for("stable display id: api:6")
        assert "HISTORY" in selected

        # Sidebar hitboxes select the view row, not its source/health rows.
        app.send(b"\x1b[<0;5;8M")
        app.wait_until(
            lambda text: "errors" in text and "fixture queue unavailable" in text,
            "mouse-selected errors view",
        )
        app.send(b"\x1b[<0;5;5M")
        app.wait_for("stable display id: api:6")
        app.send(b"\t")  # selector focus back to logs

        app.send(b"?")
        app.wait_for("left click exact row/view")
        app.send(b"\x1b[<65;24;4M")  # modal owns mouse; underlying row is unchanged
        app.send(b"?")
        app.wait_for("stable display id: api:6")

        app.send(b"/")
        app.wait_for("Filter editor")
        app.send(b'\x1b[200~level == "ERROR"\nand invalid\x1b[201~')
        app.wait_for('level == "ERROR"')
        app.send(b"\r")
        rejected = app.wait_for("query adapter is not wired")
        assert "last applied:" in rejected
        app.send(b"\x1b")
        app.wait_until(lambda text: "Filter editor" not in text, "editor close")

        app.send(b"G")
        app.wait_for("stable display id: api:16")
        app.send(b"f")
        app.wait_for("HISTORY")
        app.send(b"a")
        frozen = app.wait_for("/17")
        assert "stable display id: api:16" in frozen
        app.send(b"f")
        app.wait_for("stable display id: api:17")

        app.resize(88, 12)
        app.wait_until(
            lambda text: "stable display id: api:17" in text and "late fixture arrival 17" in text,
            "resized details layout retaining visible newest selection",
        )
        app.send(b"d")
        without_details = app.wait_until(
            lambda text: "Selected event details" not in text and "late fixture arrival 17" in text,
            "details toggle retaining newest row",
        )
        assert "Log viewport" in without_details

        app.resize(40, 10)
        app.wait_until(
            lambda text: "Log viewport" in text and "Sources / views" not in text,
            "narrow layout without sidebar",
        )
        app.resize(18, 4)
        app.wait_for("terminal too small")
        app.resize(88, 24)
        app.wait_for("DEMO FIXTURE")

        app.send(b"q")
        assert app.wait_exit() == 0
        app.assert_restored()
    except Exception:
        if app.process.poll() is None:
            app.process.terminate()
            try:
                app.process.wait(timeout=1)
            except subprocess.TimeoutExpired:
                app.process.kill()
        app.drain()
        print("PTY SCREEN:\n" + app.text(), file=sys.stderr)
        print("PTY TRANSCRIPT TAIL:\n" + repr(bytes(app.transcript[-8000:])), file=sys.stderr)
        raise
    finally:
        app.close()


def run_panic_probe(binary: pathlib.Path) -> None:
    app = PtyApp(binary, ["--demo-panic-restoration-probe"])
    try:
        assert app.wait_exit() == 0
        app.assert_restored()
    finally:
        if app.process.poll() is None:
            app.process.kill()
        app.close()


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("binary", type=pathlib.Path)
    arguments = parser.parse_args()
    binary = arguments.binary.resolve()
    if not binary.is_file():
        parser.error(f"binary does not exist: {binary}")
    run_story(binary)
    run_panic_probe(binary)
    print("PTY workflows passed: full viewport, input, resize, async UI, normal/panic restoration")


if __name__ == "__main__":
    main()
