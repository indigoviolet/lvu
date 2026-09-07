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
import time

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


# --- Folding a large view must never blank the pane -------------------------
#
# Folding walks the stream through the same bounded row cache the viewport
# reads. Walking it from the beginning, faster than the cache is deep, evicted
# the visible rows every frame: over a large capture the pane went empty on the
# toggle and stayed empty for minutes with nothing on screen saying why. The
# feed now starts at the window the user is looking at and is bounded well
# below the cache, so every frame from the toggle onwards has rows in it.

BIG_RUN = 400
BIG_LINES = 60_000


def big_source_text() -> str:
    """Long adjacent runs, so the window is inside a run wherever it sits."""
    lines = []
    for index in range(BIG_LINES):
        if index % BIG_RUN == BIG_RUN - 1:
            lines.append("connection established")
        else:
            lines.append(
                f"retry connect to 10.0.0.{index % 7} failed after {120 + index % 91}ms"
            )
    return "\n".join(lines) + "\n"


def viewport_rows(text: str) -> list[str]:
    """The log pane's own rows: inside its box, below its column header."""
    lines = text.splitlines()
    top = next((index for index, line in enumerate(lines) if "Log viewport" in line), None)
    if top is None:
        return []
    left = lines[top].rindex("\u250c", 0, lines[top].index("Log viewport"))
    rows = []
    for line in lines[top + 1:]:
        cell = line[left:]
        if "\u2518" in cell or cell.startswith("\u2514"):
            break
        rows.append(cell.strip("\u2502").strip())
    if rows and rows[0].split()[:2] == ["time", "level"]:
        rows = rows[1:]
    return [row for row in rows if row]


