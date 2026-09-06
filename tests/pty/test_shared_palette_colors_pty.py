#!/usr/bin/env python3
"""Deep real-app acceptance for the shared command-palette presentation."""

from __future__ import annotations

import os
import pathlib
import re
import sys
import tempfile
import unicodedata

from test_lvu_pty import PtyApp


THEMES = {
    "love-dark": {
        "dialog": "261b24",
        "selection": "ffa797",
    },
    "love-light": {
        "dialog": "faedea",
        "selection": "a6363a",
    },
}


def luminance(rgb: str) -> float:
    channels = [int(rgb[index : index + 2], 16) / 255 for index in (0, 2, 4)]
    linear = [value / 12.92 if value <= 0.04045 else ((value + 0.055) / 1.055) ** 2.4
              for value in channels]
    return 0.2126 * linear[0] + 0.7152 * linear[1] + 0.0722 * linear[2]


def contrast(foreground: str, background: str) -> float:
    lighter, darker = sorted((luminance(foreground), luminance(background)), reverse=True)
    return (lighter + 0.05) / (darker + 0.05)


def long_investigation_reason_visible(screen: str) -> bool:
    """Require both meaningful clauses, allowing intentional line separation."""
    return (
        "open an active investigation" in screen
        and "enter a follow-up first" in screen
    )


def find_text(app: PtyApp, needle: str, occurrence: int = 0) -> tuple[int, int]:
    matches = []
    for row, line in enumerate(app.text().splitlines()):
        start = 0
        while (column := line.find(needle, start)) >= 0:
            matches.append((column, row))
            start = column + len(needle)
    if matches:
        return matches[occurrence]
    raise AssertionError(f"could not find {needle!r} on screen\n{app.text()}")


def cell_colors(app: PtyApp, needle: str, occurrence: int = 0) -> tuple[str, str]:
    column, row = find_text(app, needle, occurrence)
    cell = app.screen.buffer[row][column]
    return cell.fg, cell.bg


def literal_result_position(app: PtyApp) -> tuple[int, int]:
    """Locate the result row, excluding query and selected-detail copies."""
    for row, line in enumerate(app.text().splitlines()):
        if re.search(r"Literal filter\s+/\s+Filter", line):
            return line.index("Literal filter"), row
    raise AssertionError(f"could not find Literal filter result row\n{app.text()}")


def click(app: PtyApp, column: int, row: int) -> None:
    app.send(
        (f"\x1b[<0;{column + 1};{row + 1}M"
         f"\x1b[<0;{column + 1};{row + 1}m").encode()
    )


def settings(root: pathlib.Path, theme: str) -> None:
    config = root / "config" / "lvu"
    config.mkdir(parents=True)
    (config / "settings.toml").write_text(
        "schema_version = 1\n"
        f'[appearance]\ntheme = "{theme}"\n'
        "delight_enabled = false\nreduced_motion = true\nascii = false\n"
        '[paseo]\nprovider = "codex/gpt-5.6-luna"\nmode = "full-access"\n'
        'thinking = "medium"\n'
        "[cache.memory]\nrows_mib = 4\nmembership_mib = 256\n"
        "[cache.disk]\ntotal_mib = 5120\nindex_per_source_mib = 256\n"
    )


def environment(root: pathlib.Path) -> dict[str, str]:
    return {
        "XDG_CONFIG_HOME": str(root / "config"),
        "XDG_DATA_HOME": str(root / "data"),
        "XDG_CACHE_HOME": str(root / "cache"),
        "MISE_DATA_DIR": os.environ.get(
            "MISE_DATA_DIR", str(pathlib.Path.home() / ".local/share/mise")
        ),
        "MISE_CONFIG_DIR": os.environ.get(
            "MISE_CONFIG_DIR", str(pathlib.Path.home() / ".config/mise")
        ),
        "MISE_CACHE_DIR": os.environ.get(
            "MISE_CACHE_DIR", str(pathlib.Path.home() / ".cache/mise")
        ),
        "UV_CACHE_DIR": os.environ.get(
            "UV_CACHE_DIR", str(pathlib.Path.home() / ".cache/uv")
        ),
        # An empty NO_COLOR must not disable explicitly requested truecolor.
        "NO_COLOR": "",
        "COLORTERM": "truecolor",
        "LVU_NO_DELIGHT": "1",
    }


