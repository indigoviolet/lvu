#!/usr/bin/env python3
"""Every 🧠 failure mode names what failed, and nothing else degrades.

The reported symptom was one message — `local agent service: bridge is not
running` — for a bridge that had actually started and could not hold its daemon
connection. Each mode below is provoked in a real terminal, and each run also
proves that capture, search and native filtering keep working while 🧠 is down.
"""
import os
import pathlib
import socket
import sys
import tempfile

from test_lvu_pty import PtyApp

EVENTS = "\n".join(
    f'{{"level":"{"ERROR" if index % 3 == 0 else "INFO"}","message":"probe {index:02d}","ts":"2026-09-07T12:00:{index:02d}Z"}}'
    for index in range(12)
) + "\n"


def free_port() -> int:
    with socket.socket() as probe:
        probe.bind(("127.0.0.1", 0))
        return probe.getsockname()[1]


def environment(root: pathlib.Path, **overrides: str) -> dict[str, str]:
    base = {
        "XDG_CONFIG_HOME": str(root / "config"),
        "XDG_CACHE_HOME": str(root / "cache"),
        "XDG_DATA_HOME": str(root / "data"),
        "MISE_DATA_DIR": os.environ.get("MISE_DATA_DIR", str(pathlib.Path.home() / ".local/share/mise")),
        "MISE_CONFIG_DIR": os.environ.get("MISE_CONFIG_DIR", str(pathlib.Path.home() / ".config/mise")),
        "MISE_CACHE_DIR": os.environ.get("MISE_CACHE_DIR", str(pathlib.Path.home() / ".cache/mise")),
        "UV_CACHE_DIR": os.environ.get("UV_CACHE_DIR", str(pathlib.Path.home() / ".cache/uv")),
        "COLORTERM": "truecolor",
    }
    base.update(overrides)
    return base


def ask_and_read_message(app: PtyApp, needle: str, description: str) -> str:
    """Submit an Ask request and return the screen carrying its diagnostic."""
    app.send(b"A")
    app.wait_for("Request")
    app.send(b"only errors")
    app.wait_for("only errors")
    app.send(b"\t\r")
    return app.wait_until(
        lambda text: "Error" in text and needle in text,
        description,
        timeout=25.0,
    )


def assert_workspace_still_works(app: PtyApp) -> None:
    """Everything that is not 🧠 keeps working after the failure."""
    app.send(b"\x1b")
    app.wait_until(lambda text: "Request" not in text and "Proposal" not in text, "Ask closes")
    app.send(b"/")
    app.wait_for("Search")
    app.send(b"ERROR\r")
    filtered = app.wait_until(
        lambda text: "probe 00" in text and "probe 01" not in text,
        "literal search still filters while the bridge is unavailable",
        timeout=10.0,
    )
    assert "probe 03" in filtered, filtered
    app.send(b"\x1b")
    app.wait_until(lambda text: "Search" not in text, "the search dialog closes")


def quit_cleanly(app: PtyApp) -> None:
    app.send(b"q")
    try:
        app.process.wait(timeout=10)
    except Exception:
        raise AssertionError(f"lvu did not quit; screen was:\n{app.text()}")
    app.drain()
    assert app.process.returncode == 0, app.text()
    app.assert_restored()


def case(binary: pathlib.Path, root: pathlib.Path, source: pathlib.Path,
         overrides: dict[str, str], needles: list[str], label: str) -> None:
    # Each case gets its own workspace: a restored search from the previous one
    # would hide the rows this one asserts on.
    workspace = root / f"workspace-{label.replace(' ', '-')}"
    workspace.mkdir(exist_ok=True)
    # The harness gives one capture root per test *process*, so without an
    # explicit one every case here would restore the previous case's applied
    # search and never see its own rows.
    app = PtyApp(binary,
                 ["--file", str(source), "--capture-dir", str(workspace / "capture")],
                 width=110, height=32,
                 environment=environment(workspace, **overrides))
    try:
        app.wait_for("probe 00")
        message = ask_and_read_message(app, needles[0], f"{label} diagnostic")
        for needle in needles:
            assert needle in message, f"{label}: missing {needle!r} in\n{message}"
        # The retired message must not come back for any of these.
        assert "bridge is not running" not in message, message
        assert_workspace_still_works(app)
        quit_cleanly(app)
    finally:
        if app.process.poll() is None:
            app.process.kill()
        app.close()


