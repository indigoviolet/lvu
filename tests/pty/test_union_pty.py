#!/usr/bin/env python3
"""Union views end to end: create from two sources, merge, filter, group, restart.

Two file sources log interleaving event times. The Union dialog merges two
views into one timestamp-ordered view; a union-level text search narrows it;
an append after publication refreshes it with no view edit; the Grouping
dialog applies over it without disturbing membership; and the union survives
a restart through ordinary persisted working state.
"""
import pathlib
import sys
import tempfile
import time
from test_lvu_pty import PtyApp
from test_enrichment_chain_pty import stop


API = "\n".join(
    [
        '{"ts":"2026-03-04T05:06:%02dZ","svc":"api","n":%d}' % (index * 2, index)
        for index in range(6)
    ]
) + "\n"

WORKER = "\n".join(
    [
        '{"ts":"2026-03-04T05:06:%02dZ","svc":"worker","n":%d}' % (index * 2 + 1, index)
        for index in range(6)
    ]
) + "\n"


def open_union_dialog(app):
    app.send(b"\x10")
    app.wait_for("Command palette")
    app.send("union".encode())
    app.wait_until(lambda text: "Union views" in text, "union palette row")
    app.send(b"\r")
    app.wait_for("┌ Union views")


def assert_no_modal(app):
    """Fail fast with the screen attached when a dialog is still open: a
    quit keypress landing in a dialog field looks exactly like an exit hang
    eight seconds later, and this names the dialog instead."""
    app.drain()
    for chrome in ("┌ Filter", "┌ Multiline grouping", "┌ Union views", "┌ Command palette"):
        assert chrome not in app.text(), ("modal still open", chrome, app.text())


def wait_closed(app, chrome, description, timeout=10.0):
    """Wait until dialog chrome is gone AND stays gone across consecutive
    polls. A single absence poll can catch an unsettled mid-redraw frame on
    a live app and pass vacuously while the dialog is still open; absence
    must hold three polls in a row to count."""
    deadline = time.monotonic() + timeout
    clean = 0
    while time.monotonic() < deadline:
        # text() alone does not read the pty: without a drain it returns the
        # last frame a wait consumed, so a closed dialog reads as still open
        # until these polls catch up with what the app already drew.
        app.drain()
        if chrome not in app.text():
            clean += 1
            if clean >= 3:
                return
        else:
            clean = 0
        time.sleep(0.05)
    raise AssertionError(f"timed out waiting for {description}; still open:\n{app.text()}")


def run(binary):
    with tempfile.TemporaryDirectory(prefix="lvu-union-pty-") as directory:
        root = pathlib.Path(directory)
        api, worker = root / "api.log", root / "worker.log"
        api.write_text(API)
        worker.write_text(WORKER)
        env = {
            "XDG_CONFIG_HOME": str(root / "config"),
            "XDG_DATA_HOME": str(root / "data"),
            "XDG_CACHE_HOME": str(root / "cache"),
        }
        args = [str(api), str(worker), "--capture-dir", str(root / "capture")]
        app = PtyApp(binary, args, width=150, height=32, cwd=root, environment=env)
        try:
            app.wait_for('"svc":"api"')

            # --- Create the union from the two open views. -----------------
            open_union_dialog(app)
            text = app.text()
            assert "All events" in text, ("both views are listed", text)
            # The active view arrives pre-selected; toggle the second row.
            app.send(b"\x1b[B")
            app.send(b" ")
            app.wait_until(
                lambda text: "2 of 2 views selected" in text,
                "both inputs checked",
            )
            app.send(b"\r")
            wait_closed(app, "┌ Union views", "union dialog closed after create")
            merged = app.wait_until(
                lambda text: "Union of" in text
                and '"svc":"api"' in text
                and '"svc":"worker"' in text,
                "union created, selected and merged",
                timeout=20,
            )
            merged = app.wait_until(
                lambda text: '"svc":"api"' in text and '"svc":"worker"' in text,
                "union holds both sources",
                timeout=20,
            )
            for needle in ['"svc":"api","n":0', '"svc":"worker","n":0']:
                assert needle in merged, ("every record merges once", merged)

            # --- A union-level text search narrows the merged stream. ------
            app.send(b"/")
            app.wait_for("Filter")
            app.send("worker".encode())
            app.wait_until(
                lambda text: "Applied" in text and "worker" in text,
                "union search applied",
                timeout=10,
            )
            app.send(b"\x1b")
            narrowed = app.wait_until(
                lambda text: '"svc":"worker"' in text,
                "worker rows remain",
                timeout=10,
            )
            assert '"svc":"api"' not in narrowed, ("api rows filtered out", narrowed)
            # The line-120 Esc already closed the editor: Esc at the base
            # screen quits by design, so no dismissal keypress belongs here.
            wait_closed(app, "┌ Filter", "filter closed after search")
            # Clear back to the full merge for the grouping step.
            app.send(b"/")
            app.wait_for("Filter")
            app.send(b"\x7f" * len("worker"))
            app.wait_until(
                lambda text: '"svc":"api"' in text and '"svc":"worker"' in text,
                "clear restores the merge",
                timeout=10,
            )
            app.send(b"\x1b")
            wait_closed(app, "┌ Filter", "filter editor closed")

            # --- File-follow progress refreshes the accepted union itself. -
            # No view edit or keypress occurs between the append and this
            # assertion. The old first identity remains visible while the new
            # physical record appears through the same raw capture.
            with api.open("a") as stream:
                stream.write(
                    '{"ts":"2026-03-04T05:06:30Z","svc":"api","n":99,"marker":"live-after-completion"}\n'
                )
            refreshed = app.wait_until(
                lambda text: "live-after-completion" in text
                and '"svc":"api","n":0' in text,
                "published union refreshed after append without a view edit",
                timeout=25,
            )
            assert '"svc":"worker","n":0' in refreshed, refreshed

            # --- Grouping applies over the union without losing rows. ------
            # These inputs are intentionally raw-only, so the default native
            # Run rule has no accepted enrichment column to select. Exercise
            # the segmented control and apply explicit Legacy Auto instead;
            # the worker tests discriminate native Run/Filter flag execution.
            # Editors stay open on Apply by design, so close explicitly: an
            # open editor would swallow the quit keypress as text.
            app.send(b"m")
            app.wait_for("┌ Multiline grouping")
            app.send(b"\t")
            app.send(b"\x1b[C" * 2)
            app.wait_for("Auto — conservative multiline detection")
            app.send(b"\t" * 2)
            app.send(b"\r")
            app.wait_until(
                lambda text: "Applied" in text and "Legacy Auto" in text,
                "legacy grouping is accepted",
                timeout=10,
            )
            app.send(b"\x1b")
            wait_closed(app, "┌ Multiline grouping", "grouping editor closed")
            app.wait_until(
                lambda text: '"svc":"api"' in text and '"svc":"worker"' in text,
                "grouping keeps the merged rows",
                timeout=10,
            )

            # --- The union survives a restart through persisted state. -----
            assert_no_modal(app)
            stop(app)
            app = PtyApp(binary, args, width=150, height=32, cwd=root, environment=env)
            app.wait_for('"svc":"api"')
            found = False
            for _ in range(8):
                app.drain()
                text = app.text()
                if "Union of" in text and '"svc":"worker"' in text:
                    found = True
                    break
                app.send(b"]")
                app.wait_until(lambda t: "api" in t or "worker" in t, "view switch")
            assert found, ("the union view did not restore", app.text())
            assert_no_modal(app)
            stop(app)
        finally:
            if app.process.poll() is None:
                stop(app)


if __name__ == "__main__":
    run(sys.argv[1])
