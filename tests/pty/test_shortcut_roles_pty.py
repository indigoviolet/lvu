#!/usr/bin/env python3
"""AI role shortcuts through the real application with a fake agent.

Failure artifacts: the full pty transcript plus the fake bridge archive go
to the volume log dir, because the 4000-byte tail in the assertion message
cannot show how a dialog opened, rendered, and closed.

The fake bridge speaks the agent protocol over stdio (LVU_AGENT_BRIDGE_PROGRAM
exists exactly for deterministic protocol testing) and answers every severity
proposal with the same reviewed enrichment. The test proves the shortcut flow:
palette task -> prefilled Ask -> Submit -> proposal review -> Apply -> step
editor Save -> accepted chain assigns the role with no Fields visit. Reject
(Esc at review) preserves the previous role; renaming the output before saving
assigns nothing. Stale completions are covered by deterministic Rust tests
instead of a raced PTY.
"""

import os
import pathlib
import shutil
import sys
import tempfile
import time

from test_lvu_pty import PtyApp
from test_enrichment_chain_pty import close_editor, paste, stop

FAILURE_LOGDIR = pathlib.Path(
    "/mnt/HC_Volume_106796581/lvu-build/lvu-muse-enrichment-logs"
)


BRIDGE = r'''#!/usr/bin/env python3
import json, os, pathlib, sys
archive = pathlib.Path(os.environ["FAKE_BRIDGE_ARCHIVE"])
for line in sys.stdin:
    request = json.loads(line)
    with archive.open("a") as output:
        output.write(json.dumps(request) + "\n")
    method = request.get("method")
    rid = request.get("request_id")
    if method == "start_session":
        result = {"session_id": "pty-shortcut"}
    elif method == "request_proposal":
        print(json.dumps({"schema_version": 1, "session_id": "pty-shortcut",
                          "kind": "proposal_started", "request_id": rid}), flush=True)
        result = {"proposal": {
            "kind": "enrichment",
            "definition": {"schema_version": 1, "stages": [{
                "id": "33333333-3333-4333-8333-333333333333",
                "name": "severity-step",
                "expressions": {"severity": "pl.col(\"level\").str.to_uppercase()"},
            }]},
            "explanation": "controlled severity derivation for role assignment",
            "originating_revision": request["originating_revision"],
        }}
    elif method == "cancel":
        result = {"cancelled": True, "remote_cancelled": True,
                  "remote_agent_may_still_be_running": False}
    else:
        result = {"accepted": True}
    print(json.dumps({"schema_version": 1, "request_id": rid,
                      "ok": True, "result": result}), flush=True)
    # Ephemeral Ask sessions archive once answered: without this event the
    # app keeps the session busy and refuses the next request with "another
    # agent request is still settling".
    if method in ("request_proposal", "cancel"):
        print(json.dumps({"schema_version": 1, "session_id": "pty-shortcut",
                          "kind": "session_archived"}), flush=True)
'''


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


def open_severity_task(app):
    app.send(b"\x10")
    app.wait_for("Command palette", timeout=5.0)
    app.send(b"Severity column")
    # The › marker proves the row is selected; a bare text wait would match
    # while typing is still in flight and Enter would hit the wrong row.
    app.wait_until(lambda text: "\u203a Severity column" in text,
                   "shortcut row selected", timeout=5.0)
    app.send(b"\r")
    app.wait_for("[ Submit ]", timeout=8.0)


def submit_and_review(app):
    click_text(app, "[ Submit ]")
    reviewed = app.wait_until(
        lambda text: "severity = " in text,
        "fake proposal under review", timeout=30.0)
    assert "controlled severity derivation" in reviewed, reviewed
    return reviewed


def open_enrichment_list(app):
    app.send(b"e")
    app.wait_for("External command", timeout=8.0)


