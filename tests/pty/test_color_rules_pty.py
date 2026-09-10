#!/usr/bin/env python3
"""Colour rules and regex span highlighting in a real terminal.

Three claims, all end to end against a real source and the real query engine:

* a rule paints the rows its predicate matched, and only those, without
  narrowing the view — the row count is the same before and after;
* a column rule classifies an enrichment output's exact value: on the
  cleared search fork, a slash shorthand derives `severity` and the rule
  paints exactly its rows without narrowing the view, while the legacy rule
  keeps its own;
* a search or rule pattern is underlined *where it matched inside the line*,
  including on a line whose invalid bytes render as replacement characters.

Run at 80x24 and again at 54x16, because a rule list, a predicate field and a
colour chooser all have to survive the narrow terminal. The column story
runs wide only: the enrichment editor plus chooser need the room.
"""
import pathlib
import sys
import tempfile

from test_lvu_pty import PtyApp
from test_enrichment_chain_pty import close_editor
from test_shared_palette_colors_pty import find_text


def settings_at(root: pathlib.Path) -> dict[str, str]:
    directory = root / "config" / "lvu"
    directory.mkdir(parents=True)
    (directory / "settings.toml").write_text(
        "schema_version = 1\n"
        "\n[paseo]\n"
        'provider = "fixture/provider"\n'
        'mode = "full-access"\n'
        'thinking = "medium"\n'
        "\n[appearance]\n"
        'theme = "love-dark"\n'
        "delight_enabled = false\n"
        "reduced_motion = true\n"
        "ascii = false\n"
        "\n[cache.memory]\nrows_mib = 4\nmembership_mib = 256\n"
        "\n[cache.disk]\ntotal_mib = 5120\nindex_per_source_mib = 256\n"
        "\n[storage]\nreserve_mib = 256\n"
        "\n[storage.retention]\n"
        "enabled = false\n"
        "maximum_total_capture_mib = 0\n"
        "maximum_age_days = 0\n"
    )
    return {
        "XDG_CONFIG_HOME": str(root / "config"),
        "XDG_DATA_HOME": str(root / "data"),
        "XDG_CACHE_HOME": str(root / "cache"),
        "LVU_NO_DELIGHT": "1",
        "NO_COLOR": "",
        "COLORTERM": "truecolor",
    }


def row_of(app: PtyApp, needle: str) -> int:
    return find_text(app, needle)[1]


def row_foreground(app: PtyApp, needle: str, occurrence: int = 0) -> str:
    """The foreground of the first cell of `needle`, which is the row's colour.

    The second occurrence by default for the comparisons below: the first row
    of the viewport is the selected one, and selection colours outrank every
    rule, so it proves nothing either way.
    """
    column, row = find_text(app, needle, occurrence)
    return app.screen.buffer[row][column].fg


def underlined_runs(app: PtyApp, row: int) -> list[str]:
    """Contiguous runs of underlined cells on `row`, as text.

    Read by column, never by byte offset: the box-drawing sidebar and every
    replacement character are multi-byte, so a byte offset into the rendered
    line is not a column and would report a span that is not there.
    """
    runs: list[str] = []
    current = ""
    line = app.screen.buffer[row]
    for column in range(app.screen.columns):
        cell = line[column]
        if cell.underscore:
            current += cell.data
        elif current:
            runs.append(current)
            current = ""
    if current:
        runs.append(current)
    return runs


def open_rules_and_add(app: PtyApp, predicate: str) -> None:
    app.send(b"c")
    app.wait_for("Colour rules")
    app.send(b"\x1ba")  # Alt-A adds a rule and puts the caret in its predicate.
    app.wait_for("Predicate")
    app.send(predicate.encode())
    app.wait_for(predicate)


def open_enrichment_and_save_step(app: PtyApp, expression: str, marker: str) -> None:
    app.send(b"e")
    app.wait_for("Steps")
    app.send(b"\x1ba")
    app.wait_for("Enrichment")
    app.send(expression.encode())
    app.wait_until(lambda text: expression in text, "the draft shows the edit")
    app.send(b"\r")
    # Saving returns to the step list; a validation error would keep the
    # editor open instead, which must fail here rather than masquerade as a
    # save the later waits cannot distinguish from one.
    app.wait_until(
        lambda text: "New step" not in text and marker in text,
        f"step saved: {marker}",
        timeout=15.0,
    )
    close_editor(app)


