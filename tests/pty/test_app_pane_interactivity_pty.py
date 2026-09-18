#!/usr/bin/env python3
"""Prove pane press-focus and visible-text dragging in the production binary."""

from __future__ import annotations

import base64
import pathlib
import re
import sys

from test_enrichment_chain_pty import stop
from test_lvu_pty import PtyApp, isolated_environment, release_scratch, scratch_root


SETTINGS = """\
schema_version = 1

[paseo]
provider = "fixture/provider"
mode = "full-access"
thinking = "medium"

[automatic_setup]
policy = "disabled"

[appearance]
theme = "terminal"
display_zone = "Z"
delight_enabled = false
reduced_motion = true
ascii = false

[cache.memory]
rows_mib = 4
membership_mib = 256

[cache.disk]
total_mib = 5120
index_per_source_mib = 256
"""


def pane_border(app: PtyApp, title: str):
    for y, line in enumerate(app.text().splitlines()):
        if title in line:
            title_x = line.index(title)
            border_x = line.rfind("┌", 0, title_x)
            if border_x >= 0:
                return app.screen.buffer[y][border_x].fg
    return None


def text_point(app: PtyApp, needle: str) -> tuple[int, int]:
    for y, line in enumerate(app.text().splitlines()):
        if needle in line:
            return line.index(needle), y
    raise AssertionError(f"no visible {needle!r}\n{app.text()}")


def mouse(app: PtyApp, code: int, point: tuple[int, int], suffix: str = "M") -> None:
    x, y = point
    app.send(f"\x1b[<{code};{x + 1};{y + 1}{suffix}".encode())


def click(app: PtyApp, point: tuple[int, int]) -> None:
    mouse(app, 0, point)
    mouse(app, 0, point, "m")


def run(binary: pathlib.Path) -> None:
    root = scratch_root()
    app = None
    try:
        source = root / "events.log"
        source.write_text("".join(f"PANE_MARKER line={index:02d}\n" for index in range(24)))
        config = root / "config" / "lvu"
        config.mkdir(parents=True)
        (config / "settings.toml").write_text(SETTINGS)
        app = PtyApp(
            binary,
            [str(source), "--capture-dir", str(root / "capture")],
            width=120,
            height=30,
            environment={**isolated_environment(root), "LVU_NO_DELIGHT": "1", "NO_COLOR": ""},
        )
        app.wait_for("PANE_MARKER line=00", timeout=15)
        app.wait_for("Sources / views", timeout=8)
        app.wait_for("Log viewport", timeout=8)
        log_active = pane_border(app, "Log viewport")
        assert log_active is not None

        # Focus changes at press, before a release can be delivered. This is
        # the real executable path, not the synthetic lvu --demo shell.
        sidebar = text_point(app, "Sources / views")
        mouse(app, 0, (sidebar[0], sidebar[1] + 2))
        app.wait_until(
            lambda _: pane_border(app, "Sources / views") == log_active,
            "sidebar focus on mouse press",
        )
        mouse(app, 0, (sidebar[0], sidebar[1] + 2), "m")

        line = text_point(app, "PANE_MARKER line=00")
        mouse(app, 0, line)
        app.wait_until(
            lambda _: pane_border(app, "Log viewport") == log_active,
            "log focus on mouse press",
        )
        mouse(app, 32, (line[0] + 10, line[1]))
        mouse(app, 0, (line[0] + 10, line[1]), "m")
        before = len(app.transcript)
        app.send(b"\x03")
        app.wait_until(
            lambda _: b"\x1b]52;c;" in app.transcript[before:],
            "visible log drag copied through OSC 52",
        )
        match = re.search(rb"\x1b\]52;c;([A-Za-z0-9+/=]*)(?:\x07|\x1b\\)", bytes(app.transcript[before:]))
        assert match is not None
        assert "PANE_MARKER" in base64.b64decode(match.group(1)).decode()

        app.send(b"d")
        app.wait_for("Selected event details", timeout=8)
        details_active = pane_border(app, "Selected event details")
        details = text_point(app, "Selected event details")
        click(app, (details[0], details[1] + 2))
        app.wait_until(
            lambda _: pane_border(app, "Selected event details") == details_active,
            "details click focus",
        )
        stop(app)
        app = None
    finally:
        if app is not None and app.process.poll() is None:
            app.process.kill()
            app.process.wait(timeout=5)
            app.close()
        release_scratch(root)


if __name__ == "__main__":
    run(pathlib.Path(sys.argv[1]).resolve())
    print("Production pane PTY passed: press focus, text drag/copy, details focus")
