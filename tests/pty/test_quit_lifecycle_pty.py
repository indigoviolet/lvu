#!/usr/bin/env python3
"""Bare-q quit lifecycle: splash, empty workspace, base, editor and layer.

A bare `q` quits from the startup splash and from the workspace base, stays
literal inside editors, and dismisses (never quits) a dialog layer. Every quit
must exit 0 with the terminal restored: alternate screen left, bracketed paste
and mouse capture disabled, wrapping back on, and PTY termios unchanged.

Fixture roots are marker-backed and only this suite's own tree is removed.
"""
import pathlib
import shutil
import sys
import tempfile

from pyte.modes import DECAWM

from test_lvu_pty import FILTER_TITLE, PtyApp

MARKER = ".lvu-test-reproducer"
LOGDIR = pathlib.Path(
    "/mnt/HC_Volume_106796581/lvu-build/lvu-quit-terminal-repro"
)


def make_root() -> pathlib.Path:
    root = pathlib.Path(tempfile.mkdtemp(prefix="lvu-quit-lifecycle-"))
    (root / MARKER).write_text("quit lifecycle PTY fixtures\n")
    return root


def environment(root: pathlib.Path) -> dict[str, str]:
    return {
        "NO_COLOR": "",
        "COLORTERM": "truecolor",
        "XDG_CONFIG_HOME": str(root / "config"),
        "XDG_DATA_HOME": str(root / "data"),
        "XDG_CACHE_HOME": str(root / "cache"),
    }


def quit_and_check(app: PtyApp, label: str) -> None:
    code = app.wait_exit(timeout=8)
    assert code == 0, f"{label}: exit was {code}, screen was:\n{app.text()}"
    app.assert_restored()
    app.drain()
    assert DECAWM in app.screen.mode, f"{label}: terminal wrapping not restored"
    transcript = bytes(app.transcript)
    leave = transcript.rfind(b"\x1b[?1049l")
    clear = transcript.rfind(b"\x1b[2J", 0, leave)
    assert clear >= 0, f"{label}: owned alternate screen not cleared before leaving"
    assert b"\x1b[2J" not in transcript[leave:], f"{label}: restored shell screen was erased"
    assert "quit lifecycle fixture" not in app.text(), f"{label}: TUI cells survived quit"


def story_splash_q(binary: pathlib.Path, root: pathlib.Path) -> None:
    """Bare q on the startup splash quits a sourceless launch cleanly."""
    home = root / "splash-q"
    home.mkdir()
    app = PtyApp(
        binary,
        ["--capture-dir", str(home / "capture"), "--fresh"],
        width=100,
        height=28,
        environment=environment(home),
    )
    try:
        app.wait_for("PRESS ANY KEY", timeout=10.0)
        app.send(b"q")
        quit_and_check(app, "splash-q")
    finally:
        if app.process.poll() is None:
            app.process.kill()
            app.process.wait()
        (LOGDIR / "quit-splash-q.ansi").write_bytes(app.transcript)
        app.close()


def story_splash_esc_then_q(binary: pathlib.Path, root: pathlib.Path) -> None:
    """Esc still enters the app from the splash; q quits the empty base."""
    home = root / "splash-esc"
    home.mkdir()
    app = PtyApp(
        binary,
        ["--capture-dir", str(home / "capture"), "--fresh"],
        width=100,
        height=28,
        environment=environment(home),
    )
    try:
        app.wait_for("PRESS ANY KEY", timeout=10.0)
        app.send(b"\x1b")
        app.wait_for("Add source", timeout=5.0)
        app.send(b"\x1b")
        app.wait_until(
            lambda text: "Add source" not in text, "source dialog popped", timeout=5.0
        )
        app.send(b"q")
        quit_and_check(app, "empty-base-q")
    finally:
        if app.process.poll() is None:
            app.process.kill()
            app.process.wait()
        (LOGDIR / "quit-empty-base-q.ansi").write_bytes(app.transcript)
        app.close()


