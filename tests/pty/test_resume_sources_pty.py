#!/usr/bin/env python3
"""Resuming the previous session's sources on the next launch.

The story is the one a user lives: open a file and a command, quit, come back.
What comes back has to be the whole set, the file has to continue rather than
re-read what is already captured, the command has to run again, and the status
line has to say that this happened rather than leaving it to be discovered.
"""

from __future__ import annotations

import pathlib
import shlex
import sys
import tempfile

from test_lvu_pty import PtyApp
from test_stdin_pty import RedirectedStdinPtyApp
from test_enrichment_chain_pty import stop

WIDTH = 200
HEIGHT = 28


def journal_bytes(root: pathlib.Path) -> bytes:
    return b"".join(path.read_bytes() for path in sorted(root.rglob("*.journal")))


def looping_command(pids: pathlib.Path) -> str:
    return (
        f"echo $$ >> {shlex.quote(str(pids))}; "
        "printf 'command-ready\\n'; while :; do sleep 1; done"
    )


def launch(binary, arguments, environment, capture):
    return PtyApp(
        binary,
        [*arguments, "--capture-dir", str(capture)],
        width=WIDTH,
        height=HEIGHT,
        environment=environment,
    )


def run(binary):
    with tempfile.TemporaryDirectory(prefix="lvu-resume-pty-") as directory:
        root = pathlib.Path(directory)
        environment = {
            "XDG_CONFIG_HOME": str(root / "config"),
            "XDG_DATA_HOME": str(root / "data"),
            "XDG_CACHE_HOME": str(root / "cache"),
        }
        capture = root / "capture"
        log = root / "sample.log"
        log.write_text("file-first\n")
        pids = root / "pids"
        command = looping_command(pids)

        # A session with one file source and one command source.
        app = launch(binary, [str(log), "--command", command], environment, capture)
        try:
            app.wait_for("file-first")
            app.wait_until(
                lambda text: "shell command" in text and "sample.log" in text,
                "both sources in the sidebar",
            )
            app.wait_until(lambda text: text.count("Running:") >= 2, "both acquiring")
            stop(app)
        finally:
            if app.process.poll() is None:
                app.process.kill()
                app.process.wait()
                app.close()
        assert len(pids.read_text().splitlines()) == 1, "one command launch per session"

        # New records arrive between sessions; the resumed file must pick them
        # up without capturing what the journal already holds.
        with log.open("a") as output:
            output.write("file-second\n")

        # The whole point: no arguments at all.
        app = launch(binary, [], environment, capture)
        try:
            app.wait_for("resumed 2 of 2 sources from the last session")
            app.wait_until(
                lambda text: "shell command" in text and "sample.log" in text,
                "both sources resumed into the sidebar",
            )
            app.wait_until(
                lambda text: text.count("Running:") >= 2,
                "both resumed sources acquiring",
            )
            app.wait_for("file-second")
            assert "file-first" in app.text(), "resumed file lost its history"
            # The command source's own view carries its output.
            app.send(b"]")
            app.wait_for("command-ready")
            stop(app)
        finally:
            if app.process.poll() is None:
                app.process.kill()
                app.process.wait()
                app.close()
        captured = journal_bytes(capture)
        assert captured.count(b"file-first") == 1, "resume re-captured history"
        assert captured.count(b"file-second") == 1, "resume duplicated a new record"
        assert len(pids.read_text().splitlines()) == 2, "one command launch per session"

        # Arguments add to the resumed set, and naming a source that is already
        # in it does not produce a second copy of it.
        extra = root / "extra.log"
        extra.write_text("extra-first\n")
        app = launch(binary, [str(extra), str(log)], environment, capture)
        try:
            app.wait_for("resumed 2 of 2 sources from the last session")
            app.wait_until(
                lambda text: text.count("Running:") >= 3,
                "resumed set plus the added source",
            )
            assert app.text().count("sample.log") == 1, "named source was duplicated"
            stop(app)
        finally:
            if app.process.poll() is None:
                app.process.kill()
                app.process.wait()
                app.close()

        # --fresh acquires nothing and deletes nothing.
        before = journal_bytes(capture)
        workspace = capture / "workspace" / "workspace.sqlite3"
        assert workspace.exists()
        app = launch(binary, ["--fresh"], environment, capture)
        try:
            # The startup title is shown only when nothing was acquired, so it
            # is itself the assertion that --fresh acquired nothing.
            app.wait_for("LOVE YOU LOG TIME")
            app.send(b"x")
            app.wait_until(lambda text: "PRESS ANY KEY" not in text, "startup title dismissed")
            app.wait_for("No sources")
            text = app.text()
            assert "resumed" not in text, text
            assert "file-first" not in text, text
            assert "command-ready" not in text, text
            app.send(b"\x03")
            assert app.wait_exit(timeout=8) == 0
            app.assert_restored()
            app.close()
        finally:
            if app.process.poll() is None:
                app.process.kill()
                app.process.wait()
                app.close()
        assert journal_bytes(capture) == before, "--fresh discarded captured data"
        assert workspace.exists(), "--fresh discarded the workspace"

        piped_capture = root / "piped"
        piped = RedirectedStdinPtyApp(
            binary,
            ["--capture-dir", str(piped_capture)],
            width=WIDTH,
            height=HEIGHT,
        )
        try:
            piped.write_stdin(b"piped-first\n")
            piped.wait_for("piped-first")
            stop(piped)
        finally:
            if piped.process.poll() is None:
                piped.process.kill()
                piped.process.wait()
                piped.close()

        # A pipeline cannot be re-attached, so the source comes back visible and
        # explained rather than silently absent.
        app = launch(binary, [], environment, piped_capture)
        try:
            app.wait_for("standard input")
            app.wait_for("not acquiring")
            app.wait_for("0 of 1 sources")
            app.wait_for("1 could not be re-acquired")
            # Nothing was acquired, so the Add source dialog holds focus and a
            # bare `q` would be typed into it.
            app.send(b"\x03")
            assert app.wait_exit(timeout=8) == 0
            app.assert_restored()
            app.close()
        finally:
            if app.process.poll() is None:
                app.process.kill()
                app.process.wait()
                app.close()

    print(
        "Resume PTY passed: session set restored with no arguments, file continued "
        "without duplicates, command relaunched once, arguments added and deduplicated, "
        "--fresh acquired nothing and deleted nothing, stdin explained in the sidebar"
    )


if __name__ == "__main__":
    run(pathlib.Path(sys.argv[1]).resolve())
