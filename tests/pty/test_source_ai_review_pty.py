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
                "program": {"exec": {"executable": "/bin/sh", "args": ["-c", "printf 'SOURCE-AI-LAUNCHED\\n'"]}},
                "cwd": "/tmp/controlled source cwd",
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


def click_text(app: PtyApp, text: str) -> None:
    app.drain()
    for y, row in enumerate(app.screen.display):
        if text in row:
            x = row.index(text)
            app.send(f"\x1b[<0;{x + 1};{y + 1}M".encode())
            app.send(f"\x1b[<0;{x + 1};{y + 1}m".encode())
            return
    raise AssertionError(f"missing clickable text {text!r}\n{app.text()}")


def run_case(binary: pathlib.Path, width: int, height: int) -> None:
    root = pathlib.Path(tempfile.mkdtemp(prefix="lvu-source-ai-review-"))
    bridge = root / "bridge.py"
    bridge.write_text(BRIDGE)
    bridge.chmod(0o755)
    archive = root / "requests.jsonl"
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
            "XDG_CONFIG_HOME": str(root / "config"),
            "XDG_DATA_HOME": str(root / "data"),
            "XDG_CACHE_HOME": str(root / "cache"),
        },
    )
    try:
        app.wait_for("FILE PATH", timeout=8)
        click_text(app, "Agent" if "Agent" in app.text() else "🧠")
        request = app.wait_for("Request", timeout=5)
        assert "preview never executes" in request
        app.send(b"follow controlled service logs\r")
        app.wait_for("Start reviewed", timeout=15)
        assert "SOURCE-AI-LAUNCHED" not in app.text()

        observed = app.text()
        for _ in range(24):
            app.send(b"\x1b[B")
            app.drain()
            observed += app.text()
            if "full controlled why" in observed:
                break
        for expected in (
            "Launch:", "Effective path/cwd:", "Restart:",
            "ALPHA=one", "DELTA=four", "Why:", "full controlled why",
        ):
            assert expected in observed, f"missing {expected!r} at {width}x{height}\n{observed}"
        assert "SOURCE-AI-LAUNCHED" not in observed
        app.send(b"\r")
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