def story_base_q(binary: pathlib.Path, root: pathlib.Path) -> None:
    """Bare q quits a sourced workspace cleanly with modes restored."""
    home = root / "base-q"
    home.mkdir()
    source = home / "events.log"
    source.write_text('{"service":"api","message":"quit lifecycle fixture"}\n')
    app = PtyApp(
        binary,
        [str(source), "--capture-dir", str(home / "capture"), "--fresh"],
        width=100,
        height=28,
        environment=environment(home),
    )
    try:
        app.wait_for("quit lifecycle fixture", timeout=10.0)
        app.send(b"q")
        quit_and_check(app, "base-q")
    finally:
        if app.process.poll() is None:
            app.process.kill()
            app.process.wait()
        (LOGDIR / "quit-base-q.ansi").write_bytes(app.transcript)
        app.close()


def story_editor_q_stays_literal(binary: pathlib.Path, root: pathlib.Path) -> None:
    """q inside the Filter editor is text; Esc closes it and q then quits."""
    home = root / "editor-q"
    home.mkdir()
    source = home / "events.log"
    source.write_text('{"service":"api","message":"quit lifecycle fixture"}\n')
    app = PtyApp(
        binary,
        [str(source), "--capture-dir", str(home / "capture"), "--fresh"],
        width=100,
        height=28,
        environment=environment(home),
    )
    try:
        app.wait_for("quit lifecycle fixture", timeout=10.0)
        app.send(b"/")
        app.wait_for(FILTER_TITLE, timeout=5.0)
        # Wait for the unique complete draft, not a q already present in the
        # log fixture, before issuing subsequent editing/dismissal input.
        app.send(b"qzz")
        app.wait_for("qzz", timeout=5.0)
        assert app.process.poll() is None, "q in an editor must remain literal"
        # Clear the literal before dismissing so the debounce does not filter
        # the fixture out from under the quit that follows.
        app.send(b"\x01\x0b")
        app.send(b"\x1b")
        app.wait_until(
            lambda text: FILTER_TITLE not in text, "editor dismissed", timeout=5.0
        )
        app.send(b"q")
        quit_and_check(app, "editor-then-q")
    finally:
        if app.process.poll() is None:
            app.process.kill()
            app.process.wait()
        (LOGDIR / "quit-editor-q.ansi").write_bytes(app.transcript)
        app.close()


def story_layer_q_dismisses(binary: pathlib.Path, root: pathlib.Path) -> None:
    """q dismisses a dialog layer without quitting; the next q quits."""
    home = root / "layer-q"
    home.mkdir()
    source = home / "events.log"
    source.write_text('{"service":"api","message":"quit lifecycle fixture"}\n')
    app = PtyApp(
        binary,
        [str(source), "--capture-dir", str(home / "capture"), "--fresh"],
        width=100,
        height=28,
        environment=environment(home),
    )
    try:
        app.wait_for("quit lifecycle fixture", timeout=10.0)
        app.send(b"e")
        app.wait_for("Enrichment", timeout=5.0)
        app.send(b"q")
        app.wait_until(
            lambda text: "Enrichment" not in text,
            "layer dismissed by q",
            timeout=5.0,
        )
        assert app.process.poll() is None, "layer dismissal must not quit"
        app.send(b"q")
        quit_and_check(app, "layer-then-q")
    finally:
        if app.process.poll() is None:
            app.process.kill()
            app.process.wait()
        (LOGDIR / "quit-layer-q.ansi").write_bytes(app.transcript)
        app.close()


def main() -> None:
    binary = pathlib.Path(sys.argv[1]).resolve()
    LOGDIR.mkdir(parents=True, exist_ok=True)
    root = make_root()
    try:
        story_splash_q(binary, root)
        story_splash_esc_then_q(binary, root)
        story_base_q(binary, root)
        story_editor_q_stays_literal(binary, root)
        story_layer_q_dismisses(binary, root)
    finally:
        # Only this suite's own marker-backed tree is removed.
        assert (root / MARKER).exists()
        shutil.rmtree(root, ignore_errors=True)
    print("Quit lifecycle PTY passed: splash-q, empty-base-q, base-q, editor-q, layer-q")


if __name__ == "__main__":
    main()
