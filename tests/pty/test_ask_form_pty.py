#!/usr/bin/env python3
"""Exercise the Ask 🧠 form on the dialog anatomy, without a remote proposal.

Covers the three reported defects: the actions sat above the fields, the
multi-line Request field moved only left and right, and the prepared timestamp
task offered an irrelevant kind selector with an unexplained prefilled prompt.
"""
import os
import pathlib
import sys
import tempfile

from test_lvu_pty import PtyApp


def environment(root: pathlib.Path) -> dict[str, str]:
    return {
        "XDG_CONFIG_HOME": str(root / "config"),
        "XDG_CACHE_HOME": str(root / "cache"),
        "XDG_DATA_HOME": str(root / "data"),
        "MISE_DATA_DIR": os.environ.get("MISE_DATA_DIR", str(pathlib.Path.home() / ".local/share/mise")),
        "MISE_CONFIG_DIR": os.environ.get("MISE_CONFIG_DIR", str(pathlib.Path.home() / ".config/mise")),
        "MISE_CACHE_DIR": os.environ.get("MISE_CACHE_DIR", str(pathlib.Path.home() / ".cache/mise")),
        "UV_CACHE_DIR": os.environ.get("UV_CACHE_DIR", str(pathlib.Path.home() / ".cache/uv")),
        "NO_COLOR": "",
        "COLORTERM": "truecolor",
    }


def settle(app: PtyApp, quiet: float = 0.25, timeout: float = 5.0) -> str:
    """Drain until the child stops emitting.

    `Screen.resize` keeps pyte's existing cells, so a predicate that only looks
    for text already on the pre-resize frame matches before the app has redrawn
    anything. Waiting for output to stop means the assertions below read the
    resized frame rather than the old one.
    """
    import time

    deadline = time.monotonic() + timeout
    seen = -1
    quiet_until = time.monotonic() + quiet
    while time.monotonic() < deadline:
        app.drain()
        if len(app.transcript) != seen:
            seen = len(app.transcript)
            quiet_until = time.monotonic() + quiet
        elif time.monotonic() >= quiet_until:
            return app.text()
        time.sleep(0.02)
    return app.text()


def quit_cleanly(app: PtyApp) -> None:
    app.send(b"q")
    try:
        app.process.wait(timeout=30)
    except Exception:
        raise AssertionError(f"lvu did not quit; screen was:\n{app.text()}")
    app.drain()
    assert app.process.returncode == 0
    app.assert_restored()


def row_of(text: str, needle: str) -> int:
    for index, line in enumerate(text.splitlines()):
        if needle in line:
            return index
    raise AssertionError(f"{needle!r} is not on screen:\n{text}")


def open_app(binary: pathlib.Path, source: pathlib.Path, root: pathlib.Path,
             phase: str = "shared", **kwargs) -> PtyApp:
    # One capture root per phase: the harness otherwise shares one per test
    # process, and a phase would restore the previous phase's workspace.
    arguments = (
        ["--demo"]
        if binary.name == "lvu"
        else ["--file", str(source), "--capture-dir", str(root / f"capture-{phase}")]
    )
    app = PtyApp(binary, arguments, environment=environment(root), **kwargs)
    if arguments[0] == "--demo":
        app.wait_for("PRESS ANY KEY")
        app.send(b" ")
    app.wait_for("broken" if arguments[0] == "--file" else "fixture request 01 completed")
    return app


def generic_form(binary: pathlib.Path, source: pathlib.Path, root: pathlib.Path) -> None:
    """`A` keeps the kind selector, and every action comes after the fields."""
    app = open_app(binary, source, root, "generic", width=100, height=24)
    try:
        app.send(b"A")
        opened = app.wait_for("Ask")
        # §3 region order: the fields first, one message row, the actions last.
        assert row_of(opened, "Kind") < row_of(opened, "Request") < row_of(opened, "[ Submit ]")
        assert row_of(opened, "Ready") < row_of(opened, "[ Submit ]")
        assert "Proposal" in opened and "Activity" in opened
        # §11.1/§11.7: no nested boxes and no button row above the fields.
        assert "[ Kind:" not in opened and "State" not in opened
        assert "[ More ]" not in opened

        request_row = row_of(opened, "Request")
        app.send(f"\x1b[<0;12;{request_row + 1}M\x1b[<0;12;{request_row + 1}m".encode())
        app.send("first 界\rsecond é".encode())
        multiline = app.wait_until(
            # pyte composes the combining sequence for its screen model; the
            # transcript still proves the original UTF-8 keystrokes.
            lambda text: "first 界" in text and "second é" in text,
            "multiline Unicode request with visible caret",
        )
        assert not app.screen.cursor.hidden
        assert app.screen.cursor.y >= request_row

        # The kind dropdown is anchored to its field and Escape closes only it.
        app.send(b"\x1b[Z\r")
        app.wait_for("Enrichment")
        app.send(b"\x1b")
        rolled_back = app.wait_until(
            lambda text: "Enrichment" not in text and "Filter" in text,
            "kind dropdown Escape rollback",
        )
        assert "first 界" in rolled_back
        app.send(b"\r")
        app.wait_for("Enrichment")
        app.send(b"\x1b[B\r")
        kind_chosen = app.wait_until(
            lambda text: "Enrichment" in text and "Filter" not in text,
            "chosen kind replaces the field value",
        )
        assert row_of(kind_chosen, "Kind") < row_of(kind_chosen, "[ Submit ]")

        app.resize(54, 16)
        narrow = settle(app)
        assert "[ Submit ]" in narrow and "Proposal" in narrow, narrow
        assert row_of(narrow, "Request") < row_of(narrow, "[ Submit ]")
        assert "Ask" in narrow

        app.send(b"\x1b")
        app.wait_until(lambda text: "Proposal" not in text, "Ask form closes")
        app.send(b"q")
        app.process.wait(timeout=5)
        app.drain()
        assert app.process.returncode == 0
        assert b"\x1b[38;2;" in app.transcript
        app.assert_restored()
    finally:
        if app.process.poll() is None:
            app.process.kill()
        app.close()


