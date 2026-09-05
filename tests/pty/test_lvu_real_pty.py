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
import subprocess

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
            width=140,
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

            # Literal search stays in Rust, publishes stable matching IDs, and
            # remains independent from the command view.
            app.send(b"/")
            app.send(b"beta")
            app.send(b"\r")
            searched = app.wait_until(
                lambda text: 'search:"beta"' in text
                and "query ready: matched 1" in text
                and "file beta" in text,
                "native literal search result",
                timeout=8.0,
            )
            assert "file alpha" not in searched
            app.send(b"\x1b")
            app.wait_until(
                lambda text: "Live literal substring" not in text,
                "literal editor closed",
            )

            app.send(b"]")
            independent = app.wait_for("command stdout", timeout=4.0)
            assert 'search:"beta"' not in independent
            app.send(b"[")
            app.wait_until(
                lambda text: "file beta" in text and 'search:"beta"' in text,
                "per-view search restored",
            )

            with source.open("ab") as stream:
                stream.write(b"file gamma hidden\nfile beta late\n")
                stream.flush()
                os.fsync(stream.fileno())
            filtered_arrival = app.wait_for("file beta late", timeout=8.0)
            assert "file gamma hidden" not in filtered_arrival

            # Advanced Polars compiles lazily and combines with literal search
            # using AND: only the beta row that also contains "late" remains.
            advanced = 'pl.col("raw").str.contains("late", literal=True)'
            app.send(b"p")
            app.send(b"\x1b[200~" + advanced.encode() + b"\x1b[201~")
            app.send(b"\r")
            app.wait_until(
                lambda text: "applied: " + advanced in text and "advanced:on" in text,
                "accepted advanced AND literal constraints",
                timeout=12.0,
            )
            app.send(b"\x1b")
            combined = app.wait_until(
                lambda text: "Advanced Polars filter" not in text
                and "file beta late" in text,
                "accepted advanced AND rows",
            )
            assert "file alpha" not in combined
            assert "file gamma hidden" not in combined

            # Invalid advanced input keeps the last valid literal view active;
            # later arrivals still flow through that accepted constraint.
            app.send(b"p")
            app.send(b"\x7f" * len(advanced))
            app.send(b"pl.col(")
            app.send(b"\r")
            app.wait_for("compiler rejected expression", timeout=8.0)
            app.send(b"\x1b")
            app.wait_until(
                lambda text: "Advanced Polars filter" not in text,
                "advanced editor closed after rejection",
            )
            preserved = app.wait_for("file beta late", timeout=4.0)
            assert "file gamma hidden" not in preserved
            with source.open("ab") as stream:
                stream.write(b"file delta hidden\nfile beta late newest\n")
                stream.flush()
                os.fsync(stream.fileno())
            still_filtered = app.wait_for("file beta late newest", timeout=8.0)
            assert "file delta hidden" not in still_filtered

            # Clear advanced independently, retaining the literal constraint.
            app.send(b"p")
            app.send(b"\x7f" * len("pl.col("))
            app.send(b"\r")
            app.wait_until(
                lambda text: "applied: " in text and "advanced:on" not in text,
                "advanced constraint cleared independently",
                timeout=8.0,
            )
            app.send(b"\x1b")
            app.wait_until(
                lambda text: "Advanced Polars filter" not in text,
                "cleared advanced editor closed",
            )

            app.send(b"/")
            app.send(b"\x7f" * len("beta"))
            app.send(b"\r")
            restored = app.wait_until(
                lambda text: "file gamma hidden" in text
                and "file delta hidden" in text
                and 'search:"beta"' not in text,
                "cleared native search restoring raw rows",
                timeout=8.0,
            )
            assert "file alpha�" in restored
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


