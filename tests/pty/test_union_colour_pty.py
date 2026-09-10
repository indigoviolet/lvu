#!/usr/bin/env python3
"""Accepted enrichment colour authority across a heterogeneous live union.

One input contributes an accepted derived ``severity`` column; the other has
only a raw JSON field with the same name. Two ordered rules prove that the
native union classifier paints only the authoritative derived row, that first
match wins, and that the accepted order and colours survive restart.
"""

import pathlib
import re
import sys
import tempfile
import time

from test_color_rules_pty import settings_at
from test_enrichment_chain_pty import close_editor, open_step_editor, paste, stop
from test_lvu_pty import PtyApp
from test_shared_palette_colors_pty import find_text
from test_union_pty import open_union_dialog, wait_closed


API_DERIVED = "api-derived-1"
WORKER_RAW = "worker-raw-1"


def switch_to(app: PtyApp, marker: str, active_name: str, attempts: int = 8) -> str:
    for _ in range(attempts):
        app.drain()
        text = app.text()
        if marker in text and f"› {active_name}" in text:
            return text
        app.send(b"]")
        time.sleep(0.12)
    raise AssertionError((f"active {active_name!r} view containing {marker!r} not found", app.text()))


def add_severity_enrichment(app: PtyApp) -> None:
    open_step_editor(app)
    paste(app, "severity = pl.lit('critical')")
    app.send(b"\r")
    app.wait_until(
        lambda text: "enrich:on" in text and "External command" in text,
        "accepted severity enrichment",
        timeout=20,
    )
    close_editor(app)


def gutter_style(app: PtyApp, marker: str) -> tuple[str, str, bool]:
    """A non-event cell carrying the row style, away from JSON token colours."""
    _, row = find_text(app, marker)
    line = app.text().splitlines()[row]
    timestamp = re.search(r"\d\d:\d\d:\d\d(?:\.\d{3})?Z", line)
    assert timestamp is not None, (marker, line, app.text())
    cell = app.screen.buffer[row][timestamp.start()]
    return cell.fg, cell.bg, cell.bold


def swatch_foreground(app: PtyApp, summary: str, occurrence: int) -> str:
    matches = []
    for row, line in enumerate(app.text().splitlines()):
        if summary in line and "██" in line:
            matches.append(app.screen.buffer[row][line.index("██")].fg)
    if len(matches) > occurrence:
        return matches[occurrence]
    raise AssertionError(
        (f"rule swatch {occurrence} for {summary!r} not visible", app.text())
    )


def choose_heterogeneous_inputs(app: PtyApp) -> None:
    open_union_dialog(app)
    # The active API Enriched view is preselected. In sidebar order the next
    # candidate is the worker's raw All events view.
    app.send(b"\x1b[B")
    app.send(b" ")
    chosen = app.wait_until(
        lambda text: "2 of 3 views selected" in text,
        "accepted API Enriched plus worker raw input selected",
    )
    selected = [line for line in chosen.splitlines() if "[x]" in line]
    assert len(selected) == 2, chosen
    assert any("Enriched" in line for line in selected), chosen
    assert any("All events" in line for line in selected), chosen
    app.send(b"\r")
    wait_closed(app, "┌ Union views", "heterogeneous union created", timeout=25)


def add_ordered_rules(app: PtyApp) -> tuple[str, str]:
    app.send(b"c")
    app.wait_for("Colour rules")

    # An accepted output makes Add enter the native classifier controls, not
    # the legacy raw-predicate editor. Both rules deliberately overlap.
    app.send(b"\x1ba")
    classifier = app.wait_until(
        lambda text: "Column" in text
        and "Value" in text
        and "‹ severity ›" in text,
        "new union rule binds the accepted severity output",
        timeout=4,
    )
    assert "Predicate" not in classifier, classifier
    app.send(b"critical")
    app.wait_for("severity = critical")

    # Value -> Colour -> Add. New rules advance to the next palette colour,
    # so the second overlapping classifier is observably lower precedence.
    app.send(b"\t\t")
    app.send(b"\r")
    second_classifier = app.wait_until(
        lambda text: "Column" in text
        and "Value" in text
        and "‹ severity ›" in text,
        "second rule also binds the accepted severity output",
        timeout=4,
    )
    assert "Predicate" not in second_classifier, second_classifier
    app.send(b"critical")
    app.settle()
    summary = "severity = critical"
    assert app.text().count(summary) == 2, app.text()
    first = swatch_foreground(app, summary, 0)
    second = swatch_foreground(app, summary, 1)
    assert first != second, (first, second, app.text())

    # Enter in Value applies the complete ordered draft.
    app.send(b"\r")
    app.wait_until(
        lambda text: "2 rules painting this view" in text,
        "ordered union colour rules accepted",
        timeout=20,
    )
    app.send(b"\x1b")
    wait_closed(app, "┌ Colour rules", "colour rules closed")
    return first, second


