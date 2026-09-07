#!/usr/bin/env python3
"""Composed-binary PTY acceptance for shared dialog controls and colors."""

from __future__ import annotations

import argparse
import os
import pathlib
import tempfile

from wcwidth import wcswidth

from test_lvu_pty import PtyApp


FRAME_END = b"\x1b[?2026l"
FORBIDDEN_FORM_HELP = (
    "Enter apply",
    "Enter activate",
    "Tab next",
    "Esc close",
    "PgUp",
    "PgDn",
    "Home/End",
)
THEMES = {
    # dialog-system.md §6.1 raised input_bg to a tone that actually reads against
    # dialog_bg (1.39:1 dark, 1.37:1 light); the old values were 1.16:1.
    "love-dark": {"input": "46303f", "selection": "ffa797"},
    "love-light": {"input": "e6c8c4", "selection": "a6363a"},
}


def environment(root: pathlib.Path) -> dict[str, str]:
    return {
        "XDG_CONFIG_HOME": str(root / "config"),
        "XDG_CACHE_HOME": str(root / "cache"),
        "XDG_DATA_HOME": str(root / "data"),
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
        "LVU_NO_DELIGHT": "1",
        "NO_COLOR": "",
        "COLORTERM": "truecolor",
    }


def write_settings(root: pathlib.Path, theme: str) -> None:
    path = root / "config" / "lvu" / "settings.toml"
    path.parent.mkdir(parents=True)
    path.write_text(
        f'''schema_version = 1
[appearance]
theme = "{theme}"
delight_enabled = false
reduced_motion = true
ascii = false
[paseo]
provider = "fixture/provider"
mode = "full-access"
thinking = "medium"
[cache.memory]
rows_mib = 4
membership_mib = 256
[cache.disk]
total_mib = 5120
index_per_source_mib = 256
'''
    )


def wait_frame(app: PtyApp, predicate, description: str, start: int) -> str:
    def ready(text: str) -> bool:
        delta = bytes(app.transcript[start:])
        return predicate(text) and FRAME_END in delta

    return app.wait_until(ready, f"completed frame for {description}", timeout=6.0)


def assert_bounded(app: PtyApp, text: str) -> None:
    lines = text.splitlines()
    assert len(lines) == app.screen.lines
    assert all(wcswidth(line) <= app.screen.columns for line in lines), (
        app.screen.columns,
        max(map(wcswidth, lines)),
    )


def assert_form_contract(text: str, buttons: tuple[str, ...] = ()) -> None:
    for forbidden in FORBIDDEN_FORM_HELP:
        assert forbidden not in text, f"generic key reminder leaked: {forbidden!r}"
    for label in buttons:
        assert f"[ {label} ]" in text, f"missing bounded button {label!r}\n{text}"


def locate(text: str, token: str) -> tuple[int, int]:
    for y, line in enumerate(text.splitlines()):
        x = line.find(token)
        if x >= 0:
            return x, y
    raise AssertionError(f"could not locate {token!r}\n{text}")


def click(app: PtyApp, x: int, y: int) -> None:
    # SGR mouse coordinates are one-based.
    app.send(f"\x1b[<0;{x + 1};{y + 1}M\x1b[<0;{x + 1};{y + 1}m".encode())


def record(
    evidence: pathlib.Path,
    theme: str,
    name: str,
    app: PtyApp,
    start: int,
    text: str,
) -> None:
    assert_bounded(app, text)
    cells = (
        app.screen.buffer[y][x]
        for y in range(app.screen.lines)
        for x in range(app.screen.columns)
    )
    assert any(cell.fg != "default" or cell.bg != "default" for cell in cells), (
        f"{theme}/{name} emitted no concrete dialog color"
    )
    stem = evidence / f"{theme}-{name}"
    stem.with_suffix(".screen.txt").write_text(text)
    stem.with_suffix(".ansi").write_bytes(bytes(app.transcript[start:]))


def open_surface(
    app: PtyApp,
    evidence: pathlib.Path,
    theme: str,
    name: str,
    key: bytes,
    marker: str,
    buttons: tuple[str, ...] = (),
) -> str:
    start = len(app.transcript)
    app.send(key)
    text = wait_frame(app, lambda value: marker in value, name, start)
    assert_form_contract(text, buttons)
    record(evidence, theme, name, app, start, text)
    return text


def close_surface(app: PtyApp, marker: str) -> None:
    start = len(app.transcript)
    app.send(b"\x1b")
    wait_frame(app, lambda text: marker not in text, f"close {marker}", start)


