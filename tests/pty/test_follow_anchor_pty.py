#!/usr/bin/env python3
"""FOLLOW anchors to what the view can serve, not to what it has counted.

While a source is still being captured the total advances every frame. Anchored
to that total, the viewport asks each frame for rows that have never been
requested; by the time they arrive the tail has moved again. The pane drew
nothing while the status line counted hundreds of thousands of rows, and the
one-row lookup at the very tail never resolved either, so FOLLOW had no
selected record and every record-scoped operation had nothing to act on.

Three things are asserted here, all while capture runs and the file grows:
rows are drawn on every frame the view claims to have any; Fields opens onto a
record; and the range in the status line is the range on screen.
"""

import json
import pathlib
import sys
import tempfile
import threading
import time

from test_lvu_pty import PtyApp, isolated_environment

# Large enough that capture is still running throughout the sample window,
# which is the condition the defect needs: a total advancing under the
# viewport every frame. W24's reproduction used a source of this size.
SEED_RECORDS = 620_000


def seed(path: pathlib.Path) -> None:
    with path.open("w", encoding="utf-8") as out:
        for index in range(SEED_RECORDS):
            out.write(
                '{"level":"INFO","service":"api","seq":%d,"message":"handled request"}\n'
                % index
            )


def append(path: pathlib.Path, stop: threading.Event, start: int) -> None:
    """Keep the file growing, so the tail keeps moving under the viewport."""
    index = start
    with path.open("a", encoding="utf-8") as out:
        while not stop.is_set():
            out.write(
                '{"level":"INFO","service":"chatty","seq":%d,"message":"handled request"}\n'
                % index
            )
            out.flush()
            index += 1
            time.sleep(0.05)


def viewport_rows(text: str) -> list[str]:
    """The log pane's own drawn rows, without its border or column header."""
    lines = text.splitlines()
    top = next((i for i, line in enumerate(lines) if "Log viewport" in line), None)
    if top is None:
        return []
    left = lines[top].rindex("┌", 0, lines[top].index("Log viewport"))
    rows = []
    for line in lines[top + 1:]:
        cell = line[left:]
        if "┘" in cell or cell.startswith("└"):
            break
        rows.append(cell.strip("│").strip())
    if rows and rows[0].split()[:2] == ["time", "level"]:
        rows = rows[1:]
    # "No rows in this view." is the pane saying it has nothing, not a row.
    return [r for r in rows if r and "No rows in this view" not in r]


def counted(text: str):
    """The `first-last/total` the status line is advertising."""
    for part in text.splitlines()[-1].split("|"):
        piece = part.strip()
        if "/" in piece and piece[0].isdigit() and "-" in piece.split("/")[0]:
            span, total = piece.split("/", 1)
            first, last = span.split("-", 1)
            try:
                return int(first), int(last), int(total)
            except ValueError:
                return None
    return None


def run(binary: pathlib.Path) -> None:
    with tempfile.TemporaryDirectory(prefix="lvu-follow-anchor-") as directory:
        root = pathlib.Path(directory)
        source = root / "grow.log"
        seed(source)
        app = PtyApp(
            binary,
            ["--capture-dir", str(root / "capture"), "--file", str(source)],
            width=80,
            height=24,
            environment=isolated_environment(root),
        )
        stop = threading.Event()
        writer = None
        try:
            app.wait_until(lambda text: counted(text) is not None
                           and counted(text)[2] > 0,
                           "the view counting rows", timeout=300)
            app.send(b"G")
            app.wait_until(lambda text: "FOLLOW" in text, "following", timeout=30)
            writer = threading.Thread(
                target=append, args=(source, stop, 9_000_000), daemon=True
            )
            writer.start()
            time.sleep(2.0)

            blank = []
            mismatched = []
            deadline = time.monotonic() + 60
            while time.monotonic() < deadline:
                app.drain()
                text = app.text()
                count = counted(text)
                rows = viewport_rows(text)
                if count is None:
                    time.sleep(0.2)
                    continue
                first, last, total = count
                # (1) A view that says it has rows draws rows.
                if total > 0 and not rows:
                    blank.append(text)
                # (3) And the range it advertises is the range it drew.
                elif total > 0 and last - first + 1 != len(rows):
                    mismatched.append((first, last, len(rows), text))
                time.sleep(0.2)

            assert not blank, (
                f"{len(blank)} frame(s) drew no rows while the status line counted "
                f"them; first was:\n{blank[0]}"
            )
            assert not mismatched, (
                f"{len(mismatched)} frame(s) advertised a range they did not draw; "
                f"first was {mismatched[0][:3]}:\n{mismatched[0][3]}"
            )

            # (2) FOLLOW has a selected record, so a record-scoped operation has
            # something to act on. Fields names the record it opened onto.
            for attempt in range(6):
                app.send(b"i")
                opened = app.wait_for("Fields", timeout=20)
                assert "Fields · record" in opened, (
                    f"attempt {attempt}: Fields opened without a record while "
                    f"following a full viewport:\n{opened}"
                )
                app.send(b"\x1b")
                app.wait_until(lambda text: "Fields" not in text, "fields closed",
                               timeout=20)
                time.sleep(0.5)
        finally:
            stop.set()
            if writer is not None:
                writer.join(timeout=3)
            (root / "screen.txt").write_text(app.text())
            if app.process.poll() is None:
                app.process.kill()
                app.process.wait(timeout=5)
            app.close()


if __name__ == "__main__":
    run(pathlib.Path(sys.argv[1]).resolve())
    print(
        "FOLLOW anchor PTY passed: rows drawn on every counted frame, a record "
        "selected for Fields, and the status range matching the pane"
    )
