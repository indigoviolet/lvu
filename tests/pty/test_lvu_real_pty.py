#!/usr/bin/env python3
"""Real-source PTY workflow for the temporary lvu-app composition binary."""

from __future__ import annotations

import argparse
import os
import pathlib
import shlex
import tempfile
import termios
import time

from test_lvu_pty import PtyApp


def wait_reaped(pid: int, timeout: float = 4.0) -> None:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        stat = pathlib.Path(f"/proc/{pid}/stat")
        if not stat.exists():
            return
        fields = stat.read_text(errors="replace").split()
        if len(fields) > 2 and fields[2] == "Z":
            return
        time.sleep(0.01)
    raise AssertionError(f"owned command process {pid} was not reaped")


def quit_cleanly(app: PtyApp) -> None:
    app.send(b"q")
    assert app.wait_exit(timeout=8.0) == 0
    app.assert_restored()


def run_story(binary: pathlib.Path) -> None:
    with tempfile.TemporaryDirectory(prefix="lvu-real-pty-") as temporary:
        root = pathlib.Path(temporary)
        capture = root / "capture"
        source = root / "actual.log"
        pid_file = root / "command.pid"
        source.write_bytes(b"file alpha\xff\n")
        shell = (
            "printf 'command stdout\\n'; "
            "printf 'command stderr\\n' >&2; "
            f"printf '%s\\n' $$ > {shlex.quote(str(pid_file))}; "
            "trap 'exit 0' TERM INT; while :; do sleep 1; done"
        )
        app = PtyApp(
            binary,
            [
                "--capture-dir",
                str(capture),
                "--file",
                str(source),
                "--command",
                shell,
            ],
            width=100,
            height=26,
        )
        try:
            initial = app.wait_for("file alpha�", timeout=6.0)
            assert "lvu live sources" in initial
            assert "actual.log" in initial
            assert "DEMO FIXTURE" not in initial

            app.send(b"]")
            command = app.wait_until(
                lambda text: "command stdout" in text and "command stderr" in text,
                "captured command stdout and stderr",
                timeout=6.0,
            )
            assert "shell command" in command

            with source.open("ab") as stream:
                stream.write(b"file beta\n")
                stream.flush()
                os.fsync(stream.fileno())
            app.send(b"[")
            app.wait_for("file beta", timeout=6.0)

            app.send(b"/")
            app.send(b"not connected")
            app.send(b"\r")
            app.wait_for("native text-query adapter is not wired", timeout=4.0)
            app.send(b"\x1b")

            pid = int(pid_file.read_text().strip())
            quit_cleanly(app)
            wait_reaped(pid)
        except Exception:
            if app.process.poll() is None:
                app.process.terminate()
            app.drain()
            raise
        finally:
            app.close()

        journals = list(capture.glob("*/capture.journal"))
        assert journals, "no durable journals were created"
        durable = b"".join(path.read_bytes() for path in journals)
        assert b"file alpha\xff" in durable, "invalid UTF-8 source bytes were not preserved"
        assert b"command stdout" in durable and b"command stderr" in durable

        # Reopen the same deterministic source identity and journal generation.
        reopened = PtyApp(
            binary,
            ["--capture-dir", str(capture)],
            width=100,
            height=26,
        )
        try:
            reopened.wait_for("Add source")
            reopened.send(b"\x1b[200~" + str(source).encode() + b"\x1b[201~")
            reopened.send(b"\r")
            screen = reopened.wait_until(
                lambda text: "file alpha�" in text and "file beta" in text,
                "reopened durable journal rows",
                timeout=6.0,
            )
            assert "Raw events" in screen
            quit_cleanly(reopened)
        finally:
            if reopened.process.poll() is None:
                reopened.process.kill()
            reopened.close()


def run_startup_failure_story(binary: pathlib.Path) -> None:
    with tempfile.TemporaryDirectory(prefix="lvu-start-failure-") as temporary:
        root = pathlib.Path(temporary)
        pid_file = root / "owned.pid"
        missing = root / "missing.log"
        shell = (
            f"printf '%s\\n' $$ > {shlex.quote(str(pid_file))}; "
            "trap 'exit 0' TERM INT; while :; do sleep 1; done"
        )
        app = PtyApp(
            binary,
            [
                "--capture-dir",
                str(root / "capture"),
                "--command",
                shell,
                "--file",
                str(missing),
            ],
        )
        try:
            deadline = time.monotonic() + 5.0
            while not pid_file.exists() and time.monotonic() < deadline:
                app.drain()
                time.sleep(0.01)
            assert pid_file.exists(), "command never reached PID readiness before later failure"
            pid = int(pid_file.read_text().strip())
            assert app.wait_exit(timeout=8.0) != 0
            app.drain()
            assert "missing.log" in bytes(app.transcript).decode("utf-8", "replace")
            wait_reaped(pid)
            assert app.before == termios.tcgetattr(app.slave)
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
    run_startup_failure_story(binary)
    print("Real-source PTY passed: file/command/live append/reopen/reap/restoration")


if __name__ == "__main__":
    main()
