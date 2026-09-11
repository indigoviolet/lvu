#!/usr/bin/env python3
"""Missing-column filter keeps applied rows and editable draft with per-batch diagnostic."""

import os
import pathlib
import sys
import tempfile
import time
import traceback

from test_lvu_pty import FILTER_TITLE, PtyApp, open_advanced_filter
from test_enrichment_chain_pty import paste, stop


MISSING = "pl.col('never_produced_xyz') == 'ERROR'"
VALID = "pl.col('level') == 'ERROR'"
FAILURE_LOGDIR = pathlib.Path(
    "/mnt/HC_Volume_106796581/lvu-build/lvu-muse-enrichment-logs"
)


def clear_field(app):
    app.send(b"\x01\x0b")


def diagnostic_shown(text):
    # The diagnostics pane wraps the sentence across rows, and each row is
    # padded and bordered, so the full sentence never appears contiguously on
    # screen (not even after whitespace collapsing: border cells sit between
    # rows). Match token pieces instead, each contiguous within one row.
    return (
        "available in this" in text
        and "batch" in text
        and "never_produced_xyz" in text
        and "filter needs" in text
    )


def run(binary):
    home = pathlib.Path.home()
    tooling = {}
    for key, xdg, fallback, suffix in [
        ("MISE_DATA_DIR", "XDG_DATA_HOME", home / ".local/share", "mise"),
        ("MISE_CONFIG_DIR", "XDG_CONFIG_HOME", home / ".config", "mise"),
        ("MISE_CACHE_DIR", "XDG_CACHE_HOME", home / ".cache", "mise"),
        ("UV_CACHE_DIR", "XDG_CACHE_HOME", home / ".cache", "uv"),
    ]:
        tooling[key] = os.environ.get(
            key, str(pathlib.Path(os.environ.get(xdg, fallback)) / suffix)
        )
    with tempfile.TemporaryDirectory(prefix="lvu-missing-col-pty-") as directory:
        root = pathlib.Path(directory)
        source = root / "events.log"
        source.write_text("level=ERROR first\nlevel=INFO second\n")
        environment = {
            **tooling,
            "XDG_CONFIG_HOME": str(root / "config"),
            "XDG_DATA_HOME": str(root / "data"),
            "XDG_CACHE_HOME": str(root / "cache"),
        }
        arguments = [str(source), "--capture-dir", str(root / "capture")]
        app = PtyApp(binary, arguments, width=120, height=30, environment=environment)
        try:
            app.wait_for("level=ERROR first", timeout=15.0)

            # Valid advanced filter first: only the ERROR row remains. The
            # open dialog covers the matching row, so accepted status is
            # awaited first, the dialog is closed, and only then are rows
            # asserted.
            open_advanced_filter(app)
            clear_field(app)
            paste(app, VALID)
            app.send(b"\r")
            app.wait_until(
                lambda text: "Applied" in text and "matched 1/2" in text,
                "valid filter accepted",
                timeout=15,
            )
            app.send(b"\x1b")
            app.wait_until(
                lambda text: FILTER_TITLE not in text, "filter closed", timeout=5
            )
            app.wait_until(
                lambda text: "level=INFO second" not in text
                and "level=ERROR first" in text,
                "valid filter rows after close",
                timeout=15,
            )

            # Candidate names a column absent from this batch. The applied rows
            # must stay, the draft must stay editable, and the message must be
            # per batch without Polars implementation detail.
            open_advanced_filter(app)
            clear_field(app)
            paste(app, MISSING)
            app.send(b"\r")
            failed = app.wait_until(
                diagnostic_shown,
                "actionable per-batch diagnostic",
                timeout=20,
            )
            assert "never_produced_xyz" in failed, failed
            assert "lowered" not in failed, failed
            assert "unable to find" not in failed, failed
            # The open rejected dialog covers the viewport's single applied
            # row, so survival is read from the dialog's own last-accepted
            # line (stable regardless of footer chip pressure); the exact
            # row text is asserted after dismissal below.
            assert "last accepted pl.col('level') == 'ERROR'" in failed, (
                "applied rows stay while the candidate fails",
                failed,
            )
            assert FILTER_TITLE in failed and "never_produced_xyz" in failed, (
                "rejected draft stays visible",
                failed,
            )

            # Real edit check: keystrokes must reach the rejected draft, not
            # just render it. Append a character, see it land, remove it.
            app.send(b"X")
            app.wait_until(
                lambda text: "ERROR'X" in text,
                "rejected draft accepts edits",
                timeout=5,
            )
            app.send(b"\x7f")
            app.wait_until(
                lambda text: "ERROR'X" not in text
                and "never_produced_xyz" in text,
                "rejected draft edit removal works",
                timeout=5,
            )

            app.send(b"\x1b")
            app.wait_until(
                lambda text: FILTER_TITLE not in text, "filter closed", timeout=5
            )
            app.wait_until(
                lambda text: "level=ERROR first" in text
                and "level=INFO second" not in text,
                "applied view preserved after dismissal",
                timeout=10,
            )

            # Live arrivals still refresh through the applied chain.
            with open(source, "a") as handle:
                handle.write("level=ERROR late\n")
                handle.flush()
            app.wait_until(
                lambda text: "level=ERROR late" in text,
                "live append through applied chain",
                timeout=15,
            )

            print(
                "Missing-column PTY passed: actionable diagnostic, applied rows, "
                "editable draft and live append"
            )
        except BaseException as original:
            # Preserve the original failure and its artifacts before any
            # cleanup. Cleanup must kill without masking the cause: sending q
            # into a still-open editor appends to the draft and raises a second
            # error that hides this one.
            try:
                FAILURE_LOGDIR.mkdir(parents=True, exist_ok=True)
                stamp = f"{time.time_ns()}"
                (FAILURE_LOGDIR / f"pty-missing-column-screen-{stamp}.txt").write_text(
                    app.text()
                )
                (FAILURE_LOGDIR / f"pty-missing-column-transcript-{stamp}.txt").write_bytes(
                    bytes(app.transcript[-8000:])
                )
                (FAILURE_LOGDIR / f"pty-missing-column-error-{stamp}.txt").write_text(
                    "".join(traceback.format_exception(original))
                )
            except Exception:
                pass
            try:
                app.process.kill()
            except Exception:
                pass
            try:
                app.close()
            except Exception:
                pass
            raise

        # Successful quit path: the editor is deliberately closed first so q
        # quits the app instead of landing in the draft.
        if FILTER_TITLE in app.text():
            app.send(b"\x1b")
            app.wait_until(
                lambda text: FILTER_TITLE not in text, "filter closed", timeout=5
            )
        stop(app)


if __name__ == "__main__":
    run(pathlib.Path(sys.argv[1]))
