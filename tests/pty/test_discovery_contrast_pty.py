"""Verify real Discovery diagnostic text retains its foreground across focus.

The subject is the diagnostics pane's colour and focus signalling, not what the
scan finds. Discovery over a real `/proc` is bounded, so on a busy machine it
may report a limited scan and no candidates at all; every assertion here has to
hold in that case too.
"""
import os
import pathlib
import sys
import tempfile
import time

from test_lvu_pty import PtyApp


def run(binary):
    with tempfile.TemporaryDirectory(prefix="lvu-discovery-contrast-") as directory:
        root = pathlib.Path(directory)
        config = root / "config/lvu"
        config.mkdir(parents=True)
        (config / "settings.toml").write_text(
            'schema_version = 1\n[appearance]\ntheme = "love-dark"\n'
            'delight_enabled = false\nreduced_motion = true\nascii = false\n'
            '[paseo]\nprovider = "codex/gpt-5.6-luna"\nmode = "full-access"\nthinking = "medium"\n'
            '[cache.memory]\nrows_mib = 4\nmembership_mib = 256\n'
            '[cache.disk]\ntotal_mib = 5120\nindex_per_source_mib = 256\n'
        )
        source = root / "event.log"
        source.write_text("contrast-fixture\n")
        app = PtyApp(binary, [str(source)], width=100, height=28, environment={
            "XDG_CONFIG_HOME": str(root / "config"),
            "XDG_DATA_HOME": str(root / "data"),
            "XDG_CACHE_HOME": str(root / "cache"),
            "MISE_DATA_DIR": os.environ.get("MISE_DATA_DIR", str(pathlib.Path.home() / ".local/share/mise")),
            "UV_CACHE_DIR": os.environ.get("UV_CACHE_DIR", str(pathlib.Path.home() / ".cache/uv")),
            "NO_COLOR": "", "COLORTERM": "truecolor",
        })
        try:
            app.wait_for("contrast-fixture")
            app.send(b"n\x04")
            app.wait_for("Details")
            # §8.7 removed the box: the pane is a heading plus indented body,
            # and focus is signalled on the heading rather than a border.
            def diagnostic_cells():
                rows = app.text().splitlines()
                located = next(
                    ((y, line) for y, line in enumerate(rows) if "Details" in line), None
                )
                if located is None:
                    return None
                y, line = located
                x = line.index("Details")
                # The body is written under the heading, indented. Until it has
                # text there is no foreground to measure.
                if y + 1 >= len(rows) or not rows[y + 1][x + 2:].strip():
                    return None
                return x, y, app.screen.buffer[y + 1][x + 2]

            # What the pane says depends on the machine: a busy box may leave the
            # process scan bounded and every candidate unfound. The colour rules
            # under test do not, and a provider always reports something, so wait
            # for a body rather than for a candidate.
            app.wait_until(
                lambda _: (cells := diagnostic_cells()) is not None
                and cells[2].fg == "f4e7ea",
                "readable unfocused diagnostics",
                timeout=10.0,
            )
            # Candidates stream in, and each one moves the heading down a row.
            # Measuring and then clicking a row that has since scrolled reads as
            # "focus does nothing", so take the coordinates only once the pane
            # has stopped moving.
            def settled_cells():
                previous = None
                deadline = time.monotonic() + 10.0
                while time.monotonic() < deadline:
                    app.drain()
                    current = diagnostic_cells()
                    if current is not None and previous == current[:2]:
                        return current
                    previous = None if current is None else current[:2]
                    time.sleep(0.25)
                raise AssertionError(
                    f"the discovery pane never stopped moving\n{app.text()}"
                )

            x, y, _ = settled_cells()
            heading = app.screen.buffer[y][x].fg
            app.send(f"\x1b[<0;{x + 2};{y + 2}M\x1b[<0;{x + 2};{y + 2}m".encode())
            try:
                app.wait_until(lambda _: app.screen.buffer[y][x].fg != heading,
                               "diagnostics focus heading")
            except AssertionError as failure:
                # Two different defects present the same way here: the click no
                # longer focuses the pane, or focus is signalled somewhere other
                # than the heading. Say which cells were read so the next reader
                # does not have to reconstruct them.
                raise AssertionError(
                    f"{failure}\nclicked ({x + 2},{y + 2}); heading cell ({x},{y}) stayed "
                    f"{heading!r}; body cell ({x + 2},{y + 1}) is "
                    f"{app.screen.buffer[y + 1][x + 2].fg!r}\n{app.text()}"
                ) from failure
            assert diagnostic_cells()[2].fg == "f4e7ea"
            assert b"38;2;244;231;234" in bytes(app.transcript)
            app.send(b"\x1b")
            app.wait_until(lambda text: "Details" not in text, "close discovery")
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
    print("Discovery contrast PTY passed")
