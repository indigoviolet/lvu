#!/usr/bin/env python3
"""The letter a Fields button underlines presses it, in a real terminal.

docs/dialog-system.md §8.10. The reported defect was that `x`, `f` and `d` did
nothing in Fields while `c` worked: the row drew six underlined letters, but
three of them were bound only under Alt, and an xterm with its default
`metaSendsEscape: false` sends Alt-f as the 8-bit character U+00E6, never as a
chord. A letter a dialog does not bind is dropped rather than passed down to
the base screen, so those three reached nothing at all.

This drives the bare letters over a pty at 80x24 and asserts the three
operations actually happened.
"""
import json
import pathlib
import sys
import tempfile

from test_lvu_pty import PtyApp


def run(binary: pathlib.Path) -> None:
    with tempfile.TemporaryDirectory(prefix="lvu-fields-mnemonics-pty-") as directory:
        root = pathlib.Path(directory)
        source = root / "events.log"
        source.write_text(
            "".join(
                json.dumps({"level": level, "msg": f"line {index}"}) + "\n"
                for index, level in enumerate(["INFO", "INFO", "WARN", "INFO", "ERROR"])
            )
        )
        environment = {
            "XDG_CONFIG_HOME": str(root / "config"),
            "XDG_DATA_HOME": str(root / "data"),
            "XDG_CACHE_HOME": str(root / "cache"),
            "LVU_NO_DELIGHT": "1",
            "NO_COLOR": "",
            "COLORTERM": "truecolor",
        }
        app = PtyApp(
            binary,
            [str(source), "--capture-dir", str(root / "capture")],
            width=80,
            height=24,
            environment=environment,
        )
        try:
            app.wait_for("line 4", timeout=10.0)
            app.send(b"i")
            fields = app.wait_for("Fields · record", timeout=8.0)
            for label in ("[ Pin ]", "[ Filter ]", "[ Exclude ]", "[ Fold ]"):
                assert label in fields, f"{label} missing at 80x24:\n{fields}"

            # `x` is E&xclude. Bare, with no text field in this dialog to type
            # into, and no Alt anywhere.
            app.send(b"x")
            excluded = app.wait_for("excluding", timeout=8.0)
            assert "excluding level = " in excluded, excluded

            # `f` is &Filter, and it joins to what is already applied.
            app.send(b"f")
            filtered = app.wait_for("filtering to", timeout=8.0)
            assert "filtering to level" in filtered, filtered
            app.send(b"\x1b")
            membership = app.wait_until(
                lambda text: "Fields · record" not in text and "line 4" in text
                and "line 0" not in text and "line 1" not in text
                and "line 2" not in text and "line 3" not in text,
                "the exact level=ERROR predicate membership",
                timeout=8.0,
            )
            assert '"level": "ERROR"' in membership, membership
            assert '"msg": "line 4"' in membership, membership
            app.send(b"i")
            app.wait_for("Fields · record", timeout=8.0)

            # `d` is Fol&d. On the base screen `d` toggles the Details pane;
            # inside the dialog the dialog's mnemonic wins (§7.5).
            app.send(b"d")
            folded = app.wait_for("folding on level", timeout=8.0)
            assert "Details" not in folded.split("Fields · record")[0], (
                "`d` toggled the base screen's Details pane instead of folding:\n"
                f"{folded}"
            )
            assert "Fields · record" in folded, "the dialog stayed open"

            app.send(b"\x1b")
            app.wait_until(lambda text: "Fields · record" not in text, "Fields closed")
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
    print("Fields mnemonics PTY passed: bare x, f and d excluded, filtered and folded at 80x24")


if __name__ == "__main__":
    run(pathlib.Path(sys.argv[1]).resolve())
