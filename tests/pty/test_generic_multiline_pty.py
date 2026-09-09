#!/usr/bin/env python3
"""Auto grouping presents messy multiline logs without changing their records."""
import os
import pathlib
import sys
import tempfile
import time

from test_folding_pty import open_palette
from test_lvu_pty import PtyApp

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
    print("Generic multiline PTY passed: raw first, Auto groups conservatively, z folds projected groups")


if __name__ == "__main__":
    run(pathlib.Path(sys.argv[1]).resolve())
