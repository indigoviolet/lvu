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

Sequential reopen over ordinary session windows is covered too: one window
settles fully terminated rows, quits cleanly, a row is appended while no
window is attached, and a second window reopens the same capture and cache
with byte-identical record history plus the new row exactly once.
"""

from __future__ import annotations

import json
import pathlib
import sys
import tempfile
import time

from test_enrichment_chain_pty import stop
from test_lvu_pty import PtyApp, isolated_environment

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


REOPEN_ALPHA = "seq-alpha"
REOPEN_BETA = "seq-beta"
REOPEN_WHILE_DOWN = "seq-while-down"


def launch_session(
    binary: pathlib.Path,
    root: pathlib.Path,
    capture: pathlib.Path,
    source: pathlib.Path,
) -> PtyApp:
    """Launch the same ordinary command a user opens in each terminal."""
    return PtyApp(
        binary,
        ["--capture-dir", str(capture), "--file", str(source)],
        width=150,
        height=28,
        cwd=root,
        environment=isolated_environment(root),
    )


def journal_for_capture(capture: pathlib.Path) -> pathlib.Path:
    """The durable journal for the capture's first source, resolved through
    the persisted session manifest — the same bytes a reopen must extend."""
    session_file = capture / "workspace" / "session.json"
    try:
        session = json.loads(session_file.read_text())
        source_id = session["sources"][0]["id"]
    except (OSError, ValueError, KeyError, IndexError) as error:
        raise AssertionError(f"session manifest unreadable at {session_file}: {error}")
    journal = capture / source_id / "capture.journal"
    assert journal.is_file(), f"journal missing for source {source_id}: {journal}"
    return journal


def settled_screen(
    app: PtyApp, capture: pathlib.Path, markers: list[str], description: str
) -> str:
    """Every marker visible, then the durable journal byte-stable across a
    full second with the markers still visible: fully settled.

    All reads go through the harness draining waits. text() renders the
    pyte screen, which only drain() feeds, so raw text() polling reads
    blank forever on a fresh window. Exact counts are asserted only on
    the returned screen, never while appending: a partial line is a
    record in flight, so fixtures are fully terminated before launch and
    quiescence is proven here before counting.
    """
    app.wait_until(
        lambda text: all(marker in text for marker in markers),
        f"{description}: rows {markers}",
        timeout=40,
    )
    journal = journal_for_capture(capture)
    first_size = journal.stat().st_size
    time.sleep(1.0)
    app.drain()
    text = app.text()
    missing = [marker for marker in markers if marker not in text]
    assert not missing, f"{description}: rows lost after settle: {missing}\n{text}"
    second_size = journal.stat().st_size
    assert first_size == second_size, (
        f"{description}: journal still growing {first_size}->{second_size}; "
        "not settled, refusing exact counts"
    )
    return text


def assert_shown_once(text: str, markers: list[str], description: str) -> None:
    for marker in markers:
        seen = text.count(marker)
        assert seen == 1, (
            f"{description}: {marker!r} shown {seen}x, expected exactly once "
            "(duplicates are replay, zero is loss)\n"
            f"{text}"
        )


def wait_clean_shutdown(capture: pathlib.Path, description: str) -> str:
    """Bounded last-window drain: socket unlinked and a clean shutdown in
    the worker log, each with its own deadline and evidence on failure."""
    socket, log = shared_worker_paths(capture)
    deadline = time.monotonic() + 30
    while socket.exists() and time.monotonic() < deadline:
        time.sleep(0.1)
    assert not socket.exists(), f"{description}: worker socket left behind: {socket}"
    deadline = time.monotonic() + 15
    log_text = ""
    while time.monotonic() < deadline:
        try:
            log_text = log.read_text(errors="replace")
        except OSError:
            log_text = ""
        if "clean shutdown" in log_text:
            return log_text
        time.sleep(0.2)
    raise AssertionError(f"{description}: no clean shutdown in worker log:\n{log_text}")


def run_sequential_reopen(binary: pathlib.Path) -> None:
    """A quits, a row lands while detached, B reopens the same capture.

    Same cache and durable capture (journal bytes extend A's snapshot, so
    record identities are stable and nothing replays), the new row appears
    exactly once, and both windows shut down bounded and clean.
    """
    root = pathlib.Path(tempfile.mkdtemp(prefix="lvu-shared-sequential-"))
    capture = root / "capture"
    source = root / "events.log"
    socket, log = shared_worker_paths(capture)
    markers = [REOPEN_ALPHA, REOPEN_BETA]
    source.write_text("".join(f"{marker}\n" for marker in markers))

    first = launch_session(binary, root, capture, source)
    try:
        settled = settled_screen(first, capture, markers, "A initial")
        assert_shown_once(settled, markers, "A initial")
        journal_bytes = journal_for_capture(capture).read_bytes()
        assert len(journal_bytes) > 0, "A settled rows but captured no journal bytes"
    finally:
        stop(first)
    log_text = wait_clean_shutdown(capture, "A close")
    assert len(serving_sessions(log)) == 1, (
        f"A must elect exactly one worker, log shows: {serving_sessions(log)}"
    )
    assert "clean shutdown" in log_text

    with source.open("a", encoding="utf-8") as stream:
        stream.write(f"{REOPEN_WHILE_DOWN}\n")
    all_markers = markers + [REOPEN_WHILE_DOWN]

    second = launch_session(binary, root, capture, source)
    try:
        reopened = settled_screen(second, capture, all_markers, "B reopen")
        assert_shown_once(reopened, all_markers, "B reopen")
        journal_after = journal_for_capture(capture).read_bytes()
        assert journal_after.startswith(journal_bytes), (
            "B journal must extend A's snapshot bytes byte-for-byte: same "
            f"records, stable IDs, no replay ({len(journal_bytes)} -> "
            f"{len(journal_after)})"
        )
        assert len(journal_after) > len(journal_bytes), (
            "while-down append missing from the reopened journal"
        )
    finally:
        stop(second)
    wait_clean_shutdown(capture, "B close")
    assert len(serving_sessions(log)) == 2, (
        "exactly one worker per window lifetime, no re-election churn: "
        f"{serving_sessions(log)}"
    )
    print("sequential reopen ok: settled rows, clean closes, exact stable IDs, no replay")


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

    run_sequential_reopen(binary)


if __name__ == "__main__":
    run(pathlib.Path(sys.argv[1]).resolve())
