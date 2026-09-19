#!/usr/bin/env python3
"""Automatic log setup stays raw-first and persists one reviewed result.

The real application talks to a deliberately slow fake bridge.  While the
proposal is in flight the canonical rows remain visible and interactive; once
the strict composite proposal arrives, lvu installs the native Enhanced view.
Reopening the same capture proves that the durable receipt suppresses a second
agent request, and the ordinary command-palette revert removes only the
generated view while leaving the captured rows intact.
"""

import json
import os
import pathlib
import shutil
import sys
import time

from test_enrichment_chain_pty import stop
from test_lvu_pty import PtyApp, isolated_environment, release_scratch, scratch_root


FAILURE_LOGDIR = pathlib.Path(
    "/mnt/HC_Volume_106796581/lvu-build/auto-setup-pty-logs"
)


BRIDGE = r'''#!/usr/bin/env python3
import json, os, pathlib, sys, time

archive = pathlib.Path(os.environ["FAKE_BRIDGE_ARCHIVE"])
started = pathlib.Path(os.environ["FAKE_BRIDGE_STARTED"])

for line in sys.stdin:
    request = json.loads(line)
    with archive.open("a") as output:
        output.write(json.dumps(request) + "\n")
    method = request["method"]
    request_id = request["request_id"]
    if method == "start_session":
        time.sleep(float(os.environ.get("FAKE_BRIDGE_START_DELAY", "0")))
        assert request.get("purpose") == "auto_setup"
        assert request.get("title") == "lvu automatic log setup"
        result = {"session_id": "automatic-setup-session"}
    elif method == "request_proposal":
        assert request["kind"] == "auto_setup"
        context = request["context"]
        assert pathlib.Path(context["manifest_path"]).is_file()
        assert context["dataset_paths"] == []
        assert isinstance(context["inline_context"], dict)
        assert len(json.dumps(context["inline_context"], ensure_ascii=False).encode("utf-8")) <= 32768
        assert context.get("inspection_command") is None
        started.write_text("proposal started\n")
        time.sleep(3.0)
        proposal = {
            "kind": "auto_setup",
            "definition": {
                "schema_version": 1,
                "enrichments": [{
                    "id": "severity-stage",
                    "output": "severity",
                    "expression": "pl.col('raw').str.extract(r'level=([A-Z]+)', 1)",
                }],
                "pinned_columns": ["severity"],
                "color_rules": [
                    {"column": "severity", "value": "ERROR", "color": "red"},
                    {"column": "severity", "value": "WARN", "color": "yellow"},
                ],
                "grouping": {"mode": "run", "column": "severity"},
            },
            "explanation": "fixture setup for severity presentation",
            "originating_revision": request["originating_revision"],
        }
        result = {"proposal": proposal}
    elif method == "cancel":
        result = {"cancelled": True, "remote_cancelled": True,
                  "remote_agent_may_still_be_running": False}
    else:
        result = {"accepted": True}
    print(json.dumps({"schema_version": 1, "request_id": request_id,
                      "ok": True, "result": result}), flush=True)
    if method == "cancel":
        print(json.dumps({"schema_version": 1,
                          "session_id": "automatic-setup-session",
                          "kind": "session_archived"}), flush=True)
'''


SETTINGS = """\
schema_version = 1

[paseo]
provider = "fixture/provider"
mode = "full-access"
thinking = "medium"

[appearance]
theme = "terminal"
display_zone = "Z"
delight_enabled = false
reduced_motion = true
ascii = false

[cache.memory]
rows_mib = 4
membership_mib = 256

[cache.disk]
total_mib = 5120
index_per_source_mib = 256
"""


def proposal_count(archive: pathlib.Path) -> int:
    if not archive.exists():
        return 0
    return sum(
        json.loads(line).get("method") == "request_proposal"
        for line in archive.read_text().splitlines()
    )


def sidebar_views(app: PtyApp) -> list[str]:
    names = []
    for line in app.text().splitlines():
        if not line.startswith("│"):
            continue
        entry = line[1:21].replace("›", " ").strip().rstrip("─│┌┐└┘▄▀").strip()
        if not entry or entry.startswith(("●", "Running:", "Setup:")):
            continue
        if any(character in entry for character in "─│┌┐└┘▄▀"):
            continue
        names.append(entry)
    return names


def start_app(binary: pathlib.Path, source: pathlib.Path, root: pathlib.Path, env: dict) -> PtyApp:
    return PtyApp(
        binary,
        [str(source), "--capture-dir", str(root / "capture")],
        width=150,
        height=32,
        environment=env,
    )