def assert_palette_contrast(app: PtyApp, theme: str) -> None:
    dialog = THEMES[theme]["dialog"]
    selection = THEMES[theme]["selection"]

    name_column, name_row = literal_result_position(app)
    name_cell = app.screen.buffer[name_row][name_column]
    name_fg, name_bg = name_cell.fg, name_cell.bg
    assert name_bg == selection, (theme, "selected row background", name_fg, name_bg)
    assert contrast(name_fg, name_bg) >= 4.5, (theme, "selected row", name_fg, name_bg)

    description_fg, description_bg = cell_colors(app, "Edit case-insensitive text search")
    assert description_bg == dialog, (theme, "description background", description_bg)
    assert contrast(description_fg, description_bg) >= 4.5, (
        theme, "description", description_fg, description_bg
    )

    input_fg, input_bg = cell_colors(app, "Literal filter", 0)
    assert input_bg != dialog, (theme, "input needs its own background")
    assert contrast(input_fg, input_bg) >= 4.5, (theme, "input", input_fg, input_bg)


def assert_aligned_shortcuts(screen: str) -> None:
    positions = []
    for line in screen.splitlines():
        match = re.search(
            r"(?:Advanced filter|Ask agent|Fields|Literal filter|Quit|Settings)\s+"
            r"(\?|/|p|i|q|,|A)\s+\w",
            line,
        )
        if match:
            positions.append(match.start(1))
    assert len(positions) >= 3, ("too few visible shortcut-bearing rows", positions, screen)
    assert len(set(positions)) == 1, ("shortcut column is not aligned", positions, screen)


def assert_mixed_recipe_columns(screen: str) -> None:
    lines = screen.splitlines()
    mixed = [
        next(line for line in lines if "Adapt suggested recipe" in line),
        next(line for line in lines if "Reject selected recipe" in line),
        next(line for line in lines if line.count("Recipes") >= 2),
    ]
    category_columns = [line.rfind("Recipes") for line in mixed]
    assert len(set(category_columns)) == 1, (
        "long and short command names shifted the category column",
        category_columns,
        mixed,
    )
    assert all(line[column - 1].isspace() for line, column in zip(mixed, category_columns))


