#!/usr/bin/env python3
"""Auto grouping presents messy multiline logs without changing their records."""
import os
import pathlib
import sys
import tempfile
import time

from test_enrichment_chain_pty import paste
from test_lvu_pty import PtyApp


def add_is_start_column(app) -> None:
    """One ordinary enrichment step marking true log lines, added the
    ordinary way. A match produces a value (an event start); anything else
    stays null and continues the open event."""
    app.send(b"e")
    app.wait_for("Steps")
    app.send(b"\x1ba")
    app.wait_for("Enrichment \u203a New step")
    paste(app, "/^(?P<is_start>ERROR)/")
    app.send(b"\r")
    app.wait_until(lambda text: "is_start" in text and "Applied" in text,
                   "the start column is applied", timeout=20)
    app.send(b"\x1b")
    app.wait_until(lambda text: "Steps" not in text, "enrichment closed")


def open_filter_tab(app) -> None:
    """Grouping opens on Run; the Filter tab carries the start rule. Tab
    reaches the tab control, Right selects Filter, and two more Tabs return
    the keys to the column slot."""
    app.send(b"\t")
    app.send(b"\x1b[C")
    app.wait_for("(?lvu:filter:")
    app.send(b"\t")
    app.send(b"\t")

# Bounded failure evidence, failure-only. The wait_until AssertionError already
# carries the pyte screen plus a 4 KiB transcript tail, but the matrix summary
# prints only its last Error/assert line, so a matrix failure arrives with no
# screen to read. Mirror the fold suite's `screen-*.txt` / `terminal-*.ansi`
# pair into a caller-provided durable directory outside this fixture's
# TemporaryDirectory (deleted on exit). Success writes nothing. Contents are
# fixture-only: this suite's synthetic events.log plus app chrome; no user
# capture data is read or written here.
_TRANSCRIPT_TAIL_BYTES = 65536


def _artifact_dir() -> pathlib.Path | None:
    raw = os.environ.get("LVU_PTY_ARTIFACT_DIR")
    if not raw:
        return None
    try:
        path = pathlib.Path(raw)
        path.mkdir(parents=True, exist_ok=True)
        return path if path.is_dir() else None
    except OSError:
        return None


def _save_failure_artifacts(app: PtyApp | None, error: BaseException) -> list[str]:
    where = _artifact_dir()
    if where is None:
        return []
    stamp = f"{int(time.time())}-{os.getpid()}"
    try:
        screen = app.text() if app is not None else "<no app>"
    except Exception as exc:  # never mask the original failure
        screen = f"<screen unavailable: {exc!r}>"
    try:
        tail = bytes(app.transcript[-_TRANSCRIPT_TAIL_BYTES:]) if app is not None else b""
    except Exception:
        tail = b""
    saved: list[str] = []
    try:
        error_path = where / f"test_generic_multiline_pty-{stamp}-error.txt"
        error_path.write_text(f"{error!r}\n", encoding="utf-8")
        saved.append(str(error_path))
        screen_path = where / f"test_generic_multiline_pty-{stamp}-screen.txt"
        screen_path.write_text(screen, encoding="utf-8")
        saved.append(str(screen_path))
        transcript_path = where / f"test_generic_multiline_pty-{stamp}-transcript.ansi"
        transcript_path.write_bytes(tail)
        saved.append(str(transcript_path))
    except OSError:
        return saved
    print(f"generic-multiline failure artifacts: {', '.join(saved)}", file=sys.stderr)
    return saved


def run(binary: pathlib.Path) -> None:
    with tempfile.TemporaryDirectory(prefix="lvu-generic-multiline-") as directory:
        root = pathlib.Path(directory)
        source = root / "events.log"
        # Under Filter grouping every non-null enrichment value opens an
        # event and everything until the next one continues — including the
        # flat interlude no lexical classifier would join. Only the leading
        # prologue precedes every start and stays a standalone orphan.
        source.write_text(
            "stray prologue\n"
            "ERROR boom\n  at worker.py:7\n"
            "flat interlude\n"
            "ERROR boom\n  at worker.py:7\n"
            "ERROR boom\n  at worker.py:7\n",
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
            raw = app.wait_for("flat interlude", timeout=12)
            assert "at worker.py:7" in raw and "physical lines" not in raw, raw

            # Recognition lives in Enrichment; Grouping only selects its
            # output column. Pasting into the Filter slot names the column in
            # place, keeping a well-formed rule.
            add_is_start_column(app)
            app.send(b"m")
            app.wait_for("Multiline grouping")
            open_filter_tab(app)
            paste(app, "is_start")
            app.wait_for("(?lvu:filter:v1:column:is_start)")
            app.send(b"\r")
            app.wait_until(lambda text: "Applied" in text and "Filter" in text,
                           "filter grouping applied", timeout=15)
            app.send(b"\x1b")
            grouped = app.wait_until(
                lambda text: "Multiline grouping" not in text
                and text.count("2 physical lines") == 2
                and "3 physical lines" in text,
                "grouped base viewport",
            )
            assert "stray prologue" in grouped, grouped
            assert "orphan continuation" in grouped, grouped
            assert "flat interlude" not in grouped, grouped
            app.send(b"q")
            assert app.wait_exit(timeout=8) == 0
            app.assert_restored()
        except BaseException as error:
            try:
                _save_failure_artifacts(app, error)
            except Exception:
                pass
            raise
        finally:
            if app.process.poll() is None:
                app.process.kill()
                app.process.wait()
            app.close()
    print("Generic multiline PTY passed: raw first, Filter groups on the enrichment column")


if __name__ == "__main__":
    run(pathlib.Path(sys.argv[1]).resolve())
