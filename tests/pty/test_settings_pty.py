#!/usr/bin/env python3
"""Exercise global TOML settings, live theme preview, and restart loading."""
import pathlib
import os
import sys
import tempfile
import tomllib

from test_lvu_pty import PtyApp


def stop(app: PtyApp) -> None:
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
    app.close()


def run(binary: pathlib.Path) -> None:
    temporary = tempfile.TemporaryDirectory(prefix="lvu-settings-pty-")
    root = pathlib.Path(temporary.name)
    source = root / "events.log"
    source.write_text("settings-visible-record\n")
    environment = {
        "XDG_CONFIG_HOME": str(root / "config"),
        "XDG_CACHE_HOME": str(root / "cache"),
        "XDG_DATA_HOME": str(root / "data"),
        "MISE_DATA_DIR": os.environ.get("MISE_DATA_DIR", str(pathlib.Path.home() / ".local/share/mise")),
        "MISE_CONFIG_DIR": os.environ.get("MISE_CONFIG_DIR", str(pathlib.Path.home() / ".config/mise")),
        "MISE_CACHE_DIR": os.environ.get("MISE_CACHE_DIR", str(pathlib.Path.home() / ".cache/mise")),
        "UV_CACHE_DIR": os.environ.get("UV_CACHE_DIR", str(pathlib.Path.home() / ".cache/uv")),
        "NO_COLOR": "", "COLORTERM": "truecolor",
    }
    arguments = ["--file", str(source)]

    first = PtyApp(binary, arguments, width=112, height=28, environment=environment)
    try:
        first.wait_for("settings-visible-record")
        first.send(b",")
        opened = first.wait_for("[ Save ]")
        provider_row = next(
            row for row, line in enumerate(opened.splitlines()) if "Provider/model" in line
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
        assert "Theme: terminal" in closed, "Escape must preserve the unchosen Theme value"
        first.send(b" ")
        first.wait_for("love-dark")
        first.send(b"\x1b[B\r")
        first.wait_for("Pending: Changes are not saved")
        first.resize(54, 12)
        first.wait_for("[ More ]")
        first.send(b"\t" * 9)
        first.resize(150, 40)
        first.wait_until(
            lambda text: "[ More ]" not in text,
            "resize removes inactive overflow control",
        )
        first.send(b"\r")
        first.wait_for("saved and applied")
    finally:
        stop(first)

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
        assert str(path) in screen
        assert str(root / "cache" / "lvu") in screen
    finally:
        stop(second)
        temporary.cleanup()


if __name__ == "__main__":
    run(pathlib.Path(sys.argv[1]).resolve())
    print("Settings PTY passed: XDG TOML save, live theme preview, restart restore")
