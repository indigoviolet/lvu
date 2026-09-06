#!/usr/bin/env python3
"""Merged views share capture and restore only explicitly opened sources."""
import pathlib
import sys
import tempfile
from test_lvu_pty import PtyApp
from test_enrichment_chain_pty import stop


def run(binary):
    with tempfile.TemporaryDirectory(prefix="lvu-merged-pty-") as directory:
        root = pathlib.Path(directory)
        first, second = root / "alpha.log", root / "beta.log"
        first.write_text("keep alpha\ndrop alpha\n")
        second.write_text("keep beta\ndrop beta\n")
        marker = root / "command-starts"
        command = f"printf 'started\\n' >> '{marker}'; printf 'command alive\\n'; sleep 300"
        env = {"XDG_CONFIG_HOME": str(root / "config"), "XDG_DATA_HOME": str(root / "data"), "XDG_CACHE_HOME": str(root / "cache")}
        args = [str(first), str(second), "--command", command, "--capture-dir", str(root / "capture")]
        app = PtyApp(binary, args, width=140, height=28, cwd=root, environment=env)
        try:
            app.wait_for("keep alpha")
            app.send(b"/"); app.wait_for("Search"); app.send(b"keep"); app.wait_for("applied: keep")
            app.send(b"\x1b"); app.wait_until(lambda text: " Search " not in text, "search closed")
            app.send(b"v"); app.wait_for("Source view")
            app.send(b"\x1bm"); app.wait_for("View sources")
            app.send(b"\x1b[B ")
            app.send(b"\x1b[1;3A")
            app.send(b"\r")
            app.wait_until(lambda text: "View sources" not in text and "keep alpha" in text and "keep beta" in text, "merged filtered rows")
            assert "drop alpha" not in app.text() and "drop beta" not in app.text()
            assert app.text().index("keep beta") < app.text().index("keep alpha"), "explicit source order was ignored"
            first.write_text(first.read_text() + "keep alpha appended\n")
            second.write_text(second.read_text() + "keep beta appended\n")
            app.wait_for("keep alpha appended"); app.wait_for("keep beta appended")
            assert marker.read_text().splitlines() == ["started"], "view editing restarted a command"
            app.send(b"v"); app.wait_for("Source view"); app.send(b"\r")
            app.wait_until(lambda text: "Source view" not in text and "keep alpha appended" in text and "keep beta appended" in text, "cloned merged view")
            stop(app)
            app = PtyApp(binary, [str(first), "--capture-dir", str(root / "capture")], width=140, height=28, cwd=root, environment=env)
            app.wait_for("Waiting for sources")
            assert marker.read_text().splitlines() == ["started"], "restoration launched a remembered command"
            app.send(b"n"); app.wait_for("Add source")
            app.send(str(second).encode() + b"\r")
            app.wait_until(lambda text: "Add source" not in text and "keep beta" in text, "explicit second source opened")
            # The newly opened source has its own view; select the restored merge.
            for _ in range(3):
                if "keep alpha appended" in app.text() and "keep beta appended" in app.text(): break
                app.send(b"[")
                app.wait_until(lambda text: "keep alpha" in text or "keep beta" in text, "view switch")
                app.assert_remains("Log", "impossible sentinel", .1)
            app.wait_for("keep alpha appended"); app.wait_for("keep beta appended")
            assert "drop alpha" not in app.text() and "drop beta" not in app.text()
            assert app.text().index("keep beta") < app.text().index("keep alpha")
            assert marker.read_text().splitlines() == ["started"]
            stop(app)
        finally:
            if app.process.poll() is None:
                app.send(b"\x03")
                try: app.wait_exit(5)
                except Exception: app.process.kill(); app.process.wait()
                app.close()
    print("Merged-view PTY passed: explicit source order, filtering, appends, shared capture, deferred restore and terminal restoration")


if __name__ == "__main__": run(pathlib.Path(sys.argv[1]).resolve())
