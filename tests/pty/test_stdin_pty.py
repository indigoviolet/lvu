#!/usr/bin/env python3
"""Controlling-terminal PTY coverage for redirected stdin capture."""

from __future__ import annotations

import argparse
import codecs
import fcntl
import os
import pathlib
import pty
import subprocess
import tempfile
import termios

import pyte

from test_lvu_pty import PtyApp


class RedirectedStdinPtyApp(PtyApp):
    """A real controlling PTY whose fd 0 is independently replaced by a pipe."""

    def __init__(
        self,
        binary: pathlib.Path,
        arguments: list[str],
        width: int = 140,
        height: int = 28,
        cwd: pathlib.Path | None = None,
        stdin_path: pathlib.Path | None = None,
        stdin_nonblocking: bool = False,
    ) -> None:
        self.master, self.slave = pty.openpty()
        if stdin_path is None:
            input_reader, self.pipe_writer = os.pipe()
            self.pipe_observer = os.dup(input_reader)
        else:
            input_reader = os.open(stdin_path, os.O_RDONLY | (os.O_NONBLOCK if stdin_nonblocking else 0))
            self.pipe_writer = -1
            self.pipe_observer = -1
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
            os.dup2(input_reader, 0)

        self.process = subprocess.Popen(
            [str(binary), *arguments],
            stdin=self.slave,
            stdout=self.slave,
            stderr=self.slave,
            close_fds=True,
            pass_fds=(input_reader,),
            preexec_fn=child_setup,
            cwd=cwd,
        )
        os.close(input_reader)
        os.set_blocking(self.master, False)

    def write_stdin(self, data: bytes) -> None:
        os.write(self.pipe_writer, data)

    def close_stdin(self) -> None:
        if self.pipe_writer >= 0:
            os.close(self.pipe_writer)
            self.pipe_writer = -1

    def close(self) -> None:
        self.close_stdin()
        if self.pipe_observer >= 0:
            os.close(self.pipe_observer)
            self.pipe_observer = -1
        super().close()


def quit_cleanly(app: PtyApp) -> None:
    app.send(b"q")
    assert app.wait_exit(timeout=8.0) == 0
    app.assert_restored()


def run_mixed_sources(binary: pathlib.Path) -> None:
    with tempfile.TemporaryDirectory(prefix="lvu-stdin-pty-") as temporary:
        root = pathlib.Path(temporary)
        source = root / "file with spaces.log"
        source.write_bytes(b"positional file event\n")
        capture = root / "capture"
        app = RedirectedStdinPtyApp(
            binary,
            [
                "--capture-dir",
                str(capture),
                str(source),
                "-c",
                "printf 'short command event\\n'",
            ],
            cwd=root,
        )
        expected = b"stdin alpha\xff\nstdin beta\n"
        try:
            app.write_stdin(expected[:8])
            app.write_stdin(expected[8:])
            app.wait_for("positional file event", timeout=8.0)
            app.send(b"]")
            app.wait_for("short command event", timeout=8.0)
            app.send(b"]")
            stdin_screen = app.wait_for("stdin alpha�", timeout=8.0)
            assert "standard input" in stdin_screen
            flags = fcntl.fcntl(app.pipe_observer, fcntl.F_GETFL)
            assert not flags & os.O_NONBLOCK, "Tokio O_NONBLOCK leaked to the parent pipe"

            # Keys travel through /dev/tty and cannot become captured fd-0 data.
            app.send(b"j")
            app.write_stdin(b"stdin gamma\n")
            app.wait_for("stdin gamma", timeout=8.0)
            app.close_stdin()
            app.wait_for("stdin gamma", timeout=4.0)
            assert app.process.poll() is None, "stdin EOF must leave the viewer open"
            quit_cleanly(app)
        finally:
            if app.process.poll() is None:
                app.process.kill()
            app.close()

        journal_bytes = b"".join(path.read_bytes() for path in capture.rglob("*.journal"))
        assert b"stdin alpha\xff" in journal_bytes, "invalid UTF-8 bytes were not preserved"
        assert b"stdin beta" in journal_bytes, "stdin bytes were not preserved"
        assert b"\nj\n" not in journal_bytes, "terminal key leaked into captured stdin"


