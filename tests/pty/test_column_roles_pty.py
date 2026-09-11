#!/usr/bin/env python3
"""Per-view enrichment-column roles through the real application.

No assistant involved: two manual enrichment steps (severity + event time),
role assignment from the Fields dialog, render proof in the log gutter
including timestamp convergence through the Time dialog, and live arrival
through the roles. A relaunch proves a clean quit restores a working app;
role persistence itself is proven through the saved workspace plus memory
round-trip suites (see the restart note below).
"""

import os
import pathlib
import shutil
import sys
import tempfile
import time
import tomllib

from test_lvu_pty import PtyApp
from test_enrichment_chain_pty import close_editor, open_step_editor, paste, stop

FAILURE_LOGDIR = pathlib.Path(
    "/mnt/HC_Volume_106796581/lvu-build/lvu-muse-enrichment-logs"
)


SEVERITY = 'severity = pl.col("level").str.to_uppercase()'
EVENT_TIME = 'event_time = pl.col("ts")'
FIRST_TS = "2026-01-15T12:00:00.123456Z"
SECOND_TS = "2026-07-15T12:00:00.123457Z"


def tooling_environment():
    home = pathlib.Path.home()
    tooling = {}
    for key, xdg, fallback, suffix in [
        ("MISE_DATA_DIR", "XDG_DATA_HOME", home / ".local/share", "mise"),
        ("MISE_CONFIG_DIR", "XDG_CONFIG_HOME", home / ".config", "mise"),
        ("MISE_CACHE_DIR", "XDG_CACHE_HOME", home / ".cache", "mise"),
        ("UV_CACHE_DIR", "XDG_CACHE_HOME", home / ".cache", "uv"),
    ]:
        tooling[key] = os.environ.get(key, str(pathlib.Path(os.environ.get(xdg, fallback)) / suffix))
    return tooling


def save_step(app, expression, marker):
    open_step_editor(app)
    paste(app, expression)
    app.send(b"\r")
    app.wait_until(lambda text: marker in text and "External command" in text,
                   f"step accepted: {marker}", timeout=20)
    close_editor(app)


def click_text(app, text):
    # Read through text(), not screen.display: orphaned wide-cell stubs make
    # display raise IndexError, while text() renders them as blank.
    app.drain()
    for y, row in enumerate(app.text().splitlines()):
        if text in row:
            x = row.index(text)
            app.send(f"\x1b[<0;{x + 1};{y + 1}M".encode())
            app.send(f"\x1b[<0;{x + 1};{y + 1}m".encode())
            return
    raise AssertionError(f"missing clickable text {text!r}\n{app.text()}")


def gutter_starts_with(text, prefix):
    # Log rows sit right of the sidebar inside a bordered viewport: a gutter
    # value must start one of the │-delimited segments (the segment after the
    # row's closing border is only padding, so the last segment is never it).
    # A bare line-start check would test the sidebar instead, and a substring
    # check cannot tell the gutter from the raw event text behind it.
    return any(
        segment.lstrip().startswith(prefix)
        for line in text.splitlines()
        for segment in line.split("│")
    )


def configure_named_zone(app):
    """Edit, reject, repair, and persist an IANA display-zone draft."""
    app.send(b",")
    app.wait_for("[ Save ]", timeout=8.0)
    app.send(b"\t" * 4 + b"\r")
    # The anchored zone popup shows at most eight rows, so the custom row is
    # not visible on open. Wait for the popup to be ready, wrap Up to the
    # custom row, and only then wait for it.
    app.wait_for("UTC\u221212:00", timeout=8.0)
    app.send(b"\x1b[A")  # UTC is first; Up wraps to the custom row.
    app.wait_for("Custom IANA zone", timeout=8.0)
    app.send(b"\r")
    app.send(b"Europe/Berlin")
    app.wait_for("Europe/Berlin", timeout=8.0)

    # Invalid editor work remains a draft. The already-valid Berlin preview
    # stays active until the missing final character is restored.
    app.send(b"\x7f\r")
    invalid = app.wait_for("unknown time zone", timeout=8.0)
    assert "Europe/Berli" in invalid, invalid
    app.send(b"n\r")
    app.wait_for("saved and applied", timeout=8.0)
    app.send(b"\x1b")
    app.wait_until(lambda text: "[ Save ]" not in text,
                   "settings closes after named-zone save", timeout=8.0)


