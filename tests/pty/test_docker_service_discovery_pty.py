#!/usr/bin/env python3
"""Docker discovery starts both a Compose aggregate and its container source."""

from __future__ import annotations

import json
import os
import pathlib
import sys
import tempfile

from test_enrichment_chain_pty import stop
from test_lvu_pty import PtyApp, isolated_environment


def discover(app: PtyApp, query: bytes, expected: str) -> None:
    app.send(b"n")
    app.wait_for("Add source")
    app.send(b"\x04")
    app.wait_for("Candidates")
    app.wait_until(lambda text: "scanning" not in text, "Docker discovery settled", timeout=15)
    app.send(query)
    app.wait_for(expected)
    app.send(b"\r")


def run(binary: pathlib.Path) -> None:
    with tempfile.TemporaryDirectory(prefix="lvu-docker-service-pty-") as directory:
        root = pathlib.Path(directory)
        project = root / "compose-project"
        project.mkdir()
        compose = project / "compose.yaml"
        compose.write_text("services: {api: {image: example}}\n")
        invocations = root / "invocations"
        bin_dir = root / "bin"
        bin_dir.mkdir()
        docker = bin_dir / "docker"
        row = json.dumps(
            {
                "ID": "api-one-id",
                "Names": "pty-api-1",
                "Image": "example",
                "State": "running",
                "Status": "Up",
                "ComposeProject": "pty",
                "ComposeService": "api",
                "ComposeReplica": "1",
                "ComposeOneoff": "False",
                "ComposeWorkingDir": str(project),
                "ComposeConfigFiles": str(compose),
            },
            separators=(",", ":"),
        )
        docker.write_text(
            "#!/bin/sh\n"
            "if [ \"$1\" = context ] && [ \"$2\" = show ]; then\n"
            "  printf 'default\\n'\n"
            "elif [ \"$1\" = ps ] && [ \"$2\" = --all ]; then\n"
            f"  printf '%s\\n' '{row}'\n"
            "elif [ \"$1\" = compose ]; then\n"
            f"  printf 'compose:%s\\n' \"$*\" >> '{invocations}'\n"
            "  printf 'service-aggregate-row\\n'\n"
            "elif [ \"$1\" = logs ]; then\n"
            f"  printf 'container:%s\\n' \"$*\" >> '{invocations}'\n"
            "  printf 'individual-container-row\\n'\n"
            "else\n"
            "  exit 44\n"
            "fi\n"
        )
        docker.chmod(0o755)

        env = isolated_environment(root)
        env["PATH"] = str(bin_dir) + os.pathsep + os.environ.get("PATH", "")
        # The fixture models environment-routed Docker. Any accidental
        # `--context default` makes its ps branch reject the invocation.
        env["DOCKER_HOST"] = "tcp://fixture.invalid:2376"
        env.pop("DOCKER_CONTEXT", None)
        app = PtyApp(
            binary,
            ["--capture-dir", str(root / "capture")],
            width=120,
            height=30,
            environment=env,
        )
        try:
            # Dismiss the sourceless startup surface into Add source, then use
            # the real Discover interaction and explicitly open the aggregate.
            app.send(b" ")
            app.wait_for("Add source")
            app.send(b"\x1b")
            discover(app, b"Docker service", "pty/api (Docker service)")
            app.wait_for("service-aggregate-row", timeout=20)

            # Rescan and open the retained per-container candidate too.
            discover(app, b"#1", "pty/api #1 (Docker)")
            app.wait_for("individual-container-row", timeout=20)
            recorded = invocations.read_text().splitlines()
            assert any(
                "compose --project-name pty" in line
                and "logs --follow --timestamps --tail 200 api" in line
                for line in recorded
            ), recorded
            assert any(
                line.endswith("logs --follow --timestamps --tail 200 api-one-id")
                for line in recorded
            ), recorded
            assert all("--context" not in line for line in recorded), recorded
            stop(app)
        finally:
            if app.process.poll() is None:
                app.process.kill()
                app.process.wait(timeout=5)
                app.close()


if __name__ == "__main__":
    run(pathlib.Path(sys.argv[1]).resolve())
    print("Docker Compose service discovery PTY passed")
