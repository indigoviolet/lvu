#!/usr/bin/env python3
"""Follow one identity across sources that name its field differently.

Two sources carry the same request under different keys (`request_id` and
`req`). Correlating from Fields must produce a view holding every record with
that value from both sources, in explicit source order, with the correlating
fields pinned; it must survive a restart; and a mapping that is never made must
leave the origin view exactly as it was.
"""
import pathlib
import sys
import tempfile
from test_lvu_pty import PtyApp
from test_enrichment_chain_pty import stop


API = "\n".join(
    [
        '{"request_id":"req-7","service":"api","msg":"api accepted"}',
        '{"request_id":"req-8","service":"api","msg":"api unrelated"}',
        '{"request_id":"req-7","service":"api","msg":"api responded"}',
    ]
) + "\n"

WORKER = "\n".join(
    [
        '{"req":"req-7","stage":"worker queued"}',
        '{"req":"req-9","stage":"worker unrelated"}',
    ]
) + "\n"


def open_fields_on_first_record(app):
    app.send(b"i")
    app.wait_for("Fields · record")


def correlate(app):
    """Select `request_id` in Fields and start the lookup.

    Correlation replaces Fields (component-model.md §6.5): the layer that
    shows the pending lookup is the one the mapping lands in, with the same
    frame and the same `[ Correlate ]` default.
    """
    app.wait_until(lambda text: "request_id" in text, "request_id offered in Fields")
    # §8.11: rows follow the record's own order, and request_id is its first
    # key, so the selection is already on it.
    app.wait_until(lambda text: "> [ ] request_id" in text or "› [ ] request_id" in text,
                   "request_id selected")
    app.send(b"r")
    app.wait_for("Correlate across sources")
    text = app.text()
    assert "Fields · record" not in text, ("Fields is replaced, not stacked", text)
    assert "[ Correlate ]" in text and "[ Cancel ]" in text, text


def choose_worker_field(app):
    """Map the worker source to its own key name, explicitly."""
    app.wait_for("Not correlated")
    app.send(b"\x1b[B")            # select the worker row
    app.send(b"\r")                # open its field options
    app.wait_until(lambda text: "req" in text, "worker field options")
    app.send(b"\x1b[B")            # move off `Not correlated` onto `req`
    app.send(b"\r")                # commit
    app.wait_until(lambda text: "Not correlated" not in text
                   or text.count("Not correlated") < 2, "worker mapped")


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
        app = PtyApp(binary, args, width=150, height=32, cwd=root, environment=env)
        try:
            app.wait_for("api accepted")

            # --- A mapping that is never accepted changes nothing. ---------
            open_fields_on_first_record(app)
            correlate(app)
            app.send(b"\x1b")
            app.wait_until(lambda text: "Correlate across sources" not in text,
                           "correlation cancelled")
            text = app.text()
            assert "Fields · record" not in text, ("Escape returned to Fields", text)
            assert "api accepted" in text and "api unrelated" in text, (
                "a cancelled correlation changed the origin view", text)

            # --- The accepted mapping produces the cross-source view. ------
            open_fields_on_first_record(app)
            correlate(app)
            choose_worker_field(app)
            app.send(b"\t")                     # focus [ Correlate ]
            app.send(b"\r")
            app.wait_until(
                lambda text: "Correlate across sources" not in text
                and "worker queued" in text
                and "api responded" in text
                and "query ready" in text,
                "correlated view scanned both sources",
                timeout=10,
            )
            text = app.text()
            assert "api accepted" in text and "api responded" in text, text
            assert "worker queued" in text, text
            # Only req-7. The other requests, in either source, are excluded.
            assert "api unrelated" not in text, text
            assert "worker unrelated" not in text, text
            # Explicit source order: every api record before the worker record.
            assert text.index("api responded") < text.index("worker queued"), text
            # The correlating fields are pinned, so the value stays visible.
            assert "req-7" in text, text

            # --- It survives a restart. ------------------------------------
            stop(app)
            app = PtyApp(binary, args, width=150, height=32, cwd=root, environment=env)
            app.wait_for("api accepted")
            found = False
            for _ in range(6):
                text = app.text()
                if "worker queued" in text and "api responded" in text:
                    found = True
                    break
                app.send(b"]")
                app.wait_until(lambda t: "api" in t or "worker" in t, "view switch")
                app.assert_remains("Log", "impossible sentinel", .15)
            assert found, ("the correlated view did not restore", app.text())
            text = app.text()
            assert "api unrelated" not in text and "worker unrelated" not in text, text
            assert text.index("api responded") < text.index("worker queued"), text
            stop(app)
        finally:
            if app.process.poll() is None:
                app.send(b"\x03")
                try:
                    app.wait_exit(5)
                except Exception:
                    app.process.kill()
                    app.process.wait()
                app.close()
    print(
        "Correlation PTY passed: differing key names mapped explicitly, records from both "
        "sources in explicit order, pinned correlating fields, restart restoration and a "
        "cancelled mapping leaving the origin view intact"
    )


if __name__ == "__main__":
    run(pathlib.Path(sys.argv[1]).resolve())
