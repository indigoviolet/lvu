#!/usr/bin/env python3
"""The explicit Analyze-again path visibly reaches a real setup session.

Automatic setup is disabled in this fixture.  The only route into the bridge
is therefore the user's Current log > Analyze again palette action.  A slow
session creation gives the terminal a real sampling frame to show, then the
same status inspector must update in place with the actual Paseo session id.
"""

import json
import pathlib
import shutil
import sys
import time

from test_auto_setup_pty import BRIDGE, FAILURE_LOGDIR, SETTINGS, proposal_count, stop
from test_lvu_pty import PtyApp, isolated_environment, release_scratch, scratch_root


DISABLED_AUTOMATIC_SETUP = """

[automatic_setup]
policy = "disabled"
"""


def start_app(binary: pathlib.Path, source: pathlib.Path, root: pathlib.Path, env: dict) -> PtyApp:
    return PtyApp(
        binary,
        [str(source), "--capture-dir", str(root / "capture")],
        width=150,
        height=32,
        environment=env,
    )


def choose(app: PtyApp, query: bytes, expected: str) -> None:
    app.send(b"\x10")
    app.wait_for("Command palette", timeout=5.0)
    app.send(query)
    app.wait_until(lambda text: expected in text, f"palette selects {expected}", timeout=8.0)
    app.send(b"\r")


def run(binary: pathlib.Path) -> None:
    root = scratch_root()
    source = root / "manual.log"
    archive = root / "bridge-requests.jsonl"
    started = root / "bridge-started"
    app = None
    try:
        source.write_text("level=INFO request=a ready\nlevel=ERROR request=b failed\n")
        config = root / "config" / "lvu"
        config.mkdir(parents=True)
        (config / "settings.toml").write_text(SETTINGS + DISABLED_AUTOMATIC_SETUP)
        bridge = root / "fake-bridge.py"
        bridge.write_text(BRIDGE)
        bridge.chmod(0o755)
        env = {
            **isolated_environment(root),
            "LVU_AGENT_BRIDGE_PROGRAM": str(bridge),
            "LVU_AGENT_BRIDGE_CWD": str(root),
            "FAKE_BRIDGE_ARCHIVE": str(archive),
            "FAKE_BRIDGE_STARTED": str(started),
            # Keep session creation long enough for a real terminal frame to
            # prove that Analyze again never looks inert.
            # Palette opening/rendering deliberately settles complete frames;
            # keep this above both palette interactions so the inspector is
            # observed before a session exists, not merely after it races in.
            "FAKE_BRIDGE_START_DELAY": "8.0",
            "LVU_NO_DELIGHT": "1",
        }
        app = start_app(binary, source, root, env)
        app.wait_for("request=b failed", timeout=12.0)
        time.sleep(0.8)
        assert proposal_count(archive) == 0, "disabled automatic setup started a bridge request"

        choose(app, b"Analyze again", "Current log › Analyze again")
        sampling = app.wait_until(
            lambda text: "setup: sampling" in text or "setup: analyzing" in text,
            "manual Analyze again visibly acknowledges queueing or analysis",
            timeout=5.0,
        )
        assert "request=b failed" in sampling, sampling

        choose(app, b"Setup status", "Current log › Setup status")
        waiting = app.wait_until(
            lambda text: "Automatic setup status" in text
            and "Paseo session: creating a Paseo conversation" in text,
            "inspector identifies the pre-session state",
            timeout=5.0,
        )
        assert "Log: manual.log / All events" in waiting, waiting
        assert "Analyze again" in waiting and "Close" in waiting, waiting

        running = app.wait_until(
            lambda text: "Paseo session: automatic-setup-session" in text,
            "inspector updates with the real Paseo session",
            timeout=12.0,
        )
        assert "automatic setup: analyzing" in running, running
        assert proposal_count(archive) == 1, archive.read_text()
        requests = [json.loads(line) for line in archive.read_text().splitlines()]
        assert next(item for item in requests if item["method"] == "start_session")["purpose"] == "auto_setup"
        stop(app)
        app = None
    except BaseException:
        FAILURE_LOGDIR.mkdir(parents=True, exist_ok=True)
        stamp = str(time.time_ns())
        if app is not None:
            (FAILURE_LOGDIR / f"manual-screen-{stamp}.txt").write_text(app.text())
            (FAILURE_LOGDIR / f"manual-transcript-{stamp}.bin").write_bytes(bytes(app.transcript))
        if archive.exists():
            shutil.copyfile(archive, FAILURE_LOGDIR / f"manual-requests-{stamp}.jsonl")
        raise
    finally:
        if app is not None and app.process.poll() is None:
            app.process.kill()
            app.process.wait(timeout=5)
            app.close()
        release_scratch(root)


if __name__ == "__main__":
    run(pathlib.Path(sys.argv[1]).resolve())
    print("Manual automatic-setup PTY passed: visible retry, status inspector and Paseo session")