def vertical_navigation(binary: pathlib.Path, source: pathlib.Path, root: pathlib.Path) -> None:
    """A wrapped request scrolls up and down and the caret stays visible."""
    app = open_app(binary, source, root, "vertical", width=100, height=30)
    try:
        app.send(b"A")
        app.wait_for("Request")
        # One long paragraph with no newline at all: the reported case where
        # Up and Down did nothing because there was only one logical line.
        app.send(b"alpha bravo charlie delta echo foxtrot golf hotel india juliett kilo lima mike ")
        app.send(b"november oscar papa quebec romeo sierra tango uniform victor whiskey xray ")
        app.send(b"zulu one two three four five six seven eight nine ten eleven twelve thirteen ")
        app.send(b"fourteen fifteen sixteen seventeen eighteen nineteen twenty yankee")
        tail = app.wait_until(lambda text: "yankee" in text, "the wrapped tail follows the caret")
        assert "alpha bravo" not in tail, "a three-row field must scroll, not grow"
        caret_bottom = app.screen.cursor.y

        for _ in range(6):
            app.send(b"\x1b[A")
        head = app.wait_until(lambda text: "alpha bravo" in text, "Up reaches earlier visual rows")
        assert app.screen.cursor.y <= caret_bottom, "the view follows the caret upwards"
        assert not app.screen.cursor.hidden
        assert "yankee" not in head, "the field scrolled rather than showing every row"

        for _ in range(6):
            app.send(b"\x1b[B")
        app.wait_until(lambda text: "yankee" in text, "Down returns to the tail")

        # Alt-N still inserts a newline, and the field keeps the caret visible.
        app.send(b"\x1bn")
        app.send(b"ZEBRA")
        with_newline = app.wait_until(lambda text: "ZEBRA" in text, "Alt-N newline")
        assert not app.screen.cursor.hidden
        assert row_of(with_newline, "ZEBRA") <= row_of(with_newline, "Proposal")

        app.send(b"\x1b")
        app.wait_until(lambda text: "Proposal" not in text, "Ask form closes")
        app.send(b"q")
        app.process.wait(timeout=5)
        app.drain()
        assert app.process.returncode == 0
        app.assert_restored()
    finally:
        if app.process.poll() is None:
            app.process.kill()
        app.close()


def prepared_task(binary: pathlib.Path, source: pathlib.Path, root: pathlib.Path) -> None:
    """Timestamp column states its task, drops the kind and offers the prompt."""
    app = open_app(binary, source, root, "prepared", width=110, height=30)
    try:
        app.send(b"t")
        app.wait_for("Time window")
        app.send(b"\x1bt")
        prepared = app.wait_until(
            lambda text: "Timestamp column" in text and "Request" in text,
            "the prepared timestamp task opens Ask",
        )
        # The task is named in the title and explained in the header.
        assert "timestamp_utc" in prepared, prepared
        # No kind selector: the task already decided it.
        assert "Kind" not in prepared, prepared
        # The prefilled request is explicitly an editable starting point.
        assert "starting point" in prepared, prepared
        assert row_of(prepared, "Request") < row_of(prepared, "[ Submit ]")

        # It really is editable, and the edit lands in the field.
        app.send(b"\x01")
        app.send(b"MYPREFIX ")
        edited = app.wait_until(lambda text: "MYPREFIX " in text, "the prepared request is editable")
        assert row_of(edited, "MYPREFIX ") < row_of(edited, "Proposal")

        app.resize(80, 24)
        narrow = settle(app)
        assert "[ Submit ]" in narrow and "Proposal" in narrow, narrow
        assert "Kind" not in narrow, narrow
        assert row_of(narrow, "Request") < row_of(narrow, "[ Submit ]")

        # Escape closes one layer at a time; Ask may sit over the Time dialog
        # it was opened from, so keep dismissing until the workspace is back.
        for _ in range(3):
            app.send(b"\x1b")
            try:
                app.wait_until(
                    lambda text: "Proposal" not in text and "Time window" not in text,
                    "layers dismissed back to the workspace",
                    timeout=1.0,
                )
                break
            except AssertionError:
                continue
        else:
            raise AssertionError(f"dialogs never closed:\n{app.text()}")
        quit_cleanly(app)
    finally:
        if app.process.poll() is None:
            app.process.kill()
        app.close()


def run(binary: pathlib.Path) -> None:
    with tempfile.TemporaryDirectory(prefix="lvu-ask-form-pty-") as temporary:
        root = pathlib.Path(temporary)
        source = root / "events.log"
        source.write_text('{"level":"ERROR","message":"broken","ts":"2026-09-07T12:00:00Z"}\n')
        generic_form(binary, source, root)
        vertical_navigation(binary, source, root)
        prepared_task(binary, source, root)


if __name__ == "__main__":
    run(pathlib.Path(sys.argv[1]).resolve())
    print("Ask form PTY passed: anatomy order, anchored kind dropdown, "
          "wrapped vertical navigation, prepared timestamp task")