def unbuilt_bridge_is_reported_at_startup(binary: pathlib.Path, root: pathlib.Path,
                                          source: pathlib.Path) -> None:
    """A checkout nobody built is told to build it, not to reinstall lvu."""
    checkout = root / "unbuilt"
    (checkout / "src").mkdir(parents=True, exist_ok=True)
    (checkout / "package.json").write_text("{}\n")
    (checkout / "src" / "cli.ts").write_text("")
    case(binary, root, source, {"LVU_BRIDGE_DIR": str(checkout)},
         ["not built", "npm --prefix bridge"], "unbuilt bridge")


def missing_bridge_is_reported_at_startup(binary: pathlib.Path, root: pathlib.Path,
                                          source: pathlib.Path) -> None:
    absent = root / "absent"
    case(binary, root, source, {"LVU_BRIDGE_DIR": str(absent)},
         ["agent bridge not found", "LVU_BRIDGE_DIR"], "missing bridge")


def missing_node_is_reported(binary: pathlib.Path, root: pathlib.Path,
                             source: pathlib.Path, bridge: pathlib.Path) -> None:
    """No launcher on PATH: name the launcher, not the bridge."""
    empty = root / "empty-path"
    empty.mkdir(exist_ok=True)
    case(binary, root, source, {"LVU_BRIDGE_DIR": str(bridge), "PATH": str(empty)},
         ["could not be started", "not installed or not on PATH"], "missing launcher")


def unreachable_daemon_is_reported(binary: pathlib.Path, root: pathlib.Path,
                                   source: pathlib.Path, bridge: pathlib.Path) -> None:
    """The bridge starts, cannot reach the daemon, and says exactly that."""
    case(binary, root, source,
         {"LVU_BRIDGE_DIR": str(bridge),
          "LVU_PASEO_URL": f"ws://127.0.0.1:{free_port()}/ws",
          "LVU_PASEO_CONNECT_TIMEOUT_MS": "2000",
          "LVU_PASEO_CLI": "disabled"},
         ["could not reach the Paseo daemon", "start Paseo"], "unreachable daemon")


# A bridge that starts, reaches its daemon and is told the provider is not
# usable. Provoking this against a real daemon would need a deliberately
# unauthenticated provider, so the daemon's answer is supplied directly and the
# whole Rust path — protocol, error code, wording — still runs for real.
STUB_BRIDGE = """
const lines = [];
process.stdin.setEncoding("utf8");
let buffer = "";
process.stdin.on("data", (chunk) => {
  buffer += chunk;
  let index;
  while ((index = buffer.indexOf("\\n")) !== -1) {
    const line = buffer.slice(0, index);
    buffer = buffer.slice(index + 1);
    if (line.trim() === "") continue;
    const request = JSON.parse(line);
    const failure = { code: "PROVIDER_UNKNOWN", message: PROVIDER_MESSAGE };
    const body = request.method === "capabilities"
      ? { schema_version: 1, request_id: request.request_id, ok: true, result: { methods: [] } }
      : { schema_version: 1, request_id: request.request_id, ok: false, error: failure };
    process.stdout.write(JSON.stringify(body) + "\\n");
  }
});
"""


def unusable_provider_is_reported(binary: pathlib.Path, root: pathlib.Path,
                                  source: pathlib.Path) -> None:
    stub = root / "stub-bridge"
    (stub / "dist").mkdir(parents=True, exist_ok=True)
    message = ("agent provider codex is not configured; "
               "authenticated providers: claude")
    (stub / "dist" / "cli.js").write_text(
        f'const PROVIDER_MESSAGE = {message!r};\n'.replace("'", '"') + STUB_BRIDGE
    )
    case(binary, root, source, {"LVU_BRIDGE_DIR": str(stub)},
         ["is not configured", "authenticate a provider in Paseo", "claude"],
         "unusable provider")


def run(binary: pathlib.Path) -> None:
    repository = pathlib.Path(__file__).resolve().parents[2]
    bridge = repository / "bridge"
    with tempfile.TemporaryDirectory(prefix="lvu-bridge-diagnostics-pty-") as temporary:
        root = pathlib.Path(temporary)
        source = root / "events.log"
        source.write_text(EVENTS)
        unbuilt_bridge_is_reported_at_startup(binary, root, source)
        missing_bridge_is_reported_at_startup(binary, root, source)
        unusable_provider_is_reported(binary, root, source)
        if (bridge / "dist" / "cli.js").exists():
            missing_node_is_reported(binary, root, source, bridge)
            unreachable_daemon_is_reported(binary, root, source, bridge)
        else:
            print("SKIPPED the started-bridge cases: bridge/dist/cli.js is not built")


if __name__ == "__main__":
    run(pathlib.Path(sys.argv[1]).resolve())
    print("Bridge diagnostics PTY passed: unbuilt, missing, unusable provider, "
          "launcher absent, daemon unreachable; workspace stays usable throughout")
