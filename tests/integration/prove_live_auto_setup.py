#!/usr/bin/env python3
"""Opt-in real-provider acceptance; never part of the deterministic PTY matrix.

Run through mise/uv with an explicit binary, authenticated provider and durable
proof directory. All log data and capture state belong to this controlled probe.
The retained fixture path permits inspecting the actual owned conversations.
"""

import argparse
import hashlib
import json
import pathlib
import re
import sqlite3
import sys
import tempfile
import time

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1] / "pty"))
from test_lvu_pty import PtyApp, isolated_environment
from test_enrichment_chain_pty import stop
from test_auto_setup_pty import SETTINGS


def choose(app, label):
    app.send(b"\x10")
    app.wait_for("Command palette", timeout=5)
    app.send(label.encode())
    app.wait_for(f"Current log › {label}", timeout=5)
    app.send(b"\r")


def sessions(root):
    return sorted(path.name for path in (root / "capture/assistance/agents").glob("*.json"))


def journals(root):
    # Raw bytes/stable identities are a capture invariant, not query evaluation.
    return {str(p.relative_to(root)): hashlib.sha256(p.read_bytes()).hexdigest()
            for p in (root / "capture").glob("*/capture.journal")}


def saved_views(root):
    with sqlite3.connect(f"file:{root / 'capture/workspace/workspace.sqlite3'}?mode=ro", uri=True) as connection:
        # Initial canonical-view persistence can still settle after raw rows
        # appear. Only derived publication is forbidden before review.
        return connection.execute("select count(*) from working_views where role = 'derived'").fetchone()[0]


def review(app, proof, label, root, require_timestamp=False):
    before = saved_views(root)
    choose(app, "Setup status")
    screen = app.wait_until(
        lambda text: "automatic setup: review" in text or "automatic setup: unavailable" in text
        or "no useful automatic setup" in text,
        "real proposal ready for review or actionable failure", timeout=160,
    )
    (proof / f"{label}-review.txt").write_text(screen)
    assert "automatic setup: review" in screen, screen
    assert re.search(r"Paseo session: [0-9a-f-]{36}", screen), screen
    assert "Apply" in screen and "severity" in screen, screen
    if require_timestamp:
        assert "timestamp_utc" in screen, screen
    assert saved_views(root) == before, "proposal changed durable views before review"
    # Close/reopen must retain the candidate without creating a derived view.
    app.send(b"\x1b")
    app.wait_until(lambda text: "Automatic setup status" not in text, "review closed", timeout=5)
    choose(app, "Setup status")
    app.wait_for("automatic setup: review", timeout=5)
    assert saved_views(root) == before, "closing review applied the proposal"
    app.send(b"\x1bp")  # Apply mnemonic in the explicit review surface.
    app.wait_until(lambda text: "setup: applied" in text and "Enhanced" in text,
                   "native enrichment committed after explicit Apply", timeout=100)
    app.send(b"\x1b")
    screen = app.wait_for("database failed", timeout=10)
    (proof / f"{label}-applied.txt").write_text(screen)
    assert "severity" in screen and "ERROR" in screen, screen
    if require_timestamp:
        assert re.search(r"07:00:01(?:\.\d+)?Z\s+ERROR", screen), "timestamp role has no accepted event-time gutter: " + screen


def run(args):
    proof = args.proof.resolve()
    proof.mkdir(parents=True, exist_ok=False)
    root = pathlib.Path(tempfile.mkdtemp(prefix="lvu-primary-live-"))
    (root / ".lvu-test-reproducer").write_text("owned live-provider acceptance fixture\n")
    (proof / "fixture-path.txt").write_text(str(root))
    source = root / "service.log"
    source.write_text(
        "2026-09-24T07:00:00Z [INFO] api request a service ready\n"
        "2026-09-24T07:00:01Z [ERROR] api request b database failed\n"
        "2026-09-24T07:00:02Z [WARN] api request c retrying\n"
        if args.raw_timestamps else
        "level=INFO request=a service ready\nlevel=ERROR request=b database failed\nlevel=WARN request=c retrying\n"
    )
    config = root / "config/lvu"
    config.mkdir(parents=True)
    (config / "settings.toml").write_text(SETTINGS.replace('"fixture/provider"', json.dumps(args.provider)))
    env = {**isolated_environment(root), "LVU_PTY_ALLOW_DEFAULT_AUTOMATIC_SETUP": "1", "LVU_NO_DELIGHT": "1"}
    apps = []
    result = {"binary": str(args.binary.resolve()), "binary_sha256": hashlib.sha256(args.binary.read_bytes()).hexdigest(),
              "provider": args.provider, "fixture": str(root), "multiwindow": not args.single_window,
              "raw_timestamps": args.raw_timestamps, "passed": False}

    def launch():
        app = PtyApp(args.binary.resolve(), [str(source), "--capture-dir", str(root / "capture")],
                     width=180, height=48, environment=env)
        apps.append(app)
        app.wait_for("database failed", timeout=20)
        return app

    try:
        first = launch()
        before = journals(root)
        assert before
        review(first, proof, "first", root, args.raw_timestamps)
        if not args.single_window:
            # Keep window one open while a second managed request acquires ownership.
            second = launch()
            choose(second, "Analyze again")
            review(second, proof, "second", root, args.raw_timestamps)
            assert first.process.poll() is None, "first window must remain open"
        assert journals(root) == before, "assistance replayed or changed captured bytes"
        result["sessions"] = sessions(root)
        assert len(result["sessions"]) >= (1 if args.single_window else 2)
        for index, app in enumerate(apps):
            stop(app)
            (proof / f"window-{index}.bin").write_bytes(bytes(app.transcript))
        apps.clear()
        reopened = launch()
        reopened.wait_for("Enhanced", timeout=30)
        time.sleep(2)
        assert sessions(root) == result["sessions"], "restore started another automatic request"
        assert journals(root) == before, "reopening replayed captured input"
        stop(reopened)
        (proof / "reopened.bin").write_bytes(bytes(reopened.transcript))
        apps.clear()
        result["passed"] = True
    finally:
        for index, app in enumerate(apps):
            app.drain()
            (proof / f"failure-{index}.txt").write_text(app.text())
            (proof / f"failure-{index}.bin").write_bytes(bytes(app.transcript))
            try:
                stop(app)
            except Exception:
                if app.process.poll() is None:
                    app.process.terminate()
                    try:
                        app.process.wait(timeout=5)
                    except Exception:
                        app.process.kill()
                        app.process.wait(timeout=5)
        (proof / "result.json").write_text(json.dumps(result, indent=2) + "\n")
        print(json.dumps(result), flush=True)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=pathlib.Path)
    parser.add_argument("--provider", required=True)
    parser.add_argument("--proof", required=True, type=pathlib.Path)
    parser.add_argument("--raw-timestamps", action="store_true")
    parser.add_argument("--single-window", action="store_true", help="partial diagnosis only; skips concurrent-window acceptance")
    run(parser.parse_args())
