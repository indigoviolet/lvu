#!/usr/bin/env python3
"""Real-source PTY workflow for the temporary lvu-app composition binary."""

from __future__ import annotations

import argparse
import json
import os
import pathlib
import shlex
import tempfile
import termios
import time
import subprocess

from test_enrichment_chain_pty import paste
from test_lvu_pty import ADVANCED_TAB, FILTER_TITLE, open_advanced_filter
from test_lvu_pty import PtyApp as _HarnessPtyApp

# This suite isolates *per story*, not per process: `main` points the XDG roots
# at a fresh directory before each story so one story's workspace, cache and
# investigations cannot be mistaken for the next one's. It does that through
# `os.environ`, which the harness cannot see — `isolated_launch` fills in its
# own process-wide roots for any XDG key a launch does not set explicitly, and
# those then win over `os.environ` in the child. The app wrote its single
# investigation record under the harness root while the assertions globbed the
# story root and found none. Passing the story's roots on every launch keeps
# the app and the assertions addressing the same directories.
_STORY_XDG: dict[str, str] = {}


def dialog_text(screen: str, title: str) -> str:
    """Visible text inside one dialog, excluding borders and background panes."""
    lines = screen.splitlines()
    marker = f"┌ {title}"
    dialogs = []
    for index, header in enumerate(lines):
        if marker not in header:
            continue
        left = header.index(marker)
        right = header.find("┐", left)
        if right < 0:
            return ""
        interior = []
        for line in lines[index + 1:]:
            if len(line) > left and line[left] == "└":
                break
            interior.append(line[left + 1:right].replace("│", " "))
        dialogs.append("\n".join(interior))
    return "\n<FRAME>\n".join(dialogs)


def contains_wrapped_words(screen: str, title: str, expected: str) -> bool:
    """Match full prose while treating terminal wrapping as whitespace."""
    return expected in " ".join(dialog_text(screen, title).split())


def assert_wrapped_remedy_matcher() -> None:
    wrapped = """┌ Ask 🧠 ──────────────────────┐
│ not installed                 │
│ or not on PATH                │
│ mise install node@26.8.1      │
└───────────────────────────────┘"""
    assert contains_wrapped_words(wrapped, "Ask 🧠", "not installed or not on PATH")
    assert not contains_wrapped_words(wrapped, "Ask 🧠", "install npm@26.8.1")
    missing_remedy = wrapped.replace("mise install node@26.8.1", "")
    assert not contains_wrapped_words(
        missing_remedy, "Ask 🧠", "mise install node@26.8.1"
    )
    wrong = wrapped.replace("or not on PATH", "but available on PATH")
    assert not contains_wrapped_words(wrong, "Ask 🧠", "not installed or not on PATH")


def cycle_to(app, predicate, description: str, key: bytes = b"]", limit: int = 6):
    """Step through views with `[`/`]` until one satisfies `predicate`.

    Every source now keeps a permanent All events view, so how many views sit
    between two named ones depends on the sources, and a restart reopens
    whichever view was last in use. The destination is what these stories mean;
    the number of presses to reach it is not part of the behaviour under test.
    """
    for _ in range(limit):
        if predicate(app.text()):
            return app.text()
        app.send(key)
        try:
            return app.wait_until(predicate, description, timeout=2.0)
        except AssertionError:
            continue
    return app.wait_until(predicate, description, timeout=8.0)


def shows_path(screen: str, label: str, path: str) -> bool:
    """Whether `label` on screen names `path`, allowing an elided tail.

    A dialog truncates a value too long for its width with a trailing `…`
    (dialog-system.md §9), and a snapshot directory ends in a UUID, so the part
    that gets dropped is the part that identifies it. What is being asserted is
    that the dialog names *this* snapshot, not how much of it fits, so the
    shown value is matched as a prefix of the recorded one.
    """
    marker = f"{label}: "
    for line in screen.splitlines():
        if marker not in line:
            continue
        shown = line.split(marker, 1)[1]
        # Stop at the next fact on the row and at the dialog border, then drop
        # the ellipsis the truncation added.
        shown = shown.split("·")[0].split("│")[0].strip().rstrip("…")
        if shown and path.startswith(shown):
            return True
    return False


def story_environment(extra: dict[str, str] | None = None) -> dict[str, str]:
    """This story's XDG roots, plus whatever the launch asked for."""
    return {**_STORY_XDG, **(extra or {})}


class PtyApp(_HarnessPtyApp):
    """A launch that runs against the current story's XDG roots."""

    def __init__(self, *args, **kwargs):
        kwargs["environment"] = story_environment(kwargs.get("environment"))
        super().__init__(*args, **kwargs)


def wait_reaped(pid: int, timeout: float = 18.0) -> None:
    # Reaping follows the worker lifecycle, not the app exit: after the
    # last detach the worker honors WORKER_SHUTDOWN_GRACE (10s, lvu-shared
    # lib.rs) before exiting, and only then do its owned command children
    # die and get reaped (bounded by the manager graceful stop, 5s
    # default). The bound is grace + stop + scheduling margin — an
    # immediate reap would race the documented grace by design.
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
    """Return to the workspace, then quit.

    `q` is text while an editor has focus, so a story that left a search or a
    dialog open — or whose Escape had not been drawn yet when the next key was
    sent — types a `q` into it and waits for an exit that was never requested.
    Both slow-exit failures caught under load were this: one screen showed
    `Applied   q` in the search editor, the other an open Fields dialog. Escape
    closes one layer, so a bounded number of them reaches the workspace from any
    nesting, and if `q` still does not quit the assertion below still says so.
    """
    for _ in range(3):
        if app.process.poll() is not None:
            break
        app.send(b"\x1b")
        time.sleep(0.15)
        app.drain()
    if app.process.poll() is None:
        app.send(b"q")
    assert app.wait_exit(timeout=8.0) == 0
    app.assert_restored()


def enter_source_dialog(app: PtyApp) -> None:
    screen = app.wait_until(lambda text: "PRESS ANY KEY" in text or "Add source" in text,
                            "startup title or source dialog", timeout=8.0)
    if "PRESS ANY KEY" in screen:
        app.send(b"\x1b")
    app.wait_for("Add source")


