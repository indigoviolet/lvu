#!/usr/bin/env python3
"""Restored filtered views must never report matches over an empty log pane.

The reported defect was a restarted view whose status said a record matched
while the log pane stayed blank. Query membership and the raw row cache are
separate paths: after a restart nothing is cached, every visible row has to be
fetched through the live provider's bounded request queue, and requests that are
dropped there used to leave a permanently blank pane behind a healthy status.

This exercises the real binary across restart with a restored accepted filter,
with a viewport far taller than one frame's request budget, with a single
matched record, and with a genuinely empty result.
"""
from __future__ import annotations

import pathlib
import sys
import time

from test_lvu_pty import PtyApp, isolated_environment, scratch_directory

WIDTH = 120
HEIGHT = 40


def start(binary: pathlib.Path, root: pathlib.Path, capture: pathlib.Path,
          source: pathlib.Path) -> PtyApp:
    return PtyApp(binary, ["--capture-dir", str(capture), "--file", str(source)],
                  width=WIDTH, height=HEIGHT,
                  environment={**isolated_environment(root), "NO_COLOR": "",
                               "COLORTERM": "truecolor"})


def quit_cleanly(app: PtyApp) -> None:
    app.send(b"q")
    assert app.wait_exit(timeout=10.0) == 0
    app.assert_restored()


def apply_search(app: PtyApp, literal: str) -> None:
    app.send(b"/")
    app.wait_for("Search", timeout=6.0)
    # Replace, never extend, any restored draft.
    app.send(b"\x01\x0b")
    app.send(b"\x1b[200~" + literal.encode() + b"\x1b[201~")
    app.send(b"\r")
    app.wait_until(lambda text: f'search:"{literal}"' in text and "matched " in text,
                   f"accepted literal {literal!r}", timeout=15.0)
    app.send(b"\x1b")
    app.wait_until(lambda text: "Log viewport" in text and "Examples:" not in text,
                   "search editor closed", timeout=6.0)


def matched_count(text: str) -> int | None:
    """The matched record count the status line is currently advertising."""
    for line in text.splitlines():
        marker = "matched "
        if marker in line:
            tail = line.split(marker, 1)[1]
            digits = "".join(iter_leading_digits(tail))
            if digits:
                return int(digits)
    return None


def iter_leading_digits(value: str):
    for character in value:
        if not character.isdigit():
            return
        yield character


def assert_matches_are_displayed(app: PtyApp, marker: str, expected: int, description: str,
                                 timeout: float = 25.0) -> None:
    """A satisfied membership must converge to displayed rows.

    It also must never rest on an advertised match count with nothing rendered,
    so the settled screen is re-checked after convergence.
    """
    app.wait_until(lambda text: matched_count(text) == expected and marker in text,
                   description, timeout=timeout)
    deadline = time.monotonic() + 3.0
    while time.monotonic() < deadline:
        app.drain()
        text = app.text()
        count = matched_count(text)
        if count is not None and count > 0:
            assert marker in text, (
                f"{description}: status advertised {count} matched records over a pane with no "
                f"visible row\n{text}")
        time.sleep(0.1)


def run(binary: pathlib.Path) -> None:
    with scratch_directory() as root:
        capture = root / "capture"
        source = root / "restored.log"
        # More matched records than a single frame's bounded row-request budget,
        # so the restored pane can only fill if requests keep being reissued.
        rows = [f"keep record {index:03d}" for index in range(120)]
        rows += [f"drop record {index:03d}" for index in range(80)]
        rows.append("solitary beacon record")
        source.write_text("\n".join(rows) + "\n")

        first = start(binary, root, capture, source)
        try:
            # The viewport follows the tail, so assert against the last record.
            first.wait_for("solitary beacon record", timeout=15.0)
            apply_search(first, "keep record")
            assert_matches_are_displayed(first, "keep record 119", 120,
                                         "filter applied before restart")
            quit_cleanly(first)
        finally:
            if first.process.poll() is None:
                first.process.kill()
            first.close()

        # Restart: nothing is cached, the index is being reopened, and every
        # visible row must be fetched again behind the restored membership.
        reopened = start(binary, root, capture, source)
        try:
            reopened.wait_until(lambda text: 'search:"keep record"' in text,
                                "restored accepted filter", timeout=20.0)
            assert_matches_are_displayed(reopened, "keep record 119", 120,
                                         "restored filtered view after restart")
            assert "drop record" not in reopened.text(), "restored filter must still exclude rows"

            # The originally reported shape: exactly one matched record.
            apply_search(reopened, "solitary beacon")
            assert_matches_are_displayed(reopened, "solitary beacon record", 1,
                                         "single matched record")

            # A genuinely empty result must stay distinguishable from the above.
            apply_search(reopened, "no such record anywhere")
            reopened.wait_until(lambda text: matched_count(text) == 0
                                and "No matches" in text,
                                "genuinely zero matches stays explained", timeout=20.0)
            assert "solitary beacon record" not in reopened.text()
            quit_cleanly(reopened)
        finally:
            if reopened.process.poll() is None:
                reopened.process.kill()
            reopened.close()

        # A second restart restores the zero-match filter. It must still report
        # zero rather than looking identical to an unfetched pane.
        final = start(binary, root, capture, source)
        try:
            final.wait_until(lambda text: 'search:"no such record anywhere"' in text
                             and matched_count(text) == 0,
                             "restored zero-match filter", timeout=20.0)
            apply_search(final, "keep record 04")
            assert_matches_are_displayed(final, "keep record 049", 10,
                                         "narrowed filter after a restored empty view")
            quit_cleanly(final)
        finally:
            if final.process.poll() is None:
                final.process.kill()
            final.close()

    print("blank restored view PTY checks passed")


if __name__ == "__main__":
    run(pathlib.Path(sys.argv[1]).resolve())