def open_rules_and_add_column_value(app: PtyApp, value: str) -> None:
    # With accepted enrichment outputs present, Add starts a column rule:
    # the caret lands in its Value field beside a Column chooser. Acceptance
    # (enrich:on) and membership publication reach the dialog through
    # different channels, so a freshly accepted chain can briefly read as
    # output-free; an untouched legacy Add removes itself on Esc, making a
    # bounded retry honest rather than a weaker assertion.
    for attempt in range(6):
        app.send(b"c")
        app.wait_for("Colour rules")
        app.send(b"\x1ba")
        try:
            app.wait_for("Value", timeout=4.0)
            app.wait_for("Column", timeout=4.0)
            break
        except AssertionError:
            app.send(b"\x1b")
            app.wait_until(
                lambda text: "Colour rules" not in text, "dialog closed"
            )
            if attempt == 5:
                raise
    app.send(value.encode())
    app.wait_for(value)


def story(binary: pathlib.Path, width: int, height: int) -> None:
    with tempfile.TemporaryDirectory(prefix="lvu-color-rules-") as directory:
        root = pathlib.Path(directory)
        source = root / "events.log"
        # One line carries bytes that are not valid UTF-8; the terminal shows
        # replacement characters and the highlight must still land on `needle`.
        source.write_bytes(
            b"".join(
                b"ready event %02d ok\n" % index if index % 3 else b"ERROR event %02d failed\n" % index
                for index in range(24)
            )
            + b"tail \xff\xfe needle end\n"
        )
        env = settings_at(root)
        app = PtyApp(
            binary,
            [str(source), "--capture-dir", str(root / "capture")],
            width=width,
            height=height,
            environment=env,
        )
        try:
            # A short terminal shows only the tail of the window, so wait for
            # any served row rather than for a particular one.
            app.wait_until(
                lambda text: "ready eve" in text and "ERROR eve" in text,
                "the first page is served",
            )
            # The painting checks want the head, so jump there first.
            app.send(b"g")
            # Raw rows arrive asynchronously; wait until the window is served.
            app.wait_until(
                lambda text: "ERROR eve" in text and "ready eve" in text,
                "the first page is populated",
            )
            before = app.text()
            # `ready` rows are INFO, so their severity colour is nothing like
            # the rule's; an ERROR row would already be red and prove nothing.
            plain_match = row_foreground(app, "ready eve", 1)
            plain_other = row_foreground(app, "ERROR eve", 1)

            # --- a rule paints the rows it matched, and nothing else --------
            open_rules_and_add(app, "ready")
            app.send(b"\r")  # Apply
            app.wait_until(
                lambda text: "Colour rules" not in text or "rule painting" in text,
                "the rules were applied",
            )
            app.send(b"\x1b")
            app.wait_until(lambda text: "Colour rules" not in text, "dialog closed")
            # The repaint is a query: wait for the view to stop being raw and
            # for the window to be served again.
            painted = app.wait_until(
                lambda text: "raw view" not in text
                    and "ERROR eve" in text
                    and "ready eve" in text,
                "the repaint settled",
            )
            assert row_foreground(app, "ready eve", 1) != plain_match, (
                f"the matched row was not repainted\n{painted}"
            )
            assert row_foreground(app, "ERROR eve", 1) == plain_other, (
                f"an unmatched row was repainted\n{painted}"
            )
            # Painting is not filtering: every row that was there still is.
            for marker in ("ERROR eve", "ready eve"):
                assert marker in painted, f"{marker} vanished\n{painted}"
            # Painting is not filtering. The rule matched only the `ready`
            # rows, so if it had narrowed the view the ERROR rows above and the
            # unmatched tail record below would both be gone.
            app.send(b"G")
            tail = app.wait_until(
                lambda text: "tail" in text,
                "the unmatched tail record is still in the view",
            )
            assert "ERROR eve" in tail, f"matched rows displaced the rest\n{tail}"
            app.send(b"g")
            app.wait_until(
                lambda text: "ERROR eve" in text and "ready eve" in text,
                "the head is still served",
            )

            # --- the rule's own pattern is underlined where it matched ------
            matched_row = find_text(app, "ready eve", 1)[1]
            assert "ready" in underlined_runs(app, matched_row), (
                f"the rule's pattern was not emphasised\n{app.text()}"
            )

            # --- a search regex is underlined, past replacement characters --
            # The event column is clipped at 54 columns, so `needle` is not on
            # screen there; the span checks are a wide-terminal story.
            if width >= 80:
                span_story(app)

            # --- the rule survives reopening the dialog ---------------------
            app.send(b"c")
            reopened = app.wait_for("Colour rules")
            # The accepted list is what the dialog resumes on, and it says so:
            # one rule, applied, not an unsaved edit.
            assert "1 rule painting this view" in reopened, reopened
            assert "1 of 1" in reopened, f"the rule was not listed\n{reopened}"
            app.send(b"\x1b")
            app.wait_until(lambda text: "Colour rules" not in text, "closed")

            # --- a column rule classifies an enrichment output -------------
            # Patterns belong in enrichment: derive severity with a slash
            # shorthand on the cleared search fork, then classify its exact
            # value. Wide terminal only: the enrichment editor plus chooser
            # need the room.
            if width >= 80:
                column_story(app)

            app.send(b"q")
            assert app.wait_exit(timeout=10) == 0
            app.assert_restored()
        finally:
            if app.process.poll() is None:
                app.process.kill()
                app.process.wait(timeout=5)
            app.close()


