#!/usr/bin/env python3
"""Native-Unix terminal/process/install-tree acceptance harness.

This drives a kernel pseudo-terminal, not Terminal.app, iTerm2, or ConPTY.
Every exit writes bounded evidence and cleans only process identities captured
from the harness-owned command fixture.
"""

from __future__ import annotations

import argparse
import base64
import fcntl
import hashlib
import json
import os
import pathlib
import platform
import pty
import shlex
import shutil
import signal
import struct
import subprocess
import sys
import tempfile
import termios
import time
from dataclasses import asdict, dataclass
from datetime import datetime, timezone


RESTORE_SEQUENCES = (b"\x1b[?1049l", b"\x1b[?2004l", b"\x1b[?1000l")
TRANSCRIPT_TAIL_BYTES = 16 * 1024


@dataclass(frozen=True)
class OwnedProcessIdentity:
    pid: int
    process_group: int


class NativePty:
    def __init__(self, binary, arguments, environment, cwd, stdin_reader=None) -> None:
        self.master, self.slave = pty.openpty()
        self.slave_name = os.ttyname(self.slave)
        fcntl.ioctl(self.slave, termios.TIOCSWINSZ, struct.pack("HHHH", 28, 120, 0, 0))
        self.before = termios.tcgetattr(self.slave)
        self.transcript = bytearray()

        def setup() -> None:
            os.setsid()
            fcntl.ioctl(0, termios.TIOCSCTTY, 0)
            if stdin_reader is not None:
                os.dup2(stdin_reader, 0)

        pass_fds = () if stdin_reader is None else (stdin_reader,)
        self.process = subprocess.Popen(
            [str(binary), *arguments], stdin=self.slave, stdout=self.slave,
            stderr=self.slave, cwd=cwd, env={**os.environ, **environment},
            close_fds=True, pass_fds=pass_fds, preexec_fn=setup,
        )
        os.set_blocking(self.master, False)
        self._cursor_replies = 0

    def drain(self) -> None:
        while True:
            try:
                data = os.read(self.master, 65536)
            except (BlockingIOError, OSError):
                return
            if not data:
                return
            self.transcript.extend(data)
            requests = self.transcript.count(b"\x1b[6n")
            for _ in range(self._cursor_replies, requests):
                os.write(self.master, b"\x1b[1;1R")
            self._cursor_replies = requests

    def wait_bytes(self, expected: bytes, timeout: float = 15.0) -> None:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            self.drain()
            if expected in self.transcript:
                return
            if self.process.poll() is not None:
                break
            time.sleep(0.02)
        raise AssertionError(
            f"missing {expected!r}; exit={self.process.poll()}; "
            f"tail={bytes(self.transcript[-3000:])!r}"
        )

    def send(self, value: bytes) -> None:
        os.write(self.master, value)

    def wait_exit(self, timeout: float = 15.0) -> int:
        deadline = time.monotonic() + timeout
        while self.process.poll() is None and time.monotonic() < deadline:
            self.drain()
            time.sleep(0.02)
        status = self.process.wait(timeout=max(0.1, deadline - time.monotonic()))
        self.drain()
        return status

    def assert_restored(self) -> None:
        self.assert_termios_unchanged()
        for sequence in RESTORE_SEQUENCES:
            assert sequence in self.transcript, f"missing terminal restore sequence {sequence!r}"

    def assert_termios_unchanged(self) -> None:
        # A hosted Darwin runner returned ENOTTY from the parent's original
        # slave descriptor after the child session exited. Reopen the same
        # harness-owned PTY without acquiring it as our controlling terminal;
        # the exact termios comparison remains meaningful after child exit.
        slave = os.open(self.slave_name, os.O_RDWR | os.O_NOCTTY)
        try:
            after = termios.tcgetattr(slave)
        finally:
            os.close(slave)
        assert after == self.before, "PTY termios was not restored"

    def close(self) -> None:
        if self.process.poll() is None:
            self.process.kill()
            self.process.wait(timeout=5)
        self.drain()
        os.close(self.master)
        os.close(self.slave)


def command_output(command: list[str]) -> str:
    result = subprocess.run(command, text=True, capture_output=True, check=False)
    return (result.stdout or result.stderr).strip()


def current_identity(pid: int) -> OwnedProcessIdentity | None:
    try:
        return OwnedProcessIdentity(pid, os.getpgid(pid))
    except ProcessLookupError:
        return None


def same_process(identity: OwnedProcessIdentity) -> bool:
    return current_identity(identity.pid) == identity


def wait_identities_gone(identities, timeout: float = 10.0):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        survivors = [identity for identity in identities if same_process(identity)]
        if not survivors:
            return []
        time.sleep(0.05)
    return [identity for identity in identities if same_process(identity)]


