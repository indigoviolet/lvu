#!/usr/bin/env python3
"""Exercise global TOML settings, live theme preview, and restart loading."""
import pathlib
import os
import sys
import tempfile
import tomllib

from test_lvu_pty import PtyApp

LOVE_DARK_FOCUS = b"\x1b[1m\x1b[38;2;35;20;25;48;2;255;167;151m"
FRAME_END = b"\x1b[?2026l"


def stop(app: PtyApp, transcript: pathlib.Path) -> None:
    try:
        if app.process.poll() is None:
            app.send(b"\x1b")
            app.wait_until(
                lambda text: "Effective values and paths" not in text,
                "settings dialog closes before quit",
            )
            app.send(b"q")
            app.process.wait(timeout=5)
        app.drain()
        assert app.process.returncode == 0
        app.assert_restored()
    finally:
        app.drain()
        transcript.write_bytes(bytes(app.transcript))
        app.close()


def focus_more(app: PtyApp) -> None:
    """Click the [ More ] control so the effective-values pane owns the arrows.

    Clicking is deterministic where a Tab count is not: it does not depend on
    how many controls the form happens to expose at this size.
    """
    app.drain()
    for y, row in enumerate(app.screen.display):
        if "[ More ]" in row:
            x = row.index("[ More ]") + 2
            app.send(f"\x1b[<0;{x + 1};{y + 1}M".encode())
            app.send(f"\x1b[<0;{x + 1};{y + 1}m".encode())
            app.drain()
            return
    raise AssertionError(f"no [ More ] control on screen\n{app.text()}")


def scroll_pane_until(app: PtyApp, needle: str) -> str:
    """Walk the focused pane down, waiting for each frame the user would see."""
    seen = app.text()
    for _ in range(24):
        if needle in seen:
            return seen
        before = app.text()
        app.send(b"\x1b[B")
        try:
            app.wait_until(lambda text: text != before, "settings pane scrolls", timeout=2)
        except AssertionError:
            pass
        seen += "\n" + app.text()
    return seen


def wait_focused_frame(app: PtyApp, label: str, start: int) -> str:
    label_bytes = label.encode()

    def complete_and_focused(text: str) -> bool:
        delta = bytes(app.transcript[start:])
        focused = delta.find(LOVE_DARK_FOCUS + label_bytes)
        return (
            focused >= 0
            and delta.find(FRAME_END, focused) >= 0
            and label in text
            and app.screen.cursor.hidden
        )

    return app.wait_until(complete_and_focused, f"completed frame focused on {label}")


def settings_environment(root: pathlib.Path) -> dict[str, str]:
    return {
        "XDG_CONFIG_HOME": str(root / "config"),
        "XDG_CACHE_HOME": str(root / "cache"),
        "XDG_DATA_HOME": str(root / "data"),
        "MISE_DATA_DIR": os.environ.get("MISE_DATA_DIR", str(pathlib.Path.home() / ".local/share/mise")),
        "MISE_CONFIG_DIR": os.environ.get("MISE_CONFIG_DIR", str(pathlib.Path.home() / ".config/mise")),
        "MISE_CACHE_DIR": os.environ.get("MISE_CACHE_DIR", str(pathlib.Path.home() / ".cache/mise")),
        "UV_CACHE_DIR": os.environ.get("UV_CACHE_DIR", str(pathlib.Path.home() / ".cache/uv")),
        "NO_COLOR": "", "COLORTERM": "truecolor",
    }


