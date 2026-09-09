#!/usr/bin/env python3
"""Auto grouping presents messy multiline logs without changing their records."""
import pathlib
import sys
import tempfile

from test_folding_pty import open_palette
from test_lvu_pty import PtyApp


def run(binary: pathlib.Path) -> None:
    with tempfile.TemporaryDirectory(prefix="lvu-generic-multiline-") as directory:
        root = pathlib.Path(directory)
        source = root / "events.log"
        source.write_text(
            "ERROR boom\n  at worker.py:7\n"
            "ERROR boom\n  at worker.py:7\n"
            "ERROR boom\n  at worker.py:7\n"
            "stray fragment\n  stray indentation\n",
            encoding="utf-8",
        )
        app = PtyApp(
            binary,
            [str(source), "--capture-dir", str(root / "capture")],
            width=120,
            height=30,
            environment={
                "XDG_CONFIG_HOME": str(root / "config"),
                "XDG_DATA_HOME": str(root / "data"),
                "XDG_CACHE_HOME": str(root / "cache"),
                "LVU_NO_DELIGHT": "1",
            },
        )
        try:
            # Raw records are visible before the presentation rule is applied.
            raw = app.wait_for("stray indentation", timeout=12)
            assert "at worker.py:7" in raw and "physical lines" not in raw, raw

            app.send(b"m")
            grouping = app.wait_for("Mode Auto")
            assert "conservative multiline detection" in grouping, grouping
            assert "(?lvu:auto:" not in grouping, grouping
            app.send(b"\r")
            app.wait_until(lambda text: "Applied" in text and "Auto" in text,
                           "automatic grouping applied", timeout=15)
            app.send(b"\x1b")
            grouped = app.wait_until(
                lambda text: "Multiline grouping" not in text
                and text.count("2 physical lines") >= 3,
                "grouped base viewport",
            )
            assert "stray fragment" in grouped, grouped
            assert "orphan continuation" in grouped, grouped

            # Folding remains the downstream presentation layer: it sees the
            # three projected groups and folds their repeated heads.
            open_palette(app, "fold repeated", "Fold repeated events")
            folded = app.wait_until(
                lambda text: "×3 events" in text
                and "ERROR boom [<num> physical lines]" in text,
                "folding projected automatic groups",
                timeout=15,
            )
            assert "stray fragment" in folded, folded
            app.send(b"q")
            assert app.wait_exit(timeout=8) == 0
            app.assert_restored()
        finally:
            if app.process.poll() is None:
                app.process.kill()
                app.process.wait()
            app.close()
    print("Generic multiline PTY passed: raw first, Auto groups conservatively, z folds projected groups")


if __name__ == "__main__":
    run(pathlib.Path(sys.argv[1]).resolve())