def run_memory_restore_story(binary: pathlib.Path) -> None:
    with tempfile.TemporaryDirectory(prefix="lvu-memory-pty-") as temporary:
        root = pathlib.Path(temporary)
        capture = root / "capture"
        source = root / "remembered.log"
        source.write_text("beta early\nbeta late\nhidden late\n")
        advanced = 'pl.col("raw").str.contains("late", literal=True)'

        first = PtyApp(binary, ["--capture-dir", str(capture), "--file", str(source)], width=160, height=25)
        try:
            first.wait_for("beta early", timeout=6.0)
            first.send(b"/"); first.send(b"beta"); first.send(b"\r")
            first.wait_until(lambda text: 'search:"beta"' in text and "query ready" in text, "accepted remembered literal", timeout=8.0)
            first.send(b"\x1b")
            time.sleep(0.1)
            first.send(b"p")
            first.send(b"\x1b[200~" + advanced.encode() + b"\x1b[201~")
            first.send(b"\r")
            first.wait_until(lambda text: "advanced:on" in text and "applied: " + advanced in text, "accepted remembered advanced", timeout=12.0)
            first.send(b"\x1b")
            time.sleep(0.1)
            first.send(b"p")
            first.send(b"\x7f" * len(advanced))
            first.send(b"pl.col(")
            first.send(b"\r")
            first.send(b"\x1b")
            time.sleep(0.1)
            quit_cleanly(first)
        finally:
            if first.process.poll() is None: first.process.kill()
            first.close()

        reopened = PtyApp(binary, ["--capture-dir", str(capture), "--file", str(source)], width=160, height=25)
        try:
            restored = reopened.wait_until(lambda text: 'search:"beta"' in text and "advanced:on" in text and "beta late" in text, "restored accepted constraints", timeout=12.0)
            assert "beta early" not in restored and "hidden late" not in restored
            reopened.send(b"p")
            editor = reopened.wait_for("pl.col(", timeout=4.0)
            assert "applied: " + advanced in editor
            assert editor.count("pl.col(") >= 2, "unfinished advanced draft was not restored"
            reopened.send(b"\x1b")
            with source.open("a") as stream:
                stream.write("beta late newest\nhidden late newest\n"); stream.flush(); os.fsync(stream.fileno())
            arrived = reopened.wait_for("beta late newest", timeout=8.0)
            assert "hidden late newest" not in arrived
            quit_cleanly(reopened)
        finally:
            if reopened.process.poll() is None: reopened.process.kill()
            reopened.close()

        recent = PtyApp(binary, ["--capture-dir", str(capture)], width=160, height=25)
        try:
            recent.wait_for("Add source")
            recent.send(b"\x04")
            recent.wait_for("remembered.log", timeout=8.0)
            recent.send(b"\r")
            recent.wait_until(lambda text: 'search:"beta"' in text and "beta late" in text, "explicit recent-source reopen", timeout=12.0)
            quit_cleanly(recent)
        finally:
            if recent.process.poll() is None: recent.process.kill()
            recent.close()


def run_discovery_story(binary: pathlib.Path) -> None:
    with tempfile.TemporaryDirectory(prefix="lvu-discovery-pty-") as temporary:
        root = pathlib.Path(temporary)
        name = "controlled-tee-events.log"
        source = root / name
        tee = subprocess.Popen(
            ["tee", str(source)],
            stdin=subprocess.PIPE,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.PIPE,
        )
        assert tee.stdin is not None
        tee.stdin.write(b"controlled discovery content\n")
        tee.stdin.flush()
        deadline = time.monotonic() + 2.0
        while not source.exists() and time.monotonic() < deadline:
            time.sleep(0.01)
        assert source.exists(), "tee fixture did not create its controlled file"

        app = PtyApp(
            binary,
            ["--capture-dir", str(root / "capture")],
            width=110,
            height=30,
            cwd=root,
        )
        try:
            app.wait_for("Add source")
            app.send(b"\x04")  # Ctrl-D: discovery mode, never autonomous start.
            app.send(name.encode())
            discovered = app.wait_until(
                lambda text: name in text
                and "1/" in text
                and "Project Medium Available" in text,
                "controlled tee/file discovery candidate",
                timeout=6.0,
            )
            assert "selection never auto-starts" in discovered
            assert "controlled discovery content" not in discovered
            app.send(b"\r")
            selected = app.wait_for("controlled discovery content", timeout=6.0)
            assert "controlled-tee" in selected and "Raw events" in selected
            quit_cleanly(app)
        finally:
            if app.process.poll() is None:
                app.process.kill()
            app.close()
            tee.stdin.close()
            tee.wait(timeout=4.0)


def run_path_completion_story(binary: pathlib.Path) -> None:
    with tempfile.TemporaryDirectory(prefix="lvu-complete-pty-") as temporary:
        root = pathlib.Path(temporary)
        nested = root / "nested space"
        nested.mkdir()
        (root / "nested spare").mkdir()
        source = nested / "über events.log"
        source.write_bytes(b"completed path content\n")
        app = PtyApp(
            binary,
            ["--capture-dir", str(root / "capture")],
            width=110,
            height=26,
            cwd=root,
        )
        try:
            app.wait_for("Add source")
            app.send(b"nested sp")
            app.send(b"\t")
            choices = app.wait_until(
                lambda text: "Choices" in text
                and "nested space/" in text
                and "nested spare/" in text,
                "ambiguous path completion choices",
            )
            assert "Kind: FILE PATH" in choices
            app.send(b"\t")  # Apply the selected directory, including its slash.
            app.wait_for("nested space/")
            app.send("üb".encode())
            app.send(b"\t")
            app.wait_for("nested space/über events.log")
            app.send(b"\r")
            captured = app.wait_for("completed path content", timeout=8.0)
            assert "Raw events" in captured
            quit_cleanly(app)
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
    run_discovery_story(binary)
    run_memory_restore_story(binary)
    run_path_completion_story(binary)
    print(
        "Real-source PTY passed: file/command/discovery/completion/live "
        "append/reopen/reap/restoration"
    )


if __name__ == "__main__":
    main()