def run_quit_before_eof(binary: pathlib.Path) -> None:
    with tempfile.TemporaryDirectory(prefix="lvu-stdin-open-pty-") as temporary:
        root = pathlib.Path(temporary)
        app = RedirectedStdinPtyApp(
            binary,
            ["--capture-dir", str(root / "capture"), "--stdin"],
            cwd=root,
        )
        try:
            app.write_stdin(b"open pipeline event\n")
            app.wait_for("open pipeline event", timeout=8.0)
            quit_cleanly(app)
        finally:
            if app.process.poll() is None:
                app.process.kill()
            app.close()


def run_redirected_regular_file(binary: pathlib.Path) -> None:
    with tempfile.TemporaryDirectory(prefix="lvu-stdin-file-pty-") as temporary:
        root = pathlib.Path(temporary)
        redirected = root / "redirected.input"
        redirected.write_bytes(b"regular redirected bytes\xff\n")
        app = RedirectedStdinPtyApp(
            binary,
            ["--capture-dir", str(root / "capture")],
            cwd=root,
            stdin_path=redirected,
        )
        try:
            screen = app.wait_for("regular redirected bytes�", timeout=8.0)
            assert "standard input" in screen
            assert app.process.poll() is None, "regular stdin EOF must leave viewer open"
            quit_cleanly(app)
        finally:
            if app.process.poll() is None:
                app.process.kill()
            app.close()


def run_character_device_policy(binary: pathlib.Path) -> None:
    with tempfile.TemporaryDirectory(prefix="lvu-stdin-device-pty-") as temporary:
        root = pathlib.Path(temporary)
        empty = RedirectedStdinPtyApp(
            binary,
            ["--capture-dir", str(root / "null-capture")],
            cwd=root,
            stdin_path=pathlib.Path("/dev/null"),
        )
        try:
            empty.wait_for("standard input", timeout=8.0)
            quit_cleanly(empty)
        finally:
            if empty.process.poll() is None:
                empty.process.kill()
            empty.close()

        unsupported = RedirectedStdinPtyApp(
            binary,
            ["--capture-dir", str(root / "device-capture")],
            cwd=root,
            stdin_path=pathlib.Path("/dev/zero"),
        )
        try:
            assert unsupported.wait_exit(timeout=5.0) != 0
            assert b"character devices are not supported" in bytes(unsupported.transcript)
            assert termios.tcgetattr(unsupported.slave) == unsupported.before
        finally:
            if unsupported.process.poll() is None:
                unsupported.process.kill()
            unsupported.close()


def run_no_controlling_terminal(binary: pathlib.Path) -> None:
    with tempfile.TemporaryDirectory(prefix="lvu-stdin-no-tty-") as temporary:
        root = pathlib.Path(temporary)
        marker = root / "must-not-start"
        result = subprocess.run(
            [
                str(binary),
                "--capture-dir",
                str(root / "capture"),
                "--command",
                f"touch {marker}",
            ],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            start_new_session=True,
            timeout=5.0,
            check=False,
        )
        assert result.returncode != 0
        assert b"controlling terminal unavailable" in result.stderr
        assert not marker.exists(), "source acquisition started before terminal validation"


def run_terminal_stdin_rejected(binary: pathlib.Path) -> None:
    app = PtyApp(binary, ["--stdin"])
    try:
        assert app.wait_exit(timeout=5.0) != 0
        assert b"stdin is a terminal" in bytes(app.transcript)
        assert termios.tcgetattr(app.slave) == app.before
    finally:
        if app.process.poll() is None:
            app.process.kill()
        app.close()


def run_named_fifo_without_writer(binary: pathlib.Path) -> None:
    with tempfile.TemporaryDirectory(prefix="lvu-stdin-fifo-pty-") as temporary:
        root = pathlib.Path(temporary)
        fifo = root / "input.fifo"
        os.mkfifo(fifo)
        app = RedirectedStdinPtyApp(binary, ["--capture-dir", str(root / "capture")],
                                    stdin_path=fifo, stdin_nonblocking=True)
        try:
            app.wait_for("standard input", timeout=5.0)
            quit_cleanly(app)
        finally:
            if app.process.poll() is None:
                app.process.kill()
                app.process.wait(timeout=5.0)
            app.close()


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("binary", type=pathlib.Path)
    args = parser.parse_args()
    binary = args.binary.resolve()
    run_mixed_sources(binary)
    run_quit_before_eof(binary)
    run_redirected_regular_file(binary)
    run_character_device_policy(binary)
    run_named_fifo_without_writer(binary)
    run_no_controlling_terminal(binary)
    run_terminal_stdin_rejected(binary)
    print("stdin PTY passed: positional/command/pipe tty split, EOF, early quit, restoration")


if __name__ == "__main__":
    main()