def run(binary):
    home = pathlib.Path.home()
    tooling = {}
    for key, xdg, fallback, suffix in [
        ("MISE_DATA_DIR", "XDG_DATA_HOME", home / ".local/share", "mise"),
        ("MISE_CONFIG_DIR", "XDG_CONFIG_HOME", home / ".config", "mise"),
        ("MISE_CACHE_DIR", "XDG_CACHE_HOME", home / ".cache", "mise"),
        ("UV_CACHE_DIR", "XDG_CACHE_HOME", home / ".cache", "uv"),
    ]:
        tooling[key] = os.environ.get(key, str(pathlib.Path(os.environ.get(xdg, fallback)) / suffix))
    with tempfile.TemporaryDirectory(prefix="lvu-shortcut-pty-") as directory:
        root = pathlib.Path(directory)
        source = root / "events.log"
        source.write_text("level=warn first\nlevel=info second\n")
        bridge = root / "bridge.py"
        bridge.write_text(BRIDGE)
        bridge.chmod(0o755)
        archive = root / "requests.jsonl"
        environment = {**tooling, "XDG_CONFIG_HOME": str(root / "config"),
                       "XDG_DATA_HOME": str(root / "data"), "XDG_CACHE_HOME": str(root / "cache"),
                       "LVU_AGENT_BRIDGE_PROGRAM": str(bridge),
                       "LVU_AGENT_BRIDGE_CWD": str(root),
                       "FAKE_BRIDGE_ARCHIVE": str(archive)}
        arguments = [str(source), "--capture-dir", str(root / "capture")]
        app = PtyApp(binary, arguments, width=150, height=38, environment=environment)
        try:
            app.wait_for("level=info second", timeout=15.0)
            assert "WARN" not in app.text()

            # Unedited proposal: Ask-apply enqueues it and acceptance assigns
            # the role with no Fields visit and no manual save.
            open_severity_task(app)
            submit_and_review(app)
            click_text(app, "[ Apply ]")
            app.wait_until(lambda text: "1  severity" in text
                           and "Applied" in text,
                           "proposed step accepted", timeout=25.0)
            close_editor(app)
            app.wait_until(lambda text: "WARN" in text,
                           "shortcut assigned the severity role", timeout=15.0)

            # Rejecting a new proposal changes nothing: the role stays set.
            open_severity_task(app)
            submit_and_review(app)
            app.send(b"\x1b")
            app.wait_until(lambda text: "Severity column" not in text,
                           "rejected proposal dismissed", timeout=8.0)
            app.wait_until(lambda text: "WARN" in text,
                           "rejection preserves the role", timeout=10.0)

            # Renaming the accepted step drops the intended output: the role
            # must not follow the new name. The chain is settled here, so no
            # earlier submission can still be in flight to race the edit.
            open_enrichment_list(app)
            click_text(app, "[ Edit ]")
            app.wait_for("Enrichment \u203a Edit step", timeout=10.0)
            app.send(b"\x01\x0b")
            paste(app, "loudness = pl.col(\"level\").str.to_uppercase()")
            # The draft must show the edit before Save runs: sending Enter
            # into a field that has not caught up saves the pristine draft.
            app.wait_until(lambda text: "loudness = " in text,
                           "renamed draft visible", timeout=10.0)
            app.send(b"\r")
            app.wait_until(lambda text: "loudness" in text and "External command" in text,
                           "renamed step accepted", timeout=20.0)
            close_editor(app)
            app.wait_until(lambda text: "WARN" not in text,
                           "no role follows a renamed output", timeout=10.0)

            archived = archive.read_text()
            assert '"method": "request_proposal"' in archived, archived
            assert '"kind": "enrichment"' in archived, archived
            stop(app)
        except BaseException as original:
            try:
                FAILURE_LOGDIR.mkdir(parents=True, exist_ok=True)
                stamp = f"{time.time_ns()}"
                (FAILURE_LOGDIR / f"shortcut-screen-{stamp}.txt").write_text(
                    app.text()
                )
                (FAILURE_LOGDIR / f"shortcut-transcript-{stamp}.bin").write_bytes(
                    bytes(app.transcript)
                )
                shutil.copyfile(
                    archive,
                    FAILURE_LOGDIR / f"shortcut-requests-{stamp}.jsonl",
                )
            except Exception as artifact_error:
                print(f"artifact preservation failed: {artifact_error}")
            raise
        finally:
            if app.process.poll() is None:
                app.process.kill()
                app.process.wait(timeout=5)
                app.close()


if __name__ == "__main__":
    binary = pathlib.Path(sys.argv[1]).resolve()
    run(binary)
    print(f"Shortcut roles PTY passed on {binary}: accept assigns, reject and rename preserve")
