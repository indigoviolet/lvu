#!/usr/bin/env python3
"""Durable state survives restart from the same working directory.

Regression for the silent data-loss defect: the assistance bridge materialised
`$XDG_DATA_HOME/lvu` about 1.7 s after startup, so a working directory holding a
legacy `.lvu-captures` root selected it on the first launch and the XDG root on
the next one. Sources, the workspace, accepted filters and saved views were
abandoned without an error, and the same file was recaptured under a new source
identity in a second workspace.

Each scenario therefore runs long enough for the bridge to start, quits, and
relaunches from the same working directory. `--capture-dir` is deliberately not
used: pinning the root by hand is what hides this bug.
"""
import os
import pathlib
import sys
import tempfile
import time

from test_lvu_pty import PtyApp


RECORDS = "alpha one\nbeta two\ngamma three\n"
# The bridge materialises its root about 1.7 s in; a shorter session cannot
# observe the flip at all.
BRIDGE_SETTLE = 2.5


def tooling_environment():
    home = pathlib.Path.home()
    environment = {}
    for key, xdg, fallback, suffix in [
        ("MISE_DATA_DIR", "XDG_DATA_HOME", home / ".local/share", "mise"),
        ("MISE_CONFIG_DIR", "XDG_CONFIG_HOME", home / ".config", "mise"),
        ("MISE_CACHE_DIR", "XDG_CACHE_HOME", home / ".cache", "mise"),
        ("UV_CACHE_DIR", "XDG_CACHE_HOME", home / ".cache", "uv"),
    ]:
        environment[key] = os.environ.get(
            key, str(pathlib.Path(os.environ.get(xdg, fallback)) / suffix)
        )
    return environment


def workspace_roots(*roots):
    return sorted(str(root) for root in roots if (root / "workspace").is_dir())


def stop(app):
    if app.process.poll() is None:
        app.send(b"q")
        code = app.wait_exit(timeout=10)
        assert code == 0, (code, app.text(), bytes(app.transcript[-4000:]))
    app.assert_restored()
    app.close()


def launch(binary, source, cwd, environment, width=170, height=26):
    app = PtyApp(binary, [str(source)], width=width, height=height, cwd=str(cwd),
                 environment=environment)
    # A restored view may already be filtered, so wait on capture readiness
    # rather than on a record the accepted search excludes.
    app.wait_for("Running: 3 records", timeout=15)
    return app


def apply_search(app):
    app.send(b"/")
    app.wait_for("Search")
    app.send(b"beta")
    # A draft on All events applies only when it is submitted.
    app.send(b"\r")
    app.wait_until(lambda text: "beta two" in text and "alpha one" not in text
                   and 'search:"beta"' in text, "search applied", timeout=8)
    app.send(b"\x1b")
    app.wait_until(lambda text: "Search" not in text, "search form closed")


def settle_bridge(app, candidates):
    """Waits for the assistance bridge to materialise its root, then some more.

    The flip needs a session that outlives the bridge start, so this is a
    readiness handshake rather than a fixed sleep. If no bridge is available in
    this environment the elapsed budget still exceeds the observed 1.7 s.
    """
    deadline = time.monotonic() + 12.0
    started = time.monotonic()
    materialised = None
    while time.monotonic() < deadline:
        app.drain()
        for candidate in candidates:
            if candidate.exists():
                materialised = candidate
                break
        if materialised is not None and time.monotonic() - started >= BRIDGE_SETTLE:
            return materialised
        time.sleep(0.05)
    assert time.monotonic() - started >= BRIDGE_SETTLE
    app.drain()
    return materialised


def scenario(binary, label, legacy_present):
    with tempfile.TemporaryDirectory(prefix="lvu-capture-root-") as directory:
        root = pathlib.Path(directory)
        cwd = root / "project"
        cwd.mkdir()
        source = root / "events.log"
        source.write_text(RECORDS)
        xdg_data = root / "data"
        environment = {**tooling_environment(),
                       "XDG_CONFIG_HOME": str(root / "config"),
                       "XDG_DATA_HOME": str(xdg_data),
                       "XDG_CACHE_HOME": str(root / "cache"),
                       "NO_COLOR": "", "COLORTERM": "truecolor"}
        legacy = cwd / ".lvu-captures"
        if legacy_present:
            legacy.mkdir()
        xdg_root = xdg_data / "lvu"

        app = launch(binary, source, cwd, environment)
        try:
            apply_search(app)
            if legacy_present:
                assert "legacy data directory" in app.text(), \
                    "the status line must state which root is in use and why"
            settle_bridge(app, [legacy / "assistance", xdg_root / "assistance"])
            first = workspace_roots(legacy, xdg_root)
            assert len(first) == 1, ("exactly one workspace after the first launch", first)
            stop(app)
        finally:
            if app.process.poll() is None:
                app.process.kill()
                app.process.wait()
                app.close()

        if legacy_present:
            assert first == [str(legacy)], ("a legacy working directory keeps its root", first)

        app = launch(binary, source, cwd, environment)
        try:
            # Restart opens on the source's unfiltered All events view; the
            # filter now lives in the view it created, which is restored beside
            # it and still carries the accepted search.
            app.wait_until(lambda text: "All events" in text and "beta" in text
                           and "alpha one" in text,
                           "both views restored from the same working directory",
                           timeout=15)
            app.send(b"]")
            app.wait_until(lambda text: 'search:"beta"' in text and "beta two" in text
                           and "alpha one" not in text,
                           "accepted search restored in the view that owns it",
                           timeout=15)
            settle_bridge(app, [legacy / "assistance", xdg_root / "assistance"])
            second = workspace_roots(legacy, xdg_root)
            assert second == first, \
                (f"{label}: restart must reuse the same root, not open a second workspace",
                 first, second)
            stop(app)
        finally:
            if app.process.poll() is None:
                app.process.kill()
                app.process.wait()
                app.close()
        if legacy_present:
            assert (legacy / "capture-root.toml").is_file(), \
                "the chosen root must be recorded so the decision is inspectable"
        print(f"capture root stable for {label}: {first[0]}")


def run(binary):
    scenario(binary, "a legacy .lvu-captures working directory", True)
    scenario(binary, "a clean working directory", False)
    print("Capture root PTY passed: recorded root, restored durable state, no second workspace")


if __name__ == "__main__":
    run(pathlib.Path(sys.argv[1]).resolve())