def assert_persisted_rule_order(app: PtyApp, first: str, second: str) -> None:
    app.send(b"c")
    summary = "severity = critical"
    rules = app.wait_until(
        lambda text: "2 rules painting this view" in text
        and text.count(summary) == 2
        and "Column" in text
        and "Value" in text
        and "‹ severity ›" in text,
        "accepted ordered rules restored",
        timeout=15,
    )
    assert "Predicate" not in rules, rules
    assert swatch_foreground(app, summary, 0) == first, rules
    assert swatch_foreground(app, summary, 1) == second, rules
    app.send(b"\x1b")
    wait_closed(app, "┌ Colour rules", "restored colour rules closed")


def run(binary: pathlib.Path) -> None:
    with tempfile.TemporaryDirectory(prefix="lvu-union-colour-pty-") as directory:
        root = pathlib.Path(directory)
        api = root / "api.log"
        worker = root / "worker.log"
        api.write_text(
            '{"ts":"2026-03-04T05:06:00Z","svc":"api","msg":"api-derived-0"}\n'
            f'{{"ts":"2026-03-04T05:06:02Z","svc":"api","msg":"{API_DERIVED}"}}\n'
        )
        worker.write_text(
            '{"ts":"2026-03-04T05:06:01Z","svc":"worker","severity":"critical","msg":"worker-raw-0"}\n'
            f'{{"ts":"2026-03-04T05:06:03Z","svc":"worker","severity":"critical","msg":"{WORKER_RAW}"}}\n'
        )
        env = settings_at(root)
        args = [str(api), str(worker), "--capture-dir", str(root / "capture")]
        app = PtyApp(binary, args, width=150, height=32, cwd=root, environment=env)
        try:
            switch_to(app, API_DERIVED, "All events")
            add_severity_enrichment(app)
            switch_to(app, API_DERIVED, "Enriched")
            choose_heterogeneous_inputs(app)
            union = app.wait_until(
                lambda text: "› Union of" in text
                and API_DERIVED in text
                and WORKER_RAW in text,
                "heterogeneous union published",
                timeout=25,
            )
            assert '"severity":"critical"' in union, union

            # Keep the tested rows out of selection styling.
            app.send(b"g")
            app.settle()
            api_before = gutter_style(app, API_DERIVED)
            worker_before = gutter_style(app, WORKER_RAW)
            first, second = add_ordered_rules(app)
            app.wait_until(
                lambda text: API_DERIVED in text and WORKER_RAW in text,
                "painted heterogeneous union served",
                timeout=20,
            )
            api_after = gutter_style(app, API_DERIVED)
            worker_after = gutter_style(app, WORKER_RAW)
            assert api_after[0] == first, (api_before, api_after, first, app.text())
            assert api_after != api_before, (api_before, api_after, app.text())
            assert api_after[0] != second, (
                "the later overlapping classifier overrode first-match ordering",
                api_after,
                first,
                second,
                app.text(),
            )
            assert worker_after == worker_before, (
                "the raw same-named field forged derived colour authority",
                worker_before,
                worker_after,
                app.text(),
            )

            stop(app)
            app = PtyApp(binary, args, width=150, height=32, cwd=root, environment=env)
            switch_to(app, API_DERIVED, "Union of")
            app.wait_until(
                lambda text: API_DERIVED in text
                and WORKER_RAW in text
                and gutter_style(app, API_DERIVED)[0] == first
                and gutter_style(app, WORKER_RAW) == worker_before,
                "heterogeneous union colours restored",
                timeout=25,
            )
            assert_persisted_rule_order(app, first, second)
            stop(app)
        finally:
            if app.process.poll() is None:
                if sys.exc_info()[0] is None:
                    stop(app)
                else:
                    app.process.kill()
                    app.process.wait(timeout=5)
                    app.close()


if __name__ == "__main__":
    run(pathlib.Path(sys.argv[1]).resolve())
    print(
        "Union colour PTY passed: derived authority, raw namesake exclusion, "
        "first-match ordering and restart persistence"
    )
