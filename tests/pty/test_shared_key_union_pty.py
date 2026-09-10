#!/usr/bin/env python3
"""Shared-key union through Fields, accepted enrichment and native filtering.

This suite intentionally opens the existing Union views chooser from Fields.
It never acquires another source and never uses the legacy correlation mapper.
"""

import pathlib
import sys
import tempfile
import time

from test_enrichment_chain_pty import close_editor, open_step_editor, paste, stop
from test_lvu_pty import PtyApp, open_advanced_filter
from test_union_pty import wait_closed


EXACT = 9_007_199_254_740_993  # above f64's exact integer range


def switch_to(app, marker, *, exclude="Union of", attempts=8):
    for _ in range(attempts):
        app.drain()
        text = app.text()
        if marker in text and (not exclude or exclude not in text):
            return text
        app.send(b"]")
        time.sleep(0.12)
    raise AssertionError((f"view containing {marker!r} was not found", app.text()))


def switch_to_union(app, marker, attempts=10):
    for _ in range(attempts):
        app.drain()
        text = app.text()
        if "Union of" in text and marker in text:
            return text
        app.send(b"]")
        time.sleep(0.12)
    raise AssertionError(("shared-key union was not found", app.text()))


def add_enrichment(app, source):
    open_step_editor(app)
    paste(app, source)
    app.send(b"\r")
    app.wait_until(
        lambda text: "enrich:on" in text and "External command" in text,
        f"accepted enrichment {source[:32]!r}",
        timeout=20,
    )
    close_editor(app)


def select_field(app, field):
    app.send(b"i")
    app.wait_for("Fields · record")
    app.wait_until(lambda text: field in text, f"{field} listed by Fields", timeout=15)
    for _ in range(32):
        text = app.text()
        if f"> [ ] {field}" in text or f"› [ ] {field}" in text:
            return
        app.send(b"\x1b[B")
        time.sleep(0.04)
        app.drain()
    raise AssertionError((f"could not select field {field!r}", app.text()))


def choose_both_views(app):
    app.wait_for("┌ Union views")
    app.send(b"\x1b[B")
    app.send(b" ")
    app.wait_until(
        lambda text: "2 of " in text and "views selected" in text,
        "both existing views selected",
    )


def create_from_selected_key(app, field="request_key"):
    select_field(app, field)
    app.send(b"r")
    app.wait_for("┌ Union views")
    assert "Correlate across sources" not in app.text(), app.text()
    choose_both_views(app)
    app.send(b"\r")
    wait_closed(app, "┌ Union views", "shared-key union resolved and created", timeout=25)