def column_story(app: PtyApp) -> None:
    """A column rule paints an enrichment output's exact value.

    Patterns belong in enrichment: a slash shorthand derives `severity`
    (ERROR rows only) on the cleared search fork, then a column rule
    classifies it. The legacy `ready` rule keeps painting its rows and the
    row count never changes. Runs on the shared app right after the search
    is cleared, so the selected fork plus FOLLOW state is the combined
    workflow the dialog must handle, not a fresh-app workaround.
    """
    # The fork from the cleared search stays selected in FOLLOW: assert both
    # before saving, so a regression here fails naming the state it found.
    selected = app.wait_until(
        lambda text: "/need\\w+/" in text and "FOLLOW" in text,
        "the cleared search fork is selected in FOLLOW",
        timeout=15.0,
    )
    assert "Enriched" not in selected, f"unexpected fork yet\n{selected}"
    open_enrichment_and_save_step(app, "/(?P<severity>ERROR)/", "severity")
    # Wait for the accepted chain itself (not merely served rows, which can
    # still be the pre-enrichment membership while the new query runs), or
    # the rules dialog would see no classifiable outputs. `enrich:on` is the
    # status proof the chain is applied, on a fork or in place.
    painted_ready = row_foreground(app, "ready eve", 1)
    plain_error = row_foreground(app, "ERROR eve", 1)
    app.wait_until(
        lambda text: "enrich:on" in text
        and "raw view" not in text
        and "ERROR eve" in text,
        "the enrichment settled",
        timeout=45.0,
    )
    open_rules_and_add_column_value(app, "ERROR")
    app.send(b"\r")  # Apply
    app.wait_until(
        lambda text: "Colour rules" not in text or "rule painting" in text,
        "the column rule was applied",
        timeout=15.0,
    )
    app.send(b"\x1b")
    app.wait_until(lambda text: "Colour rules" not in text, "dialog closed")
    settled = app.wait_until(
        lambda text: "raw view" not in text
        and "ERROR eve" in text
        and "ready eve" in text,
        "the repaint settled",
        timeout=20.0,
    )
    assert row_foreground(app, "ERROR eve", 1) != plain_error, (
        f"the classified row was not repainted\n{settled}"
    )
    assert row_foreground(app, "ready eve", 1) == painted_ready, (
        f"the legacy rule lost its rows\n{settled}"
    )
    for marker in ("ERROR eve", "ready eve"):
        assert marker in settled, f"{marker} vanished\n{settled}"


def span_story(app: PtyApp) -> None:
    """A search regex, underlined where it matched, past replacement chars."""
    app.send(b"G")
    app.wait_until(lambda text: "needle end" in text, "the tail is served")
    app.send(b"/")
    app.wait_for("Search")
    app.send(rb"/need\w+/")
    app.wait_for(r"need\w+")
    app.send(b"\r")
    app.wait_until(
        lambda text: "ready eve" not in text,
        "the search narrowed to the matching row",
    )
    app.send(b"\x1b")
    app.wait_until(lambda text: "Search" not in text, "search closed")
    app.wait_for("needle end")
    needle_row = row_of(app, "needle end")
    runs = underlined_runs(app, needle_row)
    assert "needle" in runs, (
        f"the regex span did not land on the match: {runs}\n{app.text()}"
    )
    # The replacement characters are before the match and must not be
    # part of it: a span computed on the original bytes would have
    # slid two columns left and swallowed one of them.
    assert not any("�" in run for run in runs), (
        f"a replacement character was emphasised: {runs}\n{app.text()}"
    )
    # The column story needs the whole view back on its search fork: clear
    # the search that narrowed it to the tail record.
    app.send(b"/")
    app.wait_for("Search")
    app.send(b"\x7f" * len(r"/need\w+/"))
    app.wait_until(
        lambda text: "No filter every record is shown" in text,
        "search cleared for the column story",
    )
    app.send(b"\x1b")
    app.wait_until(lambda text: "Search" not in text, "search closed")


def run(binary: pathlib.Path) -> None:
    for width, height in ((80, 24), (54, 16)):
        story(binary, width, height)


if __name__ == "__main__":
    run(pathlib.Path(sys.argv[1]).resolve())
    print("Colour rules PTY passed: predicate painting, column classification, span highlighting, narrow terminal")
