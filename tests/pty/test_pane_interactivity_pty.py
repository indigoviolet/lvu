#!/usr/bin/env python3
"""Exercise spatial pane focus and pointer-local scrolling in a real PTY."""

import pathlib
import sys
import time

from test_lvu_pty import PtyApp


CTRL_UP = b"\x1b[1;5A"
CTRL_DOWN = b"\x1b[1;5B"
CTRL_RIGHT = b"\x1b[1;5C"
CTRL_LEFT = b"\x1b[1;5D"


def pane_border(app: PtyApp, title: str):
    """Return the displayed foreground of the frame containing ``title``."""
    for y, line in enumerate(app.text().splitlines()):
        if title not in line:
            continue
        title_x = line.index(title)
        border_x = line.rfind("┌", 0, title_x)
        if border_x >= 0:
            return app.screen.buffer[y][border_x].fg
    return None


def wait_border(app: PtyApp, title: str, color, description: str) -> None:
    app.wait_until(
        lambda _: pane_border(app, title) == color,
        description,
    )


def text_point(app: PtyApp, needle: str) -> tuple[int, int]:
    for y, line in enumerate(app.text().splitlines()):
        if needle in line:
            return line.index(needle), y
    raise AssertionError(f"no visible {needle!r} target\n{app.text()}")


def pane_point(app: PtyApp, title: str, rows_below: int = 2) -> tuple[int, int]:
    x, y = text_point(app, title)
    return max(0, x - 1), y + rows_below


def click(app: PtyApp, point: tuple[int, int]) -> None:
    x, y = point
    # SGR mouse positions are one-based; a click is a complete down/up pair.
    app.send(f"\x1b[<0;{x + 1};{y + 1}M\x1b[<0;{x + 1};{y + 1}m".encode())


def wheel_down(app: PtyApp, point: tuple[int, int]) -> None:
    x, y = point
    app.send(f"\x1b[<65;{x + 1};{y + 1}M".encode())


def run(binary: pathlib.Path) -> None:
    app = PtyApp(
        binary,
        ["--demo"],
        width=88,
        height=24,
        environment={
            "LVU_NO_DELIGHT": "1",
            "NO_COLOR": "",
            "COLORTERM": "truecolor",
        },
    )
    try:
        app.wait_for("fixture request 16 completed")
        log_active = pane_border(app, "Log viewport")
        sidebar_inactive = pane_border(app, "Sources / views")
        assert log_active is not None and sidebar_inactive is not None
        assert log_active != sidebar_inactive, app.text()

        # Ctrl+Left moves from the log to the pane that is spatially left.
        app.send(CTRL_LEFT)
        wait_border(app, "Sources / views", log_active, "sidebar focus border")

        # The wheel acts on the hovered log, but keyboard focus remains in the
        # sidebar: plain Down must therefore switch views, not move a log row.
        wheel_down(app, pane_point(app, "Log viewport", 3))
        time.sleep(0.05)
        app.drain()
        assert pane_border(app, "Sources / views") == log_active, app.text()
        app.send(b"\x1b[B")
        app.wait_for("fixture queue unavailable")
        assert pane_border(app, "Sources / views") == log_active, app.text()
        app.send(b"\x1b[A")
        app.wait_for("fixture request 16 completed")

        # Click-to-focus follows the painted panes. Open Details first, then
        # click log, Details and sidebar in turn and observe their focus rings.
        app.send(CTRL_RIGHT)
        wait_border(app, "Log viewport", log_active, "log focus border")
        app.send(b"d")
        app.wait_for("Selected event details")
        details_active = pane_border(app, "Selected event details")
        details_inactive = None
        assert details_active is not None

        click(app, text_point(app, "fixture request 10 completed"))
        wait_border(app, "Log viewport", log_active, "mouse-focused log")
        details_inactive = pane_border(app, "Selected event details")
        assert details_inactive != details_active, app.text()

        click(app, pane_point(app, "Selected event details"))
        wait_border(app, "Selected event details", details_active, "mouse-focused details")

        click(app, text_point(app, "All events"))
        wait_border(app, "Sources / views", log_active, "mouse-focused sidebar")

        # Spatial vertical focus follows the stacked right-hand panes.
        app.send(CTRL_RIGHT + CTRL_DOWN)
        wait_border(app, "Selected event details", details_active, "Ctrl+Down details focus")
        app.send(CTRL_UP)
        wait_border(app, "Log viewport", log_active, "Ctrl+Up log focus")

        # Scrolling Details while the log owns the keyboard must not borrow
        # focus from the log.
        wheel_down(app, pane_point(app, "Selected event details"))
        time.sleep(0.05)
        app.drain()
        assert pane_border(app, "Log viewport") == log_active, app.text()
        assert pane_border(app, "Selected event details") == details_inactive, app.text()

        # A modal owns both modified keys and pointer input. Neither operation
        # may retarget a base pane behind Help.
        app.send(b"?")
        app.wait_for("EVERYWHERE")
        app.send(CTRL_LEFT)
        wheel_down(app, (2, 4))
        app.send(b"?")
        app.wait_for("fixture request 10 completed")
        wait_border(app, "Log viewport", log_active, "modal preserved log focus")

        # Narrow geometry removes the sidebar. A stale selector focus falls
        # back to the visible log, and directions toward the absent pane are
        # explicit no-ops. Details remains vertically reachable.
        app.send(CTRL_LEFT)
        wait_border(app, "Sources / views", log_active, "sidebar focus before resize")
        app.resize(40, 14)
        app.wait_until(
            lambda text: "Sources / views" not in text and "Log viewport" in text,
            "narrow layout without sidebar",
        )
        wait_border(app, "Log viewport", log_active, "narrow fallback to log")
        app.send(CTRL_LEFT)
        time.sleep(0.05)
        app.drain()
        assert pane_border(app, "Log viewport") == log_active, app.text()
        app.send(CTRL_DOWN)
        wait_border(app, "Selected event details", details_active, "narrow details focus")
        app.send(CTRL_LEFT)
        time.sleep(0.05)
        app.drain()
        assert pane_border(app, "Selected event details") == details_active, app.text()
        app.send(CTRL_UP)
        wait_border(app, "Log viewport", log_active, "narrow return to log")

        app.send(b"q")
        assert app.wait_exit(timeout=5) == 0
        app.assert_restored()
    finally:
        if app.process.poll() is None:
            app.process.kill()
            app.process.wait(timeout=5)
        app.close()


if __name__ == "__main__":
    run(pathlib.Path(sys.argv[1]).resolve())
    print("Pane interactivity PTY passed: click, hover-wheel, spatial focus, modal and narrow geometry")
