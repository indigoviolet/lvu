#!/usr/bin/env python3
"""Autonomous real-PTY workflows for the standalone lvu binary."""

from __future__ import annotations

import argparse
import atexit
import codecs
import contextlib
import errno
import fcntl
import os
import pathlib
import pty
import re
import shutil
import signal
import stat
import struct
import subprocess
import sys
import tempfile
import termios
import time

import pyte
from wcwidth import wcwidth


# --- Shared scratch and toolchain policy -------------------------------------
#
# PTY suites need per-run isolation of *lvu's own* state: the capture directory,
# the workspace database and the XDG config/data/cache/state roots. They do not
# need a private toolchain. Pointing XDG_DATA_HOME at a private directory
# silently relocates mise's data directory too, so every run reinstalled Node
# (~230 MB) into scratch that nothing ever removed. Pin the toolchain and cache
# directories to the real per-user locations here, at import time, so that every
# suite importing PtyApp inherits the policy even when it later overrides
# XDG_* for the child. Explicit values in the environment always win.

TOOLCHAIN_DIRECTORIES = (
    ("MISE_DATA_DIR", "XDG_DATA_HOME", ".local/share", "mise"),
    ("MISE_CONFIG_DIR", "XDG_CONFIG_HOME", ".config", "mise"),
    ("MISE_CACHE_DIR", "XDG_CACHE_HOME", ".cache", "mise"),
    ("UV_CACHE_DIR", "XDG_CACHE_HOME", ".cache", "uv"),
)

SCRATCH_PREFIX = "lvu-pty-scratch-"
# Only names this harness generates are ever swept: the prefix plus the exact
# mkdtemp suffix shape. Proof archives, previews, capture directories and cargo
# target directories do not match and are never inspected.
_SCRATCH_NAME = re.compile(r"^" + re.escape(SCRATCH_PREFIX) + r"[A-Za-z0-9_]{8}$")
_ABANDONED_SCRATCH_SECONDS = 3 * 3600
_SWEEP_LIMIT = 256

_owned_scratch: list[pathlib.Path] = []
_cleanup_installed = False


def toolchain_environment() -> dict[str, str]:
    """Real per-user toolchain/cache directories, honouring existing overrides."""
    home = pathlib.Path.home()
    resolved = {}
    for variable, xdg, fallback, suffix in TOOLCHAIN_DIRECTORIES:
        existing = os.environ.get(variable)
        if existing:
            resolved[variable] = existing
            continue
        base = os.environ.get(xdg) or str(home / fallback)
        resolved[variable] = str(pathlib.Path(base) / suffix)
    return resolved


def pin_toolchain_environment() -> None:
    """Make the toolchain locations explicit so private XDG roots cannot move them."""
    for variable, value in toolchain_environment().items():
        os.environ.setdefault(variable, value)


def isolated_environment(root: pathlib.Path) -> dict[str, str]:
    """Per-run lvu state under `root`, with the shared toolchain left in place."""
    environment = {
        "XDG_CONFIG_HOME": str(root / "config"),
        "XDG_DATA_HOME": str(root / "data"),
        "XDG_CACHE_HOME": str(root / "cache"),
        "XDG_STATE_HOME": str(root / "state"),
    }
    environment.update(toolchain_environment())
    return environment


def keeping_scratch() -> bool:
    # Presence-based, matching the product's own flag convention.
    return "LVU_PTY_KEEP_SCRATCH" in os.environ


def scratch_root() -> pathlib.Path:
    """A per-run temporary root that is removed however this process ends."""
    _install_scratch_cleanup()
    root = pathlib.Path(tempfile.mkdtemp(prefix=SCRATCH_PREFIX))
    _owned_scratch.append(root)
    return root


def release_scratch(root: pathlib.Path) -> None:
    """Remove one owned scratch root, or report it when it is being retained."""
    if root not in _owned_scratch:
        return
    _owned_scratch.remove(root)
    if keeping_scratch():
        print(f"LVU_PTY_KEEP_SCRATCH: retained {root}", file=sys.stderr, flush=True)
        return
    shutil.rmtree(root, ignore_errors=True)


