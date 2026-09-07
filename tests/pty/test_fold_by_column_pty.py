#!/usr/bin/env python3
"""Folding by a column, in the actual application.

The fold key is the value of exactly one column. By default that column is a
derived one — the row text with volatile substrings replaced — which is what
folding always did. This story builds an enrichment column, chooses it in the
Folding dialog, and shows the same rows collapsing on a completely different
axis, at 80x24 and again at 54x16.
"""

import os
import pathlib
import sys
import tempfile

from test_lvu_pty import PtyApp
from test_enrichment_chain_pty import paste, stop

SERVICE_PATTERN = r"/svc=(?P<service>\w+)/"
FLOOD = 12
WORDS = [
    "opened the queue connection",
    "loaded the routing table",
    "warmed the segment cache",
    "registered with the coordinator",
    "acknowledged the leader lease",
    "published a readiness probe",
]


def source_text() -> str:
    """Twelve rows no two of which share a shape, then a flood that does.

    The first twelve differ in every word, so the derived pattern column has
    nothing to collapse; they share a service, so a service column does. The
    flood is the opposite, which is what makes the two keys distinguishable on
    one screen.
    """
    lines = [f"svc=shipper {word}" for word in WORDS]
    lines += [f"svc=indexer {word}" for word in WORDS]
    lines += [
        f"retry connect to 10.0.0.{attempt % 7} failed after {120 + attempt}ms"
        for attempt in range(FLOOD)
    ]
    return "\n".join(lines) + "\n"


def open_palette(app, query, expected):
    app.send(b"\x10")
    app.wait_for("Command palette")
    app.send(query.encode())
    app.wait_for(expected)
    app.send(b"\r")
    app.wait_until(lambda text: "Command palette" not in text, "palette dismissed")


def add_service_column(app):
    """One ordinary enrichment step, added the ordinary way."""
    app.send(b"e")
    app.wait_for("Steps")
    app.send(b"\x1ba")
    app.wait_for("Enrichment › New step")
    paste(app, SERVICE_PATTERN)
    app.send(b"\r")
    # The canonical view's definition is fixed, so the accepted step arrives on
    # a derived view; either way the step list shows it applied.
    app.wait_until(lambda text: "1  /svc=" in text and "Applied" in text,
                   "the service column is applied", timeout=20)
    app.send(b"\x1b")
    app.wait_until(lambda text: "Steps" not in text, "enrichment closed")


