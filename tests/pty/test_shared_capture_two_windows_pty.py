#!/usr/bin/env python3
"""Two real lvu-app windows sharing one background capture worker.

DRAFT — written source-only, never executed: queued behind the v0.1.6
full gate (no concurrent builds) and a built lvu-app binary. Run with
`uv run --project tests/pty python tests/pty/test_shared_capture_two_windows_pty.py <lvu-app>`.

Current scope is the window-side lifecycle only (spawn, attach, drain):
both windows run hidden `--shared`, which attaches to the worker and then
reports the pending handle seam instead of starting a session. No local
manager or store ever starts on that path, so the assertions below prove
single-worker election, shared attachment, and clean detach — without any
split-brain ownership. Session assertions (shared views over one capture
in both windows) plug in once the remote handle seam lands.
"""

from __future__ import annotations

import pathlib
import sys
import tempfile
import time

from test_lvu_pty import PtyApp

SEAM_MESSAGE = "remote handle seam"
SOCKET_RELATIVE = pathlib.PurePath("shared-worker/control.sock")
LOG_RELATIVE = pathlib.PurePath("shared-worker/worker.log")


def launch(binary: pathlib.Path, root: pathlib.Path, capture: pathlib.Path) -> PtyApp:
    return PtyApp(
        binary,
        ["--shared", "--capture-dir", str(capture)],
        width=88,
        height=24,
        cwd=root,
        environment={
            "LVU_NO_DELIGHT": "1",
            "XDG_CONFIG_HOME": str(root / "config"),
            "XDG_DATA_HOME": str(root / "data"),
            "XDG_CACHE_HOME": str(root / "cache"),
        },
    )


def shared_worker_paths(capture: pathlib.Path) -> tuple[pathlib.Path, pathlib.Path]:
    return capture / SOCKET_RELATIVE, capture / LOG_RELATIVE


def serving_sessions(log: pathlib.Path) -> list[str]:
    return [
        line
        for line in log.read_text().splitlines()
        if "serving session" in line
    ]


def run(binary: pathlib.Path) -> None:
    root = pathlib.Path(tempfile.mkdtemp(prefix="lvu-shared-two-windows-"))
    capture = root / "capture"
    socket, log = shared_worker_paths(capture)

    # Window A cold-attaches: elects and spawns the worker, drains, exits
    # with the pending-seam report (no session yet, by explicit design).
    first = launch(binary, root, capture)
    first.wait_for(SEAM_MESSAGE, timeout=20)
    assert first.wait_exit(timeout=15) == 1
    first.close()
    assert socket.exists(), "worker socket missing after first window"
    sessions = serving_sessions(log)
    assert len(sessions) == 1, f"expected one elected worker, log shows: {sessions}"

    # Window B attaches to the SAME worker: no second election, same drain.
    second = launch(binary, root, capture)
    second.wait_for(SEAM_MESSAGE, timeout=20)
    assert second.wait_exit(timeout=15) == 1
    second.close()
    assert serving_sessions(log) == sessions, "second window must not elect again"

    # Both windows detached: the worker notices the empty audience past
    # grace and unlinks its socket after a clean shutdown.
    deadline = time.monotonic() + 30
    while socket.exists() and time.monotonic() < deadline:
        time.sleep(0.1)
    assert not socket.exists(), "worker never unlinked its socket after both drains"
    assert "clean shutdown" in log.read_text(), "worker log must record clean shutdown"


if __name__ == "__main__":
    run(pathlib.Path(sys.argv[1]).resolve())
