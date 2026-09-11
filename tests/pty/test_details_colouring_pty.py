#!/usr/bin/env python3
"""The docked Details pane colours a record exactly as the log pane does.

Details showed every value in one flat role on the dialog surface: it was
written before the identity palette and the JSON lexer and never caught up. It
now resolves colour through the same two functions the log uses, so the same
record reads the same in both. This drives the real terminal and compares the
cells.
"""

import os
import pathlib
import sys
import tempfile
import time

from test_lvu_pty import PtyApp
from test_enrichment_chain_pty import close_details, stop

RECORD = '{"svc":"indexer","req":"r-42","ms":1180,"bad":null}'


def source_text() -> str:
    return (
        '{"svc":"shipper","req":"r-41","ms":37,"ok":true}\n'
        f"{RECORD}\n"
        '{"svc":"shipper","req":"r-43","ms":29,"ok":true}\n'
    )


def settings_toml() -> str:
    return (
        "schema_version = 1\n"
        '\n[paseo]\nprovider = "fixture/provider"\nmode = "full-access"\nthinking = "medium"\n'
        '\n[appearance]\ntheme = "love-dark"\n'
        "delight_enabled = false\nreduced_motion = true\nascii = false\n"
        "\n[cache.memory]\nrows_mib = 4\nmembership_mib = 256\n"
        "\n[cache.disk]\ntotal_mib = 5120\nindex_per_source_mib = 256\n"
    )


def row_with(app, needle):
    for y, line in enumerate(app.text().splitlines()):
        if needle in line:
            return y
    raise AssertionError(f"no row containing {needle!r} in:\n{app.text()}")


def coloured(app, y, needle):
    """`(character, foreground)` for each cell of `needle` on row `y`."""
    line = app.text().splitlines()[y]
    start = line.index(needle)
    cells = app.screen.buffer[y]
    return [(cells[x].data, cells[x].fg) for x in range(start, start + len(needle))]


def foreground(app, y, needle):
    line = app.text().splitlines()[y]
    return app.screen.buffer[y][line.index(needle)].fg


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
    with tempfile.TemporaryDirectory(prefix="lvu-details-colour-pty-") as directory:
        root = pathlib.Path(directory)
        source = root / "events.log"
        source.write_text(source_text())
        config = root / "config" / "lvu"
        config.mkdir(parents=True)
        (config / "settings.toml").write_text(settings_toml())
        environment = {
            **tooling,
            "XDG_CONFIG_HOME": str(root / "config"),
            "XDG_DATA_HOME": str(root / "data"),
            "XDG_CACHE_HOME": str(root / "cache"),
            "COLORTERM": "truecolor",
            "LVU_NO_DELIGHT": "1",
        }
        arguments = [str(source), "--capture-dir", str(root / "capture")]
        app = PtyApp(binary, arguments, width=80, height=24, environment=environment)
        try:
            app.wait_until(
                lambda text: "r-43" in text and "pending" not in text,
                "settled rows before selecting Details",
                timeout=10.0,
            )
            time.sleep(0.5)
            app.wait_until(
                lambda text: "r-43" in text and "pending" not in text,
                "stable settled rows before selecting Details",
                timeout=10.0,
            )
            # The cursor sits on the first record, so the second one renders in
            # the log in its own colours rather than in the selection highlight.
            app.send(b"g")
            app.settle()
            visible = RECORD[: RECORD.index('"req"')]
            in_log = coloured(app, row_with(app, "r-42"), visible)
            assert len({fg for _, fg in in_log}) > 1, in_log

            app.send(b"j")
            app.send(b"d")
            app.wait_for("Selected event details")
            app.settle()
            details_row = row_with(app, "raw: ")
            in_details = coloured(app, details_row, visible)
            assert in_details == in_log, f"{in_details}\n!=\n{in_log}\n{app.text()}"

            # A column's name carries the identity colour its key carries inside
            # the raw line, so the pane and the line agree about what a column is.
            # `svc` is the tree cursor and carries the selection style instead;
            # these two are ordinary rows, and both fit the pane at 80x24.
            for column in ("req", "ms"):
                label = foreground(app, row_with(app, f"{column}: "), f"{column}:")
                key = foreground(app, details_row, f'"{column}"')
                assert label == key, (column, label, key, app.text())

            # The pane shares the workspace surface the identity colours were
            # measured against, not the dialog surface.
            log_bg = app.screen.buffer[row_with(app, "r-41")][30].bg
            details_bg = app.screen.buffer[details_row][30].bg
            assert details_bg == log_bg, (details_bg, log_bg, app.text())
            # Details owns its own keys; leave it before asking the app to exit.
            close_details(app)
            stop(app)
        finally:
            (root / "terminal.ansi").write_bytes(app.transcript)
            (root / "screen.txt").write_text(app.text())
            if app.process.poll() is None:
                app.process.kill()
                app.process.wait(timeout=5)
                app.close()


if __name__ == "__main__":
    run(pathlib.Path(sys.argv[1]).resolve())
    print("Details colouring PTY passed: the same record carries the same cells in the "
          "log and in Details, column names match their JSON keys, and both panes share "
          "one surface")