def run(binary: pathlib.Path) -> None:
    root = scratch_root()
    source = root / "service.log"
    archive = root / "bridge-requests.jsonl"
    started = root / "bridge-started"
    app = None
    reopened = None
    try:
        source.write_text(
            "level=INFO request=a service ready\n"
            "level=ERROR request=b database failed\n"
            "level=WARN request=c retrying\n"
        )
        config = root / "config" / "lvu"
        config.mkdir(parents=True)
        (config / "settings.toml").write_text(SETTINGS)
        bridge = root / "fake-bridge.py"
        bridge.write_text(BRIDGE)
        bridge.chmod(0o755)
        env = {
            **isolated_environment(root),
            "LVU_AGENT_BRIDGE_PROGRAM": str(bridge),
            "LVU_AGENT_BRIDGE_CWD": str(root),
            "FAKE_BRIDGE_ARCHIVE": str(archive),
            "FAKE_BRIDGE_STARTED": str(started),
            "LVU_PTY_ALLOW_DEFAULT_AUTOMATIC_SETUP": "1",
            "LVU_NO_DELIGHT": "1",
        }

        app = start_app(binary, source, root, env)
        app.wait_until(lambda text: started.exists(), "automatic proposal reached bridge", timeout=20.0)
        raw = app.wait_until(
            lambda text: "database failed" in text and "Enhanced" not in text,
            "raw rows remain visible during asynchronous analysis",
            timeout=10.0,
        )
        assert "Enhanced" not in raw, raw

        enhanced = app.wait_until(
            lambda text: "Enhanced" in text
            and "setup: applied" in text
            and "database failed" in text,
            "strict proposal installed with its inspectable Paseo session",
            timeout=25.0,
        )
        assert "All events" in enhanced and "database failed" in enhanced, enhanced
        assert proposal_count(archive) == 1, archive.read_text()
        time.sleep(0.5)  # let the asynchronous receipt upsert settle before exit
        stop(app)
        app = None

        reopened = start_app(binary, source, root, env)
        restored = reopened.wait_until(
            lambda text: "Enhanced" in text and "database failed" in text,
            "durable Enhanced view restored without another proposal",
            timeout=20.0,
        )
        assert "All events" in restored, restored
        time.sleep(1.8)  # longer than the fake proposal delay
        assert proposal_count(archive) == 1, archive.read_text()

        # The existing object is named first in the palette operation, and the
        # normal view deletion path performs the exact edit-aware revert.
        reopened.send(b"\x10")
        reopened.wait_for("Command palette", timeout=5.0)
        reopened.send(b"Revert automatic setup")
        reopened.wait_until(
            lambda text: "Current view › Revert automatic setup" in text,
            "object-first revert command selected",
            timeout=8.0,
        )
        reopened.send(b"\r")
        reverted = reopened.wait_until(
            lambda text: sidebar_views(reopened) == ["All events"]
            and "database failed" in text,
            "automatic view removed while raw rows remain",
            timeout=15.0,
        )
        assert "All events" in reverted, reverted
        stop(reopened)
        reopened = None

        requests = [json.loads(line) for line in archive.read_text().splitlines()]
        started_session = next(item for item in requests if item.get("method") == "start_session")
        assert started_session["purpose"] == "auto_setup"
        assert started_session["title"] == "lvu automatic log setup"
        proposal = next(item for item in requests if item.get("method") == "request_proposal")
        assert proposal["kind"] == "auto_setup"
        assert proposal["context"]["dataset_paths"] == []
        assert proposal_count(archive) == 1
    except BaseException:
        FAILURE_LOGDIR.mkdir(parents=True, exist_ok=True)
        stamp = str(time.time_ns())
        for name, candidate in (("active", reopened or app),):
            if candidate is not None:
                (FAILURE_LOGDIR / f"{name}-screen-{stamp}.txt").write_text(candidate.text())
                (FAILURE_LOGDIR / f"{name}-transcript-{stamp}.bin").write_bytes(
                    bytes(candidate.transcript)
                )
        if archive.exists():
            shutil.copyfile(archive, FAILURE_LOGDIR / f"requests-{stamp}.jsonl")
        raise
    finally:
        for candidate in (reopened, app):
            if candidate is not None and candidate.process.poll() is None:
                candidate.process.kill()
                candidate.process.wait(timeout=5)
                candidate.close()
        release_scratch(root)


if __name__ == "__main__":
    run(pathlib.Path(sys.argv[1]).resolve())
    print("Automatic setup PTY passed: raw-first, one durable proposal, exact revert")