@contextlib.contextmanager
def scratch_directory():
    """Scratch root context manager; cleans up on success, failure and exception."""
    root = scratch_root()
    try:
        yield root
    finally:
        release_scratch(root)


def cleanup_scratch_roots() -> None:
    for root in list(_owned_scratch):
        release_scratch(root)


def _terminate(number, frame) -> None:  # pragma: no cover - signal path
    cleanup_scratch_roots()
    signal.signal(number, signal.SIG_DFL)
    os.kill(os.getpid(), number)


def _install_scratch_cleanup() -> None:
    global _cleanup_installed
    if _cleanup_installed:
        return
    _cleanup_installed = True
    # atexit covers normal exit, assertion failures and uncaught exceptions
    # including KeyboardInterrupt; SIGTERM/SIGHUP otherwise bypass it.
    atexit.register(cleanup_scratch_roots)
    for number in (signal.SIGTERM, signal.SIGHUP):
        try:
            signal.signal(number, _terminate)
        except (ValueError, OSError):  # pragma: no cover - non-main thread
            pass


def sweep_abandoned_scratch(limit: int = _SWEEP_LIMIT) -> int:
    """Remove this harness's own scratch roots left behind by earlier runs.

    Bounded, name-matched and age-gated. Anything that is not a directory whose
    name this harness generates, is younger than the threshold, is a symlink, or
    is owned by another user is left untouched.
    """
    removed = 0
    parent = pathlib.Path(tempfile.gettempdir())
    cutoff = time.time() - _ABANDONED_SCRATCH_SECONDS
    try:
        entries = sorted(parent.iterdir())[:limit]
    except OSError:
        return 0
    for entry in entries:
        if not _SCRATCH_NAME.match(entry.name) or entry in _owned_scratch:
            continue
        try:
            info = entry.lstat()
        except OSError:
            continue
        if not stat.S_ISDIR(info.st_mode) or info.st_uid != os.getuid():
            continue
        if info.st_mtime > cutoff:
            continue
        try:
            shutil.rmtree(entry)
        except OSError as error:
            if error.errno not in (errno.ENOENT, errno.EACCES, errno.EPERM):
                raise
            continue
        removed += 1
    return removed


pin_toolchain_environment()
if "LVU_PTY_NO_SWEEP" not in os.environ:
    sweep_abandoned_scratch()


class PtyApp:
    def __init__(
        self,
        binary: pathlib.Path,
        arguments: list[str],
        width: int = 88,
        height: int = 24,
        cwd: pathlib.Path | None = None,
        environment: dict[str, str] | None = None,
    ) -> None:
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
            cwd=cwd,
            env=None if environment is None else {**os.environ, **environment},
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
            self.stream.feed(self.decoder.decode(data))
            # Full redraws can request the cursor again. Count requests across
            # chunk boundaries instead of answering only the startup query.
            requests = self.transcript.count(b"\x1b[6n")
            answered = getattr(self, "cursor_responses", 0)
            for _ in range(answered, requests):
                cursor = self.screen.cursor
                os.write(self.master, f"\x1b[{cursor.y + 1};{cursor.x + 1}R".encode())
            self.cursor_responses = requests

    def text(self) -> str:
        # pyte 0.8.2 can leave an empty wide-character stub when its leading
        # cell is overwritten. Real terminals display the orphan as blank;
        # Screen.display instead indexes char[0] and raises IndexError.
        lines = []
        for y in range(self.screen.lines):
            pieces = []
            skip_stub = False
            for x in range(self.screen.columns):
                if skip_stub:
                    skip_stub = False
                    continue
                value = self.screen.buffer[y][x].data or " "
                pieces.append(value)
                skip_stub = wcwidth(value[0]) == 2
            lines.append("".join(pieces))
        return "\n".join(lines)

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


