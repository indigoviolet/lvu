#!/usr/bin/env python3
"""Opt-in real-provider acceptance; never part of the deterministic PTY matrix.

Run through mise/uv with an explicit binary, authenticated provider and durable
proof directory. All log data and capture state belong to this controlled probe.
The retained fixture path permits inspecting the actual owned conversations.
"""

import argparse
import hashlib
import json
import os
import pathlib
import re
import shutil
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


def derived_views(root):
    with sqlite3.connect(f"file:{root / 'capture/workspace/workspace.sqlite3'}?mode=ro", uri=True) as connection:
        # Initial canonical-view persistence can still settle after raw rows
        # appear. Only derived publication is forbidden before review.
        return dict(connection.execute("select view_id, name from working_views where role = 'derived'").fetchall())


def saved_views(root):
    return len(derived_views(root))


def review(app, proof, label, root, require_timestamp=False, expect_timeout=False):
    before_views = derived_views(root)
    before = len(before_views)
    choose(app, "Setup status")
    screen = app.wait_until(
        lambda text: "automatic setup: review" in text or "automatic setup: unavailable" in text
        or "no useful automatic setup" in text,
        "real proposal ready for review or actionable failure", timeout=160,
    )
    (proof / f"{label}-review.txt").write_text(screen)
    assert "automatic setup: review" in screen, screen
    session = re.search(r"Paseo session: [0-9a-f-]{36}", screen)
    assert session, screen
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
    if expect_timeout:
        app.wait_for("setup: unavailable", timeout=100)
        choose(app, "Setup status")
        failed = app.wait_until(lambda text: "Automatic setup status" in text and "compiler timed out" in text,
                                "failure inspector with compiler detail", timeout=10)
        assert session.group(0) in failed, "failed setup lost its inspectable session: " + failed
        assert "cold start" in " ".join(failed.split()), failed
        assert saved_views(root) == before, "failed compiler published a derived view"
        (proof / f"{label}-compiler-timeout.txt").write_text(failed)
        app.send(b"\x1b")
        app.wait_until(lambda text: "Automatic setup status" not in text, "failure inspector closed", timeout=5)
        return
    settled = app.wait_until(lambda text: ("setup: applied" in text and "Enhanced" in text) or "setup: unavailable" in text,
                             "native validation settled after explicit Apply", timeout=100)
    if "setup: unavailable" in settled:
        choose(app, "Setup status")
        failed = app.wait_until(lambda text: "Automatic setup status" in text and "automatic setup: unavailable" in text,
                                "native rejection inspector", timeout=10)
        (proof / f"{label}-native-rejection.txt").write_text(failed)
        raise AssertionError("real provider proposal failed native validation:\n" + failed)
    new_views = {key: name for key, name in derived_views(root).items() if key not in before_views}
    assert len(new_views) == 1, new_views
    name = next(iter(new_views.values()))
    # Analyze again from an older Enhanced view deliberately preserves focus.
    # Select the newly published view by its visible unique name before
    # inspecting values, instead of mistaking the old view's refresh for it.
    def sidebar_row(text):
        for y, line in enumerate(text.splitlines()):
            if line.startswith("│") and line[1:21].strip().lstrip("›").strip() == name:
                return y
        return None
    screen = app.wait_until(lambda text: sidebar_row(text) is not None, "new derived view visible", timeout=10)
    y = sidebar_row(screen)
    app.send(f"\x1b[<0;9;{y + 1}M\x1b[<0;9;{y + 1}m".encode())
    screen = app.wait_until(
        lambda text: "severity" in text and "ERROR" in text and "database failed" in text
        and (not require_timestamp or re.search(r"07:00:01(?:\.\d+)?Z\s+ERROR", text)),
        "accepted output values in the newly published view", timeout=25,
    )
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
    docker_invocations = root / "docker-invocations.jsonl"
    if args.docker_fixture:
        project = root / "compose-project"
        project.mkdir()
        compose = project / "compose.yaml"
        compose.write_text("services: {api: {image: controlled-fixture}}\n")
        bin_dir = root / "bin"
        bin_dir.mkdir()
        row = {"ID": "controlled-api-id", "Names": "controlled-api-1", "Image": "controlled-fixture",
               "State": "running", "Status": "Up", "ComposeProject": "lvu-acceptance", "ComposeService": "api",
               "ComposeReplica": "1", "ComposeOneoff": "False", "ComposeWorkingDir": str(project),
               "ComposeConfigFiles": str(compose)}
        docker = bin_dir / "docker"
        docker.write_text(
            "#!/usr/bin/env python3\nimport json,pathlib,sys,time\na=sys.argv[1:]\n"
            "if a[:2]==['context','show']: print('default')\n"
            f"elif a[:2]==['ps','--all']: print({json.dumps(row)!r})\n"
            "elif a and a[0]=='compose':\n"
            f"    with pathlib.Path({str(docker_invocations)!r}).open('a') as f: f.write(json.dumps(a)+'\\n')\n"
            f"    payload=''.join('api-1 | '+line+'\\n' for line in pathlib.Path({str(source)!r}).read_text().splitlines())\n"
            "    sys.stdout.write(payload); sys.stdout.flush()\n"
            "    while True: time.sleep(30)\n"
            "else: sys.exit(44)\n"
        )
        docker.chmod(0o755)
        env["PATH"] = str(bin_dir) + os.pathsep + os.environ.get("PATH", "")
        env["DOCKER_HOST"] = "tcp://controlled-fixture.invalid:2376"
    timeout_marker = None
    if args.compiler_timeout_first:
        # Only the owned copy has a fault injector; the real bundled helper
        # and its locked dependencies execute unchanged after explicit retry.
        helper = proof / "helper-with-startup-delay"
        shutil.copytree(args.compiler_timeout_first.resolve(), helper,
                        ignore=shutil.ignore_patterns(".venv", "__pycache__", "*.pyc"))
        package = helper / "lvu_expr_helper"
        shutil.copyfile(package / "__main__.py", package / "_probe_real_main.py")
        timeout_marker = proof / "inject-compiler-timeout"
        timeout_marker.write_text("owned deterministic startup fault\n")
        (package / "__main__.py").write_text(
            "import pathlib, runpy, sys, time\n"
            f"if pathlib.Path({str(timeout_marker)!r}).exists():\n"
            "    print('controlled startup delay for timeout acceptance', file=sys.stderr, flush=True)\n"
            "    time.sleep(90)\n"
            "runpy.run_module('lvu_expr_helper._probe_real_main', run_name='__main__')\n"
        )
        env["LVU_PYTHON_HELPER_DIR"] = str(helper)
    apps = []
    result = {"binary": str(args.binary.resolve()), "binary_sha256": hashlib.sha256(args.binary.read_bytes()).hexdigest(),
              "provider": args.provider, "fixture": str(root), "multiwindow": not args.single_window,
              "raw_timestamps": args.raw_timestamps, "compiler_timeout_retry": timeout_marker is not None,
              "cancel_analysis_retry": args.cancel_analysis,
              "source_kind": "docker-compose-fixture" if args.docker_fixture else "file",
              "restart_mode": "stopped-command-no-launch" if args.docker_fixture else "file-resume",
              "passed": False}

    launches = 0
    def launch(expect_stopped=False):
        nonlocal launches
        first = launches == 0
        launches += 1
        sources = [] if args.docker_fixture else [str(source)]
        app = PtyApp(args.binary.resolve(), [*sources, "--capture-dir", str(root / "capture")],
                     width=180, height=48, environment=env)
        apps.append(app)
        if args.docker_fixture and first:
            app.send(b" ")
            app.wait_for("Add source", timeout=10)
            app.send(b"\x1b")
            app.send(b"n")
            app.wait_for("Add source", timeout=5)
            app.send(b"\x04")
            app.wait_for("Sources", timeout=10)
            app.wait_until(lambda text: "scanning" not in text, "controlled Docker discovery settled", timeout=20)
            app.send(b"lvu-acceptance/api")
            app.wait_for("› lvu-acceptance/api", timeout=10)
            app.send(b"\x1bo")  # Explicit Open action, independent of filter-field focus.
        if expect_stopped:
            app.wait_for("lvu-acceptance/api", timeout=20)
            app.wait_for("not acquiring", timeout=20)
        else:
            app.wait_for("database failed", timeout=20)
            app.wait_for("retrying", timeout=20)
        return app

    try:
        first = launch()
        before = journals(root)
        assert before
        if timeout_marker is not None:
            review(first, proof, "injected-failure", root, args.raw_timestamps, expect_timeout=True)
            assert journals(root) == before, "failed compiler changed capture bytes"
            timeout_marker.unlink()  # Only this probe's explicitly created fault marker.
            choose(first, "Analyze again")
        review(first, proof, "first", root, args.raw_timestamps)
        if not args.single_window:
            # Keep window one open while a second managed request acquires ownership.
            assert first.process.poll() is None, "first window closed before multiwindow acceptance"
            second = launch()
            choose(second, "Analyze again")
            review(second, proof, "second", root, args.raw_timestamps)
            assert first.process.poll() is None, "first window must remain open"
            if args.cancel_analysis:
                choose(first, "Analyze again")
                choose(first, "Setup status")
                running = first.wait_until(
                    lambda text: "automatic setup: analyzing" in text
                    and re.search(r"Paseo session: [0-9a-f-]{36}", text),
                    "real analysis session active before q", timeout=30,
                )
                (proof / "analysis-before-quit.txt").write_text(running)
                first.send(b"\x1b")
                first.wait_until(lambda text: "Automatic setup status" not in text, "inspector dismissed before q", timeout=5)
                first.send(b"q")
                assert first.wait_exit(timeout=12) == 0
                first.assert_restored()
                (proof / "quit-during-analysis.bin").write_bytes(bytes(first.transcript))
                first.close()
                apps.remove(first)
                choose(second, "Analyze again")
                review(second, proof, "after-other-window-quit", root, args.raw_timestamps)
        assert journals(root) == before, "assistance replayed or changed captured bytes"
        result["sessions"] = sessions(root)
        assert len(result["sessions"]) >= (1 if args.single_window else 2)
        for index, app in enumerate(apps):
            stop(app)
            (proof / f"window-{index}.bin").write_bytes(bytes(app.transcript))
        apps.clear()
        reopened = launch(expect_stopped=args.docker_fixture)
        if args.docker_fixture:
            assert derived_views(root), "stopped-command restore lost its saved Enhanced definition"
        else:
            reopened.wait_for("Enhanced", timeout=30)
        time.sleep(2)
        assert sessions(root) == result["sessions"], "restore started another automatic request"
        assert journals(root) == before, "reopening replayed captured input"
        if args.docker_fixture:
            calls = docker_invocations.read_text().splitlines()
            assert len(calls) == 1, "Docker follow command was replayed: " + repr(calls)
            shutil.copyfile(docker_invocations, proof / "docker-invocations.jsonl")
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
    parser.add_argument("--compiler-timeout-first", type=pathlib.Path, metavar="BUNDLED_HELPER_DIR",
                        help="copy this helper, inject one startup timeout, then prove real-helper retry")
    parser.add_argument("--cancel-analysis", action="store_true", help="quit during real analysis, then retry from the other open window")
    parser.add_argument("--docker-fixture", action="store_true", help="open a controlled Docker Compose service through real discovery; no live daemon claimed")
    args = parser.parse_args()
    if args.cancel_analysis and args.single_window:
        parser.error("--cancel-analysis needs the default two-window exercise")
    run(args)
