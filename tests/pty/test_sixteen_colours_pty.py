#!/usr/bin/env python3
"""lvu on a sixteen-colour terminal.

`TERM=xterm` has six usable hues and nothing else. lvu used to emit 256-colour
cube indexes there regardless, so the emulator approximated them: two identities
could land on one colour, and a colour lvu had measured as readable could be
displayed as something else. It now names the colours it means — and because the
log pane and the Details pane resolve colour through the same two functions,
both change together.

The chrome half matters for the same reason and one more: lvu paints both halves
of the selection and of the default button's accent fill, so those two are the
places an approximation could put a filled region on top of the background it
was supposed to stand out from, and nothing in lvu would know.
"""

import os
import pathlib
import sys
import tempfile

from test_lvu_pty import PtyApp
from test_enrichment_chain_pty import close_details, stop

RECORD = '{"svc":"indexer","req":"r-42","ms":1180,"bad":null}'

# pyte resolves a named ANSI colour to the hex its palette displays, which is
# xterm's default palette — the same values the theme measures its contrast
# floor against. Six usable hues, in normal and bold weight.
ANSI_HUES = {
    "cd0000", "00cd00", "cdcd00", "0000ee", "cd00cd", "00cdcd",  # normal
    "ff0000", "00ff00", "ffff00", "5c5cff", "ff00ff", "00ffff",  # bold
}
# Structure, not data.
ANSI_PUNCTUATION = {"ffffff", "e5e5e5"}
# The two regions lvu paints both halves of, as xterm displays them.
SELECTION_FG, SELECTION_BG = "ffffff", "0000ee"
ACCENT_FG, ACCENT_BG = "000000", "00cdcd"


def source_text() -> str:
    return (
        '{"svc":"shipper","req":"r-41","ms":37,"ok":true}\n'
        f"{RECORD}\n"
        '{"svc":"router","req":"r-43","ms":29,"ok":true}\n'
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


def row_with(app, needle, avoid=None):
    for y, line in enumerate(app.text().splitlines()):
        if needle in line and (avoid is None or avoid not in line):
            return y
    raise AssertionError(f"no row containing {needle!r} in:\n{app.text()}")


def cells(app, y, needle):
    line = app.text().splitlines()[y]
    start = line.index(needle)
    return [app.screen.buffer[y][x] for x in range(start, start + len(needle))]


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
    with tempfile.TemporaryDirectory(prefix="lvu-sixteen-pty-") as directory:
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
            # The whole point: a sixteen-colour terminal, and no truecolor claim.
            "TERM": "xterm",
            "COLORTERM": "",
            "LVU_NO_DELIGHT": "1",
        }
        arguments = [str(source), "--capture-dir", str(root / "capture")]
        app = PtyApp(binary, arguments, width=80, height=24, environment=environment)
        try:
            app.wait_for("r-43")
            # The cursor is on the last record (follow), so the first two render
            # in their own colours rather than in the selection highlight.
            app.settle()

            # The selected row: lvu paints both halves, so it is visible
            # whatever the terminal's own background is. Approximated RGB is
            # what used to make this land on the background it stands out from.
            app.send(b"G")
            app.settle()
            selected = [
                cell
                for cell in cells(app, row_with(app, "r-43"), '{"svc":"router"')
                if cell.data.strip()
            ]
            assert selected, app.text()
            assert {cell.bg for cell in selected} == {SELECTION_BG}, (
                sorted({cell.bg for cell in selected}),
                app.text(),
            )
            assert {cell.fg for cell in selected} == {SELECTION_FG}, (
                sorted({cell.fg for cell in selected}),
                app.text(),
            )

            # Every JSON token in the log line is a colour lvu named, which is
            # what the terminal can actually show. Before this it emitted cube
            # indexes here and the emulator picked something else.
            visible = '{"svc":"shipper","req":"r-41","ms":'
            log_row = row_with(app, "r-41")
            painted = [cell for cell in cells(app, log_row, visible) if cell.data.strip()]
            assert painted, app.text()
            unexpected = [
                (cell.data, cell.fg)
                for cell in painted
                if cell.fg not in ANSI_HUES
                and cell.fg not in ANSI_PUNCTUATION
                and cell.fg != "default"
            ]
            assert not unexpected, (unexpected, app.text())

            # The identities really are told apart: the three key colours in
            # this record differ, which is exactly what an approximation loses.
            keys = [cells(app, log_row, f'"{name}"')[0].fg for name in ("svc", "req", "ms")]
            assert all(key in ANSI_HUES for key in keys), (keys, app.text())
            assert len(set(keys)) == 3, ("identities collapsed onto one colour", keys, app.text())

            # A level is named rather than approximated. This fixture has none,
            # so the check that matters here is that nothing in the row is an
            # RGB triple lvu never chose — asserted above.

            # Details resolves colour through the same two functions, so the
            # same record carries the same cells there.
            in_log = [(c.data, c.fg, c.bold) for c in cells(app, log_row, visible)]
            app.send(b"g")
            app.send(b"d")
            app.wait_for("Selected event details")
            app.settle()
            details_row = row_with(app, "raw: ")
            in_details = [(c.data, c.fg, c.bold) for c in cells(app, details_row, visible)]
            assert in_details == in_log, (in_details, in_log, app.text())

            close_details(app)

            # --- chrome ---------------------------------------------------
            # §8.9's default button carries the accent fill, and it is not the
            # selection: a filled button must not read as "the cursor is here".
            app.send(b"t")
            app.wait_for("[ Apply ]")
            app.settle()
            apply_row = row_with(app, "[ Apply ]")
            fill = [
                cell
                for cell in cells(app, apply_row, "[ Apply ]")
                if cell.data.strip()
            ]
            assert {cell.bg for cell in fill} == {ACCENT_BG}, (
                sorted({cell.bg for cell in fill}),
                app.text(),
            )
            assert ACCENT_BG != SELECTION_BG
            assert {cell.fg for cell in fill} == {ACCENT_FG}, (
                sorted({cell.fg for cell in fill}),
                app.text(),
            )
            # §8.10: the mnemonic is an attribute, not a colour, so it survives
            # a palette that has none to spare — and it has to, because it is
            # the only thing marking which letter reaches a control.
            underlined = [
                app.screen.buffer[y][x].data
                for y in range(app.screen.lines)
                for x in range(app.screen.columns)
                if app.screen.buffer[y][x].underscore
            ]
            assert underlined, app.text()
            app.send(b"\x1b")
            app.wait_until(lambda text: "[ Apply ]" not in text, "time closes")

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
    print("Sixteen-colour PTY passed: named ANSI hues, identities still told apart, "
          "Details matching the log, a visible selection and default-button fill, "
          "and the mnemonic underline surviving")
