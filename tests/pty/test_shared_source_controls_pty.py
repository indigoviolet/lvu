#!/usr/bin/env python3
"""Exercise automatic Source suggestions and direct arrow/Enter admission."""

from __future__ import annotations

import os
import pathlib
import sys
import tempfile

from test_lvu_pty import PtyApp


def run(binary: pathlib.Path) -> None:
    with tempfile.TemporaryDirectory(prefix="lvu-source-direct-") as temporary:
        root = pathlib.Path(temporary)
        (root / "file alpha.log").write_text("alpha candidate\n")
        (root / "file über.log").write_text("unicode candidate selected\n")
        app = PtyApp(
            binary,
            ["--capture-dir", str(root / "capture")],
            width=92,
            height=24,
            cwd=root,
            environment={
                "LVU_NO_DELIGHT": "1",
                "XDG_CONFIG_HOME": str(root / "config"),
                "XDG_DATA_HOME": str(root / "data"),
                "XDG_CACHE_HOME": str(root / "cache"),
                "MISE_DATA_DIR": os.environ.get("MISE_DATA_DIR", ""),
            },
        )
        try:
            app.wait_for("Add source")
            app.send(b"file ")
            choices = app.wait_until(
                lambda text: "file alpha.log" in text and "file über.log" in text,
                "automatic bounded file suggestions",
            )
            assert "Complete path" not in choices, choices
            assert "[ Open ]" not in choices, choices
            for y, line in enumerate(app.screen.display):
                if "file über.log" in line:
                    x = line.index("file über.log") + 1
                    app.send(f"\x1b[<0;{x};{y + 1}M\x1b[<0;{x};{y + 1}m".encode())
                    break
            else:
                raise AssertionError("Unicode suggestion has no mouse target")
            app.wait_until(
                lambda text: "> file über.log" in text,
                "mouse selects the Unicode suggestion",
            )
            app.send(b"\x1b[A")
            app.wait_until(
                lambda text: "> file alpha.log" in text,
                "Up directly selects the first suggestion",
            )
            app.send(b"\x1b[B")
            selected = app.wait_until(
                lambda text: "> file über.log" in text,
                "Down selects the second suggestion from Source input",
            )
            assert "FILE PATH" in selected, selected
            app.send(b"\r")
            captured = app.wait_for("unicode candidate selected", timeout=8.0)
            assert "Raw events" in captured, captured
            app.send(b"q")
            app.process.wait(timeout=5)
            app.drain()
            assert app.process.returncode == 0
            app.assert_restored()
        finally:
            if app.process.poll() is None:
                app.process.kill()
                app.process.wait(timeout=5)
            app.close()


if __name__ == "__main__":
    run(pathlib.Path(sys.argv[1]).resolve())
    print("Shared Source direct controls PTY passed")
