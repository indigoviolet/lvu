#!/usr/bin/env python3
"""Exercise global TOML settings, live theme preview, and restart loading."""
import pathlib
import sys
import tempfile
import tomllib

from test_lvu_pty import PtyApp


def stop(app: PtyApp) -> None:
    if app.process.poll() is None:
        app.send(b"\x1b")
        app.wait_until(
            lambda text: "global settings.toml" not in text,
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
        "HOME": str(root / "home"),
        "XDG_CONFIG_HOME": str(root / "config"),
        "XDG_CACHE_HOME": str(root / "cache"),
        "XDG_DATA_HOME": str(root / "data"),
    }
    arguments = ["--file", str(source)]

    first = PtyApp(binary, arguments, width=112, height=28, environment=environment)
    try:
        first.wait_for("settings-visible-record")
        first.send(b",")
        first.wait_for("global settings.toml")
        first.send(b"\x7f" * 64 + b"fixture/provider")
        first.send(b"\x1b[B" * 3)
        first.send(b" ")
        first.wait_for("love-dark")
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
