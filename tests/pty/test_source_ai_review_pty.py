#!/usr/bin/env python3
"""Verify bounded Source-agent review and explicit launch in a real terminal."""

from __future__ import annotations

import json
import os
import pathlib
import sys
import tempfile

from test_lvu_pty import PtyApp


BRIDGE = r'''#!/usr/bin/env python3
import json, os, pathlib, sys
archive = pathlib.Path(os.environ["FAKE_BRIDGE_ARCHIVE"])
for line in sys.stdin:
    request = json.loads(line)
    with archive.open("a") as output:
        output.write(json.dumps(request) + "\n")
    if request["method"] == "start_session":
        result = {"session_id": "bounded-source-review"}
    elif request["method"] == "request_proposal":
        definition = {
            "schema_version": 1,
            "id": "33333333-3333-4333-8333-333333333333",
            "name": "reviewed command source",
            "kind": "command",
            "command": {
                "program": {"exec": {"executable": "/bin/sh", "args": ["-c", "printf 'SOURCE-AI-%s\\n' LAUNCHED"]}},
                "cwd": os.environ["FAKE_SOURCE_CWD"],
                "environment": {"ALPHA": "one", "BETA": "two", "GAMMA": "three", "DELTA": "four"},
                "restart": "never"
            },
            "identity_hints": {"fixture": "bounded-review"},
            "retention": None,
        }
        result = {"proposal": {
            "kind": "source", "definition": definition,
            "explanation": "full controlled why evidence remains reviewable before explicit start",
            "originating_revision": request["originating_revision"],
        }}
    elif request["method"] == "cancel":
        result = {"cancelled": True, "remote_cancelled": True,
                  "remote_agent_may_still_be_running": False}
    else:
        result = {"accepted": True}
    print(json.dumps({"schema_version": 1, "request_id": request["request_id"],
                      "ok": True, "result": result}), flush=True)
'''


# The proposal review intentionally DISPLAYS the proposed command, so the
# executed-output sentinel must not appear literally in that command text.
# The command assembles "SOURCE-AI-LAUNCHED" at runtime; seeing it on screen
# therefore proves execution rather than preview.
def click_text(app: PtyApp, text: str) -> None:
    app.drain()
    for y, row in enumerate(app.screen.display):
        if text in row:
            x = row.index(text)
            app.send(f"\x1b[<0;{x + 1};{y + 1}M".encode())
            app.send(f"\x1b[<0;{x + 1};{y + 1}m".encode())
            return
    raise AssertionError(f"missing clickable text {text!r}\n{app.text()}")


def preview_first_line(text: str) -> int | None:
    """First preview line the pane currently shows, or None when it all fits.

    The app coalesces redraws, so keys sent back to back can all be consumed
    before a single frame is emitted. Reading this counter lets the test wait
    for the terminal the user actually looks at instead of racing it.
    """
    for row in text.splitlines():
        if "lines " in row and " of " in row:
            window = row.split("lines ", 1)[1].split(" of ", 1)[0]
            return int(window.split("–", 1)[0])
    return None


def run_case(binary: pathlib.Path, width: int, height: int) -> None:
    root = pathlib.Path(tempfile.mkdtemp(prefix="lvu-source-ai-review-"))
    bridge = root / "bridge.py"
    bridge.write_text(BRIDGE)
    bridge.chmod(0o755)
    archive = root / "requests.jsonl"
    # Deliberately spaced directory name; it must exist or the reviewed launch
    # correctly fails with a cwd I/O error instead of exercising the review flow.
    source_cwd = root / "controlled source cwd"
    source_cwd.mkdir()
    app = PtyApp(
        binary,
        ["--capture-dir", str(root / "capture")],
        width=width,
        height=height,
        cwd=root,
        environment={
            "LVU_NO_DELIGHT": "1",
            "LVU_AGENT_BRIDGE_PROGRAM": str(bridge),
            "LVU_AGENT_BRIDGE_CWD": str(root),
            "FAKE_BRIDGE_ARCHIVE": str(archive),
            "FAKE_SOURCE_CWD": str(source_cwd),
            "XDG_CONFIG_HOME": str(root / "config"),
            "XDG_DATA_HOME": str(root / "data"),
            "XDG_CACHE_HOME": str(root / "cache"),
        },
    )
    try:
        # dialog-system.md §7.2 retires the ALL-CAPS label; §12.7 names the
        # manual field Path and puts the modes in a segmented control.
        app.wait_for("Kind", timeout=8)
        click_text(app, "Agent" if "Agent" in app.text() else "🧠")
        request = app.wait_for("Describe", timeout=5)
        # §7.1 moved the promise out of the title into the sticky message row.
        assert "preview never executes" in request
        app.send(b"follow controlled service logs\r")
        app.wait_for("Start reviewed", timeout=15)
        assert "SOURCE-AI-LAUNCHED" not in app.text()

        observed = app.text()
        for _ in range(24):
            if "full controlled why" in observed:
                break
            first = preview_first_line(app.text())
            app.send(b"\x1b[B")
            if first is None:
                # Nothing is clipped; the whole proposal is already on screen.
                app.drain()
            else:
                app.wait_until(
                    lambda text: (preview_first_line(text) or 0) > first,
                    f"the proposal preview to scroll past line {first}",
                    timeout=5,
                )
            observed += app.text()
        for expected in (
            "Launch:", "Effective path/cwd:", "Restart:",
            "ALPHA=one", "DELTA=four", "Why:", "full controlled why",
        ):
            assert expected in observed, f"missing {expected!r} at {width}x{height}\n{observed}"
        assert "SOURCE-AI-LAUNCHED" not in observed

        # Reviewing must not depend on one input device: the wheel over the
        # preview pane has to walk the same content back to the first field.
        title_row = next(
            (
                index
                for index, row in enumerate(app.text().splitlines())
                if "lines " in row and " of " in row
            ),
            None,
        )
        if title_row is not None:
            wheel_up = f"\x1b[<64;10;{title_row + 2}M".encode()
            for _ in range(24):
                first = preview_first_line(app.text())
                if first is None or first == 1:
                    break
                app.send(wheel_up)
                app.wait_until(
                    lambda text: (preview_first_line(text) or 1) < first,
                    f"the wheel to scroll the preview above line {first}",
                    timeout=5,
                )
            assert preview_first_line(app.text()) == 1, (
                f"wheel did not reach the top of the review\n{app.text()}"
            )
            assert "Name: reviewed command source" in app.text()
        assert "SOURCE-AI-LAUNCHED" not in app.text()

        app.send(b"\r")
        if width < 100:
            # A 54-column event column cannot print the whole sentinel. The
            # review and the confirmation both happened at the small size;
            # widen only to read back the row the launch actually captured.
            app.wait_until(
                lambda text: "SOURCE-AI" in text and "Start reviewed" not in text,
                "the reviewed source to capture its output",
                timeout=10,
            )
            app.resize(140, 28)
        app.wait_for("SOURCE-AI-LAUNCHED", timeout=10)
    finally:
        if app.process.poll() is None:
            app.process.kill()
            app.process.wait(timeout=5)
        app.close()


def run(binary: pathlib.Path) -> None:
    run_case(binary, 140, 28)
    run_case(binary, 54, 16)


if __name__ == "__main__":
    run(pathlib.Path(sys.argv[1]).resolve())
    print("Source AI review PTY passed: bounded details and explicit start at 140x28 and 54x16")
