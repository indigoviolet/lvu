#!/usr/bin/env python3
"""Exercise the bounded Ask form without invoking a remote proposal."""
import os
import pathlib
import sys
import tempfile

from test_lvu_pty import PtyApp


def run(binary: pathlib.Path) -> None:
    with tempfile.TemporaryDirectory(prefix="lvu-ask-form-pty-") as temporary:
        root = pathlib.Path(temporary)
        source = root / "events.log"
        source.write_text('{"level":"ERROR","message":"broken"}\n')
        environment = {
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
        arguments = ["--demo"] if binary.name == "lvu" else ["--file", str(source)]
        app = PtyApp(binary, arguments, width=100, height=24, environment=environment)
        try:
            if arguments[0] == "--demo":
                app.wait_for("PRESS ANY KEY")
                app.send(b" ")
            app.wait_for("broken" if arguments[0] == "--file" else "fixture request 01 completed")
            app.send(b"A")
            opened = app.wait_for("[ Kind: Filter")
            assert "[ Submit ]" in opened and "Proposal and activity" in opened

            request_row = next(
                row for row, line in enumerate(opened.splitlines()) if "Request" in line
            ) + 1
            app.send(f"\x1b[<0;4;{request_row + 1}M\x1b[<0;4;{request_row + 1}m".encode())
            app.send("first 界\rsecond e\u0301".encode())
            multiline = app.wait_until(
                # pyte composes the combining sequence for its screen model; the
                # transcript still proves the original UTF-8 keystrokes.
                lambda text: "first 界" in text and "second é" in text,
                "multiline Unicode request with visible caret",
            )
            assert not app.screen.cursor.hidden
            assert app.screen.cursor.y >= request_row

            app.send(b"\x1b[Z\r")
            app.wait_for("Enrichment")
            app.send(b"\x1b[B\x1b")
            rolled_back = app.wait_until(
                lambda text: "[ Kind: Filter" in text and "Enrichment" not in text,
                "kind dropdown Escape rollback",
            )
            assert "first 界" in rolled_back

            app.send(b"\r")
            app.wait_for("Enrichment")
            app.send(b"\x1b[B\r")
            app.wait_for("[ Kind: Enrichment")
            app.resize(46, 12)
            narrow = app.wait_for("[ More ]")
            assert "[ Submit ]" in narrow and "Request" in narrow and "State" in narrow

            app.send(b"\x1b")
            app.wait_until(lambda text: "Ask 🧠" not in text, "Ask form closes")
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


if __name__ == "__main__":
    run(pathlib.Path(sys.argv[1]).resolve())
    print("Ask form PTY passed: dropdown, multiline cursor, controls, overflow, restoration")
