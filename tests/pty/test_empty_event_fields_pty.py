#!/usr/bin/env python3
"""Fields opens honestly on empty and field-bearing events, and closes back
to usable raw browsing from either state.

The close proof is a bound tied to app-signaled state, not a wider number:
Escape, then dialog-chrome absence plus base rows (never optional status
wording or list content), then a base-screen key whose effect only the base
screen can produce. `d` is dropped while any layer owns the keys, so an
opening Details pane proves the modal is gone and raw browsing answers.
"""
import pathlib
import sys
import tempfile

from test_lvu_pty import PtyApp


def close_fields_and_prove_usable(app: PtyApp) -> None:
    """Escape out of Fields and prove raw browsing is usable again.

    The close wait keys on the `Fields · record N` title chrome every
    anchored state carries (empty list, pending message row, field rows)
    plus the base rows the dialog covered, so it holds across empty,
    pending and event content without depending on which one is shown and
    without reading optional status wording.
    """
    app.send(b"\x1b")
    app.wait_until(
        lambda text: "Fields · record" not in text
        and "plain unstructured line" in text
        and "structured event" in text,
        "Fields closed, base rows restored",
    )
    app.send(b"d")
    app.wait_for("Selected event details")
    app.send(b"d")
    app.wait_until(lambda text: "Selected event details" not in text, "Details closed")


def run(binary: pathlib.Path) -> None:
    with tempfile.TemporaryDirectory(prefix="lvu-empty-fields-pty-") as directory:
        root = pathlib.Path(directory)
        source = root / "plain.log"
        source.write_text(
            "plain unstructured line\n"
            '{"level": "info", "msg": "structured event"}\n'
        )
        environment = {
            "XDG_CONFIG_HOME": str(root / "config"),
            "XDG_DATA_HOME": str(root / "data"),
            "XDG_CACHE_HOME": str(root / "cache"),
            "NO_COLOR": "",
            "COLORTERM": "truecolor",
        }
        app = PtyApp(
            binary,
            [str(source), "--capture-dir", str(root / "capture")],
            width=96,
            height=20,
            environment=environment,
        )
        try:
            base = app.wait_for("structured event", timeout=8.0)
            assert "plain unstructured line" in base

            # Event context first. FOLLOW tails the initial selection, and `G`
            # pins it to the last row explicitly so the context under test
            # does not depend on selection-init behavior. `G` then `i` ride
            # one byte stream, so the app applies them in order; the single
            # wait below validates their combined effect.
            app.send(b"G")
            app.send(b"i")
            event = app.wait_until(
                lambda text: "Fields · record 1" in text and '"info"' in text,
                "Fields open on the structured event",
            )
            assert "msg" in event
            assert "[ Pin ]" in event
            assert "[ Correlate ]" in event
            assert "[ Raw context ]" not in event
            assert "No fields for this record" not in event
            close_fields_and_prove_usable(app)

            # Empty context. `g` pins selection to the first row, batched with
            # `i` for the same ordering reason as above.
            app.send(b"g")
            app.send(b"i")
            fields = app.wait_for("No fields for this record")
            assert "Fields · record 0" in fields
            assert "Space pin" not in fields
            assert "[ Color ]" not in fields
            assert "r correlate" not in fields
            # The empty state offers the record itself under its new name.
            assert "[ Inspect context ]" in fields

            # While the empty layer owns the keys, a base-screen key reaches
            # nothing: `d` must not open Details here. This pins the invariant
            # the usability probe in close_fields_and_prove_usable relies on.
            app.send(b"d")
            app.assert_remains(
                "No fields for this record", "Selected event details", duration=0.15
            )

            # Raw context is a jump to All events; this is All events, so
            # the shell says so and Fields comes straight back. One combined
            # wait replaces the two sequential ones: the re-pushed dialog plus
            # the raw-stream notice kernel. The kernel avoids depending on the
            # notice's optional trailing wording, and the combination cannot
            # pass on the pre-`o` screen the second old wait accepted.
            app.send(b"o")
            app.wait_until(
                lambda text: "Fields · record" in text and "raw stream" in text,
                "Fields returned on the raw stream",
            )
            close_fields_and_prove_usable(app)
            app.send(b"q")
            assert app.wait_exit(timeout=8) == 0
            app.assert_restored()
        finally:
            if app.process.poll() is None:
                app.send(b"\x03")
                try:
                    app.wait_exit(5)
                except Exception:
                    app.process.kill()
                    app.process.wait()
            app.close()
    print("Empty/event Fields PTY passed: honest empty and field-bearing states, raw context, deterministic close with usable raw browsing")


if __name__ == "__main__":
    run(pathlib.Path(sys.argv[1]).resolve())
