#!/usr/bin/env python3
"""Saving an enrichment step on a source large enough for the query to settle
asynchronously.

The regression this covers only appears when more than one query is in flight:
on a ten-row fixture the step's own completion is the only one, so nothing can
consume the fence that tells the shell a step was accepted. On a real source
the first Save updated the chain but reported nothing and left the editor open,
and a second Save — the obvious thing to do when nothing happened — resubmitted
the same draft and produced `duplicate enrichment output field`.

What is asserted is the four behaviours a save must have:
  (a) immediate feedback in the editor, and no second submission while one is
      outstanding;
  (b) acceptance closes the child and selects the new step in the list;
  (c) rejection keeps the editor and its error;
  (d) pressing Save twice on one draft can never produce the duplicate error.
"""

from __future__ import annotations

import json
import pathlib
import sys
import tempfile

from test_lvu_pty import PtyApp

# Large enough that the initial query is still settling when the first Save is
# pressed, which is the whole point: the defect is invisible on a small source.
ROWS = 120_000


def write_source(path: pathlib.Path) -> None:
    with path.open("w") as stream:
        for index in range(ROWS):
            stream.write(
                json.dumps(
                    {
                        "time": f"2026-03-04T05:{index // 60 % 60:02}:{index % 60:02}Z",
                        "level": "INFO" if index % 5 else "WARN",
                        "msg": f"record {index}",
                    }
                )
                + "\n"
            )


def launch(
    binary: pathlib.Path, root: pathlib.Path, log: pathlib.Path, width: int, height: int
) -> PtyApp:
    return PtyApp(
        binary,
        ["--capture-dir", str(root / "capture"), "--file", str(log)],
        width=width,
        height=height,
        cwd=root,
        environment={
            "LVU_NO_DELIGHT": "1",
            "XDG_CONFIG_HOME": str(root / "config"),
            "XDG_DATA_HOME": str(root / "data"),
            "XDG_CACHE_HOME": str(root / "cache"),
        },
    )


def paste(app: PtyApp, text: str) -> None:
    app.send(b"\x1b[200~" + text.encode() + b"\x1b[201~")


def open_new_step(app: PtyApp) -> None:
    app.send(b"e")
    app.wait_for("Steps", timeout=15)
    app.send(b"\x1ba")
    app.wait_for("Enrichment › New step", timeout=10)


def story(
    binary: pathlib.Path, root: pathlib.Path, log: pathlib.Path, width: int, height: int
) -> None:
    app = launch(binary, root, log, width, height)
    try:
        # The message text is off the right edge at these widths; the level
        # column is the first thing every ingested row puts on screen.
        app.wait_for("INFO", timeout=60)
        open_new_step(app)
        paste(app, "name4 = pl.col('time')")
        app.send(b"\r")

        # (a) The editor says something at once. Silence is what made a user
        # press Save again.
        pending = app.wait_until(
            lambda text: "Evaluating" in text,
            "the editor reports that the save is in flight",
            timeout=10,
        )
        assert "Enrichment › New step" in pending, pending

        # (a)/(d) A second Save while one is outstanding does nothing at all:
        # it must not resubmit the same draft, which is what produced the
        # duplicate error.
        app.send(b"\r")
        app.send(b"\r")

        # (b) Acceptance closes the child and the list shows the new step.
        listed = app.wait_until(
            lambda text: "Enrichment › New step" not in text and "Steps" in text,
            "the accepted step returns the user to the list",
            timeout=30,
        )
        assert "name4" in listed, listed
        assert "1 steps active" in listed or "steps active" in listed, listed
        # (d) The duplicate never appeared.
        assert "duplicate enrichment output field" not in listed, listed

        # (c) A rejected step keeps its editor and shows why.
        app.send(b"\x1ba")
        app.wait_for("Enrichment › New step", timeout=10)
        paste(app, "name4 = pl.col('time')")
        app.send(b"\r")
        rejected = app.wait_until(
            lambda text: "duplicate enrichment output field" in text,
            "the duplicate is reported when it is really a duplicate",
            timeout=30,
        )
        assert "Enrichment › New step" in rejected, rejected
        # The accepted chain survived the rejection: leaving the editor lands
        # back on the list with the one accepted step still in it, and the
        # list carries the reason the second one was refused.
        app.send(b"\x1b")
        kept = app.wait_until(
            lambda text: "Steps" in text and "name4" in text and "1 of 1" in text,
            "the accepted step is still there",
            timeout=10,
        )
        assert "duplicate enrichment output field" in kept, kept
        app.send(b"\x1b")
        app.wait_until(lambda text: "Steps" not in text, "enrichment closed", timeout=10)
    except BaseException:
        # The story's own failure is the interesting one; do not let a
        # shutdown assertion in the exit path hide it.
        app.process.kill()
        raise
    finally:
        if app.process.poll() is None:
            app.send(b"q")
            code = app.wait_exit(timeout=15)
            assert code == 0, (code, app.text())
        app.assert_restored()


def run(binary: pathlib.Path) -> None:
    log = pathlib.Path(tempfile.mkdtemp(prefix="lvu-enrich-save-source-")) / "events.log"
    write_source(log)
    # One state root per size: the first story creates a derived view and
    # persists it, and a second story that inherited it would not be starting
    # from the state the report describes.
    for width, height in [(80, 24), (54, 16)]:
        root = pathlib.Path(tempfile.mkdtemp(prefix=f"lvu-enrich-save-{width}x{height}-"))
        story(binary, root, log, width, height)


def main() -> None:
    if len(sys.argv) != 2:
        raise SystemExit("usage: test_enrichment_save_feedback_pty.py <lvu-app>")
    run(pathlib.Path(sys.argv[1]).resolve())
    print("enrichment save feedback pty ok")


if __name__ == "__main__":
    main()
