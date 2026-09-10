#!/usr/bin/env python3
"""Normalize differently named keys into a cancellable shared-key union.

The retired Correlation mapper let each raw source choose a different field.
The supported flow makes that choice explicit as accepted enrichment, then
opens the existing Union chooser from Fields. This story preserves the old
user outcome while proving cancellation, stable identities/no recapture,
native membership, input order, creation-only key pinning and restart persistence.
"""

import pathlib
import sys
import tempfile

from test_enrichment_chain_pty import stop
from test_lvu_pty import PtyApp
from test_shared_key_union_pty import (
    add_enrichment,
    choose_both_views,
    select_field,
    switch_to,
    switch_to_enriched,
    switch_to_union,
    union_stable_id,
)
from test_union_pty import wait_closed


API = "\n".join(
    [
        '{"ts":"2026-03-04T05:06:00Z","request_id":"req-7","service":"api","msg":"api accepted"}',
        '{"ts":"2026-03-04T05:06:02Z","request_id":"req-8","service":"api","msg":"api unrelated"}',
        '{"ts":"2026-03-04T05:06:04Z","request_id":"req-7","service":"api","msg":"api responded"}',
    ]
) + "\n"

WORKER = "\n".join(
    [
        '{"ts":"2026-03-04T05:06:01Z","req":"req-7","stage":"worker queued"}',
        '{"ts":"2026-03-04T05:06:03Z","req":"req-9","stage":"worker unrelated"}',
    ]
) + "\n"


def open_shared_key_chooser(app):
    select_field(app, "request_id")
    app.send(b"r")
    app.wait_for("┌ Union views")
    assert "Correlate across sources" not in app.text(), app.text()
    choose_both_views(app)


def pin_marker(field, checked):
    return f"[{'x' if checked else ' '}] {field}"


def assert_pins(app, expected):
    app.send(b"i")
    app.wait_for("Fields · record")
    screen = app.wait_until(
        lambda text: all(
            pin_marker(field, checked) in text
            for field, checked in expected.items()
        ),
        f"pin markers match {expected!r}",
    )
    for field, checked in expected.items():
        assert pin_marker(field, checked) in screen, screen
    app.send(b"\x1b")
    wait_closed(app, "┌ Fields", "pin inspection closed")


def toggle_pin(app, field, checked):
    select_field(app, field)
    app.send(b" ")
    app.wait_until(
        lambda text: pin_marker(field, checked) in text,
        f"{field} pin becomes {checked}",
    )
    app.send(b"\x1b")
    wait_closed(app, "┌ Fields", f"{field} pin edit closed")


def run(binary):
    with tempfile.TemporaryDirectory(prefix="lvu-correlation-pty-") as directory:
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
        app = PtyApp(binary, args, width=160, height=36, cwd=root, environment=env)
        try:
            app.wait_for("api accepted")

            # Each differently named raw key becomes the same structural,
            # accepted output. Raw names are never handed to a legacy mapper.
            switch_to(app, "api accepted")
            add_enrichment(app, "request_id = pl.col('request_id')")
            switch_to_enriched(app, "api accepted")
            api_id = union_stable_id(
                app, "api accepted", ("api accepted", "api unrelated", "api responded")
            )

            switch_to(app, "worker queued")
            add_enrichment(app, "request_id = pl.col('req')")
            switch_to_enriched(app, "worker queued")
            worker_id = union_stable_id(
                app, "worker queued", ("worker queued", "worker unrelated")
            )

            # A fully selected chooser can still be cancelled. It registers no
            # union and leaves both the accepted origin rows and pins intact.
            switch_to_enriched(app, "api accepted")
            toggle_pin(app, "service", True)
            assert_pins(app, {"service": True, "request_id": False})
            open_shared_key_chooser(app)
            app.send(b"\x1b")
            wait_closed(app, "┌ Union views", "shared-key chooser cancelled")
            cancelled = app.wait_until(
                lambda text: "api accepted" in text and "api unrelated" in text,
                "cancelled chooser preserved its origin view",
            )
            assert "› Union of" not in cancelled, cancelled
            assert_pins(app, {"service": True, "request_id": False})

            # Repeat the explicit choice and accept it. Timestamp ordering is
            # native union behavior; only req-7 survives the exact key.
            open_shared_key_chooser(app)
            app.send(b"\r")
            wait_closed(app, "┌ Union views", "shared-key union created", timeout=25)
            merged = app.wait_until(
                lambda text: "› Union of" in text
                and "api accepted" in text
                and "worker queued" in text
                and "api responded" in text,
                "normalized shared-key union published",
                timeout=25,
            )
            assert "api unrelated" not in merged, merged
            assert "worker unrelated" not in merged, merged
            # No timestamp role was selected, so capture time is authoritative.
            # The API source was captured first and retains its record order.
            assert merged.index("api accepted") < merged.index("api responded"), merged
            assert merged.index("api responded") < merged.index("worker queued"), merged

            # Creation initializes only the exact key pin. It neither copies
            # the origin's unrelated pin nor marks another union field.
            assert_pins(app, {"request_id": True, "service": False, "ts": False})

            # Later pin edits are ordinary persistent view state. An explicit
            # key unpin must not be undone by restore-time auto-pinning.
            toggle_pin(app, "ts", True)
            toggle_pin(app, "request_id", False)
            assert_pins(app, {"request_id": False, "ts": True})

            # The union points at original captured identities rather than
            # copying or recapturing either source's selected record.
            assert union_stable_id(
                app, "api accepted", ("api accepted", "worker queued", "api responded")
            ) == api_id
            assert union_stable_id(
                app, "worker queued", ("api accepted", "worker queued", "api responded")
            ) == worker_id

            stop(app)
            app = PtyApp(binary, args, width=160, height=36, cwd=root, environment=env)
            switch_to_union(app, "worker queued")
            restored = app.wait_until(
                lambda text: "› Union of" in text
                and "api accepted" in text
                and "worker queued" in text
                and "api responded" in text,
                "normalized shared-key union restored",
                timeout=25,
            )
            assert "api unrelated" not in restored, restored
            assert "worker unrelated" not in restored, restored
            assert_pins(app, {"request_id": False, "ts": True})
            assert union_stable_id(
                app, "api accepted", ("api accepted", "worker queued", "api responded")
            ) == api_id
            assert union_stable_id(
                app, "worker queued", ("api accepted", "worker queued", "api responded")
            ) == worker_id
            stop(app)
        finally:
            if app.process.poll() is None:
                if sys.exc_info()[0] is None:
                    stop(app)
                else:
                    app.process.kill()
                    app.process.wait(timeout=5)
                    app.close()

    print(
        "Correlation replacement PTY passed: explicit derived-key mapping, cancellation, "
        "native ordered membership, stable original IDs, creation-only key pinning and restart"
    )


if __name__ == "__main__":
    run(pathlib.Path(sys.argv[1]).resolve())
