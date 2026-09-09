#!/usr/bin/env python3
"""Verify bounded multi-source assistance review and per-item admission."""

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
partial = os.environ.get("FAKE_PARTIAL") == "1"
for line in sys.stdin:
    request = json.loads(line)
    with archive.open("a") as output:
        output.write(json.dumps(request) + "\n")
    if request["method"] == "start_session":
        result = {"session_id": "bounded-multi-review"}
    elif request["method"] == "request_proposal":
        first = {
            "schema_version": 1,
            "id": "33333333-3333-4333-8333-333333333333",
            "name": "first reviewed source",
            "kind": "command",
            "command": {
                "program": {"exec": {"executable": "/bin/sh", "args": ["-c", "printf 'MULTI-%s\\n' FIRST"]}},
                "cwd": os.environ["FAKE_SOURCE_CWD"],
                "environment": {},
                "restart": "never"
            },
            "identity_hints": {"fixture": "multi-first"},
            "retention": None,
        }
        if partial:
            second = {
                "schema_version": 1,
                "id": "44444444-4444-4444-8444-444444444444",
                "name": "bad source",
                "kind": "command",
                "command": {
                    "program": {"exec": {"executable": "/bin/sh", "args": ["-c", "printf 'MULTI-%s\\n' SECOND"]}},
                    "cwd": os.path.join(os.environ["FAKE_SOURCE_CWD"], "missing cwd"),
                    "environment": {},
                    "restart": "never"
                },
                "identity_hints": {"fixture": "multi-bad"},
                "retention": None,
            }
        else:
            second = {
                "schema_version": 1,
                "id": "44444444-4444-4444-8444-444444444444",
                "name": "second reviewed source",
                "kind": "command",
                "command": {
                    "program": {"exec": {"executable": "/bin/sh", "args": ["-c", "printf 'MULTI-%s\\n' SECOND"]}},
                    "cwd": os.environ["FAKE_SOURCE_CWD"],
                    "environment": {},
                    "restart": "never"
                },
                "identity_hints": {"fixture": "multi-second"},
                "retention": None,
            }
        result = {"proposal": {
            "kind": "sources",
            "definition": {"schema_version": 1, "sources": [first, second]},
            "explanation": "two bounded discovery candidates match the request",
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


# The review intentionally DISPLAYS each proposed command, so neither executed
# sentinel may appear literally in its command text. Both commands assemble
# their sentinel at runtime; seeing one on screen proves execution, not preview.
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
    for row in text.splitlines():
        if "Preview · lines " in row:
            window = row.split("Preview · lines ", 1)[1].split(" ·", 1)[0]
            return int(window.split("–", 1)[0])
    return None


def scroll_review(app: PtyApp) -> str:
    observed = app.text()
    for _ in range(32):
        if "two bounded discovery candidates match" in observed:
            break
        first = preview_first_line(app.text())
        app.send(b"\x1b[B")
        if first is None:
            app.drain()
        else:
            app.wait_until(
                lambda text: (preview_first_line(text) or 0) > first,
                f"the proposal preview to scroll past line {first}",
                timeout=5,
            )
        observed += app.text()
    return observed


def launch(binary: pathlib.Path, root: pathlib.Path, width: int, height: int, partial: bool) -> PtyApp:
    bridge = root / "bridge.py"
    bridge.write_text(BRIDGE)
    bridge.chmod(0o755)
    source_cwd = root / "controlled source cwd"
    source_cwd.mkdir(exist_ok=True)
    return PtyApp(
        binary,
        ["--capture-dir", str(root / "capture")],
        width=width,
        height=height,
        cwd=root,
        environment={
            "LVU_NO_DELIGHT": "1",
            "LVU_AGENT_BRIDGE_PROGRAM": str(bridge),
            "LVU_AGENT_BRIDGE_CWD": str(root),
            "FAKE_BRIDGE_ARCHIVE": str(root / "requests.jsonl"),
            "FAKE_SOURCE_CWD": str(source_cwd),
            "FAKE_PARTIAL": "1" if partial else "0",
            "XDG_CONFIG_HOME": str(root / "config"),
            "XDG_DATA_HOME": str(root / "data"),
            "XDG_CACHE_HOME": str(root / "cache"),
        },
    )


def request_proposal_kind(root: pathlib.Path) -> str:
    for line in (root / "requests.jsonl").read_text().splitlines():
        request = json.loads(line)
        if request.get("method") == "request_proposal":
            return request["kind"]
    raise AssertionError("no request_proposal reached the bridge")


def review(app: PtyApp, width: int) -> str:
    app.wait_for("Kind", timeout=8)
    click_text(app, "Agent" if "Agent" in app.text() else "🧠")
    request = app.wait_for("Describe", timeout=5)
    assert "preview never executes" in request
    app.send(b"follow the api and worker services\r")
    app.wait_for("Start 2 reviewed", timeout=15)
    if width >= 100:
        assert "Start 2 reviewed sources" in app.text()
    assert "MULTI-FIRST" not in app.text()
    assert "MULTI-SECOND" not in app.text()
    observed = scroll_review(app)
    for expected in (
        "Source 1 of 2: first reviewed source",
        "Effective path/cwd:",
        "Restart:",
        "Why:",
        "two bounded discovery candidates match",
    ):
        assert expected in observed, f"missing {expected!r}\n{observed}"
    assert "MULTI-FIRST" not in observed
    assert "MULTI-SECOND" not in observed
    return observed


def collect_both_outputs(app: PtyApp) -> None:
    # Either source may report first, so the selected view after Apply is
    # whichever completed first. Walk the views and require both sentinels.
    seen = set()
    for key in (None, b"]", b"[", b"]"):
        if key is not None:
            before = app.text()
            app.send(key)
            app.wait_until(
                lambda text: text != before,
                "the view selection to move",
                timeout=5,
            )
        text = app.text()
        seen |= {sentinel for sentinel in ("MULTI-FIRST", "MULTI-SECOND") if sentinel in text}
        if seen == {"MULTI-FIRST", "MULTI-SECOND"}:
            return
    raise AssertionError(f"only saw {sorted(seen)} across both views\n{app.text()}")


def run_success(binary: pathlib.Path, width: int, height: int) -> None:
    root = pathlib.Path(tempfile.mkdtemp(prefix="lvu-source-ai-multi-"))
    app = launch(binary, root, width, height, partial=False)
    try:
        observed = review(app, width)
        assert "second reviewed source" in observed, f"missing second item\n{observed}"
        assert request_proposal_kind(root) == "sources"
        # A repeated confirmation while the Apply settles must neither admit
        # twice nor disturb the review: the second Enter is not actionable.
        app.send(b"\r\r")
        if width < 100:
            app.wait_until(
                lambda text: "MULTI-" in text and "Start 2 reviewed" not in text,
                "the reviewed sources to capture their output",
                timeout=10,
            )
            app.resize(140, 28)
        # Settle closes the review; then both captured outputs must be
        # reachable across the two views, in either completion order.
        app.wait_for("started 2 of 2 reviewed sources", timeout=10)
        collect_both_outputs(app)
    finally:
        if app.process.poll() is None:
            app.process.kill()
            app.process.wait(timeout=5)
        app.close()


def run_partial(binary: pathlib.Path) -> None:
    root = pathlib.Path(tempfile.mkdtemp(prefix="lvu-source-ai-multi-partial-"))
    app = launch(binary, root, 140, 28, partial=True)
    try:
        observed = review(app, 140)
        assert "bad source" in observed, f"missing failing item\n{observed}"
        app.send(b"\r")
        # The good source captures; the bad one reports its cwd failure in the
        # same summary rather than being overwritten by the success.
        app.wait_for("MULTI-FIRST", timeout=10)
        app.wait_for("started 1 of 2 reviewed sources", timeout=10)
        app.wait_until(
            lambda text: "failed: bad source" in text,
            "the partial-failure summary to name its item",
            timeout=10,
        )
        assert "MULTI-SECOND" not in app.text()
    finally:
        if app.process.poll() is None:
            app.process.kill()
            app.process.wait(timeout=5)
        app.close()


def run(binary: pathlib.Path) -> None:
    run_success(binary, 140, 28)
    run_success(binary, 54, 16)
    run_partial(binary)


if __name__ == "__main__":
    run(pathlib.Path(sys.argv[1]).resolve())
    print("Source AI multi review PTY passed: batch review, explicit apply, and partial failure")