def run_theme(binary: pathlib.Path, theme: str, evidence: pathlib.Path) -> None:
    temporary = tempfile.TemporaryDirectory(prefix=f"lvu-shared-palette-{theme}-")
    root = pathlib.Path(temporary.name)
    settings(root, theme)
    source = root / "events.log"
    source.write_bytes(b"palette-alpha\npalette-beta\n")
    app = PtyApp(
        binary,
        ["--file", str(source), "--capture-dir", str(root / "capture")],
        width=100,
        height=28,
        environment=environment(root),
    )
    try:
        app.wait_for("palette-beta", timeout=8)
        app.send(b"\x10")
        blank = app.wait_for("Command palette")
        assert "Confirm derived-data cleanup" not in blank
        assert "Unavailable:" not in blank
        assert_aligned_shortcuts(blank)

        # Walk the complete blank-query result set. Disabled commands must not
        # leak in merely because selection scrolling reveals later rows.
        for _ in range(100):
            app.send(b"\x1b[B")
            app.drain()
            assert "Confirm derived-data cleanup" not in app.text()

        # A searched result set deliberately mixes 33-column unavailable names
        # with the short actionable command. Names may clip, but they cannot
        # consume or shift the shared shortcut/category columns.
        app.send(b"\x01\x0brecipe")
        recipes = app.wait_until(
            lambda text: "Adapt suggested recipe" in text
            and "Reject selected recipe" in text
            and text.count("Recipes") >= 2,
            "mixed long and short recipe command rows",
        )
        assert_mixed_recipe_columns(recipes)

        # Query replacement also proves that the palette input owns keyboard
        # focus after list scrolling. Match refresh deliberately retains the
        # prior numeric selection, so explicitly select the intended row before
        # asserting selected-detail presentation.
        app.send(b"\x01\x0bLiteral filter")
        app.wait_until(
            lambda text: text.count("Literal filter") >= 2,
            "literal filter query and result row",
        )
        literal_column, literal_row = literal_result_position(app)
        click(app, literal_column + 2, literal_row)
        literal = app.wait_for("Edit case-insensitive text search")
        assert "Literal filter" in literal
        assert_palette_contrast(app, theme)

        # A row click selects but does not execute; Enter performs the action.
        app.assert_remains("Command palette", "No filter applied.")
        app.send(b"\r")
        app.wait_for("No filter applied.")
        app.send(b"\x1b")
        app.wait_until(lambda text: "┌ Search" not in text, "Search closes")

        app.send(b"\x10confirm derived-data cleanup")
        unavailable = app.wait_for("Confirm derived-data cleanup")
        assert "Unavailable:" in unavailable
        assert "confirm cleanup in Storage preview first" in unavailable
        unavailable_fg, unavailable_bg = cell_colors(app, "confirm cleanup in Storage preview first")
        assert unavailable_bg == THEMES[theme]["dialog"]
        assert contrast(unavailable_fg, unavailable_bg) >= 4.5, (
            theme, "unavailable reason", unavailable_fg, unavailable_bg
        )
        app.send(b"\r")
        app.assert_remains("Command palette", "Storage preview ─")

        # At the narrow acceptance width the longest unavailable reason needs
        # more than the old fixed three-row details pane. Every reason fragment
        # remains visible and Enter still cannot execute the disabled command.
        app.send(b"\x01\x0bSend investigation follow-up")
        app.wait_until(
            long_investigation_reason_visible,
            "complete wide unavailable reason",
        )
        app.resize(40, 12)
        narrow = app.wait_until(
            lambda text: "Command palette" in text
            and "Unavailable:" in text
            and long_investigation_reason_visible(text),
            "complete narrow unavailable reason",
        )
        assert sum("Unavailable:" in line for line in narrow.splitlines()) == 1
        assert not app.screen.cursor.hidden
        app.send(b"\r")
        app.assert_remains("Command palette", "Storage preview ─")

        # Narrow reflow retains focus and bounded list interaction. Wheel motion
        # must not leak through the modal or make a disabled action executable.
        app.resize(54, 12)
        app.wait_until(
            lambda text: "Command palette" in text
            and long_investigation_reason_visible(text),
            "completed resized palette frame",
        )
        app.send(b"\x1b[<65;27;6M")
        app.wait_until(
            lambda text: "Command palette" in text
            and long_investigation_reason_visible(text),
            "palette after narrow wheel down",
        )
        app.send(b"\x1b[<64;27;6M")
        app.wait_until(
            lambda text: "Command palette" in text
            and long_investigation_reason_visible(text),
            "palette after narrow wheel up",
        )
        app.assert_remains("Command palette", "Storage preview ─")

        # The real terminal must keep a long Unicode query and its caret inside
        # the narrow input window. Moving left across a combining sequence and
        # inserting must retain its base character; the visible tail must never
        # begin with an orphan combining mark.
        wide_query = "界" * 40 + "e\N{COMBINING ACUTE ACCENT}Z"
        app.send(b"\x01\x0b\x1b[200~" + wide_query.encode() + b"\x1b[201~")
        windowed = app.wait_until(
            lambda text: "e\N{COMBINING ACUTE ACCENT}Z"
            in unicodedata.normalize("NFD", text)
            and "No matching commands" in text,
            "windowed wide and combining query tail",
        )
        assert not app.screen.cursor.hidden
        input_row = app.screen.cursor.y
        input_text = windowed.splitlines()[input_row].lstrip("│> ")
        assert not input_text.startswith("\N{COMBINING ACUTE ACCENT}"), input_text
        app.send(b"\x1b[DX")
        app.wait_until(
            lambda text: "e\N{COMBINING ACUTE ACCENT}XZ"
            in unicodedata.normalize("NFD", text),
            "caret insertion within combining query tail",
        )
        app.resize(100, 28)
        app.wait_until(
            lambda text: "e\N{COMBINING ACUTE ACCENT}XZ"
            in unicodedata.normalize("NFD", text),
            "wide resized combining query tail",
        )
        assert not app.screen.cursor.hidden

        app.send(b"\x1b")
        app.wait_for("palette-beta")
        app.send(b"q")
        assert app.wait_exit(timeout=8) == 0
        app.assert_restored()
    finally:
        if app.process.poll() is None:
            app.process.kill()
            app.process.wait(timeout=5)
        app.drain()
        (evidence / f"{theme}.ansi").write_bytes(bytes(app.transcript))
        app.close()
        temporary.cleanup()


def run(binary: pathlib.Path) -> None:
    evidence = pathlib.Path(tempfile.mkdtemp(prefix="lvu-shared-palette-proof-"))
    print(f"Shared palette PTY evidence: {evidence}", flush=True)
    for theme in THEMES:
        run_theme(binary, theme, evidence)


if __name__ == "__main__":
    run(pathlib.Path(sys.argv[1]).resolve())
    print("Shared palette PTY passed: actionable blank, guarded unavailable result, "
          "aligned shortcuts, colors, pointer, scroll, resize, and restoration")