def run_story(binary: pathlib.Path, environment: dict[str, str]) -> None:
    app = PtyApp(binary, ["--demo"], environment={**environment, "LVU_NO_DELIGHT": "1"})
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
        app.send(b"\x1b[<0;24;3M\x1b[<0;24;3m")
        app.assert_remains("stable display id: api:16", "stable display id: api:6")
        app.send(b"\x1b[<0;24;4M\x1b[<0;24;4m")
        selected = app.wait_for("stable display id: api:6")
        assert "HISTORY" in selected

        # Sidebar hitboxes select the view row, not its source/health rows.
        app.send(b"\x1b[<0;5;8M\x1b[<0;5;8m")
        app.wait_until(
            lambda text: "Errors only" in text and "fixture queue unavailable" in text,
            "mouse-selected errors view",
        )
        app.send(b"\x1b[<0;5;5M\x1b[<0;5;5m")
        app.wait_for("stable display id: api:6")
        app.send(b"\t")  # selector focus back to logs

        app.send(b"?")
        app.wait_for("EVERYWHERE")
        app.send(b"\x1b[<65;24;4M")  # modal owns mouse; underlying row is unchanged
        app.send(b"?")
        app.wait_for("stable display id: api:6")

        # Default search is a live, literal constraint. While no fixture row
        # matches, the previous stable selection remains available for restore.
        app.send(b"/")
        app.wait_for("┌ Search")
        app.send(b"\x1b[200~late fixture\x1b[201~")
        searched = app.wait_until(
            lambda text: "Applied  late fixture" in text
            and 'search:"late fixture"' in text,
            "debounced literal search completion",
        )
        assert "0-0/0" in searched
        app.send(b"\x1b")
        app.wait_for("No matches. Clear the search")
        app.send(b"a")
        arrived = app.wait_until(
            lambda text: "late fixture arrival 17" in text and "1-1/1" in text,
            "late arrival continuing through active search",
        )
        assert 'search:"late fixture"' in arrived

        # Clearing only the text constraint restores all original rows and the
        # stable pre-search selection. The editor remains deliberately simple.
        app.send(b"/")
        app.send(b"\x7f" * len("late fixture"))
        restored = app.wait_until(
            lambda text: "No filter applied." in text
            and "1-11/17" in text
            and 'search:"late fixture"' not in text,
            "clear search restoring fixture rows",
        )
        app.send(b"\x1b")
        app.wait_until(
            lambda text: "┌ Search" not in text,
            "search editor close",
        )
        app.wait_for("stable display id: api:6")

        # Advanced Polars remains a separate, honestly unwired demo adapter.
        app.send(b"p")
        app.wait_for("Advanced filter")
        app.send(b'\x1b[200~level == "ERROR"\x1b[201~')
        app.send(b"\r")
        rejected = app.wait_for("advanced Polars adapter is not wired")
        assert "Error" in rejected
        app.send(b"\x1b")
        app.wait_until(lambda text: "Advanced filter" not in text, "editor close")

        app.send(b"G")
        app.wait_for("stable display id: api:17")
        app.send(b"f")
        app.wait_for("HISTORY")
        app.send(b"a")
        frozen = app.wait_for("/18")
        assert "stable display id: api:17" in frozen
        app.send(b"f")
        app.wait_for("stable display id: api:18")

        app.resize(88, 12)
        app.wait_until(
            lambda text: "stable display id: api:18" in text and "late fixture arrival 18" in text,
            "resized details layout retaining visible newest selection",
        )
        app.send(b"d")
        without_details = app.wait_until(
            lambda text: "Selected event details" not in text and "late fixture arrival 18" in text,
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


def run_panic_probe(binary: pathlib.Path, environment: dict[str, str]) -> None:
    app = PtyApp(binary, ["--demo-panic-restoration-probe"], environment=environment)
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
    # Each story gets its own lvu state (XDG config/data/cache/state and any
    # workspace database underneath). The scratch root is removed on success,
    # on assertion failure, on exception and on termination.
    for story in (run_story, run_panic_probe):
        with scratch_directory() as root:
            story(binary, isolated_environment(root))
    print("PTY workflows passed: full viewport, input, resize, async UI, normal/panic restoration")


if __name__ == "__main__":
    main()