def run_large_view_never_blanks(binary, tooling):
    """At 80x24 over a 60k-row capture, no frame after the toggle is empty."""
    with tempfile.TemporaryDirectory(prefix="lvu-fold-big-pty-") as directory:
        root = pathlib.Path(directory)
        source = root / "flood.log"
        source.write_text(big_source_text())
        environment = {**tooling, "XDG_CONFIG_HOME": str(root / "config"),
                       "XDG_DATA_HOME": str(root / "data"),
                       "XDG_CACHE_HOME": str(root / "cache"),
                       "XDG_STATE_HOME": str(root / "state")}
        app = PtyApp(binary, [str(source), "--capture-dir", str(root / "capture")],
                     width=80, height=24, environment=environment)
        try:
            # Measure the toggle against a settled capture, not against ingest.
            def counter(text):
                for part in text.splitlines()[-1].split("|"):
                    if "/" in part and part.strip()[0].isdigit():
                        return part.strip()
                return ""

            settled, seen = 0, None
            deadline = time.monotonic() + 180
            while time.monotonic() < deadline:
                app.drain()
                text = app.text()
                now = counter(text)
                settled = settled + 1 if now == seen and len(viewport_rows(text)) >= 15 else 0
                seen = now
                if settled >= 12:
                    break
                time.sleep(0.1)
            else:
                raise AssertionError(f"capture never settled; last {seen!r}\n{app.text()}")
            # Partial-flush records make the captured total a little larger
            # than the line count; take it from the view rather than assume it.
            # Partial-flush records make the captured total a little larger
            # than the line count; take it from the view rather than assume it.
            assert int(seen.split("/")[-1]) >= BIG_LINES, seen

            # The toggle's own notice occupies the status line, so the fold is
            # read from the pane itself: a folded view of this fixture is one
            # collapsed entry per run separated by the unique event between runs,
            # where an unfolded one is a screen of retries.
            def separators(rows):
                return sum(1 for row in rows if "connection established" in row)

            assert separators(viewport_rows(app.text())) <= 1, app.text()

            open_palette(app, "fold repeated", "Fold repeated events")
            blank_frames = []
            folded = None
            deadline = time.monotonic() + 120
            while time.monotonic() < deadline:
                app.drain()
                text = app.text()
                rows = viewport_rows(text)
                # The invariant under test: a presentation-only recompute never
                # costs the user the rows they already had.
                if not rows:
                    blank_frames.append(text)
                if separators(rows) >= 5:
                    folded = text
                    break
                time.sleep(0.01)
            assert not blank_frames, (
                f"{len(blank_frames)} frame(s) between the toggle and the fold showed no "
                f"rows at all; first was:\n{blank_frames[0]}"
            )
            assert folded is not None, f"the visible window never folded:\n{app.text()}"

            # Toggling again cancels the recompute: the stream comes back with
            # its own length and its own rows, and no frame of that is empty.
            open_palette(app, "fold repeated", "Fold repeated events")
            deadline = time.monotonic() + 60
            restored = None
            while time.monotonic() < deadline:
                app.drain()
                text = app.text()
                rows = viewport_rows(text)
                if not rows:
                    blank_frames.append(text)
                if rows and separators(rows) <= 1:
                    restored = text
                    break
                time.sleep(0.01)
            assert not blank_frames, (
                f"cancelling folding blanked the pane:\n{blank_frames[0]}"
            )
            # The pane is a screen of individual retries again, which is what
            # the view looked like before the toggle. The stream's own length is
            # asserted at the engine seam, where the toggle's status notice is
            # not covering the counter.
            assert restored is not None, f"cancelling never restored:\n{app.text()}"
            stop(app)
        finally:
            (root / "screen.txt").write_text(app.text())
            if app.process.poll() is None:
                app.process.kill()
                app.process.wait(timeout=5)
                app.close()


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
            assert "\u00d7" not in unfolded and "\u203a retry" not in unfolded, unfolded
            assert unfolded.count("retry connect to") > 5, unfolded
            assert f"/{FLOOD + 4}" in unfolded, unfolded

            open_palette(app, "fold repeated", "Fold repeated events")
            folded = app.wait_until(lambda text: f"\u00d7{FLOOD} events" in text,
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
            expanded = app.wait_until(lambda text: f"\u00d7{FLOOD} events" not in text,
                                      "the run expands", timeout=10)
            assert f"/{FLOOD + 4}" in expanded, expanded
            assert expanded.count("retry connect to") > 5, expanded

            open_palette(app, "collapse expanded", "Collapse expanded runs")
            app.wait_until(lambda text: f"\u00d7{FLOOD} events" in text,
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
            app.wait_until(lambda text: f"/{FLOOD}" in text and f"\u00d7{FLOOD} events" not in text,
                           "unfolded, the same filter still matches every retry", timeout=15)
            # Restore folding, clear the filter, and leave it on for the restart.
            open_palette(app, "fold repeated", "Fold repeated events")
            app.wait_until(lambda text: f"\u00d7{FLOOD} events" in text, "folded again", timeout=15)
            app.send(b"/")
            app.wait_for("Search")
            app.send(b"\x01\x0b\r")
            app.wait_until(lambda text: "search:" not in text, "filter cleared", timeout=15)
            app.send(b"\x1b")
            app.wait_until(lambda text: "Search" not in text, "search dismissed")
            app.wait_until(lambda text: f"\u00d7{FLOOD} events" in text, "still folded", timeout=15)
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
            restored = reopened.wait_until(lambda text: f"\u00d7{FLOOD} events" in text,
                                           "folding survives restart", timeout=25)
            assert "fold:1 runs" in restored, restored
            assert restored.count("retry connect to") == 1, restored
            reopened.send(b"g")
            reopened.send(b"jj")
            reopened.send(b"\r")
            reopened.wait_until(lambda text: f"\u00d7{FLOOD} events" not in text,
                                "the restored fold still expands", timeout=10)
            stop(reopened)
        finally:
            if reopened.process.poll() is None:
                reopened.process.kill()
                reopened.process.wait(timeout=5)
                reopened.close()

        # Folding is display-only: the captured source is byte-for-byte intact.
        assert source.read_text() == original

    run_large_view_never_blanks(binary, tooling)


if __name__ == "__main__":
    run(pathlib.Path(sys.argv[1]).resolve())
    print("Folding PTY passed: off by default, palette-discoverable collapse, honest "
          "indicator, exact expansion, unchanged filtering, restart restoration, and a "
          "large view that never blanks the pane while it folds")