def assert_input_focus(app: PtyApp, expected_input: str) -> None:
    cursor = app.screen.cursor
    assert not cursor.hidden, "editable field did not expose a terminal caret"
    assert app.screen.buffer[cursor.y][cursor.x].bg != "default"
    neighbors = [
        app.screen.buffer[cursor.y][x].bg
        for x in range(max(0, cursor.x - 2), min(app.screen.columns, cursor.x + 3))
        if x != cursor.x
    ]
    assert expected_input in neighbors, (expected_input, cursor.x, cursor.y, neighbors)


def assert_semantic_backgrounds(
    app: PtyApp, theme: str, expected: dict[str, str]
) -> None:
    transcript = bytes(app.transcript).lower()
    for color in expected.values():
        rgb = bytes(int(color[index : index + 2], 16) for index in (0, 2, 4))
        sgr = b";".join(str(component).encode() for component in rgb)
        assert b"48;2;" + sgr in transcript, (theme, color)


def exercise_theme(
    binary: pathlib.Path, evidence: pathlib.Path, theme: str, surfaces: str
) -> None:
    with tempfile.TemporaryDirectory(prefix=f"lvu-shared-{theme}-") as directory:
        root = pathlib.Path(directory)
        write_settings(root, theme)
        source = root / "events.log"
        source.write_text('{"timestamp":"2026-09-06T12:34:56.123456789Z","message":"shared-ui"}\n')
        suggested_source = root / "suggested-events.log"
        suggested_source.write_text("source-auto-completed\n")
        app = PtyApp(
            binary,
            ["--file", str(source), "--capture-dir", str(root / "capture")],
            width=112,
            height=30,
            environment=environment(root),
        )
        expected = THEMES[theme]
        workspace_marker = "shared-ui"
        try:
            app.wait_for(workspace_marker, timeout=8.0)

            if surfaces == "all":
                search = open_surface(app, evidence, theme, "search", b"/", "Search")
                app.send("q界e\u0301".encode())
                app.wait_for("q界é")
                assert_input_focus(app, expected["input"])
                assert "[ More ]" not in search
                start = len(app.transcript)
                app.send(b"\x01\x0b")
                wait_frame(
                    app,
                    lambda text: "every record is shown" in text and "q界é" not in text,
                    "Search Ctrl-A/Ctrl-K clear",
                    start,
                )
                close_surface(app, "┌ Search")

                advanced = open_surface(
                    app, evidence, theme, "advanced", b"p", "Advanced filter"
                )
                assert_input_focus(app, expected["input"])
                assert "[ More ]" not in advanced
                close_surface(app, "┌ Advanced filter")

                enrichment = open_surface(
                    app,
                    evidence,
                    theme,
                    "enrichment",
                    b"e",
                    "Enrichment",
                    # §7.5: a trailing ellipsis marks a button that opens a
                    # child dialog.
                    ("Add", "Edit", "Remove", "External command…"),
                )
                # The step list layer has no editable field of its own; the
                # draft lives in the nested step editor, so assert the caret
                # where the input actually is.
                start = len(app.transcript)
                app.send(b"\x1ba")
                wait_frame(
                    app,
                    lambda text: "Enrichment › New step" in text,
                    "enrichment step editor",
                    start,
                )
                assert_input_focus(app, expected["input"])
                app.send(b"\x1b")
                app.wait_until(
                    lambda text: "Enrichment › New step" not in text,
                    "step editor closes",
                )
                close_surface(app, "┌ Enrichment")

                open_surface(
                    app,
                    evidence,
                    theme,
                    "enrichment-for-command",
                    b"e",
                    "Enrichment",
                    ("External command…",),
                )
                start = len(app.transcript)
                app.send(b"\t\t\t\t\r")
                command = wait_frame(
                    app,
                    lambda text: "┌ External command" in text,
                    "external command",
                    start,
                )
                assert_form_contract(command, ("Save", "Review", "Remove"))
                record(evidence, theme, "external-command", app, start, command)
                start = len(app.transcript)
                app.resize(54, 18)
                command_narrow = wait_frame(
                    app,
                    lambda text: "┌ External command" in text,
                    "narrow external command",
                    start,
                )
                assert_form_contract(command_narrow, ("Save", "Review", "Remove"))
                record(evidence, theme, "external-command-narrow", app, start, command_narrow)
                app.resize(112, 30)
                close_surface(app, "┌ External command")

                source_form = open_surface(
                    app,
                    evidence,
                    theme,
                    "source",
                    b"n",
                    "Add source",
                    ("Open",),
                )
                assert_input_focus(app, expected["input"])
                assert "[ Complete path ]" not in source_form
                # §8.6/§8.4: the three modes are a segmented control and the two
                # kinds are radios, so they are no longer bounded buttons. They
                # remain visible, focusable and clickable.
                for mode in ("Manual", "Discover", "🧠"):
                    assert mode in source_form, (mode, source_form)
                for kind in ("File", "Command"):
                    assert kind in source_form, (kind, source_form)
                for retired in ("[ Manual ]", "[ Discover ]", "[ File ]", "[ Command ]"):
                    assert retired not in source_form, (retired, source_form)
                prefix = str(suggested_source.with_name("suggested-eve"))
                app.send(b"stale")
                app.wait_for("stale")
                start = len(app.transcript)
                app.send(b"\x01\x0b")
                wait_frame(
                    app,
                    lambda text: "stale" not in text,
                    "Source Ctrl-A/Ctrl-K clear",
                    start,
                )
                start = len(app.transcript)
                app.send(b"\x1b[200~" + prefix.encode() + b"\x1b[201~")
                suggestion = wait_frame(
                    app,
                    lambda text: suggested_source.name in text,
                    "automatic Source suggestion",
                    start,
                )
                # A live suggestion adds no button of its own; the dialog keeps
                # exactly one primary action (§12.7).
                assert "[ Complete path ]" not in suggestion
                assert suggestion.count("[ Open ]") == 1, suggestion
                start = len(app.transcript)
                app.send(b"\r")
                opened = wait_frame(
                    app,
                    lambda text: "┌ Add source" not in text,
                    "direct Source suggestion activation",
                    start,
                )
                record(evidence, theme, "source-direct-enter", app, start, opened)
                app.wait_for("source-auto-completed", timeout=8.0)
                workspace_marker = "source-auto-completed"

            time_form = open_surface(
                app,
                evidence,
                theme,
                "time-wide",
                b"t",
                "Time window",
                ("Apply", "Clear", "🧠 Recognize timestamp"),
            )
            # §7.4: one message row, one state word, no `Applied:` stutter.
            assert "Start" in time_form and "End" in time_form
            assert "Applied" in time_form and "Applied:" not in time_form
            assert "Scroll up" not in time_form and "Scroll down" not in time_form
            # Time controls activate with Enter; Space is reserved for literal
            # text input and must not be treated as a generic button shortcut.
            app.send(b"\t\t\t\t\r")
            app.wait_for("UTC−12:00")
            app.send(b"\x1b")  # Dropdown first; the Time dialog must remain.
            app.wait_until(
                lambda text: "Time window" in text and "UTC−12:00" not in text,
                "timezone dropdown dismissal",
            )
            app.assert_remains("Time window", "UTC−12:00")
            start = len(app.transcript)
            app.resize(46, 12)
            # §9 replaces the scroll pseudo-buttons with a scrollbar; the
            # narrow form reflows Start/End to one field per row instead.
            narrow = wait_frame(
                app,
                lambda text: "Time window" in text and "Start time" in text,
                "narrow Time reflow",
                start,
            )
            assert "Scroll down" not in narrow, narrow
            assert_form_contract(narrow)
            record(evidence, theme, "time-narrow", app, start, narrow)
            close_surface(app, "┌ Time window")
            start = len(app.transcript)
            app.resize(112, 30)
            wait_frame(
                app,
                lambda text: workspace_marker in text,
                "wide workspace after Time",
                start,
            )

            settings = open_surface(
                app, evidence, theme, "settings-wide", b",", "Settings", ("Save",)
            )
            # §7.4 replaced `Saved: Saved; …` with the shared message row.
            assert "Saved" in settings, settings
            assert "Saved:" not in settings, settings
            # §12.14 keeps every field on one scrolling form, so [ More ] is now
            # a real overflow control: it may appear, but only with a scrollbar.
            # ui_state asserts the converse — that it disappears without overflow.
            assert "[ More ]" not in settings or "█" in settings, settings
            assert_input_focus(app, expected["input"])
            start = len(app.transcript)
            app.send(b"\t\t\t ")
            dropdown = wait_frame(
                app,
                lambda text: "love-dark" in text and "love-light" in text,
                "theme dropdown",
                start,
            )
            assert app.screen.cursor.hidden, "finite dropdown leaked an editable caret"
            other_theme = "love-light" if theme == "love-dark" else "love-dark"
            start = len(app.transcript)
            app.send(b"\x1b")
            settings = wait_frame(
                app,
                lambda text: "┌ Settings" in text and other_theme not in text,
                "theme dropdown dismissal",
                start,
            )
            start = len(app.transcript)
            app.send(b"\t ")
            settings = wait_frame(
                app,
                lambda text: "Pending   changes are not saved" in text,
                "Settings draft becomes pending",
                start,
            )
            save_x, save_y = locate(settings, "[ Save ]")
            start = len(app.transcript)
            click(app, save_x + 2, save_y)
            wait_frame(
                app,
                lambda text: "Saved     saved and applied" in text
                and "Pending   changes are not saved" not in text,
                "mouse Save acknowledgement",
                start,
            )
            start = len(app.transcript)
            app.resize(54, 12)
            narrow = wait_frame(app, lambda text: "Settings" in text, "narrow Settings", start)
            assert_form_contract(narrow)
            assert "[ More ]" in narrow, "real details overflow lacks its More control"
            record(evidence, theme, "settings-narrow", app, start, narrow)
            close_surface(app, "┌ Settings")
            start = len(app.transcript)
            app.resize(112, 30)
            wait_frame(
                app,
                lambda text: workspace_marker in text,
                "wide workspace after Settings",
                start,
            )

            if surfaces == "time-settings":
                assert_semantic_backgrounds(app, theme, expected)
                app.send(b"q")
                assert app.wait_exit(timeout=5.0) == 0
                app.assert_restored()
                return

            # Selection/list, detail and diagnostic families. These remain local:
            # no agent action is submitted and no provider process is contacted.
            for name, key, marker in (
                ("views", b"v", "┌ View · "),
                ("fields", b"i", "┌ Fields · record"),
                ("context", b"o", "┌ Raw context"),
                ("bookmarks", b"B", "┌ Bookmarks"),
                ("recipes", b"r", "┌ Recipes"),
                ("storage", b"S", "┌ Storage "),
                ("ask", b"A", "┌ Ask"),
                ("investigation", b"I", "┌ Investigation"),
            ):
                open_surface(app, evidence, theme, name, key, marker)
                start = len(app.transcript)
                app.resize(54, 16)
                narrow = wait_frame(
                    app,
                    lambda text, marker=marker: marker in text,
                    f"narrow {name}",
                    start,
                )
                assert_form_contract(narrow)
                record(evidence, theme, f"{name}-narrow", app, start, narrow)
                app.resize(112, 30)
                close_surface(app, marker)

            open_surface(app, evidence, theme, "details", b"d", "┌ Selected event details")
            start = len(app.transcript)
            app.resize(54, 16)
            details_narrow = wait_frame(
                app,
                lambda text: "┌ Selected event details" in text,
                "narrow Details",
                start,
            )
            assert_form_contract(details_narrow)
            record(evidence, theme, "details-narrow", app, start, details_narrow)
            app.resize(112, 30)
            close_surface(app, "┌ Selected event details")

            palette = open_surface(app, evidence, theme, "palette", b"\x10", "Command palette")
            assert_input_focus(app, expected["input"])
            assert "[ More ]" not in palette
            close_surface(app, "Command palette")

            help_text = open_surface(app, evidence, theme, "help", b"?", "EVERYWHERE")
            assert "PgUp/PgDn" not in help_text and "Home/End" not in help_text
            close_surface(app, "EVERYWHERE")

            # At least one completed frame must carry each concrete semantic RGB
            # background. The exact screen checks above prove the input role; a
            # mouse-focused Save supplies the selection role.
            assert_semantic_backgrounds(app, theme, expected)

            app.send(b"q")
            assert app.wait_exit(timeout=5.0) == 0
            app.assert_restored()
        finally:
            if app.process.poll() is None:
                app.process.kill()
                app.process.wait(timeout=5.0)
            app.drain()
            (evidence / f"{theme}-complete.ansi").write_bytes(bytes(app.transcript))
            app.close()


def run(binary: pathlib.Path, surfaces: str) -> None:
    evidence = pathlib.Path(tempfile.mkdtemp(prefix="lvu-shared-dialogs-proof-"))
    print(f"Shared dialog PTY evidence: {evidence}", flush=True)
    for theme in THEMES:
        exercise_theme(binary, evidence, theme, surfaces)


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("binary", type=pathlib.Path)
    parser.add_argument(
        "--surfaces", choices=("all", "time-settings"), default="all"
    )
    arguments = parser.parse_args()
    run(arguments.binary.resolve(), arguments.surfaces)
    print("Shared dialog PTY passed: dark/light, wide/narrow, keyboard/mouse and bounded controls")
