#!/usr/bin/env python3
"""A plain unstructured event opens an honest empty Fields dialog."""
import pathlib
import sys
import tempfile

from test_lvu_pty import PtyApp


def run(binary: pathlib.Path) -> None:
    with tempfile.TemporaryDirectory(prefix="lvu-empty-fields-pty-") as directory:
        root = pathlib.Path(directory)
        source = root / "plain.log"
        source.write_text("plain unstructured line\n")
        environment = {
            "XDG_CONFIG_HOME": str(root / "config"),
            "XDG_DATA_HOME": str(root / "data"),
            "XDG_CACHE_HOME": str(root / "cache"),
            "NO_COLOR": "",
            "COLORTERM": "truecolor",
        }
        app = PtyApp(
            binary,
            [str(source), "--capture-dir", str(root / "capture")],
            width=96,
            height=20,
            environment=environment,
        )
        try:
            app.wait_for("plain unstructured line", timeout=8.0)
            app.send(b"i")
            fields = app.wait_for("No fields for this record")
            assert "Fields · record" in fields
            assert "Space pin" not in fields
            assert "[ Color ]" not in fields
            assert "r correlate" not in fields
            # §11 replaced the remembered `o` with the action it stood for.
            assert "[ Raw context ]" in fields

            app.send(b"o")
            context = app.wait_for("Raw context")
            assert "plain unstructured line" in context
            app.send(b"\x1b")
            app.wait_for("No fields for this record")
            app.send(b"\x1b")
            app.wait_until(lambda text: "Fields · record" not in text, "Fields closed")
            app.wait_for("plain unstructured line")
            app.send(b"q")
            assert app.wait_exit(timeout=8) == 0
            app.assert_restored()
        finally:
            if app.process.poll() is None:
                app.send(b"\x03")
                try:
                    app.wait_exit(5)
                except Exception:
                    app.process.kill()
                    app.process.wait()
            app.close()
    print("Empty Event fields PTY passed: plain raw event, honest empty state, raw context")


if __name__ == "__main__":
    run(pathlib.Path(sys.argv[1]).resolve())