def open_folding(app):
    app.send(b"z")
    return app.wait_for("Folding")


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
    with tempfile.TemporaryDirectory(prefix="lvu-fold-column-pty-") as directory:
        root = pathlib.Path(directory)
        source = root / "events.log"
        original = source_text()
        source.write_text(original)
        environment = {**tooling, "XDG_CONFIG_HOME": str(root / "config"),
                       "XDG_DATA_HOME": str(root / "data"), "XDG_CACHE_HOME": str(root / "cache")}
        arguments = [str(source), "--capture-dir", str(root / "capture")]

        app = PtyApp(binary, arguments, width=80, height=24, environment=environment)
        try:
            app.wait_for("retry con")
            add_service_column(app)

            # The default key is unchanged: the flood of same-shaped retries
            # collapses to one line and the twelve service rows, which share no
            # shape, do not. Counting rows rather than reading the `[xN
            # repeated]` suffix keeps the assertion honest at 80 columns, where
            # the suffix is off the right edge.
            open_palette(app, "fold repeated", "Fold repeated events")
            folded = app.wait_until(
                lambda text: text.count("retry con") == 1,
                "the default key still folds the flood", timeout=20)
            assert folded.count("svc=ship") == len(WORDS), folded
            assert folded.count("svc=inde") == len(WORDS), folded

            # The dialog names the default column and offers the one setting
            # that applies only to it.
            dialog = open_folding(app)
            assert "Message pattern" in dialog, dialog
            assert "Normalisation" in dialog, dialog
            assert "Minimum run" in dialog, dialog
            assert "Scope" in dialog, dialog

            # Choose the enrichment column: one row down from the derived one.
            app.send(b"\r")
            picker = app.wait_for("(default)")
            assert "service" in picker, picker
            assert "New column" in picker, picker
            app.send(b"\x1b[B")
            app.send(b"\r")
            by_service = app.wait_until(
                lambda text: "Normalisation" not in text and "service" in text,
                "the service column becomes the fold key", timeout=20)
            # Normalisation is gone: a column key is used as it stands, so the
            # setting that rewrites the derived column cannot apply.
            assert "the key as it stands" in by_service, by_service
            assert "folding on service" in by_service, by_service
            app.send(b"\x1b")
            app.wait_until(lambda text: "Key column" not in text, "the dialog closed")

            # The same rows now fold on a completely different axis.
            collapsed = app.wait_until(
                lambda text: text.count("svc=ship") == 1
                and text.count("svc=inde") == 1,
                "each service collapses to one counted line", timeout=20)
            assert collapsed.count("retry con") == 1, collapsed

            # Presentation only: Enter restores the constituent rows in order.
            app.send(b"g")
            app.send(b"\r")
            expanded = app.wait_until(
                lambda text: text.count("svc=ship") == len(WORDS),
                "the run expands into its own rows", timeout=15)
            assert expanded.count("svc=inde") == 1, expanded
            app.send(b"\r")
            app.wait_until(lambda text: text.count("svc=ship") == 1,
                           "and collapses again", timeout=15)
            stop(app)
        finally:
            (root / "terminal-80x24.ansi").write_bytes(app.transcript)
            (root / "screen-80x24.txt").write_text(app.text())
            if app.process.poll() is None:
                app.process.kill()
                app.process.wait(timeout=5)
                app.close()

        # The key column is view state, so it comes back with the view — and the
        # dialog is operable in the smallest supported terminal.
        small = PtyApp(binary, arguments, width=54, height=16, environment=environment)
        try:
            restored = small.wait_until(
                lambda text: text.count("svc=ship") == 1
                and text.count("svc=inde") == 1,
                "the column key survives a restart", timeout=30)
            assert restored.count("retry con") == 1, restored
            narrow = open_folding(small)
            assert "Key column" in narrow, narrow
            assert "service" in narrow, narrow
            assert "Normalisation" not in narrow, narrow
            for line in narrow.splitlines():
                assert len(line) <= 54, (len(line), line)

            # Back to the default column, from the narrow terminal.
            # `(default)` marks the open list; the field alone would read
            # `Message pattern` once the row is chosen, which is not the same
            # thing to wait for.
            small.send(b"\r")
            small.wait_for("(default)")
            small.send(b"\x1b[A")
            small.send(b"\r")
            back = small.wait_until(lambda text: "Normalisation" in text,
                                    "the derived column is selected again", timeout=15)
            assert "Message pattern" in back, back
            small.send(b"\x1b")
            small.wait_until(lambda text: "Key column" not in text, "the dialog closed")
            default = small.wait_until(
                lambda text: text.count("svc=ship") == len(WORDS),
                "and the default fold returns: every service row is its own",
                timeout=20)
            # Eleven log rows fit here, so the six shipper rows and the first
            # of the indexer ones are what is on screen.
            assert default.count("svc=inde") >= 4, default
            # Eleven visible rows at this size, so the flood's counted line is
            # below them; the end of the stream is where it stands.
            small.send(b"G")
            tail = small.wait_until(lambda text: "retry con" in text,
                                    "the flood is still one line", timeout=15)
            assert tail.count("retry con") == 1, tail
            stop(small)
        finally:
            (root / "terminal-54x16.ansi").write_bytes(small.transcript)
            (root / "screen-54x16.txt").write_text(small.text())
            if small.process.poll() is None:
                small.process.kill()
                small.process.wait(timeout=5)
                small.close()

        # Folding is display-only: the captured source is byte-for-byte intact.
        assert source.read_text() == original


if __name__ == "__main__":
    run(pathlib.Path(sys.argv[1]).resolve())
    print("Fold-by-column PTY passed: the default derived column folds as before, an "
          "enrichment column becomes the key, normalisation applies only to the "
          "derived column, and the choice survives a restart at 54x16")