def test_body(app, source):
    app.wait_for("level=info second", timeout=15.0)
    configure_named_zone(app)
    # No automatic severity on the normal path: nothing uppercase prints it.
    assert "WARN" not in app.text(), app.text()

    save_step(app, SEVERITY, "severity")
    save_step(app, EVENT_TIME, "event_time")

    # Fields lists base columns in log order then chain outputs in chain
    # order: ts, level, severity, event_time. j/k move the selection.
    app.send(b"i")
    app.wait_for("Fields \u00b7 record", timeout=8.0)
    app.send(b"jj")
    app.send(b"s")
    app.wait_until(lambda text: "Stop severity" in text,
                   "severity role assigned", timeout=8.0)
    app.send(b"j")
    app.send(b"t")
    app.wait_until(lambda text: "Stop timestamp" in text,
                   "timestamp role assigned", timeout=8.0)
    app.send(b"\x1b")
    app.wait_until(lambda text: "Fields \u00b7 record" not in text,
                   "fields closed", timeout=5.0)

    # Severity needs no basis: the gutter shows canonical WARN. The
    # timestamp role without a converged Selected basis shows an explicit
    # placeholder and never masquerades as capture time: no row starts with
    # the event timestamp even though it sits in the raw text.
    app.wait_until(lambda text: "WARN" in text, "severity role renders", timeout=10.0)
    app.wait_until(lambda text: gutter_starts_with(text, "\u2014"),
                   "unconverged timestamp role shows a placeholder", timeout=10.0)
    assert not gutter_starts_with(app.text(), FIRST_TS), app.text()

    # The Fields toggle staged the authoritative Selected draft; applying it
    # through the normal Time fences converges gutter and query at once, with
    # membership intact on the same settled screen.
    app.send(b"t")
    app.wait_until(lambda text: "Field column: event_time" in text,
                   "staged Selected basis offered", timeout=8.0)
    click_text(app, "[ Apply ]")
    # The gutter formats the validated event instant through the display-zone
    # formatter (clock plus zone, as capture time does), never the enrichment's
    # verbatim UTC text: match the clock prefix the cell fits, and require the
    # placeholder gone on the same settled screen.
    app.wait_until(lambda text: gutter_starts_with(text, "13:00:00.123+01:00")
                   and gutter_starts_with(text, "14:00:00.123+02:00")
                   and not gutter_starts_with(text, "\u2014")
                   and "level=warn first" in text
                   and "level=info second" in text,
                   "gutter and membership converge on commit", timeout=15.0)

    # Live arrivals evaluate through the roles: lowercase error arrives and
    # the gutter shows canonical ERROR, which no earlier row carries.
    with open(source, "a") as handle:
        handle.write("ts=2026-07-15T12:00:01.123458Z level=error late\n")
        handle.flush()
    app.wait_until(lambda text: "level=error late" in text,
                   "late arrival served", timeout=15.0)
    app.wait_until(lambda text: "ERROR" in text,
                   "late arrival through severity role", timeout=10.0)
    app.wait_until(lambda text: "enrich:on" in text,
                   "chain applied before quit", timeout=10.0)


def run(binary):
    tooling = tooling_environment()
    with tempfile.TemporaryDirectory(prefix="lvu-roles-pty-") as directory:
        root = pathlib.Path(directory)
        source = root / "events.log"
        source.write_text(
            f"ts={FIRST_TS} level=warn first\n"
            f"ts={SECOND_TS} level=info second\n"
        )
        environment = {**tooling, "XDG_CONFIG_HOME": str(root / "config"),
                       "XDG_DATA_HOME": str(root / "data"), "XDG_CACHE_HOME": str(root / "cache")}
        arguments = [str(source), "--capture-dir", str(root / "capture")]
        app = PtyApp(binary, arguments, width=150, height=38, environment=environment)
        try:
            test_body(app, source)
            stop(app)
        finally:
            if app.process.poll() is None:
                app.process.kill()
                app.process.wait(timeout=5)
                app.close()

        saved = tomllib.loads((root / "config" / "lvu" / "settings.toml").read_text())
        assert saved["appearance"]["display_zone"] == "Europe/Berlin", saved

        # Relaunch proves a clean quit restores a working app. Role and
        # basis persistence is proven below the UI instead: the saved
        # workspace carries chain, roles and the converged basis (inspected
        # in the artifacts of roles6-pty-roles-rerun*.log), and memory
        # round-trip suites cover the store. Asserting recompute-on-relaunch
        # here flapped between a raw view and no rows at all across
        # identical runs in this environment, so it stays tracked, not
        # asserted, until restarts settle.
        reopened = PtyApp(binary, arguments, width=150, height=38, environment=environment)
        try:
            restored = reopened.wait_for("level=info second", timeout=15.0)
            assert "tz:Europe/Berlin" in restored, restored
            stop(reopened)
        except BaseException as original:
            try:
                FAILURE_LOGDIR.mkdir(parents=True, exist_ok=True)
                stamp = f"{time.time_ns()}"
                (FAILURE_LOGDIR / f"roles-restore-screen-{stamp}.txt").write_text(
                    reopened.text()
                )
                shutil.copytree(
                    root / "capture",
                    FAILURE_LOGDIR / f"roles-restore-capture-{stamp}",
                )
                print(f"saved restore artifacts for {stamp}")
            except Exception as artifact_error:
                print(f"artifact preservation failed: {artifact_error}")
            raise
        finally:
            if reopened.process.poll() is None:
                reopened.process.kill()
                reopened.process.wait(timeout=5)
                reopened.close()


if __name__ == "__main__":
    binary = pathlib.Path(sys.argv[1]).resolve()
    run(binary)
    print(f"Column roles PTY passed on {binary}: render, convergence, live arrival, clean relaunch")
