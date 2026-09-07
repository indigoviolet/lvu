#!/usr/bin/env python3
"""Repeated-pattern folding in the actual application.

A retry flood buries two interesting events. Folding collapses the flood into
one counted line, expanding it restores exactly the original events in their
original order, the choice survives a restart, and neither filtering nor the
underlying records change because a fold exists.
"""

import os
import pathlib
import sys
import tempfile

from test_lvu_pty import PtyApp
from test_enrichment_chain_pty import close_details, paste, stop

FLOOD = 40


def source_text() -> str:
    lines = ["service started on port 8080", "loaded 12 rules"]
    lines += [
        f"retry connect to 10.0.0.{attempt % 7} failed after {120 + attempt}ms"
        for attempt in range(FLOOD)
    ]
    lines += ["connection established", "ready to serve"]
    return "\n".join(lines) + "\n"


def open_palette(app, query, expected):
    """Folding must be reachable without memorising a key."""
    app.send(b"\x10")
    app.wait_for("Command palette")
    app.send(query.encode())
    app.wait_for(expected)
    app.send(b"\r")
    app.wait_until(lambda text: "Command palette" not in text, "palette dismissed")


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
    with tempfile.TemporaryDirectory(prefix="lvu-fold-pty-") as directory:
        root = pathlib.Path(directory)
        source = root / "events.log"
        original = source_text()
        source.write_text(original)
        environment = {**tooling, "XDG_CONFIG_HOME": str(root / "config"),
                       "XDG_DATA_HOME": str(root / "data"), "XDG_CACHE_HOME": str(root / "cache")}
        arguments = [str(source), "--capture-dir", str(root / "capture")]
        app = PtyApp(binary, arguments, width=150, height=40, environment=environment)
        try:
            app.wait_for("ready to serve")
            # Folding is off by default: every retry is listed individually.
            unfolded = app.text()
            assert "fold:" not in unfolded, unfolded
            assert "repeated]" not in unfolded, unfolded
            assert unfolded.count("retry connect to") > 5, unfolded
            assert f"/{FLOOD + 4}" in unfolded, unfolded

            open_palette(app, "fold repeated", "Fold repeated events")
            folded = app.wait_until(lambda text: f"[x{FLOOD} repeated]" in text,
                                    "the flood collapses to one counted line", timeout=15)
            # The interesting events are visible again, and the indicator is honest
            # about how much is hidden.
            assert "service started on port 8080" in folded, folded
            assert "connection established" in folded, folded
            assert "ready to serve" in folded, folded
            app.wait_until(lambda text: "fold:1 runs" in text and f"{FLOOD - 1} hidden" in text,
                           "the status reports the fold", timeout=10)
            collapsed = app.text()
            assert "/5" in collapsed, collapsed
            assert collapsed.count("retry connect to") == 1, collapsed

            # The collapsed line stands for a real record: Details shows that
            # record exactly as captured, with no fold decoration on it.
            app.send(b"g")
            app.send(b"jj")
            app.send(b"d")
            details = app.wait_for("Selected event details")
            assert "retry connect to 10.0.0.0 failed after 120ms" in details, details
            assert "repeated]" not in details.split("Selected event details")[1], details
            close_details(app)

            # Enter expands the run back into the original events, in order.
            app.send(b"\r")
            expanded = app.wait_until(lambda text: "repeated]" not in text,
                                      "the run expands", timeout=10)
            assert f"/{FLOOD + 4}" in expanded, expanded
            assert expanded.count("retry connect to") > 5, expanded

            open_palette(app, "collapse expanded", "Collapse expanded runs")
            app.wait_until(lambda text: f"[x{FLOOD} repeated]" in text,
                           "collapsing again", timeout=10)

            # A filter matches exactly what it matched before: folding changes
            # presentation, never membership.
            app.send(b"/")
            app.wait_for("Search")
            paste(app, "retry connect")
            app.send(b"\r")
            app.wait_until(lambda text: f"search:" in text and "/1" in text,
                           "the matched flood is one folded line", timeout=15)
            app.send(b"\x1b")
            app.wait_until(lambda text: "Search" not in text, "search dismissed")
            open_palette(app, "fold repeated", "Fold repeated events")
            app.wait_until(lambda text: f"/{FLOOD}" in text and "repeated]" not in text,
                           "unfolded, the same filter still matches every retry", timeout=15)
            # Restore folding, clear the filter, and leave it on for the restart.
            open_palette(app, "fold repeated", "Fold repeated events")
            app.wait_until(lambda text: f"[x{FLOOD} repeated]" in text, "folded again", timeout=15)
            app.send(b"/")
            app.wait_for("Search")
            app.send(b"\x01\x0b\r")
            app.wait_until(lambda text: "search:" not in text, "filter cleared", timeout=15)
            app.send(b"\x1b")
            app.wait_until(lambda text: "Search" not in text, "search dismissed")
            app.wait_until(lambda text: f"[x{FLOOD} repeated]" in text, "still folded", timeout=15)
            stop(app)
        finally:
            (root / "terminal.ansi").write_bytes(app.transcript)
            (root / "screen.txt").write_text(app.text())
            if app.process.poll() is None:
                app.process.kill()
                app.process.wait(timeout=5)
                app.close()

        reopened = PtyApp(binary, arguments, width=150, height=40, environment=environment)
        try:
            # The fold configuration is working-view state and comes back with it.
            restored = reopened.wait_until(lambda text: f"[x{FLOOD} repeated]" in text,
                                           "folding survives restart", timeout=25)
            assert "fold:1 runs" in restored, restored
            assert restored.count("retry connect to") == 1, restored
            reopened.send(b"g")
            reopened.send(b"jj")
            reopened.send(b"\r")
            reopened.wait_until(lambda text: "repeated]" not in text,
                                "the restored fold still expands", timeout=10)
            stop(reopened)
        finally:
            if reopened.process.poll() is None:
                reopened.process.kill()
                reopened.process.wait(timeout=5)
                reopened.close()

        # Folding is display-only: the captured source is byte-for-byte intact.
        assert source.read_text() == original


if __name__ == "__main__":
    run(pathlib.Path(sys.argv[1]).resolve())
    print("Folding PTY passed: off by default, palette-discoverable collapse, honest "
          "indicator, exact expansion, unchanged filtering, and restart restoration")
