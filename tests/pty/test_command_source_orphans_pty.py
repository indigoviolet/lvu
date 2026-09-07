#!/usr/bin/env python3
"""A command source must not outlive the app that started it.

lvu puts each command source in its own process group and kills that group when
a source stops, which covers every orderly path. Nothing covered the disorderly
ones: `SIGKILL` runs no cleanup, so an app that was killed — by a test harness,
by an operator, by the OOM killer — left its `sh` and whatever it had spawned
running forever. A day of PTY runs left 87 such loops on the development
machine, and the same hole means `kill -9 lvu` would leave a user's follow
command running.
"""

from __future__ import annotations

import os
import pathlib
import shlex
import signal
import sys
import tempfile
import time

from test_lvu_pty import PtyApp, isolated_environment


def shell_for(pid_file: pathlib.Path) -> str:
    """A command source that reports its pid and keeps a child of its own."""
    return (
        "printf 'orphan probe running\\n'; "
        f"printf '%s\\n' $$ > {shlex.quote(str(pid_file))}; "
        "while :; do sleep 1; done"
    )


def alive(pid: int) -> bool:
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    return True


def wait_gone(pid: int, description: str, timeout: float = 15.0) -> None:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if not alive(pid):
            return
        time.sleep(0.05)
    raise AssertionError(
        f"{description}: command source pid {pid} outlived the app it belonged to"
    )


def start(binary: pathlib.Path, root: pathlib.Path, name: str) -> tuple[PtyApp, int]:
    pid_file = root / f"{name}.pid"
    app = PtyApp(
        binary,
        ["--capture-dir", str(root / f"{name}-capture"), "--command", shell_for(pid_file)],
        width=110,
        height=26,
        environment=isolated_environment(root),
    )
    app.wait_for("orphan probe running", timeout=20.0)
    deadline = time.monotonic() + 10.0
    while not pid_file.is_file() and time.monotonic() < deadline:
        time.sleep(0.02)
    assert pid_file.is_file(), "the command fixture never reported its pid"
    command_pid = int(pid_file.read_text().strip())
    assert alive(command_pid), "the command fixture is not running"
    return app, command_pid


def wait_session_drains(app: PtyApp, description: str, timeout: float = 15.0) -> None:
    """Nothing lvu started may still be running, not only its direct child.

    A killed shell's own children are reparented rather than signalled, so they
    outlive it for as long as whatever they were doing takes. What must not
    happen is that anything is still there afterwards.
    """
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        remaining = app.session_processes()
        if not remaining:
            return
        time.sleep(0.05)
    raise AssertionError(
        f"{description}: {app.session_processes()} still running in the app's session"
    )


# Longer than tokio's idle keep-alive for blocking-pool threads, so a spawn that
# moved onto a thread which retires would show up here as a command that dies on
# its own.
SPURIOUS_DEATH_WATCH_SECONDS = 20.0


def run(binary: pathlib.Path) -> None:
    with tempfile.TemporaryDirectory(prefix="lvu-orphan-pty-") as directory:
        root = pathlib.Path(directory)

        # The parent-death signal is armed against the thread that forked, not
        # the process. That is only safe while the forking thread lives as long
        # as the app: today it is a scheduler worker of the app's own runtime,
        # which is created at startup and joined at shutdown. Move the spawn
        # onto a pool thread that retires and every command source would be
        # killed the moment that thread went idle, which is indistinguishable
        # from a command source flaking under load.
        app, command_pid = start(binary, root, "idle")
        try:
            deadline = time.monotonic() + SPURIOUS_DEATH_WATCH_SECONDS
            while time.monotonic() < deadline:
                app.drain()
                assert alive(command_pid), (
                    "the command source was killed while the app was still "
                    "running: the parent-death signal is armed against a thread "
                    "that does not live as long as the app"
                )
                time.sleep(0.25)
            app.send(b"q")
            assert app.wait_exit(timeout=15.0) == 0
            wait_gone(command_pid, "after an idle period then a clean quit")
        finally:
            if app.process.poll() is None:
                app.process.kill()
                app.process.wait()
            app.close()

        # An orderly quit stops the source, which kills its whole group.
        app, command_pid = start(binary, root, "quit")
        try:
            app.send(b"q")
            assert app.wait_exit(timeout=15.0) == 0
            app.assert_restored()
            wait_gone(command_pid, "after a clean quit")
            wait_session_drains(app, "after a clean quit")
        finally:
            app.close()

        # A signalled app runs its shutdown path.
        app, command_pid = start(binary, root, "sigterm")
        try:
            app.process.send_signal(signal.SIGTERM)
            app.wait_exit(timeout=15.0)
            wait_gone(command_pid, "after SIGTERM")
            wait_session_drains(app, "after SIGTERM")
        finally:
            if app.process.poll() is None:
                app.process.kill()
                app.process.wait()
            app.close()

        # A killed app runs nothing at all. Only the kernel can clean up here,
        # which is what the parent-death signal is for.
        app, command_pid = start(binary, root, "sigkill")
        try:
            app.process.kill()
            app.process.wait(timeout=15.0)
            wait_gone(command_pid, "after SIGKILL")
            wait_session_drains(app, "after SIGKILL")
        finally:
            app.close()

    print(
        "Command orphan PTY passed: command sources are gone after a clean quit, "
        "after SIGTERM and after SIGKILL of the app"
    )


if __name__ == "__main__":
    run(pathlib.Path(sys.argv[1]).resolve())
