#!/usr/bin/env python3
"""Long capture paths must start, show records, and clean up, on the real binary.

The control socket is derived short even when
`<capture>/shared-worker/control.sock` would exceed `sockaddr_un` byte
limits. Before that, a legitimate long capture path failed startup: the
child could not bind, exited at once, `ensure_worker` re-spawned about
every 25ms into ~100 zombie children, and the parent failed `no worker
elected ... within 5s`. The `p * 100` nesting keeps the socket string
past 120 bytes under any TMPDIR, so this fails without the derivation.

Cleanup is proven at the product boundary, not by guessing the hashed
socket path: after quit, the durable worker log must record exactly one
serving session, no bind failure, and `clean shutdown` (logged only
after the wiring drops the election and unlinks its socket), and the
overlong direct socket must never have been created.

Run with `uv run --project tests/pty python
tests/pty/test_long_capture_path_pty.py <lvu-app>`.
"""

from __future__ import annotations

import pathlib
import sys
import time

from test_enrichment_chain_pty import stop
from test_lvu_pty import (
    PtyApp,
    isolated_environment,
    release_scratch,
    scratch_root,
)


def wait_for_log_text(path: pathlib.Path, needle: str, timeout: float, description: str) -> str:
    """Bounded poll of a log file the worker appends asynchronously."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            text = path.read_text(errors="replace")
        except OSError:
            text = ""
        if needle in text:
            return text
        time.sleep(0.1)
    raise AssertionError(
        f"timed out waiting for {description} in {path} after {timeout:.0f}s:\n{text[-2000:]}"
    )


def run(binary: pathlib.Path) -> None:
    root = scratch_root()
    (root / ".lvu-test-reproducer").touch()
    print(f"fixture root: {root}", flush=True)
    deep = root / ("p" * 100)
    deep.mkdir()
    capture = deep / "capture"
    direct = capture / "shared-worker" / "control.sock"
    assert len(str(direct)) > 120, (
        f"fixture must keep the direct socket past 120 bytes, got {len(str(direct))}: {direct}"
    )
    print(f"direct socket string: {len(str(direct))} bytes", flush=True)
    source = deep / "events.log"
    source.write_text("long-path-marker-one\n")
    worker_log = capture / "shared-worker" / "worker.log"
    app = PtyApp(
        binary,
        ["--capture-dir", str(capture), "--file", str(source)],
        width=150,
        height=28,
        cwd=deep,
        environment=isolated_environment(root),
    )
    try:
        # Default 3s budget: the startup shape that failed in 5s before.
        app.wait_for("long-path-marker-one")
        print("LONG PATH STARTUP ok: initial record within the 3s budget", flush=True)
        if app.process.poll() is None:
            stop(app)
        # The wiring logs `clean shutdown` only after dropping the
        # election and unlinking its (indirect) socket.
        log_text = wait_for_log_text(worker_log, "clean shutdown", 20.0, "clean shutdown")
        assert log_text.count("serving session") == 1, (
            "exactly one worker must have served (no re-election storm):\n"
            f"{log_text[-2000:]}"
        )
        assert "cannot bind" not in log_text, (
            f"no bind failure may appear in the log:\n{log_text[-2000:]}"
        )
        assert not direct.exists(), (
            f"overlong direct socket must never be created: {direct}"
        )
        print("LONG PATH CLEANUP ok: one session, no bind failure, socket unlinked", flush=True)
    finally:
        # Removes this exact marked root unless LVU_PTY_KEEP_SCRATCH is
        # set, in which case the fixture (and its worker log) is retained
        # for forensics.
        release_scratch(root)
    print("LONG PATH PASS", flush=True)


if __name__ == "__main__":
    run(pathlib.Path(sys.argv[1]).resolve())
