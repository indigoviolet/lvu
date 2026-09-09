#!/usr/bin/env python3
"""Unified grouping at small and large scale, in the actual application.

A retry flood buries two interesting events. Recognition lives in
Enrichment: a flood column marks the retries and Run grouping collapses the
flood into one event, with no pattern normalisation anywhere. Expanding
restores exactly the original events in order, the choices survive a
restart, grouping never changes filter membership, and grouping tens of thousands
of events never blanks the pane while it settles.
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
            assert int(seen.split("/")[-1]) >= BIG_LINES, seen

            def separators(rows):
                return sum(1 for row in rows if "connection established" in row)

            assert separators(viewport_rows(app.text())) <= 1, app.text()

            # Recognition lives in Enrichment: mark the flood, then open every
            # retry as its own Filter event. Grouping tens of thousands of
            # singletons must never blank the pane while it settles.
            app.send(b"e")
            app.wait_for("Steps")
            app.send(b"\x1ba")
            app.wait_for("Enrichment \u203a New step")
            paste(app, "/(?P<flood>retry connect)/")
            app.send(b"\r")
            app.wait_until(lambda text: "flood" in text and "Applied" in text,
                           "the flood column is applied", timeout=60)
            app.send(b"\x1b")
            app.wait_until(lambda text: "Steps" not in text, "enrichment closed")
            app.send(b"m")
            app.wait_for("Multiline grouping")
            app.send(b"\t")
            app.send(b"\x1b[C")
            app.wait_for("(?lvu:filter:", timeout=10)
            app.send(b"\t")
            app.send(b"\t")
            paste(app, "flood")
            app.wait_for("(?lvu:filter:v1:column:flood)")
            app.send(b"\r")
            app.send(b"\x1b")

            def observe_transition(done, description):
                # Sample before checking completion, from the first visible
                # viewport after the dialog closes through accepted publication.
                blank_frames = []
                deadline = time.monotonic() + 60
                while time.monotonic() < deadline:
                    app.drain()
                    text = app.text()
                    if "Multiline grouping" not in text:
                        if not viewport_rows(text):
                            blank_frames.append(text)
                        if done(text):
                            assert not blank_frames, (
                                f"{description} blanked the pane:\n{blank_frames[0]}"
                            )
                            return text
                    time.sleep(0.02)
                raise AssertionError(f"{description} did not complete:\n{app.text()}")

            observe_transition(
                lambda text: "grouping:display-only" in text,
                "grouping recompute")

            # Clearing the rule ungroups the flood: the pane stays populated
            # through that recompute as well.
            app.send(b"m")
            dialog = app.wait_for("Multiline grouping")
            tab_row = next(
                line for line in dialog.splitlines()
                if "Run" in line and "Legacy" in line and "Off" in line
            )
            row = dialog.splitlines().index(tab_row) + 1
            col = tab_row.index("Off") + 1
            app.send(f"\x1b[<0;{col};{row}M\x1b[<0;{col};{row}m".encode())
            app.wait_until(lambda text: "(?lvu:" not in text,
                           "off selected", timeout=10)
            app.send(b"\r")
            app.send(b"\x1b")
            observe_transition(
                lambda text: "grouping:display-only" not in text
                and len(viewport_rows(text)) >= 15,
                "ungrouping recompute")
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

            # Recognition lives in Enrichment: mark the flood, then collapse
            # it with Run grouping on that column.
            app.send(b"e")
            app.wait_for("Steps")
            app.send(b"\x1ba")
            app.wait_for("Enrichment \u203a New step")
            paste(app, "/(?P<flood>retry connect)/")
            app.send(b"\r")
            app.wait_until(lambda text: "flood" in text and "Applied" in text,
                           "the flood column is applied", timeout=20)
            app.send(b"\x1b")
            app.wait_until(lambda text: "Steps" not in text, "enrichment closed")
            app.send(b"m")
            app.wait_for("Multiline grouping")
            app.send(b"\t")
            app.send(b"\x1b[C")
            app.wait_for("(?lvu:filter:", timeout=10)
            app.send(b"\t")
            app.send(b"\t")
            paste(app, "flood")
            app.wait_for("(?lvu:filter:v1:column:flood)")
            # Starts, not runs: every retry opens its own event here so the
            # flood stays listed while grouping is proven on it below.
            app.send(b"\r")
            app.wait_until(lambda text: "Applied" in text, "grouping applied", timeout=15)
            app.send(b"\x1b")
            app.wait_until(lambda text: "Multiline grouping" not in text,
                           "grouping closed", timeout=10)
            assert app.text().count("retry connect to") > 5, app.text()

            # Run grouping is the collapsing half: back to the Run tab, which
            # keeps a blank column ready, and name the same column there.
            app.send(b"m")
            app.wait_for("Multiline grouping")
            app.send(b"\t")
            app.send(b"\x1b[D")
            app.wait_for("(?lvu:run:v1:column:)", timeout=10)
            app.send(b"\t")
            app.send(b"\t")
            paste(app, "flood")
            app.wait_for("(?lvu:run:v1:column:flood)")
            app.send(b"\r")
            app.wait_until(lambda text: "Applied" in text, "run grouping applied", timeout=15)
            app.send(b"\x1b")
            collapsed = app.wait_until(
                lambda text: "Multiline grouping" not in text
                and text.count("retry connect to") == 1,
                "the flood collapses to one grouped line", timeout=15)
            # The interesting events are visible again.
            assert "service started on port 8080" in collapsed, collapsed
            assert "connection established" in collapsed, collapsed
            assert "ready to serve" in collapsed, collapsed

            # The collapsed line stands for real records: Details shows the
            # head exactly as captured.
            app.send(b"g")
            app.send(b"jj")
            app.send(b"d")
            details = app.wait_for("Selected event details")
            assert "retry connect to 10.0.0.0 failed after 120ms" in details, details
            close_details(app)

            # Enter expands the group back into the original events, in order.
            app.send(b"\r")
            expanded = app.wait_until(lambda text: text.count("retry connect to") > 5,
                                      "the run expands", timeout=10)
            assert f"/{FLOOD + 4}" in expanded, expanded

            # A filter matches exactly what it matched before: grouping changes
            # presentation, never membership.
            app.send(b"/")
            app.wait_for("Search")
            paste(app, "retry connect")
            app.send(b"\r")
            app.wait_until(lambda text: f"search:" in text and "/1" in text,
                           "the matched flood is one grouped line", timeout=15)
            app.send(b"\x1b")
            app.wait_until(lambda text: "Search" not in text, "search dismissed")
            app.send(b"/")
            app.wait_for("Search")
            app.send(b"\x01\x0b\r")
            app.wait_until(lambda text: "search:" not in text, "filter cleared", timeout=15)
            app.send(b"\x1b")
            app.wait_until(lambda text: "Search" not in text, "search dismissed")
            app.wait_until(lambda text: text.count("retry connect to") == 1,
                           "still grouped", timeout=15)
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
            # The grouping rule is working-view state and comes back with it:
            # the flood is one grouped line again without re-entering anything.
            restored = reopened.wait_until(
                lambda text: text.count("retry connect to") == 1
                and "service started on port 8080" in text,
                "grouping survives restart", timeout=25)
            assert "connection established" in restored, restored
            reopened.send(b"g")
            reopened.send(b"jj")
            reopened.send(b"\r")
            reopened.wait_until(lambda text: text.count("retry connect to") > 5,
                                "the restored group still expands", timeout=10)
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
    print("Folding PTY passed: unified grouping control, enrichment-defined runs, "
          "honest expansion, unchanged filtering, restart restoration, and a large "
          "view that never blanks the pane while grouping settles")