def activate_source_mode(app: PtyApp, label: str) -> None:
    screen = app.wait_until(
        lambda text: "Manual" in text and "Discover" in text and label in text,
        f"visible source mode control {label}",
    )
    y, row = next((y, row) for y, row in enumerate(screen.splitlines())
                  if "Manual" in row and "Discover" in row and label in row)
    x = row.index(label)
    app.send(f"\x1b[<0;{x + 1};{y + 1}M\x1b[<0;{x + 1};{y + 1}m".encode())


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
            # remains independent from the command view. The open Filter
            # dialog covers the matching row, so status is awaited first,
            # the dialog is closed, and only then are rows asserted.
            app.send(b"/")
            app.wait_for(FILTER_TITLE)
            time.sleep(0.3)  # fresh dialog takes focus a tick after its frame; first keystroke would be lost
            app.send(b"beta")
            app.send(b"\r")
            app.wait_until(
                lambda text: 'search:"beta"' in text and "matched 1/" in text,
                "native literal search accepted",
                timeout=8.0,
            )
            app.send(b"\x1b")
            # Wait for the dialog to be gone before the next key: Esc and `]`
            # landing in one read parse as Alt-] (see test_search_race_pty),
            # which the editor ignores, and the view never switches. The old
            # marker here named text that was never on screen, so it guarded
            # nothing.
            app.wait_until(
                lambda text: FILTER_TITLE not in text,
                "literal editor closed",
            )
            settled = app.wait_until(
                lambda text: "file beta" in text and "file alpha" not in text,
                "native literal search rows after close",
            )
            assert 'search:"beta"' in settled

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
            open_advanced_filter(app)
            app.send(b"\x1b[200~" + advanced.encode() + b"\x1b[201~")
            app.send(b"\r")
            app.wait_until(
                lambda text: "Applied   " + advanced in text and "advanced:on" in text,
                "accepted advanced AND literal constraints",
                timeout=12.0,
            )
            app.send(b"\x1b")
            combined = app.wait_until(
                lambda text: FILTER_TITLE not in text
                and "file beta late" in text,
                "accepted advanced AND rows",
            )
            assert "file alpha" not in combined
            assert "file gamma hidden" not in combined

            # Invalid advanced input keeps the last valid literal view active;
            # later arrivals still flow through that accepted constraint.
            open_advanced_filter(app)
            app.send(b"\x7f" * len(advanced))
            app.send(b"pl.col(")
            app.send(b"\r")
            app.wait_for("compiler rejected expression", timeout=8.0)
            app.send(b"\x1b")
            app.wait_until(
                lambda text: FILTER_TITLE not in text,
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
            open_advanced_filter(app)
            app.send(b"\x7f" * len("pl.col("))
            app.send(b"\r")
            app.wait_until(
                lambda text: "No filter every record is shown" in text and "advanced:on" not in text,
                "advanced constraint cleared independently",
                timeout=8.0,
            )
            app.send(b"\x1b")
            app.wait_until(
                lambda text: FILTER_TITLE not in text,
                "cleared advanced editor closed",
            )

            app.send(b"/")
            app.wait_for(FILTER_TITLE)
            time.sleep(0.3)  # fresh dialog takes focus a tick after its frame; first keystroke would be lost
            app.send(b"\x7f" * len("beta"))
            app.send(b"\r")
            app.wait_until(
                lambda text: "No filter every record is shown" in text and 'search:"beta"' not in text,
                "cleared search accepted",
            )
            app.send(b"\x1b")
            app.wait_until(lambda text: FILTER_TITLE not in text, "cleared search closed")
            restored = app.wait_until(
                lambda text: "file gamma hidden" in text
                and "file delta hidden" in text
                and 'search:"beta"' not in text,
                "cleared native search restoring raw rows",
                timeout=8.0,
            )
            assert "file alpha�" in restored

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
        # --fresh, because the point here is what the Add source dialog does
        # with a remembered identity, not what resuming the session does.
        reopened = PtyApp(
            binary,
            ["--fresh", "--capture-dir", str(capture)],
            width=100,
            height=26,
        )
        try:
            enter_source_dialog(reopened)
            reopened.send(b"\x1b[200~" + str(source).encode() + b"\x1b[201~")
            reopened.send(b"\r")
            screen = reopened.wait_until(
                lambda text: "file alpha�" in text and "file beta" in text,
                "reopened durable journal rows",
                timeout=6.0,
            )
            assert "All events" in screen
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
            first.send(b"/")
            first.wait_for(FILTER_TITLE)
            time.sleep(0.3)  # fresh dialog takes focus a tick after its frame; first keystroke would be lost
            first.send(b"beta")
            first.send(b"\r")
            first.wait_until(lambda text: 'search:"beta"' in text and "matched " in text, "accepted remembered literal", timeout=8.0)
            first.send(b"\x1b")
            first.wait_until(lambda text: FILTER_TITLE not in text, "editor closed")
            open_advanced_filter(first)
            first.send(b"\x1b[200~" + advanced.encode() + b"\x1b[201~")
            first.send(b"\r")
            first.wait_until(lambda text: "advanced:on" in text and "Applied   " + advanced in text, "accepted remembered advanced", timeout=12.0)
            first.send(b"\x1b")
            first.wait_until(lambda text: FILTER_TITLE not in text, "editor closed")
            open_advanced_filter(first)
            first.send(b"\x7f" * len(advanced))
            first.send(b"pl.col(")
            first.send(b"\r")
            first.send(b"\x1b")
            first.wait_until(lambda text: FILTER_TITLE not in text, "editor closed")
            quit_cleanly(first)
        finally:
            if first.process.poll() is None: first.process.kill()
            first.close()

        reopened = PtyApp(binary, ["--capture-dir", str(capture), "--file", str(source)], width=160, height=25)
        try:
            restored = reopened.wait_until(lambda text: 'search:"beta"' in text and "advanced:on" in text and "beta late" in text, "restored accepted constraints", timeout=12.0)
            assert "beta early" not in restored and "hidden late" not in restored
            open_advanced_filter(reopened)
            editor = reopened.wait_for("pl.col(", timeout=4.0)
            # The rejected draft's diagnostic and the accepted expression are
            # both shown, but the pane wraps them, so assert the two halves
            # rather than one contiguous run.
            assert "last accepted" in editor, editor
            assert advanced in editor, editor
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

        # --fresh: this story is about reopening a remembered source through
        # discovery, so nothing may be acquired before the dialog opens.
        recent = PtyApp(binary, ["--fresh", "--capture-dir", str(capture)], width=160, height=25)
        try:
            enter_source_dialog(recent)
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
            enter_source_dialog(app)
            app.send(b"\x04")  # Ctrl-D: discovery mode, never autonomous start.
            app.send(name.encode())
            # Which provider claims it depends on how far the bounded process
            # scan reaches: the project scan always finds the file, and the
            # process scan attributes it to the live `tee` when it gets that far.
            # Either is a correct discovery of one available candidate, and
            # pinning the weaker one made the stronger evidence look like a
            # regression.
            discovered = app.wait_until(
                lambda text: name in text
                and "1 of 1" in text
                and ("Project Medium Available" in text or "Procfs High Available" in text),
                "controlled tee/file discovery candidate",
                timeout=6.0,
            )
            assert "never starts capture" in discovered
            assert "controlled discovery content" not in discovered
            app.send(b"\r")
            selected = app.wait_for("controlled discovery content", timeout=6.0)
            assert "controlled-tee" in selected and "All events" in selected
            quit_cleanly(app)
        finally:
            if app.process.poll() is None:
                app.process.kill()
            app.close()
            tee.stdin.close()
            tee.wait(timeout=4.0)


def run_path_completion_story(binary: pathlib.Path) -> None:
    # Completion is automatic while typing: the "Complete path" and "Open" actions
    # were deliberately removed, so this drives suggestions and Enter alone.
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
            enter_source_dialog(app)
            app.send(b"nested sp")
            choices = app.wait_until(
                lambda text: "nested space/" in text and "nested spare/" in text,
                "ambiguous path suggestions appear without an explicit action",
            )
            assert "Path" in choices
            assert "Complete path" not in choices, "removed action reappeared"
            # The first match is already selected; Down/Up move within the list
            # directly from the input field, so return to it before accepting.
            app.send(b"\x1b[B")
            app.wait_until(lambda text: "› nested spare/" in text, "Down selects the second match")
            app.send(b"\x1b[A")
            app.wait_until(lambda text: "› nested space/" in text, "Up returns to the first match")
            app.send(b"\r")
            app.wait_for("nested space/")
            app.send("üb".encode())
            app.wait_until(
                lambda text: "über events.log" in text,
                "Unicode suggestion offered while typing",
            )
            app.send(b"\r")
            captured = app.wait_for("completed path content", timeout=8.0)
            assert "All events" in captured
            quit_cleanly(app)
        finally:
            if app.process.poll() is None:
                app.process.kill()
            app.close()

def run_field_presentation_story(binary: pathlib.Path) -> None:
    with tempfile.TemporaryDirectory(prefix="lvu-fields-pty-") as temporary:
        root = pathlib.Path(temporary)
        source = root / "structured.log"
        source.write_text(
            "malformed raw stays visible\n"
            'level=warn service=api request_id=same message="logfmt event"\n'
            '{"level":"error","service":"api","request_id":"same","message":"json event"}\n'
        )
        arguments = ["--capture-dir", str(root / "capture"), "--file", str(source)]
        app = PtyApp(binary, arguments, width=140, height=28, cwd=root)
        try:
            screen = app.wait_for("json event", timeout=8.0)
            assert "malformed raw stays visible" in screen
            app.send(b"d")
            # §8.11: the tree shows the record's own bytes, quotes included.
            details = app.wait_for('request_id: "same"')
            assert "raw: {" in details and 'service: "api"' in details
            app.send(b"d")  # Close the focused Details pane before opening Fields.
            app.send(b"i")
            app.wait_for("Fields · record")
            app.send(b"\x1b[B")  # service: rows follow the record's key order
            app.send(b" ")
            app.send(b"c")
            app.send(b"\x1b")
            pinned = app.wait_until(
                lambda text: "service" in text and "json event" in text,
                "pinned structured column",
            )
            assert "api" in pinned
            quit_cleanly(app)
        finally:
            if app.process.poll() is None:
                app.process.kill()
            app.close()

        reopened = PtyApp(binary, arguments, width=140, height=28, cwd=root)
        try:
            restored = reopened.wait_until(
                lambda text: "service" in text and "json event" in text,
                "restored pinned field",
                timeout=8.0,
            )
            assert "api" in restored
            quit_cleanly(reopened)
        finally:
            if reopened.process.poll() is None:
                reopened.process.kill()
            reopened.close()


def run_enrichment_story(binary: pathlib.Path) -> None:
    with tempfile.TemporaryDirectory(prefix="lvu-enrich-pty-") as temporary:
        root = pathlib.Path(temporary)
        source = root / "enrichment.log"
        source.write_text("malformed raw\nstatus=200 ok\nstatus=503 failed\n")
        arguments = ["--capture-dir", str(root / "capture"), "--file", str(source)]
        expression = (
            'status_code = pl.col("raw").str.extract(r"status=(\\d+)", 1)'
            ".cast(pl.Int64, strict=False)"
        )
        advanced = "pl.col('status_code') >= 500"
        app = PtyApp(binary, arguments, width=150, height=30, cwd=root)
        try:
            app.wait_for("status=503 failed", timeout=8.0)
            app.send(b"e")
            app.wait_for("┌ Enrichment ")
            # Layer two owns the step draft since the two-layer rework.
            app.send(b"\x1ba")
            app.wait_for("Enrichment › New step")
            app.send(b"\x1b[200~" + expression.encode() + b"\x1b[201~")
            app.send(b"\r")
            app.wait_until(
                lambda text: "1  status_code" in text and "enrich:on" in text,
                "native enrichment applied",
                timeout=12.0,
            )
            app.send(b"\x1b")
            app.wait_until(lambda text: "┌ Enrichment " not in text, "enrichment closed")
            app.send(b"d")
            app.wait_for("status_code: 503")
            app.send(b"d")
            app.send(b"i")
            app.wait_for("Fields · record")
            app.send(b"\x1b[B" * 3)
            app.send(b" ")
            app.send(b"\x1b")
            app.wait_until(
                lambda text: "status_code" in text
                and "status=503 failed" in text
                # The pinned column shows behind the dialog while it is still
                # closing; the next key must not land in Fields.
                and "Fields · record" not in text,
                "derived field pinned",
            )
            open_advanced_filter(app)
            app.send(b"\x1b[200~" + advanced.encode() + b"\x1b[201~")
            app.send(b"\r")
            app.wait_for("advanced:on", timeout=12.0)
            app.send(b"\x1b")
            with source.open("a") as stream:
                stream.write("status=404 unmatched\nstatus=500 late\n")
                stream.flush()
                os.fsync(stream.fileno())
            filtered = app.wait_for("status=500 late", timeout=10.0)
            assert "status=404 unmatched" not in filtered

            app.send(b"e")
            app.wait_for("┌ Enrichment ")
            app.send(b"\x1ba")
            app.wait_for("Enrichment › New step")
            app.send(b"\x01\x0b")  # Ctrl-A then Ctrl-K clears any resumed draft
            invalid = "status_code = pl.col("
            app.send(invalid.encode())
            app.send(b"\r")
            app.wait_for("compiler rejected expression", timeout=10.0)
            # One Escape per layer: the step editor, then the list (§10).
            app.send(b"\x1b")
            app.wait_until(lambda text: "New step" not in text, "step editor closed")
            app.send(b"\x1b")
            app.wait_until(lambda text: "┌ Enrichment " not in text, "invalid enrichment editor closed")
            preserved = app.wait_for("status=500 late")
            assert "status=404 unmatched" not in preserved
            quit_cleanly(app)
        finally:
            if app.process.poll() is None:
                app.process.kill()
            app.close()

        reopened = PtyApp(binary, arguments, width=150, height=30, cwd=root)
        try:
            restored = reopened.wait_until(
                lambda text: "enrich:on" in text
                and "advanced:on" in text
                and "status=500 late" in text,
                "restored accepted enrichment and filter",
                timeout=14.0,
            )
            assert "status=404 unmatched" not in restored
            reopened.send(b"e")
            listed = reopened.wait_for("┌ Enrichment ")
            # The accepted step is on the list layer; the unfinished draft is
            # resumed by the step editor.
            assert "1  status_code" in listed, listed
            reopened.send(b"\x1ba")
            draft = reopened.wait_for("Enrichment › New step")
            assert "status_code = pl.col(" in draft, draft
            reopened.send(b"\x1b")
            reopened.wait_until(lambda text: "New step" not in text, "step editor closed")
            reopened.send(b"\x1b")
            reopened.wait_until(
                lambda text: "┌ Enrichment " not in text,
                "restored enrichment editor closed",
            )
            open_advanced_filter(reopened)
            reopened.send(b"\x7f" * len(advanced))
            reopened.send(b"\r")
            reopened.wait_until(
                lambda text: "advanced:on" not in text,
                "advanced filter cleared before enrichment",
            )
            reopened.send(b"\x1b")
            reopened.wait_until(
                lambda text: FILTER_TITLE not in text,
                "advanced editor closed",
            )
            reopened.send(b"e")
            reopened.send(b"\x1br")
            reopened.wait_until(
                lambda text: "enrich:on" not in text and "No steps yet" in text,
                "enrichment cleared to raw view",
                timeout=10.0,
            )
            reopened.send(b"\x1b")
            reopened.wait_until(
                lambda text: "┌ Enrichment " not in text,
                "cleared enrichment editor closed",
            )
            reopened.wait_for("malformed raw")
            quit_cleanly(reopened)
        finally:
            if reopened.process.poll() is None:
                reopened.process.kill()
            reopened.close()


def run_editor_completion_story(binary: pathlib.Path) -> None:
    with tempfile.TemporaryDirectory(prefix="lvu-editor-completion-pty-") as temporary:
        root = pathlib.Path(temporary)
        source = root / "structured.log"
        source.write_text(
            '{"level":"INFO","message":"started"}\n'
            '{"level":"ERROR","message":"failed"}\n'
        )
        arguments = ["--capture-dir", str(root / "capture"), "--file", str(source)]
        app = PtyApp(binary, arguments, width=150, height=30, cwd=root)
        try:
            app.wait_for('"message":"failed"', timeout=8.0)

            open_advanced_filter(app)
            app.send(b"\t")
            popup = app.wait_for("Complete field")
            assert "level" in popup and "message" in popup
            app.send(b"\r")
            inserted = app.wait_for("pl.col('level')")
            assert "advanced:on" not in inserted
            app.send(b" == ")
            app.send(b"\t")
            app.wait_for("Complete field")
            app.send(b"\t")
            values = app.wait_for("Static sampled string literals")
            assert "ERROR" in values
            app.send(b"\r")
            app.wait_for("pl.col('level') == 'ERROR'")
            app.send(b"\r")
            app.wait_until(
                lambda text: "advanced:on" in text and "matched 1/2" in text,
                "completed native advanced filter accepted",
                timeout=12.0,
            )
            app.send(b"\x1b")
            filtered = app.wait_until(
                lambda text: '"message":"failed"' in text and FILTER_TITLE not in text,
                "advanced editor closed over the filtered rows",
            )
            assert '"message":"started"' not in filtered

            app.send(b"e")
            app.wait_for("┌ Enrichment ")
            app.send(b"\x1ba")
            app.wait_for("Enrichment › New step")
            app.send(b"\x01\x0b")  # clear any resumed draft
            app.send(b"copied_level = ")
            app.send(b"\x00")  # Ctrl-Space completes; Tab traverses form controls.
            app.wait_for("Complete field")
            app.send(b"\r")
            app.wait_for("copied_level = pl.col('level')")
            app.send(b"\r")
            app.wait_until(
                lambda text: "enrich:on" in text
                and "1  copied_level" in text,
                "completed native enrichment",
                timeout=12.0,
            )
            app.send(b"\x1be")
            app.wait_for("Enrichment › Edit step")
            app.send(b" + pl.lit('unfinished')")
            app.send(b"\x1b")
            app.wait_until(lambda text: "Edit step" not in text, "step editor closed")
            app.send(b"\x1b")
            app.wait_until(
                lambda text: "┌ Enrichment " not in text,
                "unfinished enrichment draft closed",
            )
            quit_cleanly(app)
        finally:
            if app.process.poll() is None:
                app.process.kill()
            app.close()

        reopened = PtyApp(binary, arguments, width=150, height=30, cwd=root)
        try:
            reopened.wait_until(
                lambda text: "advanced:on" in text
                and "enrich:on" in text
                and '"message":"failed"' in text,
                "restored completed constraints",
                timeout=14.0,
            )
            reopened.send(b"e")
            listed = reopened.wait_for("┌ Enrichment ")
            # The accepted step survives on the list; the unfinished edit is
            # resumed by the step editor.
            assert "1  copied_level" in listed, listed
            reopened.send(b"\x1be")
            restored = reopened.wait_for("Enrichment › Edit step")
            # The unfinished edit is restored with the accepted expression it
            # was derived from, not replaced by it.
            assert "copied_level = pl.col('level')" in restored, restored
            assert "unfinished" in restored, restored
            reopened.send(b"\x1b")
            reopened.wait_until(lambda text: "Edit step" not in text, "step editor closed")
            reopened.send(b"\x1b")
            reopened.wait_until(
                lambda text: "┌ Enrichment " not in text,
                "restored enrichment editor closed",
            )
            quit_cleanly(reopened)
        finally:
            if reopened.process.poll() is None:
                reopened.process.kill()
            reopened.close()


def run_named_views_story(binary: pathlib.Path) -> None:
    with tempfile.TemporaryDirectory(prefix="lvu-views-pty-") as temporary:
        root = pathlib.Path(temporary)
        capture = root / "capture"
        feed = root / "feed.log"
        feed.write_text("error first\ninfo first\n")
        shell = "printf 'startup-marker\\n'; trap 'exit 0' TERM INT; while :; do sleep 1; done"
        arguments = [
            "--capture-dir", str(capture), "--file", str(feed), "--command", shell
        ]
        app = PtyApp(binary, arguments, width=140, height=28, cwd=root)
        try:
            app.wait_for("error first", timeout=8.0)
            app.send(b"]")
            app.wait_for("startup-marker", timeout=8.0)
            app.send(b"[")
            app.wait_for("error first")

            # Clone the selected raw view, then give the clone an independent filter.
            # View modes are header segments (§8.6), not action buttons: the
            # unbracketed `Clone` segment selects the mode while `[ Apply ]`
            # stays the only button.
            app.send(b"v")
            cloned = app.wait_for("Clone")
            for button in ("[ New blank ]", "[ Clone ]", "[ Rename ]", "[ Sources ]"):
                assert button not in cloned, cloned
            assert "[ Apply ]" in cloned, cloned
            app.send(b"\x7f" * len("Copy of All events"))
            app.send(b"Errors\r")
            app.wait_for("Errors")
            app.send(b"e")
            app.wait_for("┌ Enrichment ")
            app.send(b"\x1ba")
            app.wait_for("Enrichment › New step")
            app.send(b"\x01\x0b")  # clear any resumed draft
            app.send(b'tag = pl.lit("errors")\r')
            app.wait_for("enrich:on", timeout=12.0)
            # A saved step returns to the list layer, so one Escape closes it.
            app.send(b"\x1b")
            app.wait_until(lambda text: "┌ Enrichment " not in text, "named enrichment closed")
            app.send(b"/")
            app.wait_for(FILTER_TITLE)
            time.sleep(0.3)  # fresh dialog takes focus a tick after its frame; first keystroke would be lost
            app.send(b"error")
            app.send(b"\r")
            app.wait_until(
                lambda text: 'search:"error"' in text,
                "errors view accepted",
                timeout=8.0,
            )
            app.send(b"\x1b")
            app.wait_until(
                lambda text: " Search " not in text,
                "errors editor closed",
            )
            app.wait_until(
                lambda text: "error first" in text,
                "errors view filter",
                timeout=8.0,
            )

            # A blank view starts without cloned constraints.
            app.send(b"v")
            app.send(b"\x1bb")  # Alt-B: blank tab.
            blanked = app.wait_for("New view")
            assert "[ New blank ]" not in blanked, blanked
            app.send(b"\x7f" * len("New view"))
            app.send(b"Info\r")
            info = app.wait_for("Info")
            assert 'search:"error"' not in info and "enrich:on" not in info
            app.send(b"/")
            app.wait_for(FILTER_TITLE)
            time.sleep(0.3)  # fresh dialog takes focus a tick after its frame; first keystroke would be lost
            app.send(b"info")
            app.send(b"\r")
            app.wait_until(lambda text: 'search:"info"' in text, "info filter applied", timeout=8.0)
            app.send(b"\x1b")
            app.wait_until(lambda text: " Search " not in text, "info editor closed")

            # Rename persists independently from its settings.
            app.send(b"v"); app.send(b"\x1br")
            renamed = app.wait_for("Rename")
            assert "[ Rename ]" not in renamed, renamed
            app.send(b"\x7f" * len("Info")); app.send(b"Information\r")
            app.wait_for("Information")

            with feed.open("a") as stream:
                stream.write("error second\ninfo second\n")
                stream.flush(); os.fsync(stream.fileno())
            current = app.wait_for("info second", timeout=8.0)
            assert "error second" not in current
            app.send(b"[")
            errors = app.wait_for("error second", timeout=8.0)
            assert 'search:"error"' in errors and "enrich:on" in errors and "info second" not in errors

            journals = list(capture.glob("*/capture.journal"))
            assert journals
            assert sum(path.read_bytes().count(b"startup-marker") for path in journals) == 1
            quit_cleanly(app)
        finally:
            if app.process.poll() is None: app.process.kill()
            app.close()

        reopened = PtyApp(binary, arguments, width=140, height=28, cwd=root)
        try:
            screen = reopened.wait_until(
                lambda text: "Errors" in text and "Information" in text,
                "all named views restored",
                timeout=12.0,
            )
            assert "All events" in screen
            # A restart reopens the view that was last in use, which the story
            # left on Errors, rather than the source's unfiltered view.
            reopened.wait_until(
                lambda text: 'search:"error"' in text and "enrich:on" in text,
                "restored Errors constraint",
                timeout=8.0,
            )
            reopened.send(b"]")
            reopened.wait_until(
                lambda text: 'search:"info"' in text,
                "restored Information constraint",
                timeout=8.0,
            )
            # View order after a restart is not positional: stepping `]`
            # past Information can land on the command source's raw view
            # instead of the file source's (the exact 1d888a3 parent lands
            # wrong the same way), so identify each raw view by its records
            # rather than by ordinal. `]` wraps. One absolute deadline is
            # shared by the whole cycle, so retries can never stretch past
            # the pre-existing 8s bound, and the iteration ceiling is only a
            # second bound: each wait gets just the time still remaining.
            def classify_view(text: str) -> str | None:
                if "startup-marker" in text:
                    return "command"
                if 'search:"error"' in text:
                    return "error"
                if 'search:"info"' in text:
                    return "info"
                if "error first" in text and "info first" in text:
                    return "file"
                return None

            def cycle_to(want: str, current: str) -> str:
                deadline = time.monotonic() + 8.0
                for _ in range(8):
                    reopened.send(b"]")
                    remaining = deadline - time.monotonic()
                    if remaining <= 0:
                        break
                    # Wait only for a real view different from the one just
                    # left: an unclassified starting/query-pending frame must
                    # never advance the cycle, or the destination publishes
                    # after we have already stepped past it.
                    seen = reopened.wait_until(
                        lambda text: classify_view(text) is not None
                        and classify_view(text) != current,
                        f"restored {want} raw view",
                        timeout=remaining,
                    )
                    cls = classify_view(seen)
                    if cls == want:
                        return seen
                    current = cls
                raise AssertionError(f"{want} raw view not reached by cycling restored views")

            cycle_to("file", "info")
            cycle_to("command", "file")
            quit_cleanly(reopened)
        finally:
            if reopened.process.poll() is None: reopened.process.kill()
            reopened.close()


def run_ask_ai_story(binary: pathlib.Path) -> None:
    with tempfile.TemporaryDirectory(prefix="lvu-ask-ai-pty-") as temporary:
        root = pathlib.Path(temporary)
        source = root / "agent.log"
        archive = root / "bridge-requests.jsonl"
        source.write_text(
            '{"level":"INFO","message":"ordinary"}\n'
            '{"level":"ERROR","message":"broken"}\n'
            '{"level":"WARN","message":"warning"}\n'
        )
        bridge = root / "fake-bridge.py"
        bridge.write_text(
            """#!/usr/bin/env python3
import json, os, pathlib, sys, time
archive = pathlib.Path(os.environ["FAKE_BRIDGE_ARCHIVE"])
ask_session_count = 0
session_purposes = {}
def archived(session_id):
    return {"schema_version": 1, "session_id": session_id,
            "kind": "session_archived", "activity_path": str(archive.parent / (session_id + ".activity.jsonl")),
            "archived_at": "2026-09-06T00:00:00Z"}
for line in sys.stdin:
    request = json.loads(line)
    with archive.open("a") as out:
        out.write(json.dumps(request) + "\\n")
    method = request["method"]
    if method == "start_session":
        cwd = pathlib.Path(request["cwd"])
        assert cwd.is_absolute() and cwd.is_dir()
        if request.get("title") == "lvu investigation":
            assert request.get("purpose") == "investigation"
            session_id = "session-investigation"
        else:
            assert request.get("purpose") == "ask"
            ask_session_count += 1
            session_id = "session-ask-" + str(ask_session_count)
        session_purposes[session_id] = request.get("purpose")
        result = {"session_id": session_id}
    elif method == "resume_session":
        result = {"session_id": request["session_id"], "resumed": True}
    elif method == "send_prompt":
        result = {"accepted": True}
        response = {"schema_version": 1, "request_id": request["request_id"], "ok": True, "result": result}
        print(json.dumps(response), flush=True)
        print(json.dumps({"schema_version": 1, "session_id": request["session_id"],
                          "kind": "turn_started", "payload": {}}), flush=True)
        if "slow investigation" in request["prompt"]:
            time.sleep(0.5)
        answer = "fixture follow-up complete" if "follow up" in request["prompt"] else "fixture investigation found broken"
        print(json.dumps({"schema_version": 1, "session_id": request["session_id"],
                          "kind": "turn_completed", "payload": {"status": "idle", "last_message": answer,
                          "remote_agent_may_still_be_running": False}}), flush=True)
        continue
    elif method == "request_proposal":
        context = request["context"]
        manifest = pathlib.Path(context["manifest_path"])
        assert manifest.is_absolute() and manifest.is_file()
        assert context["dataset_paths"] == []
        assert isinstance(context["inline_context"], dict)
        inline_bytes = len(json.dumps(context["inline_context"], ensure_ascii=False).encode("utf-8"))
        assert inline_bytes <= 98304, inline_bytes
        assert not list(manifest.parent.glob("*.parquet"))
        if "slow" in request["instruction"]:
            time.sleep(0.5)
        if "fixture failure" in request["instruction"]:
            response = {"schema_version": 1, "request_id": request["request_id"],
                        "ok": False,
                        "error": {"code": "FIXTURE", "message": "proposal failed"}}
            print(json.dumps(response), flush=True)
            continue
        if request["kind"] == "filter":
            definition = {"schema_version": 1, "expression": "pl.col('level') == 'ERROR'"}
        else:
            definition = {"schema_version": 1, "stages": [{
                "id": "11111111-1111-4111-8111-111111111111",
                "name": "AI level",
                "expressions": {"ai_level": "pl.col('level')"},
            }]}
        proposal = {
            "kind": request["kind"], "definition": definition,
            "explanation": "deterministic fixture proposal from " + request["session_id"],
            "originating_revision": request["originating_revision"],
        }
        # The agent's own verdict: the first turn of the wider-sample story says
        # the evidence it was given was not enough.
        if "needs more data" in request["instruction"]:
            proposal["needs_more_data"] = True
        result = {"proposal": proposal}
        response = {"schema_version": 1, "request_id": request["request_id"], "ok": True, "result": result}
        sequence = int(request["session_id"].rsplit("-", 1)[-1])
        if sequence % 2:
            print(json.dumps(archived(request["session_id"])), flush=True)
        print(json.dumps(response), flush=True)
        if sequence % 2 == 0:
            print(json.dumps(archived(request["session_id"])), flush=True)
        continue
    elif method == "cancel":
        result = {"cancelled": True, "remote_cancelled": True,
                  "remote_agent_may_still_be_running": False}
    else:
        result = {"accepted": True}
    response = {"schema_version": 1, "request_id": request["request_id"], "ok": True, "result": result}
    print(json.dumps(response), flush=True)
    if method == "cancel" and session_purposes.get(request["session_id"]) == "ask":
        print(json.dumps(archived(request["session_id"])), flush=True)
"""
        )
        bridge.chmod(0o755)
        environment = {
            "LVU_AGENT_BRIDGE_PROGRAM": str(bridge),
            "LVU_AGENT_BRIDGE_CWD": str(root),
            "FAKE_BRIDGE_ARCHIVE": str(archive),
        }
        app = PtyApp(
            binary,
            ["--file", str(source)],
            width=160,
            height=30,
            cwd=root,
            environment=environment,
        )
        try:
            app.wait_until(
                lambda text: "ordinary" in text and "query pending" not in text,
                "initial applied view before Ask",
                timeout=15.0,
            )
            # Startup restore can enqueue its first native refresh immediately
            # after the first quiet paint. Require a short stable quiet period
            # before freezing the successful-proposal fixture.
            time.sleep(0.5)
            app.wait_until(
                lambda text: "ordinary" in text and "query pending" not in text,
                "stable applied view before Ask",
                timeout=15.0,
            )
            app.send(b"A")
            app.wait_for("Ask 🧠")
            app.send(b"keep errors")
            app.send(b"\t\r")
            proposal = app.wait_until(
                lambda text: "Proposal" in text and "pl.col('level') == 'ERROR'" in text,
                "snapshot-backed filter proposal",
                timeout=15.0,
            )
            assert "session-ask-1" in proposal
            app.send(b"\r")
            app.wait_until(
                lambda text: "advanced:on" in text and "Applied   pl.col('level') == 'ERROR'" in text,
                "AI filter accepted by native query",
                timeout=15.0,
            )
            app.send(b"\x1b")
            app.wait_until(
                lambda text: FILTER_TITLE not in text,
                "advanced editor closed before opening Ask AI again",
                timeout=5.0,
            )

            # docs/larger-ask-sample.md: the sample is always stated, and an
            # answer that says it needed more offers one wider re-run.
            app.send(b"A")
            app.wait_for("Ask 🧠", timeout=5.0)
            app.send(b"needs more data")
            app.send(b"\t\r")
            widened = app.wait_until(
                lambda text: "wider sample" in text and "standard" in text,
                "the sample line and the wider offer",
                timeout=15.0,
            )
            assert "sample " in widened, widened
            app.send(b"\t")
            app.wait_until(
                lambda text: "Ask again with a wider sample" in text,
                "the wider control is reachable by Tab",
                timeout=5.0,
            )
            app.send(b"\r")
            history = app.wait_until(
                lambda text: "standard answer" in text and "wider answer" in text,
                "both bounded answers and their samples",
                timeout=15.0,
            )
            assert "session-ask-2" in history and "session-ask-3" in history, history

            # Both bounded entries remain visible in the responsive Proposal
            # pane at the supported narrow geometry; no transcript surface is
            # introduced for this two-answer history. Scrolling farther down
            # intentionally reveals Activity, so assert the answer history at
            # its initial reveal position rather than scrolling past it.
            app.resize(70, 16)
            narrow = app.wait_until(
                lambda text: "standard answer" in text and "wider answer" in text,
                "both answers in the narrow Proposal pane",
                timeout=5.0,
            )
            app.resize(160, 30)
            app.send(b"\x1b")
            app.wait_until(
                lambda text: "Ask 🧠" not in text,
                "wider-sample dialog closed",
                timeout=5.0,
            )
            filtered = app.wait_for("broken", timeout=5.0)
            assert "ordinary" not in filtered and "warning" not in filtered

            app.send(b"A")
            app.wait_for("Ask 🧠", timeout=5.0)
            app.send(b"\x1be")
            app.wait_until(
                lambda text: "Kind" in text and "Enrichment" in text,
                "the kind field shows the chosen kind",
                timeout=5.0,
            )
            app.send(b"derive a reusable level field")
            app.send(b"\t\r")
            app.wait_until(
                lambda text: "Proposal" in text and "ai_level = pl.col('level')" in text,
                "snapshot-backed enrichment proposal",
                timeout=15.0,
            )
            app.send(b"\r")
            app.wait_until(
                lambda text: "enrich:on" in text and "1  ai_level" in text,
                "AI enrichment accepted by native query",
                timeout=15.0,
            )
            app.send(b"\x1b")
            app.wait_until(
                lambda text: "┌ Enrichment " not in text,
                "native enrichment editor closed before the next command",
            )
            app.send(b"d")
            app.wait_for("ai_level: ERROR", timeout=8.0)
            app.send(b"d")
            app.wait_until(
                lambda text: "Selected event details" not in text,
                "Details closed before opening Ask again",
            )

            # A failed proposal is cancelled before its error is published.
            app.send(b"Afixture failure\t\r")
            app.wait_for("FIXTURE: proposal failed", timeout=10.0)
            app.send(b"\x1b")
            app.wait_until(
                lambda text: "Ask 🧠" not in text,
                "failed Ask AI dialog closed before reuse",
            )

            # Repeated requests exceed the bridge's ordinary eight-session
            # capacity while each settled ephemeral session is archived.
            for index in range(9):
                app.send(b"Areuse session " + str(index).encode() + b"\t\r")
                app.wait_until(
                    lambda text: "Proposal" in text
                    and "pl.col('level') == 'ERROR'" in text,
                    f"fresh-session proposal {index}",
                    timeout=15.0,
                )
                app.send(b"\r")
                app.wait_until(
                    lambda text: "advanced:on" in text
                    and "Applied   pl.col('level') == 'ERROR'" in text,
                    f"fresh-session native apply {index}",
                    timeout=15.0,
                )
                app.send(b"\x1b")
                app.wait_until(
                    lambda text: FILTER_TITLE not in text,
                    f"advanced editor closed after fresh session {index}",
                )

            app.send(b"A")
            app.send(b"slow stale proposal")
            app.send(b"\t\r")
            app.send(b"\x1b")
            app.wait_until(
                lambda text: "Ask 🧠" not in text and "broken" in text,
                "cancelled AI leaves viewer usable",
            )

            app.send(b"I")
            app.wait_for("Investigation ")
            app.send(b"explain this incident\t\r")
            investigation = app.wait_for("fixture investigation found broken", timeout=15.0)
            assert "session-investigation" in investigation
            assert "Snapshot:" in investigation
            app.send(b"follow up with evidence\t\r")
            app.wait_for("fixture follow-up complete", timeout=10.0)
            app.send(b"slow investigation turn\t\r")
            app.wait_for("local agent is exploring", timeout=5.0)
            app.send(b"\x1b")
            app.wait_until(
                lambda text: "Investigation 🧠" not in text,
                "investigation closed before quit",
            )
            quit_cleanly(app)
        finally:
            if app.process.poll() is None:
                app.process.kill()
            app.close()

        requests = [json.loads(line) for line in archive.read_text().splitlines()]
        proposals = [request for request in requests if request["method"] == "request_proposal"]
        starts = [request for request in requests if request["method"] == "start_session"]
        ask_starts = [request for request in starts if request.get("title") == "lvu Ask agent"]
        investigation_starts = [request for request in starts if request.get("title") == "lvu investigation"]
        cancellations = [request for request in requests if request["method"] == "cancel"]
        assert len(ask_starts) >= 12
        assert len({request["request_id"] for request in ask_starts}) == len(ask_starts)
        assert all(request.get("purpose") == "ask" for request in ask_starts)
        assert len(investigation_starts) == 1
        assert investigation_starts[0].get("purpose") == "investigation"
        assert len(proposals) >= 12
        assert cancellations, "failed/cancelled proposals must settle their owned session"
        assert any(request["session_id"] == "session-investigation" for request in cancellations)
        snapshot_dirs = {pathlib.Path(request["context"]["manifest_path"]).parent for request in proposals}
        deadline = time.monotonic() + 2.0
        while not all((directory / "lvu-agent-session.json").is_file() for directory in snapshot_dirs) and time.monotonic() < deadline:
            time.sleep(0.01)
        assert all((directory / "lvu-agent-session.json").is_file() for directory in snapshot_dirs)
        records = list((pathlib.Path(os.environ["XDG_DATA_HOME"]) / "lvu" / "investigations").glob("*/lvu-investigation.json"))
        assert len(records) == 1
        investigation_record = json.loads(records[0].read_text())
        assert pathlib.Path(investigation_record["manifest_path"]).is_file()

        reopened = PtyApp(
            binary,
            ["--file", str(source)],
            width=150,
            height=28,
            cwd=root,
            environment=environment,
        )
        try:
            # Await the durable accepted filter, not a transient unfiltered startup frame.
            restored = reopened.wait_until(lambda text: "broken" in text and "advanced:on" in text,
                                           "accepted ERROR filter restored", timeout=8.0)
            assert "ordinary" not in restored
            reopened.send(b"I")
            resumed = reopened.wait_for("Saved investigations", timeout=8.0)
            assert "session-investigation" in resumed
            reopened.send(b"\t\r")
            resumed = reopened.wait_for("session resumed; enter a follow-up", timeout=8.0)
            assert shows_path(
                resumed, "Snapshot", investigation_record["snapshot_dir"]
            ), (investigation_record["snapshot_dir"], resumed)
            before = archive.read_text().count('"method": "send_prompt"')
            reopened.send(b"follow up after restart\t\r")
            reopened.wait_for("fixture follow-up complete", timeout=8.0)
            after = archive.read_text().count('"method": "send_prompt"')
            assert after == before + 1, "resume must not send an automatic remote prompt"
            reopened.send(b"\x1b")
            reopened.wait_until(
                lambda text: "Investigation 🧠" not in text,
                "resumed investigation closed",
            )
            quit_cleanly(reopened)
        finally:
            if reopened.process.poll() is None:
                reopened.process.kill()
            reopened.close()

        resumed_requests = [json.loads(line) for line in archive.read_text().splitlines()]
        assert any(
            request["method"] == "resume_session"
            and request["session_id"] == investigation_record["session_id"]
            for request in resumed_requests
        )

        offline = PtyApp(
            binary,
            ["--capture-dir", str(root / "offline-capture"), "--file", str(source)],
            width=120,
            height=25,
            environment={"LVU_AGENT_BRIDGE_PROGRAM": str(root / "missing-bridge")},
        )
        try:
            offline.wait_for("ordinary", timeout=8.0)
            offline.send(b"Aoffline request\t\r")  # Focus and activate Submit; Enter in Request inserts a newline.
            # Ask is on the dialog anatomy, so the whole remedy is reachable:
            # what failed, which program, and how to install it. Investigation
            # and Source still box their state into one clipped row.
            offline.wait_until(
                lambda text: contains_wrapped_words(text, "Ask 🧠", "could not be started")
                and contains_wrapped_words(text, "Ask 🧠", "missing-bridge")
                and contains_wrapped_words(
                    text, "Ask 🧠", "not installed or not on PATH"
                )
                and contains_wrapped_words(
                    text, "Ask 🧠", "mise install node@26.8.1"
                ),
                "the launcher, not lvu, is named as missing, with its remedy",
                timeout=8.0,
            )
            offline.send(b"\x1b")
            offline.wait_until(
                lambda text: "Ask 🧠" not in text and "ordinary" in text,
                "offline AI dialog closed",
            )
            offline.send(b"Ioffline investigation\t\r")
            offline.wait_until(
                lambda text: "could not be started" in text
                and "missing-bridge" in text,
                "the launcher, not lvu, is named as missing",
                timeout=8.0,
            )
            offline.send(b"\x1b")
            offline.wait_until(
                lambda text: "Investigation 🧠" not in text and "ordinary" in text,
                "offline investigation dialog closed",
            )
            offline.send(b"n")
            enter_source_dialog(offline)
            activate_source_mode(offline, "🧠")
            offline.wait_for("Describe")
            offline.send(b"offline source request\r")
            offline.wait_until(
                lambda text: "could not be started" in text
                and "missing-bridge" in text,
                "the launcher, not lvu, is named as missing",
                timeout=8.0,
            )
            activate_source_mode(offline, "Manual")
            offline.wait_for("Kind", timeout=5.0)
            offline.send(b"\x1b")
            offline.wait_for("ordinary", timeout=5.0)
            quit_cleanly(offline)
        finally:
            if offline.process.poll() is None:
                offline.process.kill()
            offline.close()


def run_source_ai_story(binary: pathlib.Path) -> None:
    with tempfile.TemporaryDirectory(prefix="lvu-source-ai-pty-") as temporary:
        root = pathlib.Path(temporary)
        source = root / "suggested source.log"
        source.write_text("source-ai-live-line\n")
        archive = root / "requests.jsonl"
        bridge = root / "source-bridge.py"
        bridge.write_text(
            """#!/usr/bin/env python3
import json, os, pathlib, sys
archive = pathlib.Path(os.environ["FAKE_BRIDGE_ARCHIVE"])
source = pathlib.Path(os.environ["FAKE_SOURCE"])
session_count = 0
for line in sys.stdin:
    request = json.loads(line)
    with archive.open("a") as out:
        out.write(json.dumps(request) + "\\n")
    method = request["method"]
    if method == "start_session":
        assert request.get("title") == "lvu source definition assistance"
        assert request.get("purpose") == "source_assistance"
        session_count += 1
        result = {"session_id": "source-definition-session-" + str(session_count)}
    elif method == "request_proposal":
        assert request["kind"] == "sources"
        manifest = pathlib.Path(request["context"]["manifest_path"])
        assert manifest.is_absolute() and manifest.is_file()
        assert request["context"]["dataset_paths"] == []
        assert request["context"].get("inline_context") is None
        definition = {
            "schema_version": 1,
            "id": "22222222-2222-4222-8222-222222222222",
            "name": "AI suggested file",
            "kind": "file",
            "path": str(source),
            "follow": True,
            "identity_hints": {"fixture": "source-ai"},
            "retention": None,
        }
        result = {"proposal": {
            "kind": "source", "definition": definition,
            "explanation": "matched deterministic controlled file",
            "originating_revision": request["originating_revision"],
        }}
    elif method == "cancel":
        result = {"cancelled": True, "remote_cancelled": True,
                  "remote_agent_may_still_be_running": False}
    else:
        result = {"accepted": True}
    print(json.dumps({"schema_version": 1, "request_id": request["request_id"],
                      "ok": True, "result": result}), flush=True)
    if method == "request_proposal":
        print(json.dumps({"schema_version": 1, "session_id": request["session_id"],
                          "kind": "session_archived", "activity_path": str(archive.parent / (request["session_id"] + ".activity.jsonl")),
                          "archived_at": "2026-09-06T00:00:00Z"}), flush=True)
"""
        )
        bridge.chmod(0o755)
        app = PtyApp(
            binary,
            [],
            width=140,
            height=28,
            cwd=root,
            environment={
                "LVU_AGENT_BRIDGE_PROGRAM": str(bridge),
                "LVU_AGENT_BRIDGE_CWD": str(root),
                "FAKE_BRIDGE_ARCHIVE": str(archive),
                "FAKE_SOURCE": str(source),
            },
        )
        try:
            enter_source_dialog(app)
            app.wait_for("NO VIEW", timeout=8.0)
            activate_source_mode(app, "🧠")
            app.wait_for("Describe")
            app.send(b"follow the controlled backend file\r")
            preview = app.wait_until(
                lambda text: "AI suggested file" in text
                and "matched deterministic controlled file" in text,
                "source proposal preview",
                timeout=15.0,
            )
            assert "source-ai-live-line" not in preview, "preview must not launch capture"
            app.send(b"\r")
            app.wait_for("source-ai-live-line", timeout=10.0)

            app.send(b"n")
            enter_source_dialog(app)
            activate_source_mode(app, "🧠")
            app.wait_for("Describe", timeout=5.0)
            app.send(b"follow it again\r")
            app.wait_until(
                lambda text: "AI suggested file" in text
                and "Review only" in text,
                "duplicate source proposal review",
                timeout=15.0,
            )
            app.send(b"\r")
            app.wait_for("source-ai-live-line", timeout=8.0)
            quit_cleanly(app)
        finally:
            if app.process.poll() is None:
                app.process.kill()
            app.close()

        requests = [json.loads(line) for line in archive.read_text().splitlines()]
        assert sum(item["method"] == "request_proposal" for item in requests) == 2
        starts = [item for item in requests if item["method"] == "start_session"]
        assert len(starts) == 2 and all(item.get("purpose") == "source_assistance" for item in starts)
        journals = list((pathlib.Path(os.environ["XDG_DATA_HOME"]) / "lvu").glob("*/capture.journal"))
        assert len(journals) == 1, "duplicate AI definition must reuse the existing capture"
        contexts = list(
            (pathlib.Path(os.environ["XDG_DATA_HOME"]) / "lvu" / "investigations" / "assistance").glob("source-ai-*")
        )
        assert len(contexts) == 2
        assert all((path / "manifest.json").is_file() for path in contexts)
        assert all((path / "lvu-agent-session.json").is_file() for path in contexts)


def run_recipe_story(binary: pathlib.Path) -> None:
    with tempfile.TemporaryDirectory(prefix="lvu-recipes-pty-") as temporary:
        root = pathlib.Path(temporary)
        capture = root / "capture"
        first = root / "first.log"
        second = root / "second.log"
        first.write_text('{"level":"INFO","message":"info one"}\n{"level":"ERROR","message":"error one"}\n')
        second.write_text('{"level":"INFO","message":"info two"}\n{"level":"ERROR","message":"error two"}\n')
        archive = root / "recipe-bridge.jsonl"
        bridge = root / "recipe-bridge.py"
        bridge.write_text("""#!/usr/bin/env python3
import json, pathlib, sys
session_count = 0
for line in sys.stdin:
    request = json.loads(line)
    if request["method"] == "start_session":
        assert request.get("purpose") == "ask"
        session_count += 1
        result = {"session_id": "session-recipe-adapt-" + str(session_count)}
    elif request["method"] == "request_proposal":
        assert request["kind"] == "view"
        assert pathlib.Path(request["context"]["manifest_path"]).is_file()
        assert request["context"]["dataset_paths"] == []
        assert isinstance(request["context"]["inline_context"], dict)
        assert len(json.dumps(request["context"]["inline_context"], ensure_ascii=False).encode("utf-8")) <= 32768
        assert not list(pathlib.Path(request["context"]["manifest_path"]).parent.glob("*.parquet"))
        definition = {"schema_version": 1,
            "id": "11111111-1111-4111-8111-111111111111", "name": "Adapted errors",
            "source_ids": [request["instruction"].split("source-id=")[-1].split()[0]],
            "filter": {"schema_version": 1, "expression": "pl.col('error_flag')"},
            "recipe_stage_revisions": [],
            "enrichments": [
                {"id": "copy-stage", "source": "level_copy = pl.col('level')"},
                {"id": "flag-stage", "source": "error_flag = pl.col('level_copy') == 'ERROR'"}
            ]}
        result = {"proposal": {"kind": "view", "definition": definition,
            "explanation": "adapted from matching schema",
            "originating_revision": request["originating_revision"]}}
    elif request["method"] == "cancel":
        result = {"cancelled": True, "remote_cancelled": True,
                  "remote_agent_may_still_be_running": False}
    else:
        result = {"accepted": True}
    print(json.dumps({"schema_version": 1, "request_id": request["request_id"],
                      "ok": True, "result": result}), flush=True)
    if request["method"] == "request_proposal":
        print(json.dumps({"schema_version": 1, "session_id": request["session_id"],
                          "kind": "session_archived", "activity_path": "/fixture/recipe-activity.jsonl",
                          "archived_at": "2026-09-06T00:00:00Z"}), flush=True)
""")
        bridge.chmod(0o755)
        arguments = ["--capture-dir", str(capture), "--file", str(first), "--file", str(second)]
        environment = {"LVU_AGENT_BRIDGE_PROGRAM": str(bridge), "LVU_AGENT_BRIDGE_CWD": str(root)}
        app = PtyApp(binary, arguments, width=140, height=28, cwd=root, environment=environment)
        try:
            app.wait_for("error one", timeout=8.0)
            app.send(b"/error\r\x1b")
            app.wait_until(lambda text: 'search:"error"' in text and "info one" not in text, "accepted source-one search", timeout=8.0)
            app.send(b"r")
            app.wait_for("Saved recipes", timeout=5.0)
            app.send(b"\x1bs")
            app.wait_for("Only accepted settings are saved", timeout=5.0)
            app.send(b"Errors recipe\r")
            app.wait_until(
                lambda text: "Errors recipe" in text and "1 saved recipes" in text,
                "durably saved recipe returned to browse mode",
                timeout=8.0,
            )
            app.send(b"\x1b")
            app.wait_until(lambda text: "Saved recipes" not in text, "recipe dialog closed", timeout=5.0)
            app.send(b"]")
            app.wait_for("info two", timeout=8.0)
            app.send(b"r")
            app.wait_for("Errors recipe", timeout=8.0)
            suggested = app.wait_for("Suggested because:", timeout=8.0)
            assert "same file source family" in suggested
            app.send(b"\x1ba")
            app.wait_until(
                lambda text: "Kind" in text and "Recipe adaptation" in text,
                "the recipe adaptation kind is fixed",
                timeout=5.0,
            )
            app.send(b"\t\r")
            app.wait_until(
                lambda text: "Proposal" in text and "none yet" not in text,
                "recipe adaptation proposal",
                timeout=15.0,
            )
            app.send(b"\r")
            applied = app.wait_until(lambda text: 'search:"error"' in text and "error two" in text, "recipe applied to second source", timeout=12.0)
            assert "info two" not in applied
            app.send(b"e"); app.wait_for("level_copy =")
            app.wait_for("error_flag =")
            app.send(b"\x1b"); app.wait_until(lambda t: "Ordered enrichments" not in t, "enrichment editor closed")
            with second.open("a") as stream:
                stream.write('{"level":"ERROR","message":"late adapted error"}\n{"level":"INFO","message":"late adapted info"}\n')
            late = app.wait_for("late adapted error", timeout=10.0)
            assert "late adapted info" not in late
            cycle_to(
                app,
                lambda text: "error one" in text and 'search:"error"' in text,
                "source-one view remains independent",
                key=b"[",
            )
            quit_cleanly(app)
        finally:
            if app.process.poll() is None: app.process.kill()
            app.close()

        reopened = PtyApp(binary, arguments, width=140, height=28, cwd=root)
        try:
            reopened.wait_for("error one", timeout=10.0)
            cycle_to(
                reopened,
                lambda text: "late adapted error" in text and "late adapted info" not in text,
                "the restored adapted view of the second source",
            )
            reopened.send(b"e"); reopened.wait_for("level_copy ="); reopened.wait_for("error_flag =")
            reopened.send(b"\x1b"); reopened.wait_until(lambda t: "Ordered enrichments" not in t, "restored enrichment editor closed")
            reopened.send(b"r")
            reopened.wait_for("Errors recipe", timeout=8.0)
            reopened.send(b"\x1b")
            reopened.wait_until(lambda text: "Saved recipes" not in text, "recipe dialog closed after restart", timeout=5.0)
            quit_cleanly(reopened)
        finally:
            if reopened.process.poll() is None: reopened.process.kill()
            reopened.close()


def click_time_action(app, label):
    app.wait_for(label)
    y, row = next((y, row) for y, row in enumerate(app.text().splitlines()) if label in row)
    x = row.index(label) + 2
    app.send(f"\x1b[<0;{x + 1};{y + 1}M\x1b[<0;{x + 1};{y + 1}m".encode())


def set_absolute_time(app, start_date, start_clock, end_date, end_clock):
    # From Basis, select Absolute in Window, then tab past the Gap-jump
    # threshold to the first editable segment.
    app.send(b"\t\r\x1b[B\r\t\t")
    app.wait_for("Absolute")
    for date, clock in [(start_date, start_clock), (end_date, end_clock)]:
        for value in (date, clock):
            app.send(b"\x01\x0b\x1b[200~" + value.encode() + b"\x1b[201~\t")
        app.send(b"\t")  # Keep the UTC timezone choice.
    app.send(b"\r")  # Apply button.


def run_capture_time_story(binary: pathlib.Path) -> None:
    with tempfile.TemporaryDirectory(prefix="lvu-time-pty-") as temporary:
        root = pathlib.Path(temporary)
        capture = root / "capture"
        source = root / "time.log"
        source.write_text("info outside search\nerror selected\n")
        arguments = ["--capture-dir", str(capture), "--file", str(source)]
        app = PtyApp(binary, arguments, width=140, height=28, cwd=root)
        try:
            app.wait_for("error selected", timeout=8.0)
            app.send(b"/error\r\x1b")
            app.wait_for('search:"error"', timeout=8.0)
            app.send(b"t")
            app.wait_for("Capture", timeout=5.0)
            set_absolute_time(app, "2000-01-01", "00:00:00", "2100-01-01", "00:00:00")
            screen = app.wait_for("capture-time:absolute", timeout=10.0)
            assert 'search:"error"' in screen and "error selected" in screen
            quit_cleanly(app)
        finally:
            if app.process.poll() is None: app.process.kill()
            app.close()

        reopened = PtyApp(binary, arguments, width=140, height=28, cwd=root)
        try:
            reopened.wait_for("capture-time:absolute", timeout=10.0)
            reopened.send(b"t")
            reopened.wait_for("Capture", timeout=5.0)
            click_time_action(reopened, "[ Clear ]")
            reopened.wait_until(lambda text: "capture-time:" not in text, "capture time cleared", timeout=10.0)
            reopened.send(b"t")
            reopened.wait_for("Time window")
            reopened.send(b"\t\r\x1b[B\x1b[B\r")
            reopened.wait_for("Last 5m")
            click_time_action(reopened, "[ Apply ]")
            rolling = reopened.wait_for("capture-time:rolling", timeout=10.0)
            assert 'search:"error"' in rolling and "error selected" in rolling
            with source.open("a") as stream:
                stream.write("error rolling arrival\n")
                stream.flush()
            reopened.wait_for("error rolling arrival", timeout=10.0)
            quit_cleanly(reopened)
        finally:
            if reopened.process.poll() is None: reopened.process.kill()
            reopened.close()

        final = PtyApp(binary, arguments, width=140, height=28, cwd=root)
        try:
            screen = final.wait_until(
                lambda text: "capture-time:rolling" in text
                and 'search:"error"' in text and "error rolling arrival" in text,
                "restored rolling policy and its loaded matching rows", timeout=10.0,
            )
            assert 'search:"error"' in screen and "error rolling arrival" in screen
            final.send(b"t")
            final.wait_for("Time window")
            click_time_action(final, "[ Clear ]")
            final.wait_until(lambda text: "capture-time:" not in text, "rolling capture time cleared", timeout=10.0)
            quit_cleanly(final)
        finally:
            if final.process.poll() is None: final.process.kill()
            final.close()


def run_event_time_story(binary: pathlib.Path) -> None:
    with tempfile.TemporaryDirectory(prefix="lvu-event-time-pty-") as temporary:
        root = pathlib.Path(temporary)
        capture = root / "capture"
        source = root / "events.log"
        source.write_text(
            '{"message":"utc match","timestamp":"2026-09-05T12:30:45Z"}\n'
            'time=2026-09-05T14:30:45+02:00 message="offset match"\n'
            '{"message":"boundary","ts":"2026-09-05T12:30:46Z"}\n'
            '{"message":"ambiguous visible raw","ts":"2026-09-05 12:30:45"}\n'
            'missing event time visible raw\n'
        )
        arguments = ["--capture-dir", str(capture), "--file", str(source)]
        app = PtyApp(binary, arguments, width=150, height=30, cwd=root)
        try:
            app.wait_for("ambiguous visible raw", timeout=8.0)
            app.send(b"t")
            app.wait_for("Capture", timeout=5.0)
            app.send(b"\r\x1b[B\r")
            app.wait_for("Recognized event", timeout=5.0)
            set_absolute_time(app, "2026-09-05", "12:30:45", "2026-09-05", "12:30:46")
            filtered = app.wait_for("event-time:absolute", timeout=10.0)
            assert "utc match" in filtered and "offset match" in filtered
            assert "boundary" not in filtered
            assert "ambiguous visible raw" not in filtered
            app.wait_for("invalid/ambiguous", timeout=8.0)
            quit_cleanly(app)
        finally:
            if app.process.poll() is None: app.process.kill()
            app.close()

        reopened = PtyApp(binary, arguments, width=150, height=30, cwd=root)
        try:
            reopened.wait_for("event-time:absolute", timeout=10.0)
            reopened.send(b"t")
            reopened.wait_for("order: recognized", timeout=5.0)
            click_time_action(reopened, "[ Clear ]")
            restored = reopened.wait_for("ambiguous visible raw", timeout=10.0)
            assert "missing event time visible raw" in restored
            quit_cleanly(reopened)
        finally:
            if reopened.process.poll() is None: reopened.process.kill()
            reopened.close()


def run_multiline_grouping_story(binary: pathlib.Path) -> None:
    with tempfile.TemporaryDirectory(prefix="lvu-grouping-pty-") as temporary:
        root = pathlib.Path(temporary)
        capture = root / "capture"
        source = root / "stack.log"
        source.write_bytes(
            b"Error: first\xff\n"
            b"  at first.rs:10\n"
            b"ERROR next event\n"
            b"  at next.rs:20\n"
        )
        arguments = ["--capture-dir", str(capture), "--file", str(source)]
        app = PtyApp(binary, arguments, width=150, height=30, cwd=root)
        try:
            app.wait_for("at next.rs:20", timeout=8.0)
            # Recognition lives in Enrichment: a true-line column whose
            # non-null values open events. Grouping only selects it.
            app.send(b"e")
            app.wait_for("Steps", timeout=10.0)
            app.send(b"\x1ba")
            app.wait_for("Enrichment \u203a New step", timeout=10.0)
            paste(app, "/^(?P<is_start>Error|ERROR)/")
            app.send(b"\r")
            app.wait_until(
                lambda text: "is_start" in text and "Applied" in text,
                "the start column is applied",
                timeout=20.0,
            )
            app.send(b"\x1b")
            app.wait_until(
                lambda text: "Steps" not in text,
                "enrichment closed",
                timeout=10.0,
            )
            app.send(b"m")
            app.wait_for("Multiline grouping", timeout=5.0)
            app.send(b"\t")
            app.send(b"\x1b[C")
            app.wait_for("(?lvu:filter:", timeout=5.0)
            app.send(b"\t")
            app.send(b"\t")
            paste(app, "is_start")
            app.wait_for("(?lvu:filter:v1:column:is_start)", timeout=5.0)
            app.send(b"\r")
            app.wait_for("grouping:display-only", timeout=10.0)
            app.send(b"\x1b")
            collapsed = app.wait_until(
                lambda text: "Multiline grouping" not in text
                and "[2 physical lines]" in text,
                "collapsed display-only groups",
                timeout=8.0,
            )
            assert "at next.rs:20" not in collapsed

            app.send(b"\r")
            expanded = app.wait_for("at next.rs:20", timeout=5.0)
            assert expanded.index("ERROR next event") < expanded.index("at next.rs:20")
            assert expanded.count("at next.rs:20") == 1, expanded
            row = next(
                index + 1
                for index, line in enumerate(expanded.splitlines())
                if "ERROR next event" in line
            )
            app.send(f"\x1b[<0;45;{row}M\x1b[<0;45;{row}m".encode())
            app.wait_until(
                lambda text: "at next.rs:20" not in text,
                "mouse-collapsed selected group",
                timeout=5.0,
            )
            app.send(b"\r")
            app.wait_for("at next.rs:20", timeout=5.0)

            with source.open("ab") as stream:
                stream.write(b"  at late.rs:30\n")
                stream.flush()
                os.fsync(stream.fileno())
            grown = app.wait_for("at late.rs:30", timeout=10.0)
            assert "at next.rs:20" in grown, "expanded group stayed anchored while growing"
            app.send(b"\r")
            app.wait_for("[3 physical lines]", timeout=5.0)

            app.send(b"/")
            app.wait_for(FILTER_TITLE)
            time.sleep(0.3)  # fresh dialog takes focus a tick after its frame; first keystroke would be lost
            app.send(b"late.rs")
            app.send(b"\r")
            app.wait_until(
                lambda text: 'search:"late.rs"' in text,
                "late.rs search accepted",
                timeout=10.0,
            )
            app.send(b"\x1b")
            app.wait_until(
                lambda text: "Live literal substring" not in text,
                "search editor closed before asserting rows",
            )
            orphan = app.wait_for("orphan continuation", timeout=10.0)
            assert "Error: first" not in orphan and "ERROR next event" not in orphan
            app.send(b"/")
            app.wait_for(FILTER_TITLE)
            time.sleep(0.3)  # fresh dialog takes focus a tick after its frame; first keystroke would be lost
            app.send(b"\x7f" * len("late.rs"))
            app.send(b"\r")
            app.wait_until(
                lambda text: 'search:"' not in text and "No filter" in text,
                "search clear accepted while grouping remains",
                timeout=10.0,
            )
            app.send(b"\x1b")
            app.wait_until(
                lambda text: "Live literal substring" not in text,
                "cleared search editor closed before quit",
                timeout=5.0,
            )
            app.wait_for("[3 physical lines]", timeout=10.0)
            quit_cleanly(app)
        finally:
            if app.process.poll() is None:
                app.process.kill()
            app.close()

        reopened = PtyApp(binary, arguments, width=150, height=30, cwd=root)
        try:
            restored = reopened.wait_for("grouping:display-only", timeout=12.0)
            assert "[3 physical lines]" in restored
            quit_cleanly(reopened)
        finally:
            if reopened.process.poll() is None:
                reopened.process.kill()
            reopened.close()


def run_storage_story(binary: pathlib.Path) -> None:
    with tempfile.TemporaryDirectory(prefix="lvu-storage-pty-") as temporary:
        root = pathlib.Path(temporary)
        capture = root / "capture"
        derived = pathlib.Path(os.environ["XDG_CACHE_HOME"]) / "lvu" / "derived"
        durable = capture / "11111111-1111-1111-1111-111111111111"
        investigations = capture / "investigations" / "kept-session"
        source = root / "storage.log"
        seed_source = root / "seed.log"
        derived.mkdir(parents=True)
        durable.mkdir(parents=True)
        investigations.mkdir(parents=True)
        unknown_uuid = derived / "22222222-2222-2222-2222-222222222222.rows.idx"
        unknown = derived / "owner-unknown.bin"
        raw_sentinel = durable / "journal.lvu"
        export_sentinel = investigations / "dataset.parquet"
        unknown_uuid.write_bytes(b"uuid-named-unknown")
        unknown.write_bytes(b"preserve-unknown")
        raw_sentinel.write_bytes(b"preserve-raw")
        export_sentinel.write_bytes(b"preserve-export")
        source.write_text("storage initial\n")
        seed_source.write_text("seed index owner\n")
        seed = PtyApp(binary, ["--capture-dir", str(capture), "--file", str(seed_source)], width=100, height=20)
        try:
            seed.wait_for("seed index owner", timeout=8.0)
            quit_cleanly(seed)
        finally:
            if seed.process.poll() is None:
                seed.process.kill()
            seed.close()
        generated = [path for path in derived.glob("*.rows.idx") if path != unknown_uuid]
        assert len(generated) == 1, "fixture must use a genuinely generated LVUIDX2 artifact"
        unused = generated[0]
        # --fresh: the seed source exists only to leave an unused derived index
        # behind. Resuming it would put it back under active capture and make
        # its artifact recomputable-in-use rather than unused.
        app = PtyApp(binary, ["--fresh", "--capture-dir", str(capture), "--file", str(source)], width=150, height=28)
        try:
            app.wait_for("storage initial", timeout=8.0)
            app.send(b"S")
            inspected = app.wait_for("unused, recomputable", timeout=8.0)
            assert "Storage" in inspected
            assert "not a process RSS limit" in inspected
            app.send(b"c")
            app.wait_for("press c again", timeout=5.0)
            app.send(b"c")
            app.wait_for("cleared", timeout=8.0)
            assert not unused.exists(), app.text()
            assert unknown_uuid.read_bytes() == b"uuid-named-unknown"
            assert unknown.read_bytes() == b"preserve-unknown"
            assert raw_sentinel.read_bytes() == b"preserve-raw"
            assert export_sentinel.read_bytes() == b"preserve-export"
            app.send(b"\x1b")
            with source.open("a") as stream:
                stream.write("storage still capturing\n")
                stream.flush()
                os.fsync(stream.fileno())
            app.wait_for("storage still capturing", timeout=8.0)
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
    # Isolate lvu state without relocating the already-installed toolchain/cache.
    home = pathlib.Path.home()
    for variable, xdg, fallback, suffix in [
        ("MISE_DATA_DIR", "XDG_DATA_HOME", home / ".local/share", "mise"),
        ("MISE_CONFIG_DIR", "XDG_CONFIG_HOME", home / ".config", "mise"),
        ("MISE_CACHE_DIR", "XDG_CACHE_HOME", home / ".cache", "mise"),
        ("UV_CACHE_DIR", "XDG_CACHE_HOME", home / ".cache", "uv"),
    ]:
        os.environ.setdefault(variable, str(pathlib.Path(os.environ.get(xdg, fallback)) / suffix))
    for story in [
        run_story,
        run_startup_failure_story,
        run_discovery_story,
        run_memory_restore_story,
        run_path_completion_story,
        run_field_presentation_story,
        run_enrichment_story,
        run_editor_completion_story,
        run_named_views_story,
        run_ask_ai_story,
        run_source_ai_story,
        run_recipe_story,
        run_capture_time_story,
        run_event_time_story,
        run_multiline_grouping_story,
        run_storage_story,
    ]:
        with tempfile.TemporaryDirectory(prefix="lvu-story-xdg-") as xdg:
            _STORY_XDG.clear()
            for variable, name in [("XDG_CONFIG_HOME", "config"), ("XDG_CACHE_HOME", "cache"), ("XDG_DATA_HOME", "data")]:
                os.environ[variable] = str(pathlib.Path(xdg) / name)
                # Every launch in this story addresses the same roots the
                # assertions below read; see `story_environment`.
                _STORY_XDG[variable] = os.environ[variable]
            story(binary)
    print(
        "Real-source PTY passed: file/command/discovery/completion/live "
        "append/reopen/reap/restoration/named-views/recipes/capture-time/event-time/multiline/storage/ask-ai/source-ai/investigation-resume"
    )


if __name__ == "__main__":
    assert_wrapped_remedy_matcher()
    main()
