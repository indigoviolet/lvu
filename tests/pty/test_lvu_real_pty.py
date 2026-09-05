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
            app.wait_until(
                lambda text: "Live literal substring" not in text,
                "cleared search editor closed before quit",
            )

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
            details = app.wait_for("request_id: same")
            assert "raw: {" in details and "service: api" in details
            app.send(b"i")
            app.wait_for("Event fields")
            app.send(b"\x1b[B" * 3)  # service (JSON keys are sorted)
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
            app.send(b"\x1b[200~" + expression.encode() + b"\x1b[201~")
            app.send(b"\r")
            app.wait_until(
                lambda text: "applied: " + expression in text and "enrich:on" in text,
                "native enrichment applied",
                timeout=12.0,
            )
            app.send(b"\x1b")
            app.wait_until(lambda text: "Native enrichment" not in text, "enrichment closed")
            app.send(b"d")
            app.wait_for("status_code: 503")
            app.send(b"i")
            app.wait_for("Event fields")
            app.send(b"\x1b[B" * 3)
            app.send(b" ")
            app.send(b"\x1b")
            app.wait_until(
                lambda text: "status_code" in text and "status=503 failed" in text,
                "derived field pinned",
            )
            app.send(b"p")
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
            app.send(b"\x7f" * len(expression))
            invalid = "status_code = pl.col("
            app.send(invalid.encode())
            app.send(b"\r")
            app.wait_for("compiler rejected expression", timeout=10.0)
            app.send(b"\x1b")
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
            draft = reopened.wait_for("status_code = pl.col(")
            assert "applied: " + expression in draft
            reopened.send(b"\x1b")
            reopened.wait_until(
                lambda text: "Native enrichment" not in text,
                "restored enrichment editor closed",
            )
            reopened.send(b"p")
            reopened.send(b"\x7f" * len(advanced))
            reopened.send(b"\r")
            reopened.wait_until(
                lambda text: "advanced:on" not in text,
                "advanced filter cleared before enrichment",
            )
            reopened.send(b"\x1b")
            reopened.wait_until(
                lambda text: "Advanced Polars filter" not in text,
                "advanced editor closed",
            )
            reopened.send(b"e")
            reopened.send(b"\x7f" * len(invalid))
            reopened.send(b"\r")
            reopened.wait_until(
                lambda text: "enrich:on" not in text and "malformed raw" in text,
                "enrichment cleared to raw view",
                timeout=10.0,
            )
            reopened.send(b"\x1b")
            reopened.wait_until(
                lambda text: "Native enrichment" not in text,
                "cleared enrichment editor closed",
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
            app.send(b"v")
            app.wait_for("CLONE SETTINGS")
            app.send(b"\x7f" * len("Copy of Raw events"))
            app.send(b"Errors\r")
            app.wait_for("Errors")
            app.send(b"e")
            app.send(b'tag = pl.lit("errors")\r')
            app.wait_for("enrich:on", timeout=12.0)
            app.send(b"\x1b")
            app.wait_until(lambda text: "Native enrichment" not in text, "named enrichment closed")
            app.send(b"/"); app.send(b"error"); app.send(b"\r")
            app.wait_until(
                lambda text: 'search:"error"' in text and "error first" in text,
                "errors view filter",
                timeout=8.0,
            )
            app.send(b"\x1b")
            app.wait_until(
                lambda text: " Search " not in text,
                "errors editor closed",
            )

            # A blank view starts without cloned constraints.
            app.send(b"v")
            app.send(b"\x1bb")  # Alt-B: blank view.
            app.wait_for("NEW BLANK")
            app.send(b"\x7f" * len("New view"))
            app.send(b"Info\r")
            info = app.wait_for("Info")
            assert 'search:"error"' not in info and "enrich:on" not in info
            app.send(b"/"); app.send(b"info"); app.send(b"\r")
            app.wait_until(lambda text: 'search:"info"' in text, "info filter applied", timeout=8.0)
            app.send(b"\x1b")
            app.wait_until(lambda text: " Search " not in text, "info editor closed")

            # Rename persists independently from its settings.
            app.send(b"v"); app.send(b"\x1br")
            app.wait_for("RENAME")
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
            assert "Raw events" in screen
            reopened.send(b"]")  # command raw view
            reopened.wait_for("startup-marker", timeout=8.0)
            reopened.send(b"]")  # Errors
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
for line in sys.stdin:
    request = json.loads(line)
    with archive.open("a") as out:
        out.write(json.dumps(request) + "\\n")
    method = request["method"]
    if method == "start_session":
        cwd = pathlib.Path(request["cwd"])
        assert cwd.is_absolute() and cwd.is_dir()
        result = {"session_id": "session-investigation" if request.get("title") == "lvu investigation" else "session-fixture"}
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
        assert context["dataset_paths"]
        assert all(pathlib.Path(path).is_absolute() and pathlib.Path(path).is_file()
                   for path in context["dataset_paths"])
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
        result = {"proposal": {
            "kind": request["kind"], "definition": definition,
            "explanation": "deterministic fixture proposal",
            "originating_revision": request["originating_revision"],
        }}
    elif method == "cancel":
        result = {"cancelled": True, "remote_cancelled": True,
                  "remote_agent_may_still_be_running": False}
    else:
        result = {"accepted": True}
    response = {"schema_version": 1, "request_id": request["request_id"], "ok": True, "result": result}
    print(json.dumps(response), flush=True)
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
            app.wait_for("ordinary", timeout=8.0)
            app.send(b"A")
            app.wait_for("Ask AI (local Paseo)")
            app.send(b"keep errors")
            app.send(b"\r")
            proposal = app.wait_until(
                lambda text: "Proposal:" in text and "pl.col('level') == 'ERROR'" in text,
                "snapshot-backed filter proposal",
                timeout=15.0,
            )
            assert "session-fixture" in proposal
            app.send(b"\r")
            app.wait_until(
                lambda text: "advanced:on" in text and "applied: pl.col('level') == 'ERROR'" in text,
                "AI filter accepted by native query",
                timeout=15.0,
            )
            app.send(b"\x1b")
            filtered = app.wait_for("broken", timeout=5.0)
            assert "ordinary" not in filtered and "warning" not in filtered

            app.send(b"A")
            app.send(b"\x1be")
            app.send(b"derive a reusable level field")
            app.send(b"\r")
            app.wait_until(
                lambda text: "Proposal:" in text and "ai_level = pl.col('level')" in text,
                "snapshot-backed enrichment proposal",
                timeout=15.0,
            )
            app.send(b"\r")
            app.wait_until(
                lambda text: "enrich:on" in text and "applied: ai_level = pl.col('level')" in text,
                "AI enrichment accepted by native query",
                timeout=15.0,
            )
            app.send(b"\x1b")
            app.wait_until(
                lambda text: "Native enrichment" not in text,
                "native enrichment editor closed before the next command",
            )
            app.send(b"d")
            app.wait_for("ai_level: ERROR", timeout=8.0)

            # A failed proposal is cancelled before its error is published;
            # the retained session remains reusable for the next request.
            app.send(b"Afixture failure\r")
            app.wait_for("FIXTURE: proposal failed", timeout=10.0)
            app.send(b"\x1b")
            app.wait_until(
                lambda text: "Ask AI (local Paseo)" not in text,
                "failed Ask AI dialog closed before reuse",
            )

            # Repeated requests exceed the bridge's ordinary eight-session
            # capacity while reusing one application-owned session.
            for index in range(9):
                app.send(b"Areuse session " + str(index).encode() + b"\r")
                app.wait_until(
                    lambda text: "Proposal:" in text
                    and "pl.col('level') == 'ERROR'" in text,
                    f"reused-session proposal {index}",
                    timeout=15.0,
                )
                app.send(b"\r")
                app.wait_until(
                    lambda text: "advanced:on" in text
                    and "applied: pl.col('level') == 'ERROR'" in text,
                    f"reused-session native apply {index}",
                    timeout=15.0,
                )
                app.send(b"\x1b")
                app.wait_until(
                    lambda text: "Native advanced Polars" not in text,
                    f"native advanced editor closed after reuse {index}",
                )

            app.send(b"A")
            app.send(b"slow stale proposal")
            app.send(b"\r")
            app.send(b"\x1b")
            app.wait_until(
                lambda text: "Ask AI (local Paseo)" not in text and "broken" in text,
                "cancelled AI leaves viewer usable",
            )

            app.send(b"I")
            app.wait_for("Investigate with local Paseo")
            app.send(b"explain this incident\r")
            investigation = app.wait_for("fixture investigation found broken", timeout=15.0)
            assert "session-investigation" in investigation
            assert "Snapshot:" in investigation
            app.send(b"follow up with evidence\r")
            app.wait_for("fixture follow-up complete", timeout=10.0)
            app.send(b"slow investigation turn\r")
            app.wait_for("local agent is exploring", timeout=5.0)
            app.send(b"\x1b")
            app.wait_until(
                lambda text: "Investigate with local Paseo" not in text,
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
        ask_starts = [request for request in starts if request.get("title") == "lvu Ask AI"]
        investigation_starts = [request for request in starts if request.get("title") == "lvu investigation"]
        cancellations = [request for request in requests if request["method"] == "cancel"]
        assert len(ask_starts) == 1
        assert len(investigation_starts) == 1
        assert len(proposals) >= 12
        assert cancellations, "failed/cancelled proposals must settle their owned session"
        assert any(request["session_id"] == "session-investigation" for request in cancellations)
        snapshot_dirs = {pathlib.Path(request["context"]["manifest_path"]).parent for request in proposals}
        deadline = time.monotonic() + 2.0
        while not all((directory / "lvu-agent-session.json").is_file() for directory in snapshot_dirs) and time.monotonic() < deadline:
            time.sleep(0.01)
        assert all((directory / "lvu-agent-session.json").is_file() for directory in snapshot_dirs)
        records = list((root / ".lvu-captures" / "investigations").glob("*/lvu-investigation.json"))
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
            reopened.wait_for("ordinary", timeout=8.0)
            reopened.send(b"I")
            resumed = reopened.wait_for("Saved investigations", timeout=8.0)
            assert "session-investigation" in resumed
            reopened.send(b"\r")
            resumed = reopened.wait_for("session resumed; enter a follow-up", timeout=8.0)
            assert investigation_record["snapshot_dir"] in resumed
            before = archive.read_text().count('"method": "send_prompt"')
            reopened.send(b"follow up after restart\r")
            reopened.wait_for("fixture follow-up complete", timeout=8.0)
            after = archive.read_text().count('"method": "send_prompt"')
            assert after == before + 1, "resume must not send an automatic remote prompt"
            reopened.send(b"\x1b")
            reopened.wait_until(
                lambda text: "Investigate with local Paseo" not in text,
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
            offline.send(b"Aoffline request\r")
            offline.wait_for("local Paseo bridge unavailable", timeout=8.0)
            offline.send(b"\x1b")
            offline.wait_until(
                lambda text: "Ask AI (local Paseo)" not in text and "ordinary" in text,
                "offline AI dialog closed",
            )
            offline.send(b"Ioffline investigation\r")
            offline.wait_for("local Paseo bridge unavailable", timeout=8.0)
            offline.send(b"\x1b")
            offline.wait_until(
                lambda text: "Investigate with local Paseo" not in text and "ordinary" in text,
                "offline investigation dialog closed",
            )
            offline.send(b"n")
            offline.wait_for("Add source")
            offline.send(b"\x01offline source request\r")
            offline.wait_for("local Paseo bridge unavailable", timeout=8.0)
            offline.send(b"\x01")
            offline.wait_for("FILE PATH", timeout=5.0)
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
for line in sys.stdin:
    request = json.loads(line)
    with archive.open("a") as out:
        out.write(json.dumps(request) + "\\n")
    method = request["method"]
    if method == "start_session":
        assert request.get("title") == "lvu source definition assistance"
        result = {"session_id": "source-definition-session"}
    elif method == "request_proposal":
        assert request["kind"] == "source"
        manifest = pathlib.Path(request["context"]["manifest_path"])
        assert manifest.is_absolute() and manifest.is_file()
        assert request["context"]["dataset_paths"] == []
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
            app.wait_for("No view selected", timeout=8.0)
            app.send(b"\x01")  # Ctrl-A: source-definition assistance.
            app.wait_for("Ask AI for a source")
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
            app.wait_for("Add source", timeout=5.0)
            app.send(b"\x01")
            app.wait_for("Ask AI for a source", timeout=5.0)
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
        journals = list((root / ".lvu-captures").glob("*/capture.journal"))
        assert len(journals) == 1, "duplicate AI definition must reuse the existing capture"
        contexts = list(
            (root / ".lvu-captures" / "investigations").glob("source-ai-*")
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
        first.write_text("info one\nerror one\n")
        second.write_text("info two\nerror two\n")
        arguments = ["--capture-dir", str(capture), "--file", str(first), "--file", str(second)]
        app = PtyApp(binary, arguments, width=140, height=28, cwd=root)
        try:
            app.wait_for("error one", timeout=8.0)
            app.send(b"/error\r\x1b")
            app.wait_until(lambda text: 'search:"error"' in text and "info one" not in text, "accepted source-one search", timeout=8.0)
            app.send(b"r")
            app.wait_for("Named recipes", timeout=5.0)
            app.send(b"\x1bs")
            app.wait_for("Mode: Save", timeout=5.0)
            app.send(b"Errors recipe\r")
            app.wait_for("saved immutable revision", timeout=8.0)
            app.send(b"\x1b")
            app.wait_until(lambda text: "Named recipes" not in text, "recipe dialog closed", timeout=5.0)
            app.send(b"]")
            app.wait_for("info two", timeout=8.0)
            app.send(b"r")
            app.wait_for("Errors recipe", timeout=8.0)
            app.send(b"\r")
            applied = app.wait_until(lambda text: 'search:"error"' in text and "error two" in text, "recipe applied to second source", timeout=12.0)
            assert "info two" not in applied
            app.send(b"[")
            app.wait_until(lambda text: "error one" in text and 'search:"error"' in text, "source-one view remains independent", timeout=8.0)
            quit_cleanly(app)
        finally:
            if app.process.poll() is None: app.process.kill()
            app.close()

        reopened = PtyApp(binary, arguments, width=140, height=28, cwd=root)
        try:
            reopened.wait_for("error one", timeout=10.0)
            reopened.send(b"r")
            reopened.wait_for("Errors recipe", timeout=8.0)
            reopened.send(b"\x1b")
            reopened.wait_until(lambda text: "Named recipes" not in text, "recipe dialog closed after restart", timeout=5.0)
            quit_cleanly(reopened)
        finally:
            if reopened.process.poll() is None: reopened.process.kill()
            reopened.close()


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
            app.wait_for("Capture time (UTC, half-open [start, end))", timeout=5.0)
            app.send(b"2000-01-01T00:00:00Z\t2100-01-01T00:00:00Z\r")
            screen = app.wait_for("capture-time:on", timeout=10.0)
            assert 'search:"error"' in screen and "error selected" in screen
            quit_cleanly(app)
        finally:
            if app.process.poll() is None: app.process.kill()
            app.close()

        reopened = PtyApp(binary, arguments, width=140, height=28, cwd=root)
        try:
            reopened.wait_for("capture-time:on", timeout=10.0)
            reopened.send(b"t")
            reopened.wait_for("Capture time (UTC, half-open [start, end))", timeout=5.0)
            reopened.send(b"\x1bc")
            reopened.wait_until(lambda text: "capture-time:on" not in text, "capture time cleared", timeout=10.0)
            quit_cleanly(reopened)
        finally:
            if reopened.process.poll() is None: reopened.process.kill()
            reopened.close()


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
    run_field_presentation_story(binary)
    run_enrichment_story(binary)
    run_named_views_story(binary)
    run_ask_ai_story(binary)
    run_source_ai_story(binary)
    run_recipe_story(binary)
    run_capture_time_story(binary)
    print(
        "Real-source PTY passed: file/command/discovery/completion/live "
        "append/reopen/reap/restoration/named-views/recipes/capture-time/ask-ai/source-ai/investigation-resume"
    )


if __name__ == "__main__":
    main()