def numeric_restart_live_and_namesake(binary):
    with tempfile.TemporaryDirectory(prefix="lvu-shared-key-numeric-pty-") as directory:
        root = pathlib.Path(directory)
        api = root / "api.log"
        worker = root / "worker.log"
        api.write_text(
            '{"service":"api","request_key":"raw-api-other","api_id":9007199254740992,"msg":"api-other"}\n'
            f'{{"service":"api","request_key":"raw-api-selected","api_id":{EXACT},"msg":"api-selected"}}\n'
        )
        worker.write_text(
            '{"service":"worker","request_key":"raw-worker-other","worker_id":9007199254740994,"msg":"worker-other"}\n'
            f'{{"service":"worker","request_key":"raw-worker-selected","worker_id":{EXACT},"msg":"worker-selected"}}\n'
        )
        env = {
            "XDG_CONFIG_HOME": str(root / "config"),
            "XDG_DATA_HOME": str(root / "data"),
            "XDG_CACHE_HOME": str(root / "cache"),
        }
        args = [str(api), str(worker), "--capture-dir", str(root / "capture")]
        app = PtyApp(binary, args, width=160, height=36, cwd=root, environment=env)
        try:
            switch_to(app, "api-selected")
            add_enrichment(app, "request_key = pl.col('api_id').cast(pl.UInt64)")
            switch_to(app, "worker-selected")
            add_enrichment(app, "request_key = pl.col('worker_id').cast(pl.UInt64)")
            switch_to(app, "api-selected")
            create_from_selected_key(app)

            merged = app.wait_until(
                lambda text: "Union of" in text
                and "api-selected" in text
                and "worker-selected" in text,
                "typed shared-key union published",
                timeout=25,
            )
            assert "api-other" not in merged and "worker-other" not in merged, merged

            with worker.open("a") as stream:
                stream.write(
                    f'{{"service":"worker","request_key":"raw-live-match","worker_id":{EXACT},"msg":"live-match"}}\n'
                    f'{{"service":"worker","request_key":"raw-live-other","worker_id":{EXACT + 9},"msg":"live-other"}}\n'
                )
            live = app.wait_until(
                lambda text: "live-match" in text,
                "live append re-filtered by the same typed key",
                timeout=25,
            )
            assert "live-other" not in live, live

            # A rejected union-level candidate cannot replace the accepted
            # membership or key. The editor remains on the invalid draft.
            open_advanced_filter(app)
            paste(app, "pl.col(")
            app.send(b"\r")
            app.wait_for("compiler rejected expression", timeout=15)
            assert "live-match" in app.text(), app.text()
            app.send(b"\x1b")
            app.wait_for("live-match")

            stop(app)
            app = PtyApp(binary, args, width=160, height=36, cwd=root, environment=env)
            switch_to_union(app, "live-match")
            app.wait_until(
                lambda text: "Union of" in text and "worker-selected" in text,
                "persisted exact-key union restored",
                timeout=25,
            )
            restored = app.text()
            assert "api-other" not in restored and "live-other" not in restored, restored

            # Remove the origin enrichment, leaving the raw namesake in the
            # source. Fields may still show that raw column, but structural
            # accepted-output provenance must reject using it as a shared key.
            switch_to(app, "api-selected")
            app.send(b"e")
            app.wait_for("Steps")
            app.send(b"r")
            app.wait_until(lambda text: "enrich:on" not in text, "enrichment removed", timeout=20)
            app.send(b"\x1b")
            wait_closed(app, "┌ Enrichment", "enrichment list closed")
            select_field(app, "request_key")
            app.send(b"r")
            choose_both_views(app)
            app.send(b"\r")
            app.wait_for("accepted enrichment", timeout=15)
            assert "┌ Union views" in app.text(), app.text()
            app.send(b"\x1b")
            wait_closed(app, "┌ Union views", "rejected raw namesake chooser closed")

            # The earlier accepted union remains available after rejection.
            restored = switch_to_union(app, "live-match")
            assert "live-other" not in restored, restored
            stop(app)
        finally:
            if app.process.poll() is None:
                stop(app)


def slash_and_long_same_prefix(binary):
    prefix = "shared-" + "x" * 220
    selected = prefix + "-A"
    other = prefix + "-B"
    pattern = r"/key=(?P<request_key>\S+)/"
    with tempfile.TemporaryDirectory(prefix="lvu-shared-key-slash-pty-") as directory:
        root = pathlib.Path(directory)
        api = root / "api.log"
        worker = root / "worker.log"
        api.write_text(f"api-other key={other}\napi-selected key={selected}\n")
        worker.write_text(f"worker-other key={other}\nworker-selected key={selected}\n")
        env = {
            "XDG_CONFIG_HOME": str(root / "config"),
            "XDG_DATA_HOME": str(root / "data"),
            "XDG_CACHE_HOME": str(root / "cache"),
        }
        args = [str(api), str(worker), "--capture-dir", str(root / "capture")]
        app = PtyApp(binary, args, width=160, height=36, cwd=root, environment=env)
        try:
            switch_to(app, "api-selected")
            add_enrichment(app, pattern)
            switch_to(app, "worker-selected")
            add_enrichment(app, pattern)
            switch_to(app, "api-selected")
            create_from_selected_key(app)
            merged = app.wait_until(
                lambda text: "Union of" in text
                and "api-selected" in text
                and "worker-selected" in text,
                "slash-output shared-key union published",
                timeout=25,
            )
            # Both values have the same long visible prefix. Only native full
            # string equality may decide membership; rendered truncation may not.
            assert "api-other" not in merged and "worker-other" not in merged, merged
            stop(app)
        finally:
            if app.process.poll() is None:
                stop(app)


def run(binary):
    numeric_restart_live_and_namesake(binary)
    slash_and_long_same_prefix(binary)


if __name__ == "__main__":
    run(pathlib.Path(sys.argv[1]).resolve())
    print(
        "Shared-key union PTY passed: typed equality, slash outputs, long keys, "
        "live append, rejection rollback, raw-namesake fence and restart"
    )