def cleanup_owned_processes(identities):
    """Kill only a recorded group with a still-matching recorded member."""
    live = [identity for identity in identities if same_process(identity)]
    for group in sorted({identity.process_group for identity in live}):
        if any(
            identity.pid == group
            and identity.process_group == group
            and same_process(identity)
            for identity in identities
        ):
            try:
                os.killpg(group, signal.SIGKILL)
            except ProcessLookupError:
                pass
    for identity in identities:
        if same_process(identity):
            try:
                os.kill(identity.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
    return wait_identities_gone(identities)


def recorded_pids(path: pathlib.Path, *, strict: bool):
    if not path.is_file():
        return []
    values = []
    for value in path.read_text().split():
        try:
            pid = int(value)
        except ValueError:
            if strict:
                raise AssertionError("command fixture wrote a non-integer PID")
            continue
        if pid > 0 and pid not in values:
            values.append(pid)
    if strict and len(values) != 2:
        raise AssertionError("command fixture did not record its shell and child PIDs")
    return values


def observed_identities_from_file(path: pathlib.Path):
    # This is the failure-cleanup path: recover every valid PID already written
    # by our private fixture without letting malformed evidence skip cleanup.
    return [
        identity
        for pid in recorded_pids(path, strict=False)
        if (identity := current_identity(pid)) is not None
    ]


def identities_from_file(path: pathlib.Path):
    identities = [
        identity
        for pid in recorded_pids(path, strict=True)
        if (identity := current_identity(pid)) is not None
    ]
    if len(identities) != 2:
        raise AssertionError("command fixture exited before its identity was recorded")
    shell, child = identities
    assert shell is not None and child is not None
    if shell.process_group != shell.pid or child.process_group != shell.process_group:
        raise AssertionError("command fixture processes do not belong to their owned group")
    return [shell, child]


def isolated_environment(root: pathlib.Path) -> dict[str, str]:
    return {
        "HOME": str(root / "home"), "XDG_CONFIG_HOME": str(root / "config"),
        "XDG_CACHE_HOME": str(root / "cache"), "XDG_DATA_HOME": str(root / "data"),
        "NO_COLOR": "", "LVU_REDUCED_MOTION": "1",
    }


def command_text(pid_file: pathlib.Path) -> str:
    quoted = shlex.quote(str(pid_file))
    return (
        "sleep 300 & child=$!; "
        f"printf '%s %s\\n' $$ \"$child\" > {quoted}; "
        "printf 'platform-command-event\\n'; wait \"$child\""
    )


def transcript_evidence(app: NativePty) -> dict:
    app.drain()
    content = bytes(app.transcript)
    tail = content[-TRANSCRIPT_TAIL_BYTES:]
    return {
        "bytes": len(content), "sha256": hashlib.sha256(content).hexdigest(),
        "tail_bytes": len(tail), "tail_base64": base64.b64encode(tail).decode("ascii"),
    }


def run_probe(args, evidence: dict) -> None:
    binary = args.binary.resolve()
    if not binary.is_file() or not os.access(binary, os.X_OK):
        raise AssertionError(f"binary is not executable: {binary}")

    evidence["phase"] = "cli_help"
    result = subprocess.run([binary, "--help"], stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    assert result.returncode == 0 and b"Usage: lvu [OPTIONS]" in result.stdout
    assert b"lvu-app" not in result.stdout
    evidence["checks"].append({"name": "cli_help", "result": "pass"})

    evidence["phase"] = "resources"
    result = subprocess.run([binary, "--resources"], stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    assert result.returncode == 0
    if args.require_installed_resources:
        assert result.stdout.count(b"origin: installed beside the executable") == 2
        assert b"Python expression helper: found" in result.stdout
        assert b"agent bridge: found" in result.stdout
        name = "installed_resource_resolution"
    else:
        name = "resource_diagnostic"
    evidence["checks"].append({"name": name, "result": "pass"})

    fixture = pathlib.Path(tempfile.mkdtemp(prefix="lvu-sol-platform-"))
    marker = fixture / ".lvu-test-reproducer"
    marker.write_text("owned by platform runtime harness\n")
    environment = isolated_environment(fixture)
    try:
        evidence["phase"] = "file_capture_ctrl_c_terminal_restore"
        source = fixture / "events.log"
        source.write_text("platform-file-event\n", encoding="utf-8")
        app = NativePty(binary, [str(source), "--capture-dir", str(fixture / "file-capture")], environment, fixture)
        try:
            app.wait_bytes(b"platform-file-event")
            app.send(b"\x03")
            assert app.wait_exit() == 0
            app.assert_restored()
            evidence["checks"].append({"name": evidence["phase"], "result": "pass"})
        finally:
            evidence["transcripts"]["file_capture"] = transcript_evidence(app)
            app.close()

        evidence["phase"] = "orderly_command_tree_cleanup"
        pid_file = fixture / "command.pids"
        identities = []
        app = NativePty(
            binary,
            ["--capture-dir", str(fixture / "command-capture"), "--command", command_text(pid_file)],
            environment, fixture,
        )
        try:
            app.wait_bytes(b"platform-command-event")
            deadline = time.monotonic() + 5
            while not pid_file.exists() and time.monotonic() < deadline:
                time.sleep(0.02)
            identities = identities_from_file(pid_file)
            evidence["owned_processes"] = [asdict(identity) for identity in identities]
            if args.inject_failure_after_command_spawn:
                raise RuntimeError("injected failure after owned command spawn")
            app.send(b"q")
            assert app.wait_exit() == 0
            app.assert_restored()
            survivors = wait_identities_gone(identities)
            assert not survivors, f"owned command processes survived: {survivors}"
            evidence["checks"].append({"name": evidence["phase"], "result": "pass"})
        finally:
            cleanup_error = None
            survivors = []
            try:
                if not identities:
                    identities = observed_identities_from_file(pid_file)
                    evidence["owned_processes"] = [asdict(identity) for identity in identities]
                survivors = cleanup_owned_processes(identities)
            except Exception as error:
                cleanup_error = error
            finally:
                evidence["cleanup"] = {
                    "attempted": [asdict(identity) for identity in identities],
                    "survivors": [asdict(identity) for identity in survivors],
                    "error": None if cleanup_error is None else str(cleanup_error)[:2048],
                }
                evidence["transcripts"]["command_capture"] = transcript_evidence(app)
                app.close()
            if cleanup_error is not None:
                raise AssertionError(f"harness cleanup failed: {cleanup_error}")
            if survivors:
                raise AssertionError(f"harness cleanup left owned processes alive: {survivors}")

        evidence["phase"] = "piped_stdin"
        reader, writer = os.pipe()
        app = NativePty(
            binary, ["--stdin", "--capture-dir", str(fixture / "stdin-capture")],
            environment, fixture, stdin_reader=reader,
        )
        os.close(reader)
        try:
            try:
                os.write(writer, b"platform-stdin-event\n")
            except BrokenPipeError:
                if args.expect_piped_stdin == "supported":
                    raise
            os.close(writer)
            writer = -1
            if args.expect_piped_stdin == "supported":
                app.wait_bytes(b"platform-stdin-event")
                app.send(b"q")
                assert app.wait_exit() == 0
                app.assert_restored()
                result = "pass"
            else:
                assert app.wait_exit() != 0
                app.wait_bytes(b"stdin pipe capture requires isolated nonblocking descriptors")
                app.assert_termios_unchanged()
                result = "expected_unsupported"
            evidence["checks"].append({"name": "piped_stdin", "result": result})
        finally:
            if writer >= 0:
                os.close(writer)
            evidence["transcripts"]["piped_stdin"] = transcript_evidence(app)
            app.close()
    finally:
        if not marker.is_file() or not fixture.name.startswith("lvu-sol-platform-"):
            raise AssertionError(f"refusing to remove unmarked fixture {fixture}")
        shutil.rmtree(fixture)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("binary", type=pathlib.Path)
    parser.add_argument("--expect-piped-stdin", choices=("supported", "unsupported"), required=True)
    parser.add_argument("--require-installed-resources", action="store_true")
    parser.add_argument("--evidence", type=pathlib.Path, required=True)
    parser.add_argument("--inject-failure-after-command-spawn", action="store_true", help=argparse.SUPPRESS)
    args = parser.parse_args()
    binary = args.binary.resolve()
    limitations = [
        "kernel pseudo-terminal only; no Terminal.app or iTerm2 emulator",
        "mouse, OSC 52 clipboard, truecolor, and ConPTY are not exercised",
        "SIGKILL cleanup is not exercised by the acceptance path",
    ]
    if platform.system() == "Darwin":
        limitations.append("Darwin has no implemented parent-death cleanup for command sources")
    evidence = {
        "schema_version": 1, "kind": "unix_runtime_probe",
        "recorded_at_utc": datetime.now(timezone.utc).isoformat(),
        "host": platform.platform(), "machine": platform.machine(),
        "source_sha": os.environ.get("GITHUB_SHA") or command_output(["git", "rev-parse", "HEAD"]),
        "binary": str(binary),
        "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest() if binary.is_file() else None,
        "piped_stdin_expectation": args.expect_piped_stdin,
        "accepted": False, "phase": "initialization", "failed_phase": None,
        "failure": None, "checks": [], "transcripts": {}, "owned_processes": [],
        "cleanup": {"attempted": [], "survivors": []}, "limitations": limitations,
    }
    try:
        run_probe(args, evidence)
        evidence["accepted"] = True
        evidence["phase"] = "complete"
    except Exception as error:
        evidence["failed_phase"] = evidence["phase"]
        evidence["failure"] = {"type": type(error).__name__, "message": str(error)[:2048]}
    finally:
        args.evidence.parent.mkdir(parents=True, exist_ok=True)
        args.evidence.write_text(json.dumps(evidence, indent=2) + "\n", encoding="utf-8")
    if not evidence["accepted"]:
        print(
            f"native Unix runtime probe failed in {evidence['failed_phase']}: {evidence['failure']}",
            file=sys.stderr,
        )
        return 1
    print("native Unix runtime probe passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
