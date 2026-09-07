#!/usr/bin/env python3
"""Raw context remains independent of the accepted filter and live selection."""
import pathlib
import sys
import tempfile
from test_lvu_pty import PtyApp
from test_enrichment_chain_pty import stop


def run(binary):
    with tempfile.TemporaryDirectory(prefix="lvu-context-pty-") as directory:
        root = pathlib.Path(directory)
        source = root / "events.log"
        source.write_text("".join(f"{'needle' if n == 10 else 'hidden'} row-{n:02}\n" for n in range(50)))
        env = {"XDG_CONFIG_HOME": str(root / "config"), "XDG_DATA_HOME": str(root / "data"), "XDG_CACHE_HOME": str(root / "cache")}
        app = PtyApp(binary, [str(source), "--capture-dir", str(root / "capture")], width=110, height=24, environment=env)
        try:
            app.wait_for("row-49")
            app.send(b"/"); app.wait_for("Search"); app.send(b"needle"); app.wait_for("Applied   needle")
            app.send(b"\x1b"); app.wait_until(lambda t: " Search " not in t, "search closed")
            app.wait_for("needle row-10")
            assert "hidden row-09" not in app.text()
            app.send(b"o"); app.wait_for("Raw context")
            app.wait_for("hidden row-09"); app.wait_for("hidden row-11")
            app.send(b"\x1b[B" * 15); app.wait_for("hidden row-25")
            app.send(b"g"); app.wait_for("needle row-10")
            with source.open("a") as output: output.write("needle live-row-50\n")
            app.wait_for("/ 51 · raw")
            assert "needle row-10" in app.text(), "context anchor follows selection unexpectedly"
            app.resize(70, 12); app.wait_for("↑/↓ scroll")
            app.send(b"\x1b"); app.wait_until(lambda t: "Raw context" not in t, "context closed")
            app.wait_for("needle live-row-50")
            assert "hidden row-09" not in app.text()
            app.send(b"/"); app.wait_for("Applied   needle")
            app.send(b"\x1b"); app.wait_until(lambda t: " Search " not in t, "search closed again")
            stop(app)
        finally:
            if app.process.poll() is None:
                app.send(b"\x03")
                try: app.wait_exit(5)
                except Exception: app.process.kill(); app.process.wait()
                app.close()
    print("Raw context PTY passed: hidden neighbors, paging, fixed live anchor, resize, filter preservation, restoration")


if __name__ == "__main__": run(pathlib.Path(sys.argv[1]).resolve())
