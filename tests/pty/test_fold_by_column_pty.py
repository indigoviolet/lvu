#!/usr/bin/env python3
"""Run grouping on an enrichment column, in the actual application.

A run is a group of consecutive rows sharing one exact enrichment value.
This story builds an enrichment column, selects it in the Grouping dialog's
Run tab, and shows the service rows collapsing on that axis while the
keyless flood stays listed individually — at 80x24 and again at 54x16.
Recognition lives in Enrichment; Grouping only names its output column, so
no pattern normalisation takes part.
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
    """Twelve rows sharing a service key, then a keyless flood.

    The first twelve share `service`, so Run grouping collapses them into two
    events; the flood carries no key, so every retry stays listed on its own.
    """
    lines = [f"svc=shipper {word}" for word in WORDS]
    lines += [f"svc=indexer {word}" for word in WORDS]
    lines += [
        f"retry connect to 10.0.0.{attempt % 7} failed after {120 + attempt}ms"
        for attempt in range(FLOOD)
    ]
    return "\n".join(lines) + "\n"


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


def open_grouping(app):
    app.send(b"m")
    return app.wait_for("Multiline grouping")


def choose_run_column(app, column):
    """Grouping opens on the Run tab with a blank column and the caret in
    its slot: pasting names the column in place, keeping a valid rule."""
    paste(app, column)
    app.wait_for(f"(?lvu:run:v1:column:{column})")


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

            # The Run tab opens with a blank column: name the enrichment
            # output and every run of equal values becomes one group, while
            # the keyless flood rows stay listed individually — nulls never
            # join runs, not even each other.
            dialog = open_grouping(app)
            assert "Run" in dialog, dialog
            assert "Legacy" in dialog, dialog
            assert "empty draft turns grouping" in dialog, dialog
            choose_run_column(app, "service")
            app.send(b"\r")
            app.wait_until(lambda text: "Applied" in text,
                           "run grouping applied", timeout=15)
            app.send(b"\x1b")
            app.wait_until(lambda text: "Multiline grouping" not in text,
                           "the dialog closed", timeout=10)

            # Each service collapses to one counted line; the flood has no
            # key and stays individual.
            collapsed = app.wait_until(
                lambda text: text.count("svc=ship") == 1
                and text.count("svc=inde") == 1,
                "each service collapses to one counted line", timeout=20)
            assert collapsed.count("retry con") == FLOOD, collapsed

            # Presentation only: expanding swaps the shipper head for its six
            # member lines, leaving the indexer run collapsed; collapsing
            # restores the head. (Count suffixes sit off the right edge at 80
            # columns, so head presence is the honest signal.)
            app.send(b"g")
            app.send(b"\r")
            expanded = app.wait_until(
                lambda text: text.count("svc=ship") == len(WORDS)
                and text.count("svc=inde") == 1,
                "the run expands into its own rows", timeout=15)
            assert "retry con" in expanded, expanded
            app.send(b"\r")
            app.wait_until(lambda text: text.count("svc=ship") == 1
                           and text.count("svc=inde") == 1,
                           "and collapses again", timeout=15)
            stop(app)
        finally:
            (root / "terminal-80x24.ansi").write_bytes(app.transcript)
            (root / "screen-80x24.txt").write_text(app.text())
            if app.process.poll() is None:
                app.process.kill()
                app.process.wait(timeout=5)
                app.close()

        # The run column is view state, so it comes back with the view — and
        # the dialog is operable in the smallest supported terminal.
        small = PtyApp(binary, arguments, width=54, height=16, environment=environment)
        try:
            # Eleven rows fit, so only the flood's first singles are
            # visible: persistence is the two collapsed heads either way.
            restored = small.wait_until(
                lambda text: text.count("svc=ship") == 1
                and text.count("svc=inde") == 1,
                "the run column survives a restart", timeout=30)
            assert restored.count("retry con") >= 1, restored
            narrow = open_grouping(small)
            assert "Run" in narrow, narrow
            assert "service" in narrow, narrow
            for line in narrow.splitlines():
                assert len(line) <= 54, (len(line), line)

            # Off clears the draft, from the narrow terminal: click the Off
            # segment, which also hands the keys to the field. Applying the
            # empty rule (dialog request plus worker ungroup) is pinned at the
            # Rust layer instead of through an unobservable focus walk here.
            tab_row = next(
                line for line in narrow.splitlines()
                if "Run" in line and "Legacy" in line and "Off" in line
            )
            row = narrow.splitlines().index(tab_row) + 1
            col = tab_row.index("Off") + 1
            small.send(f"\x1b[<0;{col};{row}M\x1b[<0;{col};{row}m".encode())
            small.wait_until(lambda text: "(?lvu:" not in text,
                             "off selected", timeout=10)
            small.send(b"\x1b")
            small.wait_until(lambda text: "Multiline grouping" not in text,
                             "the dialog closed", timeout=10)
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
    print("Run-grouping PTY passed: equal enrichment values collapse, keyless rows "
          "stay individual, expansion restores order, the choice survives a "
          "restart at 54x16, and Off clears the draft")
