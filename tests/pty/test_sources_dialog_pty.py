#!/usr/bin/env python3
"""Remembered command sources stay selectable and removable across restart."""

from __future__ import annotations

import pathlib
import shlex
import sys
import tempfile
import time

from test_enrichment_chain_pty import stop
from test_lvu_pty import PtyApp, isolated_environment


def launch(binary: pathlib.Path, capture: pathlib.Path, env: dict[str, str], *args: str) -> PtyApp:
    return PtyApp(
        binary,
        [*args, "--capture-dir", str(capture)],
        width=140,
        height=30,
        environment=env,
    )


def run(binary: pathlib.Path) -> None:
    with tempfile.TemporaryDirectory(prefix="lvu-sources-dialog-pty-") as directory:
        root = pathlib.Path(directory)
        capture = root / "capture"
        env = isolated_environment(root)
        launches = root / "launches"
        command = (
            f"printf 'launch\\n' >> {shlex.quote(str(launches))}; "
            "printf 'remembered-command-row\\n'"
        )

        # Establish a remembered command in the durable session.
        app = launch(binary, capture, env, "--command", command)
        try:
            app.wait_for("remembered-command-row", timeout=20)
            assert launches.read_text().splitlines() == ["launch"]
            stop(app)
        finally:
            if app.process.poll() is None:
                app.process.kill()
                app.process.wait(timeout=5)
                app.close()

        # Command sources deliberately do not auto-launch. They must still own
        # a selectable All events view and be manageable without discovery.
        app = launch(binary, capture, env)
        try:
            app.wait_for("not acquiring", timeout=20)
            app.wait_until(
                lambda text: "› All events" in text,
                "remembered command has a selectable canonical view",
            )
            # Lowercase n is always Add source; uppercase N manages existing
            # sources and therefore acts on the highlighted row.
            app.send(b"n")
            app.wait_for("Add source")
            assert "Existing sources" not in app.text(), app.text()
            app.send(b"\x1b")
            app.wait_until(lambda text: "Add source" not in text, "Add source closes")
            app.send(b"N")
            app.wait_for("Sources")
            app.wait_for("Full status")
            app.wait_for("remembered commands never start automatically")
            app.wait_for("explicit")
            app.wait_for("launch")

            app.send(b"R")
            app.wait_until(
                lambda _text: launches.read_text().splitlines() == ["launch", "launch"],
                "explicit restart launches the remembered command exactly once",
                timeout=20,
            )

            # Input -> Add source -> Restart -> Remove, then Enter twice.
            app.send(b"\t\t\t\r")
            app.wait_for("Delete again to confirm")
            app.send(b"\r")
            app.wait_until(
                lambda text: "shell command" not in text and "not acquiring" not in text,
                "removed source leaves the current workspace",
                timeout=20,
            )
            stop(app)
        finally:
            if app.process.poll() is None:
                app.process.kill()
                app.process.wait(timeout=5)
                app.close()

        # The worker session update is durable: restart cannot resurrect the
        # removed command, while its journal is intentionally retained.
        app = launch(binary, capture, env)
        try:
            time.sleep(0.8)
            app.drain()
            assert "shell command" not in app.text(), app.text()
            assert "not acquiring" not in app.text(), app.text()
            assert list(capture.rglob("*.journal")), "removal must retain captured bytes"
            # An empty workspace intentionally keeps Add source open; Ctrl-C
            # is the global clean-exit path even while that editor has focus.
            app.send(b"\x03")
            assert app.wait_exit(timeout=12) == 0
            app.assert_restored()
            app.close()
        finally:
            if app.process.poll() is None:
                app.process.kill()
                app.process.wait(timeout=5)
                app.close()

    print(
        "Sources dialog PTY passed: selectable remembered command, full status, "
        "immediate removal, durable non-resurrection, retained journal"
    )


if __name__ == "__main__":
    run(pathlib.Path(sys.argv[1]).resolve())
