#!/usr/bin/env python3
"""Iterative named regex and Polars extraction through the actual application."""

import os
import pathlib
import sys
import tempfile

from test_lvu_pty import PtyApp


PATTERN = r"/request\[(?P<request>r-\d+)\] code<(?P<status>\d+)>/"
UPPER = "upper_request = pl.col('request').str.to_uppercase()"


def paste(app, text):
    app.send(b"\x1b[200~" + text.encode() + b"\x1b[201~")


def close_editor(app):
    app.send(b"\x1b")
    app.wait_until(lambda text: "Native enrichment" not in text and "Advanced filter" not in text,
                   "editor closed")


def stop(app):
    if app.process.poll() is None:
        app.send(b"q")
        assert app.wait_exit(timeout=8) == 0
    app.assert_restored()
    app.close()


def run(binary):
    home = pathlib.Path.home()
    tooling = {}
    for key, xdg, fallback, suffix in [
        ("MISE_DATA_DIR", "XDG_DATA_HOME", home / ".local/share", "mise"),
        ("MISE_CONFIG_DIR", "XDG_CONFIG_HOME", home / ".config", "mise"),
        ("MISE_CACHE_DIR", "XDG_CACHE_HOME", home / ".cache", "mise"),
        ("UV_CACHE_DIR", "XDG_CACHE_HOME", home / ".cache", "uv"),
    ]:
        tooling[key] = os.environ.get(key, str(pathlib.Path(os.environ.get(xdg, fallback)) / suffix))
    with tempfile.TemporaryDirectory(prefix="lvu-chain-pty-") as directory:
        root = pathlib.Path(directory)
        source = root / "events.log"
        original = "unmatched raw\nrequest[r-7] code<503>\nrequest[r-8] code<200>\n"
        source.write_text(original)
        environment = {**tooling, "XDG_CONFIG_HOME": str(root / "config"),
                       "XDG_DATA_HOME": str(root / "data"), "XDG_CACHE_HOME": str(root / "cache")}
        arguments = [str(source), "--capture-dir", str(root / "capture")]
        app = PtyApp(binary, arguments, width=150, height=38, environment=environment)
        try:
            app.wait_for("request[r-8] code<200>")
            app.send(b"e")
            app.wait_for("Native enrichment")
            paste(app, PATTERN)
            app.send(b"\r")
            app.wait_until(lambda text: "1. /request" in text and "enrich:on" in text,
                           "named capture step accepted", timeout=15)
            close_editor(app)
            app.send(b"d")
            app.wait_for("request: r-8")
            app.wait_for("status: 200")

            app.send(b"e")
            app.wait_for("Native enrichment")
            app.send(b"\x1ba")
            paste(app, UPPER)
            app.send(b"\r")
            app.wait_for("2. upper_request", timeout=15)
            close_editor(app)
            app.wait_for("upper_request: R-8")
            app.wait_for("request: r-8")

            app.send(b"p")
            app.wait_for("Advanced filter")
            paste(app, "pl.col('status') == '503'")
            app.send(b"\r")
            app.wait_for("advanced:on", timeout=15)
            close_editor(app)
            app.wait_for("upper_request: R-7")
            assert "request[r-8] code<200>" not in app.text()

            app.send(b"e")
            app.wait_for("Native enrichment")
            app.send(b"\x1ba")
            paste(app, "broken = pl.col(")
            app.send(b"\r")
            app.wait_for("compiler rejected expression", timeout=15)
            assert "1. /request" in app.text() and "2. upper_request" in app.text()
            close_editor(app)
            with source.open("a") as stream:
                stream.write("request[r-9] code<503>\n")
            app.wait_for("upper_request: R-9", timeout=15)
            app.wait_for("request: r-9")
            stop(app)
        finally:
            if app.process.poll() is None:
                app.process.kill()
                app.process.wait(timeout=5)
                app.close()

        reopened = PtyApp(binary, arguments, width=150, height=38, environment=environment)
        try:
            reopened.wait_until(lambda text: "enrich:on" in text and "advanced:on" in text
                                and "request[r-9] code<503>" in text,
                                "restored complete chain and filter", timeout=20)
            reopened.send(b"e")
            reopened.wait_for("broken = pl.col(")
            assert "1. /request" in reopened.text() and "2. upper_request" in reopened.text()
            close_editor(reopened)
            reopened.send(b"d")
            reopened.wait_for("upper_request: R-9")
            stop(reopened)
        finally:
            if reopened.process.poll() is None:
                reopened.process.kill()
                reopened.process.wait(timeout=5)
                reopened.close()
        assert source.read_text() == original + "request[r-9] code<503>\n"


if __name__ == "__main__":
    run(pathlib.Path(sys.argv[1]).resolve())
    print("Enrichment chain PTY passed: named captures, dependent addition, invalid-add rollback, live arrivals, restart and exact source preservation")
