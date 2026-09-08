#!/usr/bin/env python3
"""The indicator art keeps its shape under a dialog's scrim, in a real terminal.

The TestBackend test beside this one (`crates/lvu/tests/scrim_shapes.rs`) holds
the buffer; this holds what actually reaches the emulator, which is where the
bug was seen. A half-block cell draws two pixels — the foreground is the upper
one and the background the lower — so the reported "grey bar over a red bar"
was every foreground repainted `muted` while the backgrounds kept their red.

Read by column and by cell colour, never from the text: the shape is entirely
in the SGR pairs, and the glyphs are identical before and after.
"""
import pathlib
import sys
import tempfile

from test_lvu_pty import PtyApp

# The sidebar's corner art: five columns, three rows above the bottom border.
ART_COLUMNS = range(1, 6)
ART_ROWS = range(19, 22)
BLOCKS = {"▀", "▄", "█", "▌", "▐", "░", "▒", "▓"}


def settings_at(root: pathlib.Path) -> dict[str, str]:
    directory = root / "config" / "lvu"
    directory.mkdir(parents=True)
    (directory / "settings.toml").write_text(
        "schema_version = 1\n"
        '\n[paseo]\nprovider = "fixture/provider"\nmode = "full-access"\n'
        'thinking = "medium"\n'
        '\n[appearance]\ntheme = "love-dark"\n'
        "delight_enabled = true\nreduced_motion = true\nascii = false\n"
        "\n[cache.memory]\nrows_mib = 4\nmembership_mib = 256\n"
        "\n[cache.disk]\ntotal_mib = 5120\nindex_per_source_mib = 256\n"
        "\n[storage]\nreserve_mib = 256\n"
        "\n[storage.retention]\n"
        "enabled = false\nmaximum_total_capture_mib = 0\nmaximum_age_days = 0\n"
    )
    return {
        "XDG_CONFIG_HOME": str(root / "config"),
        "XDG_DATA_HOME": str(root / "data"),
        "XDG_CACHE_HOME": str(root / "cache"),
        "COLORTERM": "truecolor",
        "NO_COLOR": "",
    }


def art_cells(app: PtyApp) -> list[tuple[str, str, str]]:
    """`(symbol, fg, bg)` for every cell of the corner art."""
    return [
        (
            app.screen.buffer[row][column].data,
            app.screen.buffer[row][column].fg,
            app.screen.buffer[row][column].bg,
        )
        for row in ART_ROWS
        for column in ART_COLUMNS
    ]


def story(binary: pathlib.Path) -> None:
    with tempfile.TemporaryDirectory(prefix="lvu-scrim-shapes-") as directory:
        root = pathlib.Path(directory)
        source = root / "events.log"
        # An empty source: the log pane has nothing in it, which is the state
        # the bug was reported from, and the corner art is unaffected by rows.
        source.write_text("")
        app = PtyApp(
            binary,
            [str(source), "--capture-dir", str(root / "capture")],
            width=80,
            height=24,
            environment=settings_at(root),
        )
        try:
            app.wait_for("Sources / views")
            app.wait_until(
                lambda _: any(
                    symbol in BLOCKS for symbol, _, _ in art_cells(app)
                ),
                "the corner art is drawn",
            )
            before = art_cells(app)

            app.send(b"/")
            app.wait_for("Search")
            after = art_cells(app)

            assert [cell[0] for cell in before] == [cell[0] for cell in after], (
                f"the scrim changed a glyph\n{before}\n{after}"
            )
            muted = {
                cell.fg
                for cell in (app.screen.buffer[22][x] for x in range(1, 8))
            }
            for index, ((symbol, fg, bg), (_, dim_fg, dim_bg)) in enumerate(
                zip(before, after)
            ):
                if symbol not in BLOCKS:
                    continue
                # The failure exactly: every foreground became the one muted
                # colour the border and the scrimmed text wear.
                assert dim_fg not in muted, (
                    f"cell {index} was repainted as text ({dim_fg}), so the "
                    f"half-block lost its shape\n{after}"
                )
                # The two pixels still differ wherever they differed, which is
                # what makes the cell a shape rather than a bar.
                if fg != bg:
                    assert dim_fg != dim_bg, (
                        f"cell {index} flattened to one colour\n{after}"
                    )
                # Dimmed, not recoloured: a red pixel stays red.
                if fg != "default" and dim_fg != "default":
                    assert dominant(fg) == dominant(dim_fg), (
                        f"cell {index} changed hue: {fg} -> {dim_fg}"
                    )

            app.send(b"\x1b")
            app.wait_until(lambda text: "Search" not in text, "the dialog closed")
            assert art_cells(app) == before, "the art did not come back unchanged"

            app.send(b"q")
            assert app.wait_exit(timeout=10) == 0
            app.assert_restored()
        finally:
            if app.process.poll() is None:
                app.process.kill()
                app.process.wait(timeout=5)
            app.close()


def dominant(colour: str) -> str:
    """Which channel of a `rrggbb` string is the largest, as a coarse hue."""
    if colour == "default" or len(colour) != 6:
        return colour
    red, green, blue = (int(colour[i : i + 2], 16) for i in (0, 2, 4))
    largest = max(red, green, blue)
    return "".join(
        name
        for name, value in (("r", red), ("g", green), ("b", blue))
        if value == largest
    )


def run(binary: pathlib.Path) -> None:
    story(binary)


if __name__ == "__main__":
    run(pathlib.Path(sys.argv[1]).resolve())
    print("Scrim shapes PTY passed: the corner art dims without flattening")