def run(binary: pathlib.Path) -> None:
    evidence = pathlib.Path(tempfile.mkdtemp(prefix="lvu-settings-proof-"))
    print(f"Settings PTY evidence: {evidence}", flush=True)
    temporary = tempfile.TemporaryDirectory(prefix="lvu-settings-pty-")
    root = pathlib.Path(temporary.name)
    source = root / "events.log"
    source.write_text("settings-visible-record\n")
    environment = settings_environment(root)
    arguments = ["--fresh", "--file", str(source)]

    first = PtyApp(binary, arguments, width=112, height=28, environment=environment)
    try:
        first.wait_for("settings-visible-record")
        first.send(b",")
        opened = first.wait_for("[ Save ]")
        provider_row = next(
            row for row, line in enumerate(opened.splitlines()) if "Provider / model" in line
        )
        provider_column = first.screen.cursor.x + 1
        first.send(
            (
                f"\x1b[<0;{provider_column};{provider_row + 1}M"
                f"\x1b[<0;{provider_column};{provider_row + 1}m"
            ).encode()
        )
        first.wait_until(
            lambda _text: first.screen.cursor.y == provider_row,
            "pointer-focused Provider caret inside its field row",
        )
        first.send(b"\x7f" * 64 + b"fixture/provider")
        first.send(b"\t" * 3)
        first.send(b" ")
        first.wait_for("love-dark")
        first.send(b"\x1b[B\x1b")
        closed = first.wait_until(
            lambda text: "love-dark" not in text,
            "Theme dropdown closes with Escape",
        )
        assert "terminal" in closed, "Escape must preserve the unchosen Theme value"
        assert "love-dark" not in closed, closed
        first.send(b" ")
        first.wait_for("love-dark")
        first.send(b"\x1b[B\r")
        first.wait_for("Pending   changes are not saved")
        first.resize(54, 12)
        first.wait_for("[ More ]")
        first.send(b"\t" * 9)
        first.resize(150, 40)
        resized = first.wait_until(
            lambda text: "[ More ]" not in text and "[ Save ]" in text,
            "resize removes inactive overflow control",
        )
        save_row = next(
            row for row, line in enumerate(resized.splitlines()) if "[ Save ]" in line
        )
        save_column = resized.splitlines()[save_row].index("[ Save ]") + 2
        first.send(
            (
                f"\x1b[<0;{save_column};{save_row + 1}M"
                f"\x1b[<0;{save_column};{save_row + 1}m"
            ).encode()
        )
        first.wait_for("saved and applied")
        assert b"\x1b[38;2;" in first.transcript, "Settings workflow did not emit truecolor SGR"
    finally:
        stop(first, evidence / "pointer-save.ansi")

    path = root / "config" / "lvu" / "settings.toml"
    settings = tomllib.loads(path.read_text())
    assert settings["paseo"]["provider"] == "fixture/provider"
    assert settings["appearance"]["theme"] == "love-dark"

    second = PtyApp(binary, arguments, width=112, height=28, environment=environment)
    try:
        second.wait_for("settings-visible-record")
        second.send(b",")
        screen = second.wait_for("fixture/provider")
        assert "love-dark" in screen
        # The form no longer covers the whole terminal, so the effective values
        # sit in a scrollable pane (dialog-system.md §12.14). Focus it and
        # scroll: every path must still be reachable, none may be hidden.
        second.wait_for("[ More ]")
        # Tab to the effective-values pane, then walk it down until the paths
        # appear. Arrow keys reach the pane only while it holds focus, so the
        # loop tabs first and accumulates what each viewport shows.
        second.wait_for("[ More ]")
        focus_more(second)
        paths = scroll_pane_until(second, str(root / "cache" / "lvu"))
        assert str(path) in paths, paths
        assert str(root / "cache" / "lvu") in paths, paths
    finally:
        stop(second, evidence / "pointer-restart.ansi")
        temporary.cleanup()

    keyboard_temporary = tempfile.TemporaryDirectory(prefix="lvu-settings-keyboard-pty-")
    keyboard_root = pathlib.Path(keyboard_temporary.name)
    keyboard_source = keyboard_root / "events.log"
    keyboard_source.write_text("settings-keyboard-record\n")
    keyboard_environment = settings_environment(keyboard_root)
    keyboard_arguments = ["--fresh", "--file", str(keyboard_source)]
    keyboard = PtyApp(
        binary, keyboard_arguments, width=112, height=28, environment=keyboard_environment
    )
    try:
        keyboard.wait_for("settings-keyboard-record")
        keyboard.send(b",")
        keyboard.wait_for("[ Save ]")
        keyboard.send(b"\t" * 3 + b" ")
        keyboard.wait_for("love-dark")
        keyboard.send(b"\x1b[B\r")
        keyboard.wait_for("Pending   changes are not saved")
        keyboard.resize(54, 12)
        keyboard.wait_for("[ More ]")
        more_start = len(keyboard.transcript)
        keyboard.send(b"\t" * 9)
        wait_focused_frame(keyboard, "[ More ]", more_start)

        keyboard.send(b"\r")
        keyboard.assert_remains("Pending   changes are not saved", "saved and applied")
        assert not (keyboard_root / "config" / "lvu" / "settings.toml").exists()

        save_start = len(keyboard.transcript)
        keyboard.resize(150, 40)
        resized = wait_focused_frame(keyboard, "[ Save ]", save_start)
        assert "[ More ]" not in resized
        keyboard.send(b"\r")
        keyboard.wait_for("saved and applied")
    finally:
        stop(keyboard, evidence / "keyboard-save.ansi")

    keyboard_path = keyboard_root / "config" / "lvu" / "settings.toml"
    keyboard_settings = tomllib.loads(keyboard_path.read_text())
    assert keyboard_settings["appearance"]["theme"] == "love-dark"
    keyboard_restart = PtyApp(
        binary, keyboard_arguments, width=112, height=28, environment=keyboard_environment
    )
    try:
        keyboard_restart.wait_for("settings-keyboard-record")
        keyboard_restart.send(b",")
        keyboard_restart.wait_for("love-dark")
        # Same as above: the paths live in the scrollable effective-values pane.
        keyboard_restart.wait_for("[ More ]")
        keyboard_restart.wait_for("[ More ]")
        focus_more(keyboard_restart)
        restarted = scroll_pane_until(keyboard_restart, str(keyboard_path))
        assert str(keyboard_path) in restarted, restarted
        assert "love-dark" in restarted, restarted
    finally:
        stop(keyboard_restart, evidence / "keyboard-restart.ansi")
        keyboard_temporary.cleanup()


if __name__ == "__main__":
    run(pathlib.Path(sys.argv[1]).resolve())
    print("Settings PTY passed: XDG TOML save, live theme preview, restart restore")
