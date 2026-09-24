#!/usr/bin/env python3
"""A rejected native proposal feeds explicit retry without losing raw data.

The model reply is controlled; compilation and execution use the real helper
and Rust engine. Real-provider acceptance is a separate opt-in integration gate.
"""
import hashlib
import pathlib
import sys
import time

from test_auto_setup_pty import BRIDGE, SETTINGS, choose, proposal_count, start_app
from test_enrichment_chain_pty import stop
from test_lvu_pty import isolated_environment, release_scratch, scratch_root


def run(binary):
    root = scratch_root()
    (root / ".lvu-test-reproducer").write_text("owned automatic setup retry fixture\n")
    source = root / "service.log"
    source.write_text("level=INFO service ready\nlevel=ERROR database failed\nlevel=WARN retrying\n")
    config = root / "config/lvu"
    config.mkdir(parents=True)
    (config / "settings.toml").write_text(SETTINGS)
    bridge = root / "bridge.py"
    bridge.write_text(BRIDGE)
    bridge.chmod(0o755)
    archive = root / "requests.jsonl"
    env = {**isolated_environment(root), "LVU_AGENT_BRIDGE_PROGRAM": str(bridge),
           "LVU_AGENT_BRIDGE_CWD": str(root), "FAKE_BRIDGE_ARCHIVE": str(archive),
           "FAKE_BRIDGE_STARTED": str(root / "started"), "FAKE_SETUP_REQUIRE_REPAIR": "1",
           "LVU_PTY_ALLOW_DEFAULT_AUTOMATIC_SETUP": "1", "LVU_NO_DELIGHT": "1"}
    app = None
    try:
        app = start_app(binary, source, root, env)
        app.wait_for("retrying", timeout=20)
        # Original-byte invariance is independent of derived query evaluation.
        journals = lambda: {str(p): hashlib.sha256(p.read_bytes()).hexdigest()
                            for p in (root / "capture").glob("*/capture.journal")}
        before = journals()
        assert before
        app.wait_for("setup: review", timeout=20)
        choose(app, b"Setup status", "Current log › Setup status")
        app.wait_for("to_upper()", timeout=5)
        app.send(b"\x1bp")
        app.wait_for("setup: unavailable", timeout=15)
        choose(app, b"Setup status", "Current log › Setup status")
        failure = app.wait_until(lambda text: "Automatic setup status" in text and "compiler rejected expression" in text,
                                 "native failure remains inspectable", timeout=5)
        assert "Paseo session: automatic-setup-session" in failure, failure
        assert journals() == before
        app.send(b"\x1ba")
        app.wait_for("setup: review", timeout=20)
        choose(app, b"Setup status", "Current log › Setup status")
        app.wait_for("to_uppercase()", timeout=5)
        app.send(b"\x1bp")
        applied = app.wait_until(lambda text: "setup: applied" in text and "Enhanced" in text and "severity" in text,
                                 "corrected native proposal applied", timeout=15)
        assert "database failed" in applied, applied
        assert proposal_count(archive) == 2
        assert journals() == before
        stop(app)
        app = None
    except BaseException:
        proof = pathlib.Path("/mnt/HC_Volume_106796581/lvu-build/auto-setup-pty-logs")
        proof.mkdir(parents=True, exist_ok=True)
        stamp = str(time.time_ns())
        if app is not None:
            (proof / f"retry-{stamp}.txt").write_text(app.text())
            (proof / f"retry-{stamp}.bin").write_bytes(bytes(app.transcript))
        if archive.exists():
            (proof / f"retry-{stamp}.jsonl").write_bytes(archive.read_bytes())
        raise
    finally:
        if app is not None:
            try:
                stop(app)
            except Exception:
                if app.process.poll() is None:
                    app.process.kill()
                    app.process.wait(timeout=5)
        release_scratch(root)


if __name__ == "__main__":
    run(pathlib.Path(sys.argv[1]).resolve())
    print("Automatic setup retry PTY passed: native rejection, feedback, reviewed repair, raw bytes preserved")
